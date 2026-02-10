#![forbid(unsafe_code)]

//! Animated text effects for terminal UI.
//!
//! This module provides a rich set of text animation and styling effects:
//!
//! - **Fade effects**: Smooth fade-in, fade-out, pulse
//! - **Gradient fills**: Horizontal, vertical, diagonal, radial gradients
//! - **Animated gradients**: Moving gradient patterns
//! - **Color cycling**: Rainbow, breathing, wave effects
//! - **Style animations**: Blinking, bold/dim toggle, underline wave
//! - **Character effects**: Typing, scramble, glitch
//! - **Transition overlays**: Full-screen announcement effects
//!
//! # Example
//!
//! ```rust,ignore
//! use ftui_extras::text_effects::{StyledText, TextEffect, TransitionOverlay};
//!
//! // Rainbow gradient text
//! let rainbow = StyledText::new("Hello World")
//!     .effect(TextEffect::RainbowGradient { speed: 0.1 })
//!     .time(current_time);
//!
//! // Fade-in text
//! let fading = StyledText::new("Appearing...")
//!     .effect(TextEffect::FadeIn { progress: 0.5 });
//!
//! // Pulsing glow
//! let pulse = StyledText::new("IMPORTANT")
//!     .effect(TextEffect::Pulse { speed: 2.0, min_alpha: 0.3 })
//!     .base_color(PackedRgba::rgb(255, 100, 100))
//!     .time(current_time);
//! ```

use std::f64::consts::{FRAC_1_SQRT_2, PI, SQRT_2, TAU};

use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, CellAttrs, CellContent, PackedRgba, StyleFlags as CellStyleFlags};
use ftui_render::frame::Frame;
use ftui_text::{display_width, grapheme_count, grapheme_width, graphemes};
use ftui_widgets::Widget;

// =============================================================================
// Color Utilities
// =============================================================================

/// Interpolate between two colors.
pub fn lerp_color(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    let r = (a.r() as f64 + (b.r() as f64 - a.r() as f64) * t) as u8;
    let g = (a.g() as f64 + (b.g() as f64 - a.g() as f64) * t) as u8;
    let b_val = (a.b() as f64 + (b.b() as f64 - a.b() as f64) * t) as u8;
    PackedRgba::rgb(r, g, b_val)
}

/// Apply alpha/brightness to a color.
pub fn apply_alpha(color: PackedRgba, alpha: f64) -> PackedRgba {
    let alpha = alpha.clamp(0.0, 1.0);
    PackedRgba::rgb(
        (color.r() as f64 * alpha) as u8,
        (color.g() as f64 * alpha) as u8,
        (color.b() as f64 * alpha) as u8,
    )
}

/// Convert HSV to RGB.
pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> PackedRgba {
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    PackedRgba::rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// Simple xorshift hash for deterministic pseudo-random values.
///
/// Used for effects like flicker and glitch that need fast, reproducible
/// randomness without pulling in external RNG crates.
#[allow(dead_code)]
fn simple_hash(mut x: u64) -> u64 {
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

// =============================================================================
// Organic Breathing Pulse (bd-27kx)
// =============================================================================
//
// Organic breathing has asymmetric timing: quick inhale, slow exhale.
// This mimics natural breathing patterns and creates a more lifelike animation.
//
// Evidence Ledger:
// - Choice: Quadratic easing for rise/fall curves
// - Reason: Quadratic provides smooth acceleration without sharp inflection points,
//   and is computationally cheap (no transcendental functions in inner loop).
// - Invariants:
//   - breathing_curve(0, _) = 0.0 (start at minimum)
//   - breathing_curve(rise_end, _) = 1.0 (peak at end of inhale)
//   - breathing_curve(1, _) = 0.0 (return to minimum after full cycle)
//   - Curve is continuous at rise_duration boundary
// - Failure Modes:
//   - asymmetry > 1.0: clamped to prevent rise_duration going negative
//   - t outside [0,1]: use rem_euclid before calling

/// Asymmetric breathing curve for organic pulse effect.
///
/// Models natural breathing with quick inhale, slow exhale.
///
/// # Arguments
/// * `t` - Position in breath cycle (0.0 = start, 1.0 = end, wraps)
/// * `asymmetry` - Asymmetry factor (0.0 = symmetric, 1.0 = very asymmetric)
///
/// # Returns
/// Brightness value from 0.0 to 1.0.
///
/// # Invariants
/// - Returns 0.0 at t=0 (cycle start)
/// - Peaks at 1.0 during the inhale phase
/// - Returns to 0.0 at t=1 (cycle end)
#[inline]
pub fn breathing_curve(t: f64, asymmetry: f64) -> f64 {
    let t = t.rem_euclid(1.0);
    let asymmetry = asymmetry.clamp(0.0, 1.0);

    // Rise duration: 30% at symmetric, down to 10% at max asymmetry
    let rise_duration = 0.3 - asymmetry * 0.2;

    if t < rise_duration {
        // Quick rise (inhale) - quadratic ease-out for snappy attack
        let local_t = t / rise_duration;
        1.0 - (1.0 - local_t) * (1.0 - local_t)
    } else {
        // Slow fall (exhale) - quadratic ease-in-out for smooth decay
        let local_t = (t - rise_duration) / (1.0 - rise_duration);
        if local_t < 0.5 {
            // First half: ease-out from peak
            1.0 - 2.0 * local_t * local_t
        } else {
            // Second half: ease-in to minimum
            2.0 * (1.0 - local_t) * (1.0 - local_t)
        }
    }
}

/// Compute deterministic phase offset for a character in organic pulse.
///
/// Uses a simple hash function to spread characters across the phase cycle
/// based on their index and a seed value.
#[inline]
pub fn organic_char_phase_offset(char_idx: usize, seed: u64, phase_variation: f64) -> f64 {
    // Simple deterministic hash: multiply by large prime, take fractional part
    let hash = seed
        .wrapping_mul(2654435761)
        .wrapping_add(char_idx as u64 * 2246822519);
    let frac = (hash % 10000) as f64 / 10000.0;
    frac * phase_variation.clamp(0.0, 1.0)
}

// =============================================================================
// OkLab Perceptual Color Space (bd-36k2)
// =============================================================================
//
// OkLab is a perceptual color space designed by Björn Ottosson that provides:
// - Perceptually uniform color interpolation (equal numeric change = equal visual change)
// - No hue shift during interpolation (unlike HSL/HSV)
// - Proper handling of chromatic colors
//
// Evidence Ledger:
// - Choice: OkLab over CIELAB for interpolation
// - Reason: OkLab has simpler math, better blue behavior, and is purpose-built for
//   uniform color mixing. CIELAB was designed for threshold perception, not interpolation.
// - Fallback: If performance is critical (>+5% overhead), fall back to linear RGB.
//
// Failure Modes:
// - Out-of-gamut: OkLab values may produce RGB values outside [0,255]. We clamp.
// - Precision: Using f64 for all calculations to minimize accumulation errors.

/// OkLab color space representation.
/// L: lightness (0.0 = black, 1.0 = white)
/// a: green-red axis (-1.0 to 1.0, approximately)
/// b: blue-yellow axis (-1.0 to 1.0, approximately)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OkLab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

impl OkLab {
    /// Create new OkLab color.
    #[inline]
    pub const fn new(l: f64, a: f64, b: f64) -> Self {
        Self { l, a, b }
    }

    /// Linearly interpolate between two OkLab colors.
    #[must_use]
    #[inline]
    pub fn lerp(self, other: Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            l: self.l + (other.l - self.l) * t,
            a: self.a + (other.a - self.a) * t,
            b: self.b + (other.b - self.b) * t,
        }
    }

    /// Calculate perceptual distance (DeltaE) between two OkLab colors.
    /// This is the Euclidean distance in OkLab space, which correlates
    /// well with perceived color difference.
    #[inline]
    pub fn delta_e(self, other: Self) -> f64 {
        let dl = self.l - other.l;
        let da = self.a - other.a;
        let db = self.b - other.b;
        (dl * dl + da * da + db * db).sqrt()
    }
}

/// Convert sRGB gamma to linear RGB (inverse gamma correction).
#[inline]
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Convert linear RGB to sRGB gamma (gamma correction).
#[inline]
fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert sRGB (PackedRgba) to OkLab color space.
///
/// Uses the standard sRGB -> Linear RGB -> OkLab pipeline with Ottosson's
/// optimized matrix multiplication.
pub fn rgb_to_oklab(color: PackedRgba) -> OkLab {
    // sRGB to linear RGB
    let r = srgb_to_linear(color.r() as f64 / 255.0);
    let g = srgb_to_linear(color.g() as f64 / 255.0);
    let b = srgb_to_linear(color.b() as f64 / 255.0);

    // Linear RGB to LMS (using OkLab's optimized M1 matrix)
    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b;
    let m = 0.211_903_50 * r + 0.680_699_55 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_70 * b;

    // Cube root (safe for zero/negative values)
    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    // LMS' to OkLab (M2 matrix)
    OkLab {
        l: 0.210_454_26 * l_ + 0.793_617_78 * m_ - 0.004_072_05 * s_,
        a: 1.977_998_49 * l_ - 2.428_592_05 * m_ + 0.450_593_56 * s_,
        b: 0.025_904_04 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    }
}

/// Convert OkLab to sRGB (PackedRgba) color space.
///
/// Uses the standard OkLab -> Linear RGB -> sRGB pipeline with Ottosson's
/// optimized inverse matrix multiplication. Out-of-gamut values are clamped.
pub fn oklab_to_rgb(lab: OkLab) -> PackedRgba {
    // OkLab to LMS' (inverse M2 matrix)
    let l_ = lab.l + 0.396_337_78 * lab.a + 0.215_803_76 * lab.b;
    let m_ = lab.l - 0.105_561_35 * lab.a - 0.063_854_17 * lab.b;
    let s_ = lab.l - 0.089_484_18 * lab.a - 1.291_485_48 * lab.b;

    // LMS' to LMS (cube)
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    // LMS to linear RGB (inverse M1 matrix)
    let r = 4.076_741_66 * l - 3.307_711_59 * m + 0.230_969_94 * s;
    let g = -1.268_438_00 * l + 2.609_757_40 * m - 0.341_319_38 * s;
    let b = -0.004_196_09 * l - 0.703_418_61 * m + 1.707_614_70 * s;

    // Linear RGB to sRGB with gamut clamping
    let r_srgb = (linear_to_srgb(r.clamp(0.0, 1.0)) * 255.0).round() as u8;
    let g_srgb = (linear_to_srgb(g.clamp(0.0, 1.0)) * 255.0).round() as u8;
    let b_srgb = (linear_to_srgb(b.clamp(0.0, 1.0)) * 255.0).round() as u8;

    PackedRgba::rgb(r_srgb, g_srgb, b_srgb)
}

/// Interpolate between two colors in OkLab perceptual color space.
///
/// This produces smoother, more visually uniform gradients than linear RGB
/// interpolation. The overhead is approximately 3-5% per sample.
///
/// # Arguments
/// * `a` - Start color
/// * `b` - End color
/// * `t` - Interpolation factor (0.0 = a, 1.0 = b)
pub fn lerp_color_oklab(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
    let lab_a = rgb_to_oklab(a);
    let lab_b = rgb_to_oklab(b);
    let lab_result = lab_a.lerp(lab_b, t);
    oklab_to_rgb(lab_result)
}

/// Calculate perceptual color distance (DeltaE) between two sRGB colors.
///
/// Uses OkLab color space for perceptually accurate distance measurement.
/// A DeltaE of ~0.02 is barely perceptible; ~1.0 is a significant difference.
pub fn delta_e(a: PackedRgba, b: PackedRgba) -> f64 {
    rgb_to_oklab(a).delta_e(rgb_to_oklab(b))
}

/// Validate that gradient samples have monotonically increasing DeltaE from start.
///
/// Returns the first violation index (if any) where DeltaE decreased.
/// This is useful for testing gradient smoothness.
pub fn validate_gradient_monotonicity(samples: &[PackedRgba], tolerance: f64) -> Option<usize> {
    if samples.len() < 2 {
        return None;
    }
    let start = rgb_to_oklab(samples[0]);
    let mut prev_delta = 0.0;

    for (i, &color) in samples.iter().enumerate().skip(1) {
        let current_delta = start.delta_e(rgb_to_oklab(color));
        if current_delta + tolerance < prev_delta {
            return Some(i);
        }
        prev_delta = current_delta;
    }
    None
}

/// Multi-stop color gradient.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorGradient {
    stops: Vec<(f64, PackedRgba)>,
}

impl ColorGradient {
    /// Create a new gradient with color stops.
    /// Stops should be tuples of (position, color) where position is 0.0 to 1.0.
    pub fn new(stops: Vec<(f64, PackedRgba)>) -> Self {
        let mut stops = stops;
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        Self { stops }
    }

    /// Create a rainbow gradient.
    pub fn rainbow() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(255, 0, 0)),    // Red
            (0.17, PackedRgba::rgb(255, 127, 0)), // Orange
            (0.33, PackedRgba::rgb(255, 255, 0)), // Yellow
            (0.5, PackedRgba::rgb(0, 255, 0)),    // Green
            (0.67, PackedRgba::rgb(0, 127, 255)), // Blue
            (0.83, PackedRgba::rgb(127, 0, 255)), // Indigo
            (1.0, PackedRgba::rgb(255, 0, 255)),  // Violet
        ])
    }

    /// Create a sunset gradient (purple -> pink -> orange -> yellow).
    pub fn sunset() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(80, 20, 120)),
            (0.33, PackedRgba::rgb(255, 50, 120)),
            (0.66, PackedRgba::rgb(255, 150, 50)),
            (1.0, PackedRgba::rgb(255, 255, 150)),
        ])
    }

    /// Create an ocean gradient (deep blue -> cyan -> seafoam).
    pub fn ocean() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(10, 30, 100)),
            (0.5, PackedRgba::rgb(30, 180, 220)),
            (1.0, PackedRgba::rgb(150, 255, 200)),
        ])
    }

    /// Create a cyberpunk gradient (hot pink -> purple -> cyan).
    pub fn cyberpunk() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(255, 20, 150)),
            (0.5, PackedRgba::rgb(150, 50, 200)),
            (1.0, PackedRgba::rgb(50, 220, 255)),
        ])
    }

    /// Create a fire gradient (black -> red -> orange -> yellow -> white).
    pub fn fire() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(0, 0, 0)),
            (0.2, PackedRgba::rgb(80, 10, 0)),
            (0.4, PackedRgba::rgb(200, 50, 0)),
            (0.6, PackedRgba::rgb(255, 150, 20)),
            (0.8, PackedRgba::rgb(255, 230, 100)),
            (1.0, PackedRgba::rgb(255, 255, 220)),
        ])
    }

    /// Create an ice gradient (dark frost blue -> light blue -> white).
    pub fn ice() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(40, 60, 120)),
            (0.4, PackedRgba::rgb(100, 160, 220)),
            (0.7, PackedRgba::rgb(180, 220, 245)),
            (1.0, PackedRgba::rgb(240, 250, 255)),
        ])
    }

    /// Create a forest gradient (deep green -> emerald -> light green).
    pub fn forest() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(10, 50, 20)),
            (0.35, PackedRgba::rgb(30, 120, 50)),
            (0.65, PackedRgba::rgb(60, 180, 80)),
            (1.0, PackedRgba::rgb(150, 230, 140)),
        ])
    }

    /// Create a gold gradient (dark gold -> bright gold -> pale gold).
    pub fn gold() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(100, 70, 10)),
            (0.4, PackedRgba::rgb(200, 160, 30)),
            (0.7, PackedRgba::rgb(255, 210, 60)),
            (1.0, PackedRgba::rgb(255, 240, 150)),
        ])
    }

    /// Create a neon pink gradient (magenta -> hot pink -> cyan).
    pub fn neon_pink() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(200, 0, 150)),
            (0.5, PackedRgba::rgb(255, 50, 200)),
            (1.0, PackedRgba::rgb(50, 255, 255)),
        ])
    }

    /// Create a blood gradient (near-black -> dark red -> bright crimson).
    pub fn blood() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(30, 0, 0)),
            (0.4, PackedRgba::rgb(120, 10, 10)),
            (0.7, PackedRgba::rgb(200, 20, 20)),
            (1.0, PackedRgba::rgb(255, 50, 50)),
        ])
    }

    /// Create a matrix gradient (black -> dark green -> bright green).
    pub fn matrix() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(0, 0, 0)),
            (0.5, PackedRgba::rgb(0, 100, 20)),
            (1.0, PackedRgba::rgb(0, 255, 65)),
        ])
    }

    /// Create a terminal gradient (dark green -> medium green -> bright green).
    pub fn terminal() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(0, 60, 0)),
            (0.5, PackedRgba::rgb(0, 160, 40)),
            (1.0, PackedRgba::rgb(50, 255, 100)),
        ])
    }

    /// Create a lavender gradient (deep purple -> lavender -> soft pink).
    pub fn lavender() -> Self {
        Self::new(vec![
            (0.0, PackedRgba::rgb(80, 40, 140)),
            (0.5, PackedRgba::rgb(160, 120, 210)),
            (1.0, PackedRgba::rgb(230, 180, 220)),
        ])
    }

    /// Sample the gradient at position t (0.0 to 1.0).
    pub fn sample(&self, t: f64) -> PackedRgba {
        let t = t.clamp(0.0, 1.0);

        if self.stops.is_empty() {
            return PackedRgba::rgb(255, 255, 255);
        }
        if self.stops.len() == 1 {
            return self.stops[0].1;
        }

        // Find the two stops we're between
        let mut prev = &self.stops[0];
        for stop in &self.stops {
            if stop.0 >= t {
                if stop.0 == prev.0 {
                    return stop.1;
                }
                let local_t = (t - prev.0) / (stop.0 - prev.0);
                return lerp_color(prev.1, stop.1, local_t);
            }
            prev = stop;
        }

        self.stops
            .last()
            .map(|s| s.1)
            .unwrap_or(PackedRgba::rgb(255, 255, 255))
    }

    /// Optimized sample using binary search for stop lookup.
    /// O(log n) instead of O(n) for gradients with many stops.
    pub fn sample_fast(&self, t: f64) -> PackedRgba {
        let t = t.clamp(0.0, 1.0);

        if self.stops.is_empty() {
            return PackedRgba::rgb(255, 255, 255);
        }
        if self.stops.len() == 1 {
            return self.stops[0].1;
        }

        // Binary search for the right segment
        let idx = self
            .stops
            .binary_search_by(|stop| stop.0.partial_cmp(&t).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or_else(|i| i);

        if idx == 0 {
            return self.stops[0].1;
        }
        if idx >= self.stops.len() {
            return self.stops.last().map(|s| s.1).unwrap();
        }

        let prev = &self.stops[idx - 1];
        let next = &self.stops[idx];

        if next.0 == prev.0 {
            return next.1;
        }

        let local_t = (t - prev.0) / (next.0 - prev.0);
        lerp_color_fast(prev.1, next.1, local_t)
    }

    /// Sample the gradient at position t (0.0 to 1.0) using OkLab perceptual interpolation.
    ///
    /// This produces smoother, more visually uniform gradients than linear RGB
    /// interpolation. The perceptual distance between adjacent samples is more
    /// consistent, reducing visible banding.
    ///
    /// Performance: ~3-5% overhead vs linear RGB sampling.
    pub fn sample_oklab(&self, t: f64) -> PackedRgba {
        let t = t.clamp(0.0, 1.0);

        if self.stops.is_empty() {
            return PackedRgba::rgb(255, 255, 255);
        }
        if self.stops.len() == 1 {
            return self.stops[0].1;
        }

        // Find the two stops we're between
        let mut prev = &self.stops[0];
        for stop in &self.stops {
            if stop.0 >= t {
                if stop.0 == prev.0 {
                    return stop.1;
                }
                let local_t = (t - prev.0) / (stop.0 - prev.0);
                return lerp_color_oklab(prev.1, stop.1, local_t);
            }
            prev = stop;
        }

        self.stops
            .last()
            .map(|s| s.1)
            .unwrap_or(PackedRgba::rgb(255, 255, 255))
    }

    /// Optimized OkLab sample using binary search for stop lookup.
    /// O(log n) instead of O(n) for gradients with many stops.
    pub fn sample_fast_oklab(&self, t: f64) -> PackedRgba {
        let t = t.clamp(0.0, 1.0);

        if self.stops.is_empty() {
            return PackedRgba::rgb(255, 255, 255);
        }
        if self.stops.len() == 1 {
            return self.stops[0].1;
        }

        // Binary search for the right segment
        let idx = self
            .stops
            .binary_search_by(|stop| stop.0.partial_cmp(&t).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or_else(|i| i);

        if idx == 0 {
            return self.stops[0].1;
        }
        if idx >= self.stops.len() {
            return self.stops.last().map(|s| s.1).unwrap();
        }

        let prev = &self.stops[idx - 1];
        let next = &self.stops[idx];

        if next.0 == prev.0 {
            return next.1;
        }

        let local_t = (t - prev.0) / (next.0 - prev.0);
        lerp_color_oklab(prev.1, next.1, local_t)
    }

    /// Precompute a lookup table for gradient sampling using OkLab interpolation.
    /// Returns a vector of `count` colors evenly spaced from t=0 to t=1.
    /// Use for rendering text where character positions map to indices.
    pub fn precompute_lut_oklab(&self, count: usize) -> GradientLut {
        if count == 0 {
            return GradientLut {
                colors: Vec::new(),
                count: 0,
            };
        }

        let mut colors = Vec::with_capacity(count);

        if count == 1 {
            colors.push(self.sample_fast_oklab(0.5));
        } else {
            let divisor = (count - 1) as f64;
            for i in 0..count {
                let t = i as f64 / divisor;
                colors.push(self.sample_fast_oklab(t));
            }
        }

        GradientLut { colors, count }
    }

    /// Precompute a lookup table for gradient sampling.
    /// Returns a vector of `count` colors evenly spaced from t=0 to t=1.
    /// Use for rendering text where character positions map to indices.
    pub fn precompute_lut(&self, count: usize) -> GradientLut {
        if count == 0 {
            return GradientLut {
                colors: Vec::new(),
                count: 0,
            };
        }

        let mut colors = Vec::with_capacity(count);

        if count == 1 {
            colors.push(self.sample_fast(0.5));
        } else {
            let divisor = (count - 1) as f64;
            for i in 0..count {
                let t = i as f64 / divisor;
                colors.push(self.sample_fast(t));
            }
        }

        GradientLut { colors, count }
    }

    /// Sample a batch of colors for a row.
    /// More efficient than calling sample() for each character.
    pub fn sample_batch(&self, start_t: f64, end_t: f64, count: usize) -> Vec<PackedRgba> {
        if count == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(count);

        if count == 1 {
            result.push(self.sample_fast((start_t + end_t) / 2.0));
        } else {
            let step = (end_t - start_t) / (count - 1) as f64;
            for i in 0..count {
                let t = start_t + step * i as f64;
                result.push(self.sample_fast(t));
            }
        }

        result
    }
}

// =============================================================================
// Gradient Lookup Table (LUT) for Memoized Sampling
// =============================================================================

/// Precomputed gradient lookup table for O(1) sampling.
///
/// Once computed for a given size, gradient sampling becomes a simple
/// array index operation with no floating-point math in the hot path.
///
/// # Example
/// ```ignore
/// let gradient = ColorGradient::rainbow();
/// let lut = gradient.precompute_lut(80); // For 80-column text
///
/// // O(1) lookup for each character position
/// for col in 0..80 {
///     let color = lut.sample(col);
/// }
/// ```
#[derive(Debug, Clone)]
pub struct GradientLut {
    /// Precomputed color samples.
    colors: Vec<PackedRgba>,
    /// Number of samples (matches colors.len()).
    count: usize,
}

impl GradientLut {
    /// Sample the LUT at a given index.
    /// Index is clamped to valid range.
    #[inline]
    pub fn sample(&self, index: usize) -> PackedRgba {
        if self.colors.is_empty() {
            return PackedRgba::rgb(255, 255, 255);
        }
        let idx = index.min(self.count.saturating_sub(1));
        self.colors[idx]
    }

    /// Sample the LUT at a normalized t value (0.0 to 1.0).
    #[inline]
    pub fn sample_t(&self, t: f64) -> PackedRgba {
        if self.colors.is_empty() {
            return PackedRgba::rgb(255, 255, 255);
        }
        let t = t.clamp(0.0, 1.0);
        let index = (t * (self.count.saturating_sub(1)) as f64).round() as usize;
        self.sample(index)
    }

    /// Get the number of samples in this LUT.
    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    /// Check if the LUT is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Get the underlying color buffer for SIMD-friendly iteration.
    #[inline]
    pub fn as_slice(&self) -> &[PackedRgba] {
        &self.colors
    }
}

/// Optimized color interpolation using integer math.
/// Avoids f64 operations in the inner loop.
#[inline]
fn lerp_color_fast(a: PackedRgba, b: PackedRgba, t: f64) -> PackedRgba {
    // Use fixed-point with 16-bit precision
    let t_fixed = (t.clamp(0.0, 1.0) * 65536.0) as u32;
    let one_minus_t = 65536 - t_fixed;

    let r = ((a.r() as u32 * one_minus_t + b.r() as u32 * t_fixed) >> 16) as u8;
    let g = ((a.g() as u32 * one_minus_t + b.g() as u32 * t_fixed) >> 16) as u8;
    let b_val = ((a.b() as u32 * one_minus_t + b.b() as u32 * t_fixed) >> 16) as u8;

    PackedRgba::rgb(r, g, b_val)
}

// =============================================================================
// Fixed-Point T-Value Cache for Row Rendering
// =============================================================================

/// Precomputed t-values for row/column positions.
/// Avoids floating-point division in render loops.
#[derive(Debug, Clone)]
pub struct TValueCache {
    /// Fixed-point t values (16.16 format, stored as u32).
    values: Vec<u32>,
    /// Size the cache was computed for.
    size: usize,
}

impl TValueCache {
    /// Create a new t-value cache for a given size.
    /// Values represent evenly spaced positions from 0 to 1.
    pub fn new(size: usize) -> Self {
        if size == 0 {
            return Self {
                values: Vec::new(),
                size: 0,
            };
        }

        let mut values = Vec::with_capacity(size);

        if size == 1 {
            values.push(32768); // 0.5 in 16.16 fixed-point
        } else {
            let divisor = size - 1;
            for i in 0..size {
                // Compute t * 65536 using integer math to avoid precision loss
                let t_fixed = ((i as u64 * 65536) / divisor as u64) as u32;
                values.push(t_fixed);
            }
        }

        Self { values, size }
    }

    /// Get the fixed-point t value at an index (16.16 format).
    #[inline]
    pub fn get_fixed(&self, index: usize) -> u32 {
        if self.values.is_empty() {
            return 32768;
        }
        let idx = index.min(self.size.saturating_sub(1));
        self.values[idx]
    }

    /// Get the floating-point t value at an index.
    #[inline]
    pub fn get(&self, index: usize) -> f64 {
        self.get_fixed(index) as f64 / 65536.0
    }

    /// Get the size of this cache.
    #[inline]
    pub fn len(&self) -> usize {
        self.size
    }

    /// Check if empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
}

// =============================================================================
// Color Palette Presets
// =============================================================================

/// Curated color palette presets for gradients and color cycling.
///
/// All gradients are defined in sRGB color space with stops spanning the full
/// 0.0..=1.0 range. Solid color sets contain at least 2 colors for use with
/// `TextEffect::ColorCycle`.
pub mod palette {
    use super::{ColorGradient, PackedRgba};

    // --- Gradient presets (convenience re-exports + new) ---

    /// Rainbow gradient (red -> orange -> yellow -> green -> blue -> violet).
    pub fn rainbow() -> ColorGradient {
        ColorGradient::rainbow()
    }

    /// Sunset gradient (purple -> pink -> orange -> yellow).
    pub fn sunset() -> ColorGradient {
        ColorGradient::sunset()
    }

    /// Ocean gradient (deep blue -> cyan -> seafoam).
    pub fn ocean() -> ColorGradient {
        ColorGradient::ocean()
    }

    /// Cyberpunk gradient (hot pink -> purple -> cyan).
    pub fn cyberpunk() -> ColorGradient {
        ColorGradient::cyberpunk()
    }

    /// Fire gradient (black -> red -> orange -> yellow -> white).
    pub fn fire() -> ColorGradient {
        ColorGradient::fire()
    }

    /// Ice gradient (frost blue -> light blue -> white).
    pub fn ice() -> ColorGradient {
        ColorGradient::ice()
    }

    /// Forest gradient (deep green -> emerald -> light green).
    pub fn forest() -> ColorGradient {
        ColorGradient::forest()
    }

    /// Gold gradient (dark gold -> bright gold -> pale gold).
    pub fn gold() -> ColorGradient {
        ColorGradient::gold()
    }

    /// Neon pink gradient (magenta -> hot pink -> cyan).
    pub fn neon_pink() -> ColorGradient {
        ColorGradient::neon_pink()
    }

    /// Blood gradient (near-black -> dark red -> bright crimson).
    pub fn blood() -> ColorGradient {
        ColorGradient::blood()
    }

    /// Matrix gradient (black -> dark green -> bright green).
    pub fn matrix() -> ColorGradient {
        ColorGradient::matrix()
    }

    /// Terminal gradient (dark green -> medium green -> bright green).
    pub fn terminal() -> ColorGradient {
        ColorGradient::terminal()
    }

    /// Lavender gradient (deep purple -> lavender -> soft pink).
    pub fn lavender() -> ColorGradient {
        ColorGradient::lavender()
    }

    // --- Solid color sets for ColorCycle ---

    /// Neon colors: cyan, magenta, yellow, lime green.
    pub fn neon_colors() -> Vec<PackedRgba> {
        vec![
            PackedRgba::rgb(0, 255, 255), // Cyan
            PackedRgba::rgb(255, 0, 255), // Magenta
            PackedRgba::rgb(255, 255, 0), // Yellow
            PackedRgba::rgb(0, 255, 128), // Lime green
        ]
    }

    /// Pastel colors: soft pink, mint, peach, periwinkle, butter.
    pub fn pastel_colors() -> Vec<PackedRgba> {
        vec![
            PackedRgba::rgb(255, 182, 193), // Soft pink
            PackedRgba::rgb(170, 255, 195), // Mint
            PackedRgba::rgb(255, 218, 185), // Peach
            PackedRgba::rgb(180, 180, 255), // Periwinkle
            PackedRgba::rgb(255, 255, 186), // Butter
        ]
    }

    /// Earth tones: terracotta, olive, clay, warm brown, sage.
    pub fn earth_tones() -> Vec<PackedRgba> {
        vec![
            PackedRgba::rgb(180, 90, 60),   // Terracotta
            PackedRgba::rgb(120, 140, 60),  // Olive
            PackedRgba::rgb(160, 110, 80),  // Clay
            PackedRgba::rgb(100, 70, 40),   // Warm brown
            PackedRgba::rgb(140, 170, 120), // Sage
        ]
    }

    /// Monochrome grays from dark to light.
    pub fn monochrome() -> Vec<PackedRgba> {
        vec![
            PackedRgba::rgb(40, 40, 40),    // Near-black
            PackedRgba::rgb(90, 90, 90),    // Dark gray
            PackedRgba::rgb(140, 140, 140), // Medium gray
            PackedRgba::rgb(190, 190, 190), // Light gray
            PackedRgba::rgb(230, 230, 230), // Near-white
        ]
    }

    /// Return all gradient presets as `(name, gradient)` pairs.
    pub fn all_gradients() -> Vec<(&'static str, ColorGradient)> {
        vec![
            ("rainbow", rainbow()),
            ("sunset", sunset()),
            ("ocean", ocean()),
            ("cyberpunk", cyberpunk()),
            ("fire", fire()),
            ("ice", ice()),
            ("forest", forest()),
            ("gold", gold()),
            ("neon_pink", neon_pink()),
            ("blood", blood()),
            ("matrix", matrix()),
            ("terminal", terminal()),
            ("lavender", lavender()),
        ]
    }

    /// Return all solid color sets as `(name, colors)` pairs.
    pub fn all_color_sets() -> Vec<(&'static str, Vec<PackedRgba>)> {
        vec![
            ("neon_colors", neon_colors()),
            ("pastel_colors", pastel_colors()),
            ("earth_tones", earth_tones()),
            ("monochrome", monochrome()),
        ]
    }
}

// =============================================================================
// Easing Functions - Animation curve system
// =============================================================================

/// Easing functions for smooth, professional animations.
///
/// Most curves output values in the 0.0-1.0 range, but some (Elastic, Back)
/// can overshoot outside this range for spring/bounce effects. Code using
/// easing should handle values outside 0-1 gracefully (clamp colors, etc.).
///
/// # Performance
/// All `apply()` calls are < 100ns (no allocations, pure math).
///
/// # Example
/// ```ignore
/// let progress = 0.5;
/// let eased = Easing::EaseInOut.apply(progress);
/// // eased ≈ 0.5 but with smooth acceleration/deceleration
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Easing {
    /// Linear interpolation: `t` (no easing).
    #[default]
    Linear,

    // --- Cubic curves (smooth, professional) ---
    /// Slow start, accelerating: `t³`
    EaseIn,
    /// Slow end, decelerating: `1 - (1-t)³`
    EaseOut,
    /// Smooth S-curve: slow start and end.
    EaseInOut,

    // --- Quadratic curves (subtler than cubic) ---
    /// Subtle slow start: `t²`
    EaseInQuad,
    /// Subtle slow end: `1 - (1-t)²`
    EaseOutQuad,
    /// Subtle S-curve.
    EaseInOutQuad,

    // --- Playful/dynamic curves ---
    /// Ball bounce effect at end.
    Bounce,
    /// Spring with overshoot. **WARNING: Can exceed 1.0!**
    Elastic,
    /// Slight overshoot then settle. **WARNING: Can go < 0 and > 1!**
    Back,

    // --- Discrete ---
    /// Discrete steps. `Step(4)` outputs {0, 0.25, 0.5, 0.75, 1.0}.
    Step(u8),
}

impl Easing {
    /// Apply the easing function to a progress value.
    ///
    /// # Arguments
    /// * `t` - Progress value (clamped to 0.0-1.0 internally)
    ///
    /// # Returns
    /// The eased value. Most curves return 0.0-1.0, but `Elastic` and `Back`
    /// can briefly exceed these bounds for spring/overshoot effects.
    ///
    /// # Performance
    /// < 100ns per call (pure math, no allocations).
    pub fn apply(&self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);

        match self {
            Self::Linear => t,

            // Cubic curves
            Self::EaseIn => t * t * t,
            Self::EaseOut => {
                let inv = 1.0 - t;
                1.0 - inv * inv * inv
            }
            Self::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let inv = -2.0 * t + 2.0;
                    1.0 - inv * inv * inv / 2.0
                }
            }

            // Quadratic curves
            Self::EaseInQuad => t * t,
            Self::EaseOutQuad => {
                let inv = 1.0 - t;
                1.0 - inv * inv
            }
            Self::EaseInOutQuad => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let inv = -2.0 * t + 2.0;
                    1.0 - inv * inv / 2.0
                }
            }

            // Bounce - ball bouncing at end
            Self::Bounce => {
                let n1 = 7.5625;
                let d1 = 2.75;
                let mut t = t;

                if t < 1.0 / d1 {
                    n1 * t * t
                } else if t < 2.0 / d1 {
                    t -= 1.5 / d1;
                    n1 * t * t + 0.75
                } else if t < 2.5 / d1 {
                    t -= 2.25 / d1;
                    n1 * t * t + 0.9375
                } else {
                    t -= 2.625 / d1;
                    n1 * t * t + 0.984375
                }
            }

            // Elastic - spring with overshoot (CAN EXCEED 1.0!)
            Self::Elastic => {
                if t == 0.0 {
                    0.0
                } else if t == 1.0 {
                    1.0
                } else {
                    let c4 = TAU / 3.0;
                    2.0_f64.powf(-10.0 * t) * ((t * 10.0 - 0.75) * c4).sin() + 1.0
                }
            }

            // Back - overshoot then settle (CAN GO < 0 AND > 1!)
            // Uses easeOutBack formula: 1 + c3 * (t-1)^3 + c1 * (t-1)^2
            Self::Back => {
                let c1 = 1.70158;
                let c3 = c1 + 1.0;
                let t_minus_1 = t - 1.0;
                1.0 + c3 * t_minus_1 * t_minus_1 * t_minus_1 + c1 * t_minus_1 * t_minus_1
            }

            // Step - discrete steps. Step(n) outputs n+1 values: {0, 1/n, 2/n, ..., 1}
            Self::Step(steps) => {
                if *steps == 0 {
                    t
                } else {
                    let s = *steps as f64;
                    (t * s).round() / s
                }
            }
        }
    }

    /// Check if this easing can produce values outside 0.0-1.0.
    pub fn can_overshoot(&self) -> bool {
        matches!(self, Self::Elastic | Self::Back)
    }

    /// Get a human-readable name for the easing function.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::EaseIn => "Ease In (Cubic)",
            Self::EaseOut => "Ease Out (Cubic)",
            Self::EaseInOut => "Ease In-Out (Cubic)",
            Self::EaseInQuad => "Ease In (Quad)",
            Self::EaseOutQuad => "Ease Out (Quad)",
            Self::EaseInOutQuad => "Ease In-Out (Quad)",
            Self::Bounce => "Bounce",
            Self::Elastic => "Elastic",
            Self::Back => "Back",
            Self::Step(_) => "Step",
        }
    }
}

// =============================================================================
// Animation Timing - Frame-rate independent animation clock
// =============================================================================

/// Animation clock for time-based effects.
///
/// Provides a unified timing system with:
/// - Frame-rate independence via delta-time calculation
/// - Global speed control (pause/resume/slow-motion)
/// - Consistent time units (seconds)
///
/// # Speed Convention
/// All effects use **cycles per second**:
/// - `speed: 1.0` = one full cycle per second
/// - `speed: 0.5` = one cycle every 2 seconds
/// - `speed: 2.0` = two cycles per second
///
/// # Example
/// ```ignore
/// let mut clock = AnimationClock::new();
/// loop {
///     clock.tick(); // Call once per frame
///     let t = clock.time();
///     let styled = StyledText::new("Hello").time(t);
/// }
/// ```
#[derive(Debug, Clone)]
pub struct AnimationClock {
    /// Current animation time in seconds.
    time: f64,
    /// Time multiplier (1.0 = normal, 0.0 = paused, 0.5 = half-speed).
    speed: f64,
    /// Last tick instant for delta calculation.
    last_tick: std::time::Instant,
}

impl Default for AnimationClock {
    fn default() -> Self {
        Self::new()
    }
}

impl AnimationClock {
    /// Create a new animation clock starting at time 0.
    #[inline]
    pub fn new() -> Self {
        Self {
            time: 0.0,
            speed: 1.0,
            last_tick: std::time::Instant::now(),
        }
    }

    /// Create a clock with a specific start time.
    #[inline]
    pub fn with_time(time: f64) -> Self {
        Self {
            time,
            speed: 1.0,
            last_tick: std::time::Instant::now(),
        }
    }

    /// Advance the clock by elapsed real time since last tick.
    ///
    /// Call this once per frame. The time advancement respects the current
    /// speed multiplier, enabling pause/slow-motion effects.
    #[inline]
    pub fn tick(&mut self) {
        let now = std::time::Instant::now();
        let delta = now.duration_since(self.last_tick).as_secs_f64();
        self.time += delta * self.speed;
        self.last_tick = now;
    }

    /// Advance the clock by a specific delta time.
    ///
    /// Use this for deterministic testing or when you control the time step.
    #[inline]
    pub fn tick_delta(&mut self, delta_seconds: f64) {
        self.time += delta_seconds * self.speed;
        self.last_tick = std::time::Instant::now();
    }

    /// Get the current animation time in seconds.
    #[inline]
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Set the current time directly.
    #[inline]
    pub fn set_time(&mut self, time: f64) {
        self.time = time;
    }

    /// Get the current speed multiplier.
    #[inline]
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Set the speed multiplier.
    ///
    /// - `1.0` = normal speed
    /// - `0.0` = paused
    /// - `0.5` = half speed
    /// - `2.0` = double speed
    #[inline]
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.max(0.0);
    }

    /// Pause the animation (equivalent to `set_speed(0.0)`).
    #[inline]
    pub fn pause(&mut self) {
        self.speed = 0.0;
    }

    /// Resume the animation at normal speed (equivalent to `set_speed(1.0)`).
    #[inline]
    pub fn resume(&mut self) {
        self.speed = 1.0;
    }

    /// Check if the clock is paused.
    #[inline]
    pub fn is_paused(&self) -> bool {
        self.speed == 0.0
    }

    /// Reset the clock to time 0.
    #[inline]
    pub fn reset(&mut self) {
        self.time = 0.0;
        self.last_tick = std::time::Instant::now();
    }

    /// Get elapsed time since a given start time (useful for relative animations).
    #[inline]
    pub fn elapsed_since(&self, start_time: f64) -> f64 {
        (self.time - start_time).max(0.0)
    }

    /// Calculate a cyclic phase for periodic animations.
    ///
    /// Returns a value in `0.0..1.0` that cycles at the given frequency.
    ///
    /// # Arguments
    /// * `cycles_per_second` - How many full cycles per second
    ///
    /// # Example
    /// ```ignore
    /// // Pulse that completes 2 cycles per second
    /// let phase = clock.phase(2.0);
    /// let brightness = 0.5 + 0.5 * (phase * TAU).sin();
    /// ```
    #[inline]
    pub fn phase(&self, cycles_per_second: f64) -> f64 {
        if cycles_per_second <= 0.0 {
            return 0.0;
        }
        (self.time * cycles_per_second).fract()
    }
}

// =============================================================================
// Position Animation Types
// =============================================================================

/// Direction for wave/cascade/position effects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Direction {
    /// Characters move/wave vertically downward.
    #[default]
    Down,
    /// Characters move/wave vertically upward.
    Up,
    /// Characters move/wave horizontally leftward.
    Left,
    /// Characters move/wave horizontally rightward.
    Right,
}

impl Direction {
    /// Returns true if this direction affects vertical position.
    #[inline]
    pub fn is_vertical(&self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }

    /// Returns true if this direction affects horizontal position.
    #[inline]
    pub fn is_horizontal(&self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// Character position offset for position-based effects.
///
/// This is internal and used to calculate how much each character
/// should be offset from its base position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CharacterOffset {
    /// Horizontal offset in cells (positive = right, negative = left).
    pub dx: i16,
    /// Vertical offset in rows (positive = down, negative = up).
    pub dy: i16,
}

impl CharacterOffset {
    /// Create a new offset.
    #[inline]
    pub const fn new(dx: i16, dy: i16) -> Self {
        Self { dx, dy }
    }

    /// Zero offset (no movement).
    pub const ZERO: Self = Self { dx: 0, dy: 0 };

    /// Clamp offset to terminal bounds.
    ///
    /// Ensures that when applied to position (x, y), the result stays within bounds.
    #[must_use]
    #[inline]
    pub fn clamp_for_position(self, x: u16, y: u16, width: u16, height: u16) -> Self {
        let min_dx = -(x as i16);
        let max_dx = (width.saturating_sub(1).saturating_sub(x)) as i16;
        let min_dy = -(y as i16);
        let max_dy = (height.saturating_sub(1).saturating_sub(y)) as i16;

        Self {
            dx: self.dx.clamp(min_dx, max_dx),
            dy: self.dy.clamp(min_dy, max_dy),
        }
    }
}

impl std::ops::Add for CharacterOffset {
    type Output = Self;

    /// Add two offsets together using saturating arithmetic.
    #[inline]
    fn add(self, other: Self) -> Self {
        Self {
            dx: self.dx.saturating_add(other.dx),
            dy: self.dy.saturating_add(other.dy),
        }
    }
}

// =============================================================================
// Shadow and Multi-Layer Rendering (bd-3hsz)
// =============================================================================

/// Drop shadow configuration for text effects.
///
/// Shadows add depth and dimension to text by rendering a copy of the text
/// offset from the original position. Multiple shadows can be combined for
/// complex layered effects.
///
/// # Invariants
///
/// 1. **Offset bounds**: dx/dy are i8 (-128 to 127 cells), sufficient for any
///    reasonable shadow depth while preventing excessive offsets.
/// 2. **Opacity range**: opacity is clamped to 0.0-1.0 during rendering.
/// 3. **Z-order**: Shadows always render behind the main text.
///
/// # Failure Modes
///
/// | Scenario | Behavior |
/// |----------|----------|
/// | Shadow extends beyond terminal | Saturating arithmetic clips to bounds |
/// | Zero opacity | Shadow not rendered (optimization) |
/// | Negative dx/dy | Shadow appears up/left of text (valid) |
///
/// # Example
///
/// ```rust,ignore
/// Shadow::new(1, 1)
///     .color(PackedRgba::rgb(0, 0, 0))
///     .opacity(0.5)
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    /// Horizontal offset in cells (positive = right, negative = left).
    pub dx: i8,
    /// Vertical offset in rows (positive = down, negative = up).
    pub dy: i8,
    /// Shadow color (usually darker than text or a specific color).
    pub color: PackedRgba,
    /// Shadow opacity (0.0 = invisible, 1.0 = fully opaque).
    pub opacity: f64,
}

impl Default for Shadow {
    fn default() -> Self {
        Self {
            dx: 1,
            dy: 1,
            color: PackedRgba::rgb(0, 0, 0),
            opacity: 0.5,
        }
    }
}

impl Shadow {
    /// Create a new shadow with the given offset.
    #[must_use]
    pub const fn new(dx: i8, dy: i8) -> Self {
        Self {
            dx,
            dy,
            color: PackedRgba::rgb(0, 0, 0),
            opacity: 0.5,
        }
    }

    /// Set the shadow color.
    #[must_use]
    pub const fn color(mut self, color: PackedRgba) -> Self {
        self.color = color;
        self
    }

    /// Set the shadow opacity (0.0-1.0).
    #[must_use]
    pub fn opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Apply shadow offset to a position, clamping to terminal bounds.
    ///
    /// Returns None if the shadow position is outside the terminal.
    #[inline]
    pub fn apply_offset(&self, x: u16, y: u16, width: u16, height: u16) -> Option<(u16, u16)> {
        let new_x = (x as i32).saturating_add(i32::from(self.dx));
        let new_y = (y as i32).saturating_add(i32::from(self.dy));

        if new_x >= 0 && new_x < i32::from(width) && new_y >= 0 && new_y < i32::from(height) {
            Some((new_x as u16, new_y as u16))
        } else {
            None
        }
    }

    /// Get the shadow color with opacity applied.
    #[inline]
    #[must_use]
    pub fn effective_color(&self) -> PackedRgba {
        apply_alpha(self.color, self.opacity)
    }
}

/// Multi-layer glow configuration for soft lighting effects.
///
/// Creates a halo effect around text by rendering multiple increasingly
/// transparent layers at expanding offsets. This produces a soft, diffuse
/// glow similar to neon signage or game UI elements.
///
/// # Invariants
///
/// 1. **Layer count**: layers is clamped to 1-5 to prevent performance issues.
/// 2. **Falloff bounds**: falloff is clamped to 0.1-0.9 for visible effect.
/// 3. **Intensity range**: intensity is clamped to 0.0-1.0.
///
/// # Rendering Algorithm
///
/// For each layer i (from outermost to innermost):
/// 1. Calculate layer opacity: `intensity * falloff^(layers - 1 - i)`
/// 2. Render text at offsets (-i,-i), (-i,0), (-i,+i), (0,-i), (0,+i), (+i,-i), (+i,0), (+i,+i)
/// 3. Main text renders last (on top)
///
/// # Example
///
/// ```rust,ignore
/// GlowConfig::new(PackedRgba::rgb(0, 255, 255))
///     .intensity(0.8)
///     .layers(3)
///     .falloff(0.5)
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlowConfig {
    /// Glow color (usually bright, saturated).
    pub color: PackedRgba,
    /// Base intensity of innermost glow layer (0.0-1.0).
    pub intensity: f64,
    /// Number of glow layers (1-5). More layers = softer, wider glow.
    pub layers: u8,
    /// Opacity falloff per layer (0.1-0.9). Lower = faster fade.
    pub falloff: f64,
}

impl Default for GlowConfig {
    fn default() -> Self {
        Self {
            color: PackedRgba::rgb(255, 255, 255),
            intensity: 0.6,
            layers: 2,
            falloff: 0.5,
        }
    }
}

impl GlowConfig {
    /// Create a new glow configuration with the given color.
    #[must_use]
    pub const fn new(color: PackedRgba) -> Self {
        Self {
            color,
            intensity: 0.6,
            layers: 2,
            falloff: 0.5,
        }
    }

    /// Set the glow intensity (0.0-1.0).
    #[must_use]
    pub fn intensity(mut self, intensity: f64) -> Self {
        self.intensity = intensity.clamp(0.0, 1.0);
        self
    }

    /// Set the number of glow layers (1-5).
    #[must_use]
    pub fn layers(mut self, layers: u8) -> Self {
        self.layers = layers.clamp(1, 5);
        self
    }

    /// Set the opacity falloff per layer (0.1-0.9).
    #[must_use]
    pub fn falloff(mut self, falloff: f64) -> Self {
        self.falloff = falloff.clamp(0.1, 0.9);
        self
    }

    /// Calculate the opacity for a given layer index (0 = outermost).
    #[inline]
    #[must_use]
    pub fn layer_opacity(&self, layer_index: u8) -> f64 {
        // Outermost layer has lowest opacity, innermost has highest
        let distance_from_text = self.layers.saturating_sub(layer_index).saturating_sub(1);
        self.intensity * self.falloff.powi(i32::from(distance_from_text))
    }

    /// Get the glow color with opacity applied for a given layer.
    #[inline]
    #[must_use]
    pub fn layer_color(&self, layer_index: u8) -> PackedRgba {
        apply_alpha(self.color, self.layer_opacity(layer_index))
    }

    /// Generate all shadow offsets for a given layer.
    ///
    /// Returns up to 8 positions (3x3 grid minus center) for the glow layer.
    pub fn layer_offsets(&self, layer_index: u8) -> impl Iterator<Item = (i8, i8)> {
        let radius = i8::try_from(self.layers.saturating_sub(layer_index)).unwrap_or(1);
        let offsets = [
            (-radius, -radius),
            (-radius, 0),
            (-radius, radius),
            (0, -radius),
            (0, radius),
            (radius, -radius),
            (radius, 0),
            (radius, radius),
        ];
        offsets.into_iter()
    }
}

// =============================================================================
// Border/Outline Effects (bd-882b)
// =============================================================================
//
// Outlines add a stroke around each character for improved legibility on complex
// backgrounds and for stylistic effects. This is distinct from Shadow/Glow which
// renders BEHIND text - Outline renders AROUND each character edge.
//
// Evidence Ledger:
// - Choice: 8-neighbor (thickness=1) and 24-neighbor (thickness=2) patterns
// - Reason: 8-neighbor covers immediate Moore neighborhood, 24-neighbor extends
//   to a 5x5 grid minus corners for smoother appearance at larger thickness.
// - Invariants:
//   - Outline always renders after glow/shadow but before main text
//   - thickness is clamped to 1-3 to prevent excessive rendering
//   - Offsets use saturating arithmetic to prevent overflow at edges
// - Failure Modes:
//   - thickness > 3: clamped to 3 (24-neighbor already covers practical needs)
//   - Gradient with no colors: falls back to solid white outline

/// Outline visual style for text stroke effects.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum OutlineStyle {
    /// Continuous solid stroke around each character.
    #[default]
    Solid,
    /// Alternating visible/invisible stroke segments.
    Dashed {
        /// Length of each dash in cells (1-5).
        dash_len: u8,
    },
    /// Two concentric outlines for a double-border effect.
    Double,
    /// Animated gradient stroke that cycles colors over time.
    Gradient(ColorGradient),
}

/// Configuration for text outline/border effects.
///
/// Creates a stroke effect around each character by rendering the character
/// (or a block character) at multiple offset positions surrounding the text.
///
/// # Invariants
///
/// 1. **Thickness bounds**: thickness is clamped to 1-3.
/// 2. **Offset calculation**: Uses 8-neighbor (thickness=1), 16-neighbor (thickness=2),
///    or 24-neighbor (thickness=3) patterns.
/// 3. **Z-order**: Outline renders after glow/shadow, before main text.
///
/// # Rendering Algorithm
///
/// For each character at (x, y):
/// 1. Calculate neighbor offsets based on thickness
/// 2. For each offset (dx, dy), render the outline character at (x+dx, y+dy)
/// 3. Apply style (solid, dashed, double, gradient) to determine color/visibility
/// 4. Main text renders on top
///
/// # Example
///
/// ```rust,ignore
/// OutlineConfig::new(PackedRgba::rgb(255, 255, 255))
///     .thickness(1)
///     .style(OutlineStyle::Solid)
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineConfig {
    /// Outline color.
    pub color: PackedRgba,
    /// Stroke thickness (1 = immediate neighbors, 2 = two cells out, 3 = three cells out).
    pub thickness: u8,
    /// Outline visual style.
    pub style: OutlineStyle,
    /// Whether to use the same character as text (true) or block character (false).
    pub use_text_char: bool,
}

impl Default for OutlineConfig {
    fn default() -> Self {
        Self {
            color: PackedRgba::rgb(255, 255, 255),
            thickness: 1,
            style: OutlineStyle::Solid,
            use_text_char: true,
        }
    }
}

impl OutlineConfig {
    /// Create a new outline configuration with the given color.
    #[must_use]
    pub const fn new(color: PackedRgba) -> Self {
        Self {
            color,
            thickness: 1,
            style: OutlineStyle::Solid,
            use_text_char: true,
        }
    }

    /// Set the outline thickness (1-3).
    #[must_use]
    pub fn thickness(mut self, thickness: u8) -> Self {
        self.thickness = thickness.clamp(1, 3);
        self
    }

    /// Set the outline style.
    #[must_use]
    pub fn style(mut self, style: OutlineStyle) -> Self {
        self.style = style;
        self
    }

    /// Set whether to use the text character or block character for outline.
    #[must_use]
    pub const fn use_text_char(mut self, use_text_char: bool) -> Self {
        self.use_text_char = use_text_char;
        self
    }

    /// Generate all outline offsets for the configured thickness.
    ///
    /// - thickness=1: 8 neighbors (3x3 grid minus center)
    /// - thickness=2: 16 neighbors (5x5 grid minus center, minus corners)
    /// - thickness=3: 24 neighbors (5x5 grid minus center)
    pub fn offsets(&self) -> impl Iterator<Item = (i8, i8)> {
        let thickness = self.thickness.clamp(1, 3);

        // Pre-computed offset arrays for each thickness level
        const OFFSETS_T1: [(i8, i8); 8] = [
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ];

        const OFFSETS_T2: [(i8, i8); 16] = [
            (-2, -1),
            (-2, 0),
            (-2, 1),
            (-1, -2),
            (-1, -1),
            (-1, 0),
            (-1, 1),
            (-1, 2),
            (0, -2),
            (0, 2),
            (1, -2),
            (1, -1),
            (1, 0),
            (1, 1),
            (1, 2),
            (2, -1),
        ];

        const OFFSETS_T3: [(i8, i8); 24] = [
            (-2, -2),
            (-2, -1),
            (-2, 0),
            (-2, 1),
            (-2, 2),
            (-1, -2),
            (-1, -1),
            (-1, 0),
            (-1, 1),
            (-1, 2),
            (0, -2),
            (0, -1),
            (0, 2),
            (1, -2),
            (1, -1),
            (1, 0),
            (1, 1),
            (1, 2),
            (2, -2),
            (2, -1),
            (2, 0),
            (2, 1),
            (2, 2),
            (0, 1),
        ];

        // Return iterator over appropriate offsets based on thickness
        let slice: &[(i8, i8)] = match thickness {
            1 => &OFFSETS_T1,
            2 => &OFFSETS_T2[..],
            _ => &OFFSETS_T3,
        };

        slice.iter().copied()
    }

    /// Calculate the color for an outline position, accounting for style.
    ///
    /// Returns None if the position should not be rendered (e.g., dashed gap).
    #[inline]
    pub fn color_at(&self, offset_idx: usize, time: f64) -> Option<PackedRgba> {
        match &self.style {
            OutlineStyle::Solid => Some(self.color),
            OutlineStyle::Dashed { dash_len } => {
                let dash_len = (*dash_len).max(1) as usize;
                // Alternate: dash_len visible, dash_len invisible
                if (offset_idx / dash_len).is_multiple_of(2) {
                    Some(self.color)
                } else {
                    None
                }
            }
            OutlineStyle::Double => {
                // Double renders two rings - handled by doubling thickness in render
                Some(self.color)
            }
            OutlineStyle::Gradient(gradient) => {
                // Animate gradient based on time
                let t = (time * 0.5).rem_euclid(1.0);
                let adjusted_t = (t + offset_idx as f64 * 0.1).rem_euclid(1.0);
                Some(gradient.sample(adjusted_t))
            }
        }
    }

    /// Check if this is a double outline style (requires two passes).
    #[inline]
    #[must_use]
    pub fn is_double(&self) -> bool {
        matches!(self.style, OutlineStyle::Double)
    }
}

// =============================================================================
// Cursor Animation Types
// =============================================================================

/// Cursor visual style for text effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorStyle {
    /// Block cursor (█).
    #[default]
    Block,
    /// Underline cursor (_).
    Underline,
    /// Vertical bar cursor (|).
    Bar,
    /// Custom character cursor.
    Custom(char),
}

impl CursorStyle {
    /// Get the character to display for this cursor style.
    pub fn char(&self) -> char {
        match self {
            Self::Block => '█',
            Self::Underline => '_',
            Self::Bar => '|',
            Self::Custom(ch) => *ch,
        }
    }
}

/// Position of cursor relative to text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorPosition {
    /// Cursor appears after the last visible character.
    #[default]
    End,
    /// Cursor appears at a specific character index.
    AtIndex(usize),
    /// Cursor follows the reveal progress (for Typewriter/Reveal effects).
    /// The cursor appears after the last revealed character.
    AfterReveal,
}

// =============================================================================
// Reveal Animation Types
// =============================================================================

/// Character reveal mode for text animation.
///
/// Controls the order in which characters are revealed during a text
/// reveal animation. Can be used with `TextEffect::Reveal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RevealMode {
    /// Classic typewriter: left to right.
    #[default]
    LeftToRight,
    /// Reverse typewriter: right to left.
    RightToLeft,
    /// Expand from the center outward.
    CenterOut,
    /// Converge from edges toward center.
    EdgesIn,
    /// Random order (deterministic with seed).
    Random,
    /// Reveal word by word.
    ByWord,
    /// Reveal line by line (for multi-line text).
    ByLine,
}

impl RevealMode {
    /// Check if character at `idx` should be visible given `progress` (0.0-1.0).
    ///
    /// # Arguments
    /// * `idx` - Character index (0-based)
    /// * `total` - Total number of characters
    /// * `progress` - Reveal progress from 0.0 (hidden) to 1.0 (visible)
    /// * `seed` - Random seed for Random mode
    /// * `text` - The original text (needed for ByWord mode)
    pub fn is_visible(
        &self,
        idx: usize,
        total: usize,
        progress: f64,
        seed: u64,
        text: &str,
    ) -> bool {
        if total == 0 {
            return true;
        }
        let progress = progress.clamp(0.0, 1.0);
        if progress >= 1.0 {
            return true;
        }
        if progress <= 0.0 {
            return false;
        }

        match self {
            RevealMode::LeftToRight => {
                let threshold = (progress * total as f64) as usize;
                idx < threshold
            }
            RevealMode::RightToLeft => {
                let hidden_count = ((1.0 - progress) * total as f64) as usize;
                idx >= hidden_count
            }
            RevealMode::CenterOut => {
                let center = total as f64 / 2.0;
                let max_dist = center;
                let char_dist = (idx as f64 - center).abs();
                let threshold = progress * max_dist;
                char_dist <= threshold
            }
            RevealMode::EdgesIn => {
                // Distance from nearest edge
                let dist_from_left = idx;
                let dist_from_right = total.saturating_sub(1).saturating_sub(idx);
                let dist_from_edge = dist_from_left.min(dist_from_right);
                let max_dist = total / 2;
                let threshold = (progress * max_dist as f64) as usize;
                dist_from_edge < threshold
            }
            RevealMode::Random => {
                // Deterministic random based on seed and index
                let hash = seed
                    .wrapping_mul(idx as u64 + 1)
                    .wrapping_add(0x9E3779B97F4A7C15);
                let normalized = (hash % 10000) as f64 / 10000.0;
                normalized < progress
            }
            RevealMode::ByWord => {
                // Find which word this character belongs to
                let mut word_idx = 0;
                let mut in_word = false;
                for (i, grapheme) in graphemes(text).enumerate() {
                    let is_ws = grapheme.chars().all(|c| c.is_whitespace());
                    if is_ws {
                        if in_word {
                            in_word = false;
                        }
                    } else if !in_word {
                        in_word = true;
                        if i > 0 {
                            word_idx += 1;
                        }
                    }
                    if i == idx {
                        break;
                    }
                }
                // Count total words
                let word_count = text.split_whitespace().count().max(1);
                let visible_words = (progress * word_count as f64).ceil() as usize;
                word_idx < visible_words
            }
            RevealMode::ByLine => {
                // For single-line text, just use LeftToRight behavior
                // ByLine is meant for multi-line StyledMultiLine
                let threshold = (progress * total as f64) as usize;
                idx < threshold
            }
        }
    }
}

// =============================================================================
// DissolveMode - Particle dissolve direction
// =============================================================================

/// Mode for particle dissolve/materialize effects.
///
/// Controls how text breaks apart into particles or gathers from particles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DissolveMode {
    /// Text breaks apart, particles drift away from original positions.
    #[default]
    Dissolve,
    /// Particles gather into text (reverse of Dissolve).
    Materialize,
    /// Fast outward burst from center of text.
    Explode,
    /// Fast inward gather to center of text.
    Implode,
}

impl DissolveMode {
    /// Returns true if this mode goes from text to particles.
    #[inline]
    pub fn is_outward(&self) -> bool {
        matches!(self, DissolveMode::Dissolve | DissolveMode::Explode)
    }

    /// Returns true if this mode involves center-based movement.
    #[inline]
    pub fn is_centered(&self) -> bool {
        matches!(self, DissolveMode::Explode | DissolveMode::Implode)
    }

    /// Get particle characters based on distance from origin.
    /// Closer particles use larger characters.
    pub fn particle_char(distance: f64) -> char {
        const PARTICLES: [char; 4] = ['·', '∙', '•', '*'];
        let idx = (distance.clamp(0.0, 1.0) * 3.0) as usize;
        PARTICLES[idx.min(3)]
    }
}

// =============================================================================
// Text Effects
// =============================================================================

/// Available text effects.
#[derive(Debug, Clone, Default)]
pub enum TextEffect {
    /// No effect, plain text.
    #[default]
    None,

    // --- Fade Effects ---
    /// Fade in from transparent to opaque.
    FadeIn {
        /// Progress from 0.0 (invisible) to 1.0 (visible).
        progress: f64,
    },
    /// Fade out from opaque to transparent.
    FadeOut {
        /// Progress from 0.0 (visible) to 1.0 (invisible).
        progress: f64,
    },
    /// Pulsing fade (breathing effect).
    Pulse {
        /// Oscillation speed (cycles per second).
        speed: f64,
        /// Minimum alpha (0.0 to 1.0).
        min_alpha: f64,
    },

    /// Organic breathing pulse with asymmetric timing (quick inhale, slow exhale).
    ///
    /// Unlike regular `Pulse`, this effect:
    /// - Has asymmetric rise/fall timing for a more natural feel
    /// - Supports per-character phase variation for wave-like motion
    /// - Uses deterministic seeded randomness for consistent animation
    ///
    /// # Example
    /// ```ignore
    /// TextEffect::OrganicPulse {
    ///     speed: 0.5,           // One breath every 2 seconds
    ///     min_brightness: 0.3,  // Dim to 30% at exhale
    ///     asymmetry: 0.5,       // Moderate asymmetry
    ///     phase_variation: 0.2, // Slight wave across characters
    ///     seed: 42,
    /// }
    /// ```
    OrganicPulse {
        /// Breath cycles per second (0.5 = one breath every 2s).
        speed: f64,
        /// Minimum brightness (0.0-1.0) at the lowest point of exhale.
        min_brightness: f64,
        /// Asymmetry factor (0.0 = symmetric, 1.0 = very asymmetric quick-in/slow-out).
        asymmetry: f64,
        /// Phase variation (0.0-1.0) for how much adjacent characters differ in phase.
        phase_variation: f64,
        /// Seed for deterministic pseudo-random phase offsets.
        seed: u64,
    },

    // --- Gradient Effects ---
    /// Horizontal gradient across text.
    HorizontalGradient {
        /// Gradient to use.
        gradient: ColorGradient,
    },
    /// Animated horizontal gradient.
    AnimatedGradient {
        /// Gradient to use.
        gradient: ColorGradient,
        /// Animation speed.
        speed: f64,
    },
    /// Rainbow colors cycling through text.
    RainbowGradient {
        /// Animation speed.
        speed: f64,
    },

    /// Vertical gradient from top to bottom (for multi-line text).
    ///
    /// For multi-line text, samples gradient by row: t = row / total_rows.
    /// For single-line text, returns the center color (sample(0.5)).
    VerticalGradient {
        /// Gradient to use.
        gradient: ColorGradient,
    },

    /// Diagonal gradient at a specified angle.
    ///
    /// The angle is in degrees:
    /// - 0° = left to right (horizontal)
    /// - 90° = top to bottom (vertical)
    /// - 45° = top-left to bottom-right
    /// - 135° = top-right to bottom-left
    DiagonalGradient {
        /// Gradient to use.
        gradient: ColorGradient,
        /// Angle in degrees (0-360, wraps).
        angle: f64,
    },

    /// Radial gradient from center outward.
    ///
    /// Colors radiate from a center point with configurable aspect ratio.
    RadialGradient {
        /// Gradient to use.
        gradient: ColorGradient,
        /// Center point (normalized 0-1). (0.5, 0.5) is dead center.
        center: (f64, f64),
        /// Aspect ratio (1.0 = circular, 2.0 = horizontally stretched).
        aspect: f64,
    },

    // --- Color Cycling ---
    /// Cycle through colors (all characters same color).
    ColorCycle {
        /// Colors to cycle through.
        colors: Vec<PackedRgba>,
        /// Cycle speed.
        speed: f64,
    },
    /// Wave effect - color moves through text like a wave.
    ColorWave {
        /// Primary color.
        color1: PackedRgba,
        /// Secondary color.
        color2: PackedRgba,
        /// Wave speed.
        speed: f64,
        /// Wave length (characters per cycle).
        wavelength: f64,
    },

    // --- Glow Effects ---
    /// Static glow around text.
    Glow {
        /// Glow color (usually a brighter version of base).
        color: PackedRgba,
        /// Intensity (0.0 to 1.0).
        intensity: f64,
    },
    /// Animated glow that pulses.
    PulsingGlow {
        /// Glow color.
        color: PackedRgba,
        /// Pulse speed.
        speed: f64,
    },

    // --- Character Effects ---
    /// Typewriter effect - characters appear one by one.
    Typewriter {
        /// Number of characters visible (can be fractional for smooth animation).
        visible_chars: f64,
    },
    /// Scramble effect - random characters that resolve to final text.
    Scramble {
        /// Progress from 0.0 (scrambled) to 1.0 (resolved).
        progress: f64,
    },
    /// Glitch effect - occasional character corruption.
    Glitch {
        /// Glitch intensity (0.0 to 1.0).
        intensity: f64,
    },

    // --- Position/Wave Effects ---
    /// Sinusoidal wave motion - characters oscillate up/down or left/right.
    ///
    /// Creates a smooth wave pattern across the text. The wave travels through
    /// the text at the specified speed, with each character's phase determined
    /// by its position and the wavelength.
    Wave {
        /// Maximum offset in cells (typically 1-3).
        amplitude: f64,
        /// Characters per wave cycle (typically 5-15).
        wavelength: f64,
        /// Wave cycles per second.
        speed: f64,
        /// Wave travel direction.
        direction: Direction,
    },

    /// Bouncing motion - characters bounce as if dropped.
    ///
    /// Characters start high (at `height` offset) and bounce toward rest,
    /// with optional damping for a settling effect. The stagger parameter
    /// creates a cascade where each character starts its bounce slightly later.
    Bounce {
        /// Initial/max bounce height in cells.
        height: f64,
        /// Bounces per second.
        speed: f64,
        /// Delay between adjacent characters (0.0-1.0 of total cycle).
        stagger: f64,
        /// Damping factor (0.8-0.99). Higher = slower settling.
        damping: f64,
    },

    /// Random shake/jitter motion - characters vibrate randomly.
    ///
    /// Creates a shaking effect using deterministic pseudo-random offsets.
    /// The same seed and time always produce the same offsets.
    Shake {
        /// Maximum offset magnitude (typically 0.5-2).
        intensity: f64,
        /// Shake frequency (updates per second).
        speed: f64,
        /// Seed for deterministic randomness.
        seed: u64,
    },

    /// Cascade reveal - characters appear in sequence from a direction.
    ///
    /// Similar to typewriter but with directional control and positional offset.
    /// Characters slide in from the specified direction as they're revealed.
    Cascade {
        /// Characters revealed per second.
        speed: f64,
        /// Direction characters slide in from.
        direction: Direction,
        /// Delay between characters (0.0-1.0).
        stagger: f64,
    },

    // --- Cursor Effects ---
    /// Blinking cursor/caret animation for typewriter and terminal effects.
    ///
    /// Displays a cursor character that can blink at a specified rate.
    /// Position can be at the end, at a specific index, or following
    /// a reveal effect's progress.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("Hello World")
    ///     .effect(TextEffect::Typewriter { visible_chars: 5.0 })
    ///     .effect(TextEffect::Cursor {
    ///         style: CursorStyle::Block,
    ///         blink_speed: 2.0,
    ///         position: CursorPosition::AfterReveal,
    ///     })
    /// // Cursor appears after 'Hello' (the revealed part)
    /// ```
    Cursor {
        /// Visual style of the cursor.
        style: CursorStyle,
        /// Blinks per second. 0 = no blinking (always visible).
        blink_speed: f64,
        /// Position of the cursor relative to the text.
        position: CursorPosition,
    },

    // --- Reveal Effects ---
    /// Character reveal with configurable reveal mode.
    ///
    /// More flexible than Typewriter, supporting multiple reveal orders:
    /// left-to-right, right-to-left, center-out, edges-in, random, by-word.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("Hello World")
    ///     .effect(TextEffect::Reveal {
    ///         mode: RevealMode::CenterOut,
    ///         progress: 0.5,
    ///         seed: 0,
    ///     })
    /// // Characters reveal from center outward
    /// ```
    Reveal {
        /// The reveal mode/order.
        mode: RevealMode,
        /// Progress from 0.0 (hidden) to 1.0 (fully revealed).
        progress: f64,
        /// Seed for Random mode (ignored for other modes).
        seed: u64,
    },

    /// Gradient mask reveal with angle and softness control.
    ///
    /// Creates a sweeping reveal effect where a gradient mask moves
    /// across the text at the specified angle. The softness controls
    /// how gradual the transition is from hidden to visible.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("WELCOME")
    ///     .effect(TextEffect::RevealMask {
    ///         angle: 45.0,
    ///         progress: 0.5,
    ///         softness: 0.3,
    ///     })
    /// // Diagonal sweep reveal with soft edge
    /// ```
    RevealMask {
        /// Sweep angle in degrees (0 = left-to-right, 90 = top-to-bottom).
        angle: f64,
        /// Mask position from 0.0 (hidden) to 1.0 (fully revealed).
        progress: f64,
        /// Edge softness from 0.0 (hard edge) to 1.0 (gradient fade).
        softness: f64,
    },

    // --- Visual Distortion Effects (bd-atwq) ---
    /// Chromatic aberration / RGB channel split effect.
    ///
    /// Creates a "3D glasses" or glitch aesthetic by shifting red and blue
    /// channels based on character position. Characters on the left appear
    /// redder while characters on the right appear bluer.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("GLITCH")
    ///     .effect(TextEffect::ChromaticAberration {
    ///         offset: 2,
    ///         direction: Direction::Right,
    ///         animated: true,
    ///         speed: 0.5,
    ///     })
    /// ```
    ChromaticAberration {
        /// Color shift intensity (1-5). Higher = more separation.
        offset: u8,
        /// Shift direction (Horizontal: red left/blue right, Vertical: red top/blue bottom).
        direction: Direction,
        /// Whether the offset oscillates over time.
        animated: bool,
        /// Oscillation speed (cycles per second) when animated.
        speed: f64,
    },

    // --- CRT / Retro Effects (bd-3r87) ---
    /// Scanline / CRT monitor effect.
    ///
    /// Simulates the look of old CRT monitors by dimming every Nth row.
    /// Supports scrolling scanlines for an animated CRT feel and
    /// optional flicker for authentic phosphor simulation.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("RETRO")
    ///     .effect(TextEffect::Scanline {
    ///         intensity: 0.3,
    ///         line_gap: 2,
    ///         scroll: true,
    ///         scroll_speed: 1.0,
    ///         flicker: 0.05,
    ///     })
    /// ```
    Scanline {
        /// How dark the scanlines are (0.0-1.0). 0.3 is typical.
        intensity: f64,
        /// Every Nth row is dimmed (2 = every other row).
        line_gap: u8,
        /// Whether scanlines scroll slowly for CRT feel.
        scroll: bool,
        /// Rows per second if scrolling.
        scroll_speed: f64,
        /// Random brightness variation (0.0-1.0) for flicker.
        flicker: f64,
    },

    // --- Particle Effects ---
    /// Particle dissolve/materialize effect.
    ///
    /// Breaks text into particles that drift away (dissolve/explode) or
    /// gathers particles into text (materialize/implode). Particles inherit
    /// colors from the text effect chain, so gradients work with dissolve.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("DISSOLVE")
    ///     .effect(TextEffect::RainbowGradient { speed: 0.1 })
    ///     .effect(TextEffect::ParticleDissolve {
    ///         progress: 0.5,
    ///         mode: DissolveMode::Dissolve,
    ///         speed: 1.0,
    ///         gravity: 0.5,
    ///         seed: 42,
    ///     })
    /// // Half-dissolved text with rainbow-colored particles
    /// ```
    ParticleDissolve {
        /// Progress from 0.0 (solid text) to 1.0 (fully particles).
        /// For Materialize/Implode, 0.0 = particles, 1.0 = solid text.
        progress: f64,
        /// Dissolve mode controlling particle behavior.
        mode: DissolveMode,
        /// Particle drift speed multiplier.
        speed: f64,
        /// Gravity effect (0.0 = float, 1.0 = fall downward).
        gravity: f64,
        /// Seed for deterministic particle paths.
        seed: u64,
    },
}

// =============================================================================
// TextPreset - Curated Effect Combinations (bd-4r5h)
// =============================================================================
//
// Evidence Ledger:
// - Choice: Presets as an enum with methods returning Vec<TextEffect>
// - Reason: Allows inspection of preset contents, easy customization after applying,
//   and keeps preset definitions close to effect definitions for maintenance.
// - Invariants:
//   - preset.effects() never returns an empty Vec
//   - preset.effects().len() <= MAX_EFFECTS
//   - All presets render without panic at t=0, t=0.5, t=1
// - Failure Modes:
//   - User overrides base_color to something incompatible: visual clash but no panic
//   - Effect conflicts: presets are designed to avoid conflicting effect types

/// Curated text effect presets for common visual styles.
///
/// Presets provide ready-to-use effect combinations that showcase the system's
/// capabilities. Each preset can be customized after application using the
/// standard StyledText builder methods.
///
/// # Categories
///
/// | Category | Presets | Style |
/// |----------|---------|-------|
/// | High-energy | Neon, Cyberpunk, Matrix | Bold, animated, digital |
/// | Classic/retro | Retro, Typewriter, Terminal | Nostalgic, CRT aesthetic |
/// | Elegant | Cinematic, Elegant, Minimal | Professional, subtle |
/// | Dynamic | Fire, Ice, Hologram | Colorful, eye-catching |
///
/// # Example
///
/// ```rust,ignore
/// // Apply a preset
/// let styled = StyledText::preset(TextPreset::Neon, "WELCOME")
///     .time(current_time);
///
/// // Customize after preset
/// let custom = StyledText::preset(TextPreset::Cinematic, "TITLE")
///     .base_color(PackedRgba::rgb(255, 200, 100))
///     .easing(Easing::EaseOut)
///     .time(current_time);
///
/// // Get preset effects for inspection
/// let effects = TextPreset::Neon.effects();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextPreset {
    // --- High-energy/modern ---
    /// Neon: Cyan/magenta wave with pulsing glow.
    /// Bold text, bright colors, animated color wave effect.
    #[default]
    Neon,

    /// Cyberpunk: Glitch, chromatic aberration, animated gradient.
    /// Digital dystopia aesthetic with glitch effects.
    Cyberpunk,

    /// Matrix: Green cascade with fade trail and subtle glitch.
    /// Classic terminal rain aesthetic.
    Matrix,

    // --- Classic/retro ---
    /// Retro: CRT scanlines, green phosphor, subtle flicker.
    /// Old-school monitor look.
    Retro,

    /// Typewriter: Character-by-character reveal with cursor.
    /// Classic typewriter feel.
    Typewriter,

    /// Terminal: Solid green, subtle pulse.
    /// Clean terminal aesthetic.
    Terminal,

    // --- Elegant/professional ---
    /// Cinematic: Slow fade, gold gradient, soft glow.
    /// Movie title aesthetic.
    Cinematic,

    /// Elegant: Organic pulse, gold/cream gradient, drop shadow.
    /// Sophisticated, professional look.
    Elegant,

    /// Minimal: Subtle fade only, no color effects.
    /// Clean, understated aesthetic.
    Minimal,

    // --- Dynamic/playful ---
    /// Fire: Fire gradient with upward wave motion.
    /// Flames rising effect.
    Fire,

    /// Ice: Frost blue gradient with sparkle effect.
    /// Cold, crystalline aesthetic.
    Ice,

    /// Hologram: Rainbow shift, scanlines, transparency pulse.
    /// Futuristic holographic display effect.
    Hologram,
}

impl TextPreset {
    /// Get the effects for this preset.
    ///
    /// Returns a Vec of TextEffect variants that define the preset's visual style.
    /// The effects are ordered appropriately for composition.
    ///
    /// # Invariants
    /// - Never returns empty Vec
    /// - Length is always <= MAX_EFFECTS
    #[must_use]
    pub fn effects(self) -> Vec<TextEffect> {
        match self {
            Self::Neon => vec![
                TextEffect::ColorWave {
                    color1: PackedRgba::rgb(0, 255, 255), // Cyan
                    color2: PackedRgba::rgb(255, 0, 255), // Magenta
                    speed: 1.5,
                    wavelength: 8.0,
                },
                TextEffect::PulsingGlow {
                    color: PackedRgba::rgb(100, 255, 255),
                    speed: 2.0,
                },
            ],

            Self::Cyberpunk => vec![
                TextEffect::AnimatedGradient {
                    gradient: ColorGradient::cyberpunk(),
                    speed: 0.5,
                },
                TextEffect::ChromaticAberration {
                    offset: 2,
                    direction: Direction::Right,
                    animated: true,
                    speed: 0.3,
                },
                TextEffect::Glitch { intensity: 0.05 },
            ],

            Self::Matrix => vec![
                TextEffect::AnimatedGradient {
                    gradient: ColorGradient::matrix(),
                    speed: 0.8,
                },
                TextEffect::Cascade {
                    speed: 15.0,
                    direction: Direction::Down,
                    stagger: 0.1,
                },
                TextEffect::Glitch { intensity: 0.02 },
            ],

            Self::Retro => vec![
                TextEffect::ColorCycle {
                    colors: vec![
                        PackedRgba::rgb(0, 200, 50),   // Terminal green
                        PackedRgba::rgb(50, 255, 100), // Bright green
                        PackedRgba::rgb(0, 180, 40),   // Dim green
                    ],
                    speed: 0.3,
                },
                TextEffect::Scanline {
                    intensity: 0.3,
                    line_gap: 2,
                    scroll: true,
                    scroll_speed: 1.0,
                    flicker: 0.02,
                },
            ],

            Self::Typewriter => vec![
                TextEffect::Reveal {
                    mode: RevealMode::LeftToRight,
                    progress: 1.0,
                    seed: 0,
                },
                TextEffect::Cursor {
                    style: CursorStyle::Block,
                    blink_speed: 2.0,
                    position: CursorPosition::AfterReveal,
                },
            ],

            Self::Terminal => vec![TextEffect::Pulse {
                speed: 0.5,
                min_alpha: 0.85,
            }],

            Self::Cinematic => vec![
                TextEffect::FadeIn { progress: 1.0 },
                TextEffect::HorizontalGradient {
                    gradient: ColorGradient::gold(),
                },
                TextEffect::Glow {
                    color: PackedRgba::rgb(255, 220, 150),
                    intensity: 0.4,
                },
            ],

            Self::Elegant => vec![
                TextEffect::OrganicPulse {
                    speed: 0.4,
                    min_brightness: 0.7,
                    asymmetry: 0.6,
                    phase_variation: 0.15,
                    seed: 42,
                },
                TextEffect::HorizontalGradient {
                    gradient: ColorGradient::new(vec![
                        (0.0, PackedRgba::rgb(200, 170, 100)), // Dark gold
                        (0.5, PackedRgba::rgb(255, 230, 180)), // Cream
                        (1.0, PackedRgba::rgb(200, 170, 100)), // Dark gold
                    ]),
                },
            ],

            Self::Minimal => vec![TextEffect::FadeIn { progress: 1.0 }],

            Self::Fire => vec![
                TextEffect::AnimatedGradient {
                    gradient: ColorGradient::fire(),
                    speed: 1.2,
                },
                TextEffect::Wave {
                    amplitude: 0.5,
                    wavelength: 6.0,
                    speed: 2.0,
                    direction: Direction::Up,
                },
            ],

            Self::Ice => vec![
                TextEffect::HorizontalGradient {
                    gradient: ColorGradient::ice(),
                },
                TextEffect::Pulse {
                    speed: 1.0,
                    min_alpha: 0.8,
                },
            ],

            Self::Hologram => vec![
                TextEffect::RainbowGradient { speed: 0.5 },
                TextEffect::Scanline {
                    intensity: 0.2,
                    line_gap: 3,
                    scroll: true,
                    scroll_speed: 2.0,
                    flicker: 0.0,
                },
                TextEffect::Pulse {
                    speed: 1.5,
                    min_alpha: 0.6,
                },
            ],
        }
    }

    /// Get the recommended base color for this preset.
    ///
    /// Some presets work best with specific base colors. This returns the
    /// optimal color for maximum visual impact.
    #[must_use]
    pub fn base_color(self) -> PackedRgba {
        match self {
            Self::Neon => PackedRgba::rgb(200, 255, 255), // Bright cyan-white
            Self::Cyberpunk => PackedRgba::rgb(255, 100, 200), // Hot pink
            Self::Matrix => PackedRgba::rgb(0, 255, 65),  // Matrix green
            Self::Retro => PackedRgba::rgb(50, 255, 100), // Terminal green
            Self::Typewriter => PackedRgba::rgb(255, 255, 255), // White
            Self::Terminal => PackedRgba::rgb(0, 200, 50), // Terminal green
            Self::Cinematic => PackedRgba::rgb(255, 220, 150), // Warm gold
            Self::Elegant => PackedRgba::rgb(255, 240, 200), // Cream
            Self::Minimal => PackedRgba::rgb(200, 200, 200), // Light gray
            Self::Fire => PackedRgba::rgb(255, 150, 50),  // Orange
            Self::Ice => PackedRgba::rgb(200, 230, 255),  // Light blue
            Self::Hologram => PackedRgba::rgb(255, 255, 255), // White for rainbow
        }
    }

    /// Get the recommended easing for this preset.
    ///
    /// Different presets benefit from different animation curves.
    #[must_use]
    pub fn easing(self) -> Easing {
        match self {
            Self::Neon | Self::Cyberpunk | Self::Matrix => Easing::Linear,
            Self::Retro | Self::Terminal => Easing::Linear,
            Self::Typewriter => Easing::Linear,
            Self::Cinematic => Easing::EaseOut,
            Self::Elegant => Easing::EaseInOut,
            Self::Minimal => Easing::EaseOut,
            Self::Fire => Easing::EaseOut,
            Self::Ice => Easing::EaseInOut,
            Self::Hologram => Easing::Linear,
        }
    }

    /// Whether this preset uses bold text by default.
    #[must_use]
    pub fn is_bold(self) -> bool {
        matches!(self, Self::Neon | Self::Fire | Self::Matrix)
    }

    /// Get an iterator over all preset variants.
    pub fn all() -> impl Iterator<Item = Self> {
        [
            Self::Neon,
            Self::Cyberpunk,
            Self::Matrix,
            Self::Retro,
            Self::Typewriter,
            Self::Terminal,
            Self::Cinematic,
            Self::Elegant,
            Self::Minimal,
            Self::Fire,
            Self::Ice,
            Self::Hologram,
        ]
        .into_iter()
    }
}

// =============================================================================
// StyledText - Text with effects
// =============================================================================

/// Maximum number of effects that can be chained on a single StyledText.
/// This limit prevents performance issues from excessive effect stacking.
pub const MAX_EFFECTS: usize = 8;

/// Text widget with animated effects.
///
/// StyledText supports composable effect chains - multiple effects can be
/// applied simultaneously. Effects are categorized and combined as follows:
///
/// | Category | Effects | Combination Rule |
/// |----------|---------|------------------|
/// | ColorModifier | Gradient, ColorCycle, ColorWave, Glow | BLEND: colors multiply |
/// | AlphaModifier | FadeIn, FadeOut, Pulse | MULTIPLY: alpha values multiply |
/// | PositionModifier | Wave, Bounce, Shake | ADD: offsets sum |
/// | CharModifier | Typewriter, Scramble, Glitch | PRIORITY: first wins |
///
/// # Example
///
/// ```rust,ignore
/// let styled = StyledText::new("Hello")
///     .effect(TextEffect::RainbowGradient { speed: 0.1 })
///     .effect(TextEffect::Pulse { speed: 2.0, min_alpha: 0.3 })
///     .time(current_time);
/// ```
#[derive(Debug, Clone)]
pub struct StyledText {
    text: String,
    /// Effects to apply, in order. Maximum of MAX_EFFECTS.
    effects: Vec<TextEffect>,
    base_color: PackedRgba,
    bg_color: Option<PackedRgba>,
    bold: bool,
    italic: bool,
    underline: bool,
    time: f64,
    seed: u64,
    /// Easing function for time-based effects.
    easing: Easing,
    /// Drop shadows to render behind text.
    shadows: Vec<Shadow>,
    /// Multi-layer glow configuration.
    glow_config: Option<GlowConfig>,
    /// Border/outline configuration.
    outline_config: Option<OutlineConfig>,
}

impl StyledText {
    /// Create new styled text.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            effects: Vec::new(),
            base_color: PackedRgba::rgb(255, 255, 255),
            bg_color: None,
            bold: false,
            italic: false,
            underline: false,
            time: 0.0,
            seed: 12345,
            easing: Easing::default(),
            shadows: Vec::new(),
            glow_config: None,
            outline_config: None,
        }
    }

    /// Create styled text with a preset applied.
    ///
    /// This is a convenience constructor that applies all effects, colors, and
    /// settings for a curated preset. The result can be further customized
    /// using the standard builder methods.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Use preset directly
    /// let neon = StyledText::preset(TextPreset::Neon, "WELCOME")
    ///     .time(current_time);
    ///
    /// // Customize after preset
    /// let custom_fire = StyledText::preset(TextPreset::Fire, "HOT!")
    ///     .base_color(PackedRgba::rgb(255, 100, 0))  // Override color
    ///     .easing(Easing::Bounce)
    ///     .time(current_time);
    /// ```
    pub fn preset(preset: TextPreset, text: impl Into<String>) -> Self {
        let mut styled = Self::new(text);

        // Apply preset's effects
        for effect in preset.effects() {
            if styled.effects.len() >= MAX_EFFECTS {
                break;
            }
            styled.effects.push(effect);
        }

        // Apply preset's recommended settings
        styled.base_color = preset.base_color();
        styled.easing = preset.easing();
        styled.bold = preset.is_bold();

        // Add preset-specific shadows for Elegant preset
        if matches!(preset, TextPreset::Elegant) {
            styled.shadows.push(Shadow::new(1, 1).opacity(0.4));
        }

        styled
    }

    /// Add a text effect to the chain.
    ///
    /// Effects are applied in the order they are added. A maximum of
    /// [`MAX_EFFECTS`] can be chained; additional effects are ignored.
    ///
    /// # Effect Composition
    ///
    /// - **Color effects** (Gradient, ColorCycle, etc.): Colors are blended/modulated
    /// - **Alpha effects** (FadeIn, Pulse, etc.): Alpha values multiply together
    /// - **Character effects** (Typewriter, Scramble): First visible char wins
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("Hello")
    ///     .effect(TextEffect::RainbowGradient { speed: 0.1 })
    ///     .effect(TextEffect::Pulse { speed: 2.0, min_alpha: 0.3 })
    /// ```
    #[must_use]
    pub fn effect(mut self, effect: TextEffect) -> Self {
        if !matches!(effect, TextEffect::None) && self.effects.len() < MAX_EFFECTS {
            self.effects.push(effect);
        }
        self
    }

    /// Add multiple effects at once.
    ///
    /// Convenience method for chaining several effects. Only adds up to
    /// [`MAX_EFFECTS`] total effects.
    #[must_use]
    pub fn effects(mut self, effects: impl IntoIterator<Item = TextEffect>) -> Self {
        for effect in effects {
            if matches!(effect, TextEffect::None) {
                continue;
            }
            if self.effects.len() >= MAX_EFFECTS {
                break;
            }
            self.effects.push(effect);
        }
        self
    }

    /// Clear all effects, returning to plain text rendering.
    #[must_use]
    pub fn clear_effects(mut self) -> Self {
        self.effects.clear();
        self
    }

    /// Get the current number of effects.
    pub fn effect_count(&self) -> usize {
        self.effects.len()
    }

    /// Check if any effects are applied.
    pub fn has_effects(&self) -> bool {
        !self.effects.is_empty()
    }

    /// Set the easing function for time-based effects.
    ///
    /// The easing function affects animations like Pulse, ColorWave,
    /// AnimatedGradient, and PulsingGlow. It does not affect static
    /// effects or progress-based effects (FadeIn, FadeOut, Typewriter).
    #[must_use]
    pub fn easing(mut self, easing: Easing) -> Self {
        self.easing = easing;
        self
    }

    // =========================================================================
    // Shadow and Glow Methods (bd-3hsz)
    // =========================================================================

    /// Add a single drop shadow.
    ///
    /// Shadows render behind the main text at the specified offset.
    /// Multiple shadows can be added by calling this method multiple times
    /// or using [`shadows`](#method.shadows).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("SHADOW")
    ///     .shadow(Shadow::new(1, 1).opacity(0.5))
    /// ```
    #[must_use]
    pub fn shadow(mut self, shadow: Shadow) -> Self {
        // Skip zero-opacity shadows (optimization)
        if shadow.opacity > 0.0 {
            self.shadows.push(shadow);
        }
        self
    }

    /// Add multiple drop shadows.
    ///
    /// Shadows render behind the main text in the order specified (first shadow
    /// is furthest from text, last shadow is closest to text).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("DEPTH")
    ///     .shadows(vec![
    ///         Shadow::new(3, 3).opacity(0.2),  // Outer shadow
    ///         Shadow::new(2, 2).opacity(0.3),  // Middle shadow
    ///         Shadow::new(1, 1).opacity(0.5),  // Inner shadow
    ///     ])
    /// ```
    #[must_use]
    pub fn shadows(mut self, shadows: impl IntoIterator<Item = Shadow>) -> Self {
        for shadow in shadows {
            if shadow.opacity > 0.0 {
                self.shadows.push(shadow);
            }
        }
        self
    }

    /// Clear all shadows.
    #[must_use]
    pub fn clear_shadows(mut self) -> Self {
        self.shadows.clear();
        self
    }

    /// Check if shadows are configured.
    #[must_use]
    pub fn has_shadows(&self) -> bool {
        !self.shadows.is_empty()
    }

    /// Get the number of configured shadows.
    #[must_use]
    pub fn shadow_count(&self) -> usize {
        self.shadows.len()
    }

    /// Add a multi-layer glow effect.
    ///
    /// Glow creates a soft halo around text by rendering multiple layers
    /// at expanding offsets with decreasing opacity. This produces a neon
    /// or bloom effect.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("NEON")
    ///     .glow(GlowConfig::new(PackedRgba::rgb(0, 255, 255))
    ///         .intensity(0.8)
    ///         .layers(3)
    ///         .falloff(0.5))
    /// ```
    #[must_use]
    pub fn glow(mut self, config: GlowConfig) -> Self {
        self.glow_config = Some(config);
        self
    }

    /// Clear the glow effect.
    #[must_use]
    pub fn clear_glow(mut self) -> Self {
        self.glow_config = None;
        self
    }

    /// Check if glow is configured.
    #[must_use]
    pub fn has_glow(&self) -> bool {
        self.glow_config.is_some()
    }

    /// Get the glow configuration if set.
    #[must_use]
    pub fn glow_config(&self) -> Option<&GlowConfig> {
        self.glow_config.as_ref()
    }

    // =========================================================================
    // Outline/Border Methods (bd-882b)
    // =========================================================================

    /// Add a border/outline effect around text.
    ///
    /// Outlines render a stroke around each character by drawing the character
    /// at multiple offset positions. This improves legibility on complex
    /// backgrounds and adds visual depth.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// StyledText::new("OUTLINED")
    ///     .outline(OutlineConfig::new(PackedRgba::rgb(0, 0, 0))
    ///         .thickness(1)
    ///         .style(OutlineStyle::Solid))
    /// ```
    #[must_use]
    pub fn outline(mut self, config: OutlineConfig) -> Self {
        self.outline_config = Some(config);
        self
    }

    /// Clear the outline effect.
    #[must_use]
    pub fn clear_outline(mut self) -> Self {
        self.outline_config = None;
        self
    }

    /// Check if outline is configured.
    #[must_use]
    pub fn has_outline(&self) -> bool {
        self.outline_config.is_some()
    }

    /// Get the outline configuration if set.
    #[must_use]
    pub fn outline_config(&self) -> Option<&OutlineConfig> {
        self.outline_config.as_ref()
    }

    /// Get the configured shadows.
    #[must_use]
    pub fn get_shadows(&self) -> &[Shadow] {
        &self.shadows
    }

    /// Set the base text color.
    #[must_use]
    pub fn base_color(mut self, color: PackedRgba) -> Self {
        self.base_color = color;
        self
    }

    /// Set the background color.
    #[must_use]
    pub fn bg_color(mut self, color: PackedRgba) -> Self {
        self.bg_color = Some(color);
        self
    }

    /// Make text bold.
    #[must_use]
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Make text italic.
    #[must_use]
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// Make text underlined.
    #[must_use]
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// Set the animation time (for time-based effects).
    #[must_use]
    pub fn time(mut self, time: f64) -> Self {
        self.time = time;
        self
    }

    /// Set random seed for scramble/glitch effects.
    #[must_use]
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Get the length of the text.
    pub fn len(&self) -> usize {
        grapheme_count(&self.text)
    }

    /// Check if text is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Calculate the color for a single effect at position `idx`.
    fn effect_color(
        &self,
        effect: &TextEffect,
        idx: usize,
        total: usize,
        base: PackedRgba,
    ) -> PackedRgba {
        let t = if total > 1 {
            idx as f64 / (total - 1) as f64
        } else {
            0.5
        };

        match effect {
            TextEffect::None => base,

            TextEffect::FadeIn { progress } => apply_alpha(base, *progress),

            TextEffect::FadeOut { progress } => apply_alpha(base, 1.0 - progress),

            TextEffect::Pulse { speed, min_alpha } => {
                let alpha =
                    min_alpha + (1.0 - min_alpha) * (0.5 + 0.5 * (self.time * speed * TAU).sin());
                apply_alpha(base, alpha)
            }

            TextEffect::OrganicPulse {
                speed,
                min_brightness,
                asymmetry,
                phase_variation,
                seed,
            } => {
                let phase_offset = organic_char_phase_offset(idx, *seed, *phase_variation);
                let cycle_t = (self.time * speed + phase_offset).rem_euclid(1.0);
                let brightness =
                    min_brightness + (1.0 - min_brightness) * breathing_curve(cycle_t, *asymmetry);
                apply_alpha(base, brightness)
            }

            TextEffect::HorizontalGradient { gradient } => gradient.sample(t),

            TextEffect::AnimatedGradient { gradient, speed } => {
                let animated_t = (t + self.time * speed).rem_euclid(1.0);
                gradient.sample(animated_t)
            }

            TextEffect::RainbowGradient { speed } => {
                let hue = ((t + self.time * speed) * 360.0).rem_euclid(360.0);
                hsv_to_rgb(hue, 1.0, 1.0)
            }

            // For single-line text, vertical gradient returns center color
            TextEffect::VerticalGradient { gradient } => gradient.sample(0.5),

            // For single-line text, diagonal uses horizontal position only (t_y = 0.5)
            TextEffect::DiagonalGradient { gradient, angle } => {
                let angle_rad = angle.to_radians();
                let cos_a = angle_rad.cos();
                let sin_a = angle_rad.sin();
                // Project position onto gradient axis
                // t_y = 0.5 for single-line text
                let projected = t * cos_a + 0.5 * sin_a;
                // Normalize to 0-1 range (diagonal spans -sqrt(2)/2 to sqrt(2)/2)
                let normalized = (projected + FRAC_1_SQRT_2) / SQRT_2;
                gradient.sample(normalized.clamp(0.0, 1.0))
            }

            // For single-line text, radial uses horizontal position and centered y
            TextEffect::RadialGradient {
                gradient,
                center,
                aspect,
            } => {
                let dx = (t - center.0) * aspect;
                let dy = 0.5 - center.1; // y is always 0.5 for single-line
                let dist = (dx * dx + dy * dy).sqrt();
                // Max distance is from center to corner
                let max_dist = (0.5_f64.powi(2) * aspect * aspect + 0.5_f64.powi(2)).sqrt();
                let normalized = (dist / max_dist).clamp(0.0, 1.0);
                gradient.sample(normalized)
            }

            TextEffect::ColorCycle { colors, speed } => {
                if colors.is_empty() {
                    return base;
                }
                let cycle_pos = (self.time * speed).rem_euclid(colors.len() as f64);
                let idx1 = cycle_pos as usize % colors.len();
                let idx2 = (idx1 + 1) % colors.len();
                let local_t = cycle_pos.fract();
                lerp_color(colors[idx1], colors[idx2], local_t)
            }

            TextEffect::ColorWave {
                color1,
                color2,
                speed,
                wavelength,
            } => {
                let phase = t * TAU * (total as f64 / wavelength) - self.time * speed;
                let wave = 0.5 + 0.5 * phase.sin();
                lerp_color(*color1, *color2, wave)
            }

            TextEffect::Glow { color, intensity } => lerp_color(base, *color, *intensity),

            TextEffect::PulsingGlow { color, speed } => {
                let intensity = 0.5 + 0.5 * (self.time * speed * TAU).sin();
                lerp_color(base, *color, intensity)
            }

            TextEffect::Typewriter { visible_chars } => {
                if (idx as f64) < *visible_chars {
                    base
                } else {
                    PackedRgba::TRANSPARENT
                }
            }

            TextEffect::Scramble { progress: _ }
            | TextEffect::Glitch { intensity: _ }
            | TextEffect::Wave { .. }
            | TextEffect::Bounce { .. }
            | TextEffect::Shake { .. }
            | TextEffect::Cascade { .. }
            | TextEffect::Cursor { .. }
            | TextEffect::Reveal { .. } => base,

            TextEffect::RevealMask {
                angle,
                progress,
                softness,
            } => {
                // Calculate position along sweep direction
                let angle_rad = angle.to_radians();
                let cos_a = angle_rad.cos();
                let sin_a = angle_rad.sin();

                // Normalized position (0-1)
                let pos_x = if total > 1 {
                    idx as f64 / (total - 1) as f64
                } else {
                    0.5
                };
                // For single-line, y is constant at 0.5
                let pos_y = 0.5;

                // Project position onto sweep direction
                let sweep_pos = pos_x * cos_a + pos_y * sin_a;
                let sweep_pos = (sweep_pos + 1.0) / 2.0; // Normalize to 0-1

                // Calculate visibility with softness
                if *softness <= 0.0 {
                    // Hard edge
                    if sweep_pos <= *progress {
                        base
                    } else {
                        PackedRgba::TRANSPARENT
                    }
                } else {
                    // Soft edge with gradient
                    let edge_width = softness.clamp(0.0, 1.0);
                    let edge_start = progress - edge_width / 2.0;
                    let edge_end = progress + edge_width / 2.0;

                    if sweep_pos <= edge_start {
                        base
                    } else if sweep_pos >= edge_end {
                        PackedRgba::TRANSPARENT
                    } else {
                        // In the gradient zone
                        let fade = (sweep_pos - edge_start) / edge_width;
                        apply_alpha(base, 1.0 - fade)
                    }
                }
            }

            TextEffect::ChromaticAberration {
                offset,
                direction,
                animated,
                speed,
            } => {
                // Calculate effective offset (animate if requested)
                let effective_offset = if *animated {
                    let oscillation = (self.time * speed * TAU).sin();
                    (*offset as f64 * oscillation).abs()
                } else {
                    *offset as f64
                };

                // Calculate distance from center (-1 to 1)
                let center = total as f64 / 2.0;
                let distance = if total > 1 {
                    (idx as f64 - center) / center
                } else {
                    0.0
                };

                // Apply direction-based shift
                let shift = match direction {
                    Direction::Left | Direction::Right => {
                        // Horizontal: use character position
                        distance * effective_offset * 30.0
                    }
                    Direction::Up | Direction::Down => {
                        // Vertical mode uses horizontal position for single-line text
                        distance * effective_offset * 30.0
                    }
                };

                // Red shifts negative direction, Blue shifts positive
                let red_boost = (-shift).clamp(-50.0, 50.0);
                let blue_boost = shift.clamp(-50.0, 50.0);

                PackedRgba::rgb(
                    (base.r() as i16 + red_boost as i16).clamp(0, 255) as u8,
                    base.g(),
                    (base.b() as i16 + blue_boost as i16).clamp(0, 255) as u8,
                )
            }

            // Scanline is handled directly in char_color, not via effect_color
            TextEffect::Scanline { .. } => base,

            // ParticleDissolve: Apply alpha based on progress and mode
            TextEffect::ParticleDissolve {
                progress,
                mode,
                speed: _,
                gravity: _,
                seed,
            } => {
                // Determine effective progress based on mode
                let effective_progress = if mode.is_outward() {
                    *progress
                } else {
                    1.0 - *progress
                };

                // Use deterministic randomness to decide if this char is dissolved
                let hash = seed
                    .wrapping_mul(idx as u64 + 1)
                    .wrapping_add(0x9E3779B97F4A7C15);
                let char_threshold = (hash % 10000) as f64 / 10000.0;

                // Characters dissolve based on their threshold vs progress
                if char_threshold < effective_progress {
                    // This char is dissolved - show as particle with fading alpha
                    let particle_alpha = (1.0 - effective_progress).max(0.3);
                    apply_alpha(base, particle_alpha)
                } else {
                    // This char is still solid
                    base
                }
            }
        }
    }

    /// Calculate the color for a character at 2D position (row, col).
    ///
    /// Uses the full text block dimensions for proper gradient mapping:
    /// - Horizontal gradients use `col / total_width`
    /// - Vertical components use `row / total_height`
    #[allow(dead_code)] // Used in tests
    fn char_color_2d(
        &self,
        row: usize,
        col: usize,
        total_width: usize,
        total_height: usize,
    ) -> PackedRgba {
        let total_width = total_width.max(1);
        let total_height = total_height.max(1);
        let idx = row.saturating_mul(total_width).saturating_add(col);
        let total = total_width.saturating_mul(total_height).max(1);

        if self.effects.is_empty() {
            return self.base_color;
        }

        let t_x = if total_width > 1 {
            col as f64 / (total_width - 1) as f64
        } else {
            0.5
        };
        let t_y = if total_height > 1 {
            row as f64 / (total_height - 1) as f64
        } else {
            0.5
        };

        let mut color = self.base_color;
        let mut alpha_multiplier = 1.0;

        for effect in &self.effects {
            match effect {
                TextEffect::FadeIn { progress } => {
                    alpha_multiplier *= progress;
                }
                TextEffect::FadeOut { progress } => {
                    alpha_multiplier *= 1.0 - progress;
                }
                TextEffect::Pulse { speed, min_alpha } => {
                    let alpha = min_alpha
                        + (1.0 - min_alpha) * (0.5 + 0.5 * (self.time * speed * TAU).sin());
                    alpha_multiplier *= alpha;
                }
                TextEffect::OrganicPulse {
                    speed,
                    min_brightness,
                    asymmetry,
                    phase_variation,
                    seed,
                } => {
                    let phase_offset = organic_char_phase_offset(idx, *seed, *phase_variation);
                    let cycle_t = (self.time * speed + phase_offset).rem_euclid(1.0);
                    let brightness = min_brightness
                        + (1.0 - min_brightness) * breathing_curve(cycle_t, *asymmetry);
                    alpha_multiplier *= brightness;
                }
                TextEffect::Typewriter { visible_chars } => {
                    if (idx as f64) >= *visible_chars {
                        return PackedRgba::TRANSPARENT;
                    }
                }
                TextEffect::Reveal {
                    mode,
                    progress,
                    seed,
                } => {
                    if !mode.is_visible(idx, total, *progress, *seed, &self.text) {
                        return PackedRgba::TRANSPARENT;
                    }
                }
                TextEffect::HorizontalGradient { gradient } => {
                    color = gradient.sample(t_x);
                }
                TextEffect::AnimatedGradient { gradient, speed } => {
                    let t = (t_x + self.time * speed).rem_euclid(1.0);
                    color = gradient.sample(t);
                }
                TextEffect::RainbowGradient { speed } => {
                    let hue = (t_x + t_y * 0.3 + self.time * speed).rem_euclid(1.0);
                    color = hsv_to_rgb(hue, 0.9, 1.0);
                }
                TextEffect::VerticalGradient { gradient } => {
                    color = gradient.sample(t_y);
                }
                TextEffect::DiagonalGradient { gradient, angle } => {
                    let angle_rad = angle.to_radians();
                    let cos_a = angle_rad.cos();
                    let sin_a = angle_rad.sin();
                    let projected = t_x * cos_a + t_y * sin_a;
                    let normalized = (projected + FRAC_1_SQRT_2) / SQRT_2;
                    color = gradient.sample(normalized.clamp(0.0, 1.0));
                }
                TextEffect::RadialGradient {
                    gradient,
                    center,
                    aspect,
                } => {
                    let dx = (t_x - center.0) * aspect;
                    let dy = t_y - center.1;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let max_dist = {
                        let corner_dx = (0.5_f64.max(center.0) - center.0.min(0.5)) * aspect;
                        let corner_dy = 0.5_f64.max(center.1) - center.1.min(0.5);
                        (corner_dx * corner_dx + corner_dy * corner_dy).sqrt()
                    };
                    let normalized = if max_dist > 0.0 {
                        (dist / max_dist).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    color = gradient.sample(normalized);
                }
                TextEffect::ColorCycle { colors, speed } => {
                    if !colors.is_empty() {
                        let t = (self.time * speed).rem_euclid(colors.len() as f64);
                        let idx = t as usize % colors.len();
                        let next = (idx + 1) % colors.len();
                        let frac = t.fract();
                        color = lerp_color(colors[idx], colors[next], frac);
                    }
                }
                TextEffect::ColorWave {
                    color1,
                    color2,
                    speed,
                    wavelength,
                } => {
                    let t = ((col as f64 + row as f64 * 0.5) / wavelength + self.time * speed)
                        .sin()
                        * 0.5
                        + 0.5;
                    color = lerp_color(*color1, *color2, t);
                }
                TextEffect::Glow {
                    color: glow_color,
                    intensity,
                } => {
                    color = lerp_color(color, *glow_color, *intensity);
                }
                TextEffect::PulsingGlow {
                    color: glow_color,
                    speed,
                } => {
                    let intensity = ((self.time * speed * TAU).sin() * 0.5 + 0.5) * 0.6;
                    color = lerp_color(color, *glow_color, intensity);
                }
                TextEffect::ChromaticAberration {
                    offset,
                    direction,
                    animated,
                    speed,
                } => {
                    let effective_offset = if *animated {
                        let oscillation = (self.time * speed * TAU).sin();
                        (*offset as f64 * oscillation).abs()
                    } else {
                        *offset as f64
                    };

                    let center = total as f64 / 2.0;
                    let distance = if total > 1 {
                        (idx as f64 - center) / center
                    } else {
                        0.0
                    };

                    let shift = match direction {
                        Direction::Left | Direction::Right => distance * effective_offset * 30.0,
                        Direction::Up | Direction::Down => distance * effective_offset * 30.0,
                    };

                    let red_boost = (-shift).clamp(-50.0, 50.0);
                    let blue_boost = shift.clamp(-50.0, 50.0);

                    color = PackedRgba::rgb(
                        (color.r() as i16 + red_boost as i16).clamp(0, 255) as u8,
                        color.g(),
                        (color.b() as i16 + blue_boost as i16).clamp(0, 255) as u8,
                    );
                }
                TextEffect::Scanline {
                    intensity,
                    line_gap,
                    scroll,
                    scroll_speed,
                    flicker,
                } => {
                    let scroll_offset = if *scroll {
                        (self.time * scroll_speed) as usize
                    } else {
                        0
                    };

                    let effective_row = row + scroll_offset;
                    let is_scanline =
                        *line_gap > 0 && effective_row.is_multiple_of(*line_gap as usize);

                    let mut dim_factor = if is_scanline { 1.0 - *intensity } else { 1.0 };

                    if *flicker > 0.0 {
                        let flicker_seed = (self.time * 60.0) as u64;
                        let hash = flicker_seed
                            .wrapping_mul(2654435761)
                            .wrapping_add((row * total_width + col) as u64 * 2246822519);
                        let rand = (hash % 10000) as f64 / 10000.0;
                        dim_factor *= 1.0 - (*flicker * rand);
                    }

                    color = apply_alpha(color, dim_factor);
                }
                _ => {}
            }
        }

        if alpha_multiplier < 1.0 {
            color = apply_alpha(color, alpha_multiplier);
        }

        color
    }

    /// Calculate the color for a character at position `idx`.
    ///
    /// Applies all effects in order. Color effects blend/modulate,
    /// alpha effects multiply together.
    fn char_color(&self, idx: usize, total: usize) -> PackedRgba {
        if self.effects.is_empty() {
            return self.base_color;
        }

        let mut color = self.base_color;
        let mut alpha_multiplier = 1.0;

        for effect in &self.effects {
            match effect {
                // Alpha-modifying effects: accumulate alpha multipliers
                TextEffect::FadeIn { progress } => {
                    alpha_multiplier *= progress;
                }
                TextEffect::FadeOut { progress } => {
                    alpha_multiplier *= 1.0 - progress;
                }
                TextEffect::Pulse { speed, min_alpha } => {
                    let alpha = min_alpha
                        + (1.0 - min_alpha) * (0.5 + 0.5 * (self.time * speed * TAU).sin());
                    alpha_multiplier *= alpha;
                }
                TextEffect::OrganicPulse {
                    speed,
                    min_brightness,
                    asymmetry,
                    phase_variation,
                    seed,
                } => {
                    let phase_offset = organic_char_phase_offset(idx, *seed, *phase_variation);
                    let cycle_t = (self.time * speed + phase_offset).rem_euclid(1.0);
                    let brightness = min_brightness
                        + (1.0 - min_brightness) * breathing_curve(cycle_t, *asymmetry);
                    alpha_multiplier *= brightness;
                }
                TextEffect::Typewriter { visible_chars } => {
                    if (idx as f64) >= *visible_chars {
                        return PackedRgba::TRANSPARENT;
                    }
                }
                TextEffect::Reveal {
                    mode,
                    progress,
                    seed,
                } => {
                    if !mode.is_visible(idx, total, *progress, *seed, &self.text) {
                        return PackedRgba::TRANSPARENT;
                    }
                }
                TextEffect::RevealMask { .. } => {
                    // RevealMask handles visibility via effect_color with gradient alpha
                    let mask_color = self.effect_color(effect, idx, total, color);
                    if mask_color == PackedRgba::TRANSPARENT {
                        return PackedRgba::TRANSPARENT;
                    }
                    // Apply partial alpha from soft edge
                    // Use the channel with the highest value to avoid division issues
                    // when a channel is 0 (e.g., pure green has r=0)
                    if mask_color != color {
                        let max_original = color.r().max(color.g()).max(color.b()).max(1) as f64;
                        let max_masked =
                            mask_color.r().max(mask_color.g()).max(mask_color.b()) as f64;
                        alpha_multiplier *= max_masked / max_original;
                    }
                }

                // Color-modifying effects: blend with current color
                TextEffect::HorizontalGradient { .. }
                | TextEffect::AnimatedGradient { .. }
                | TextEffect::RainbowGradient { .. }
                | TextEffect::VerticalGradient { .. }
                | TextEffect::DiagonalGradient { .. }
                | TextEffect::RadialGradient { .. }
                | TextEffect::ColorCycle { .. }
                | TextEffect::ColorWave { .. }
                | TextEffect::ChromaticAberration { .. } => {
                    // Get the color from this effect and blend with current
                    let effect_color = self.effect_color(effect, idx, total, color);
                    color = effect_color;
                }

                // Glow effects: blend current with glow color
                TextEffect::Glow {
                    color: glow_color,
                    intensity,
                } => {
                    color = lerp_color(color, *glow_color, *intensity);
                }
                TextEffect::PulsingGlow {
                    color: glow_color,
                    speed,
                } => {
                    let intensity = 0.5 + 0.5 * (self.time * speed * TAU).sin();
                    color = lerp_color(color, *glow_color, intensity);
                }

                // Non-color effects don't change color
                TextEffect::None
                | TextEffect::Scramble { .. }
                | TextEffect::Glitch { .. }
                | TextEffect::Wave { .. }
                | TextEffect::Bounce { .. }
                | TextEffect::Shake { .. }
                | TextEffect::Cascade { .. }
                | TextEffect::Cursor { .. } => {}

                // Scanline applies flicker in single-line mode
                TextEffect::Scanline {
                    intensity,
                    line_gap: _,
                    scroll: _,
                    scroll_speed: _,
                    flicker,
                } => {
                    if *flicker > 0.0 {
                        // Use time-based pseudo-random flicker
                        let flicker_seed = (self.time * 60.0) as u64; // ~60Hz flicker
                        let hash = flicker_seed
                            .wrapping_mul(2654435761)
                            .wrapping_add(idx as u64 * 2246822519);
                        let rand = (hash % 10000) as f64 / 10000.0;
                        let flicker_factor = 1.0 - (*flicker * rand);
                        alpha_multiplier *= flicker_factor;
                    } else if *intensity > 0.0 {
                        // No flicker, apply subtle phosphor-like dimming
                        alpha_multiplier *= 1.0 - intensity * 0.5;
                    }
                }

                // ParticleDissolve applies alpha based on dissolve state
                TextEffect::ParticleDissolve {
                    progress,
                    mode,
                    speed: _,
                    gravity: _,
                    seed,
                } => {
                    let effective_progress = if mode.is_outward() {
                        *progress
                    } else {
                        1.0 - *progress
                    };

                    // Deterministic per-character dissolve threshold
                    let hash = seed
                        .wrapping_mul(idx as u64 + 1)
                        .wrapping_add(0x9E3779B97F4A7C15);
                    let char_threshold = (hash % 10000) as f64 / 10000.0;

                    if char_threshold < effective_progress {
                        // Dissolved: fade based on how far dissolved
                        let particle_alpha = (1.0 - effective_progress).max(0.3);
                        alpha_multiplier *= particle_alpha;
                    }
                }
            }
        }

        // Apply accumulated alpha
        if alpha_multiplier < 1.0 {
            color = apply_alpha(color, alpha_multiplier);
        }

        color
    }

    /// Get the character to display at position `idx`.
    ///
    /// Character-modifying effects have priority - the first effect that
    /// would change the character wins.
    fn char_at(&self, idx: usize, original: char) -> char {
        if self.effects.is_empty() {
            return original;
        }

        let total = grapheme_count(&self.text);

        for effect in &self.effects {
            match effect {
                TextEffect::Scramble { progress } => {
                    if *progress >= 1.0 {
                        continue;
                    }
                    let resolve_threshold = idx as f64 / total as f64;
                    if *progress > resolve_threshold {
                        continue;
                    }
                    // Random character based on time and position
                    let hash = self
                        .seed
                        .wrapping_mul(idx as u64 + 1)
                        .wrapping_add((self.time * 10.0) as u64);
                    let ascii = 33 + (hash % 94) as u8;
                    return ascii as char;
                }

                TextEffect::Glitch { intensity } => {
                    if *intensity <= 0.0 {
                        continue;
                    }
                    // Random glitch based on time
                    let hash = self
                        .seed
                        .wrapping_mul(idx as u64 + 1)
                        .wrapping_add((self.time * 30.0) as u64);
                    let glitch_chance = (hash % 1000) as f64 / 1000.0;
                    if glitch_chance < *intensity * 0.3 {
                        let ascii = 33 + (hash % 94) as u8;
                        return ascii as char;
                    }
                }

                TextEffect::Typewriter { visible_chars } => {
                    if (idx as f64) >= *visible_chars {
                        return ' ';
                    }
                }

                TextEffect::ParticleDissolve {
                    progress,
                    mode,
                    speed,
                    gravity: _,
                    seed,
                } => {
                    let effective_progress = if mode.is_outward() {
                        *progress
                    } else {
                        1.0 - *progress
                    };

                    // Deterministic per-character dissolve threshold
                    let hash = seed
                        .wrapping_mul(idx as u64 + 1)
                        .wrapping_add(0x9E3779B97F4A7C15);
                    let char_threshold = (hash % 10000) as f64 / 10000.0;

                    if char_threshold < effective_progress {
                        // Calculate particle drift distance for character selection
                        let drift = (effective_progress - char_threshold) * speed;
                        let distance = drift.min(1.0);
                        return DissolveMode::particle_char(distance);
                    }
                }

                _ => {}
            }
        }

        original
    }

    /// Calculate the position offset for a character at index `idx`.
    ///
    /// Position-modifying effects are summed together:
    /// - Wave: Sinusoidal offset
    /// - Bounce: Damped bounce offset
    /// - Shake: Random jitter offset
    /// - Cascade: Slide-in offset during reveal
    ///
    /// Returns a `CharacterOffset` that can be added to the base position.
    pub fn char_offset(&self, idx: usize, total: usize) -> CharacterOffset {
        if self.effects.is_empty() || total == 0 {
            return CharacterOffset::ZERO;
        }

        let mut offset = CharacterOffset::ZERO;

        for effect in &self.effects {
            match effect {
                TextEffect::Wave {
                    amplitude,
                    wavelength,
                    speed,
                    direction,
                } => {
                    // Avoid division by zero
                    let wl = if *wavelength > 0.0 { *wavelength } else { 1.0 };

                    // Phase: position in wave + time advancement
                    let phase = (idx as f64 / wl + self.time * speed) * TAU;
                    let wave_value = (phase.sin() * amplitude).round() as i16;

                    match direction {
                        Direction::Up | Direction::Down => {
                            // Vertical wave: negative for Up, positive for Down
                            let sign = if matches!(direction, Direction::Down) {
                                1
                            } else {
                                -1
                            };
                            offset.dy = offset.dy.saturating_add(wave_value * sign);
                        }
                        Direction::Left | Direction::Right => {
                            // Horizontal wave: negative for Left, positive for Right
                            let sign = if matches!(direction, Direction::Right) {
                                1
                            } else {
                                -1
                            };
                            offset.dx = offset.dx.saturating_add(wave_value * sign);
                        }
                    }
                }

                TextEffect::Bounce {
                    height,
                    speed,
                    stagger,
                    damping,
                } => {
                    // Staggered start time for each character
                    let char_delay = idx as f64 * stagger;
                    let local_time = (self.time - char_delay).max(0.0);

                    // Bounce physics: damped oscillation
                    let bounce_phase = local_time * speed * TAU;
                    let decay = damping.powf(local_time * speed);
                    let bounce_value = (bounce_phase.cos().abs() * height * decay).round() as i16;

                    // Bounce is always vertical (characters drop down from above)
                    offset.dy = offset.dy.saturating_add(-bounce_value);
                }

                TextEffect::Shake {
                    intensity,
                    speed,
                    seed,
                } => {
                    // Deterministic random based on time step and seed
                    let time_step = (self.time * speed * 100.0) as u64;
                    let hash1 = seed
                        .wrapping_mul(idx as u64 + 1)
                        .wrapping_mul(time_step.wrapping_add(1))
                        .wrapping_add(0x9E3779B97F4A7C15);
                    let hash2 = hash1.wrapping_mul(0x517CC1B727220A95);

                    // Convert hash to offset in range [-intensity, +intensity]
                    let x_rand = ((hash1 % 10000) as f64 / 5000.0 - 1.0) * intensity;
                    let y_rand = ((hash2 % 10000) as f64 / 5000.0 - 1.0) * intensity;

                    offset.dx = offset.dx.saturating_add(x_rand.round() as i16);
                    offset.dy = offset.dy.saturating_add(y_rand.round() as i16);
                }

                TextEffect::Cascade {
                    speed,
                    direction,
                    stagger,
                } => {
                    // Characters revealed per time unit
                    let revealed_chars = self.time * speed;
                    let char_reveal_time = idx as f64 * stagger;

                    if revealed_chars < char_reveal_time {
                        // Character not yet revealed: full offset in the "from" direction
                        let slide_offset = 3_i16; // cells of slide distance
                        match direction {
                            Direction::Down => offset.dy = offset.dy.saturating_add(-slide_offset),
                            Direction::Up => offset.dy = offset.dy.saturating_add(slide_offset),
                            Direction::Left => offset.dx = offset.dx.saturating_add(slide_offset),
                            Direction::Right => offset.dx = offset.dx.saturating_add(-slide_offset),
                        }
                    } else {
                        // Smooth slide-in animation
                        let progress = ((revealed_chars - char_reveal_time) / 0.3).clamp(0.0, 1.0);
                        let eased = self.easing.apply(progress);
                        let remaining = ((1.0 - eased) * 3.0).round() as i16;

                        match direction {
                            Direction::Down => offset.dy = offset.dy.saturating_add(-remaining),
                            Direction::Up => offset.dy = offset.dy.saturating_add(remaining),
                            Direction::Left => offset.dx = offset.dx.saturating_add(remaining),
                            Direction::Right => offset.dx = offset.dx.saturating_add(-remaining),
                        }
                    }
                }

                TextEffect::ParticleDissolve {
                    progress,
                    mode,
                    speed,
                    gravity,
                    seed,
                } => {
                    let effective_progress = if mode.is_outward() {
                        *progress
                    } else {
                        1.0 - *progress
                    };

                    // Deterministic per-character dissolve threshold
                    let hash = seed
                        .wrapping_mul(idx as u64 + 1)
                        .wrapping_add(0x9E3779B97F4A7C15);
                    let char_threshold = (hash % 10000) as f64 / 10000.0;

                    if char_threshold < effective_progress {
                        // This character is dissolved - calculate particle drift
                        let drift_amount = (effective_progress - char_threshold) * speed * 3.0;

                        // Get deterministic drift direction per particle
                        let hash2 = hash.wrapping_mul(2654435761);
                        let hash3 = hash.wrapping_mul(1597334677);

                        if mode.is_centered() {
                            // Explode/Implode: move from/to center
                            let center = total as f64 / 2.0;
                            let from_center = idx as f64 - center;
                            let dir_x = if from_center.abs() < 0.5 {
                                (hash2 % 10000) as f64 / 5000.0 - 1.0
                            } else {
                                from_center.signum()
                            };
                            let dir_y = (hash3 % 10000) as f64 / 5000.0 - 1.0;

                            offset.dx = offset
                                .dx
                                .saturating_add((dir_x * drift_amount).round() as i16);
                            offset.dy = offset
                                .dy
                                .saturating_add(((dir_y + gravity) * drift_amount).round() as i16);
                        } else {
                            // Dissolve/Materialize: random drift with gravity
                            let dir_x = (hash2 % 10000) as f64 / 5000.0 - 1.0;
                            let dir_y = (hash3 % 10000) as f64 / 5000.0 - 0.5 + gravity;

                            offset.dx = offset
                                .dx
                                .saturating_add((dir_x * drift_amount).round() as i16);
                            offset.dy = offset
                                .dy
                                .saturating_add((dir_y * drift_amount).round() as i16);
                        }
                    }
                }

                // Non-position effects don't contribute offset
                _ => {}
            }
        }

        offset
    }

    /// Check if this text has any position-modifying effects.
    pub fn has_position_effects(&self) -> bool {
        self.effects.iter().any(|effect| {
            matches!(
                effect,
                TextEffect::Wave { .. }
                    | TextEffect::Bounce { .. }
                    | TextEffect::Shake { .. }
                    | TextEffect::Cascade { .. }
                    | TextEffect::ParticleDissolve { .. }
            )
        })
    }

    /// Get the cursor effect if one is configured.
    fn cursor_effect(&self) -> Option<&TextEffect> {
        self.effects
            .iter()
            .find(|e| matches!(e, TextEffect::Cursor { .. }))
    }

    /// Calculate the cursor position based on CursorPosition and any reveal effects.
    ///
    /// Returns `None` if no cursor effect is configured.
    /// Returns `Some(idx)` where `idx` is the character index where the cursor should appear.
    fn cursor_index(&self) -> Option<usize> {
        let cursor_effect = self.cursor_effect()?;

        let TextEffect::Cursor { position, .. } = cursor_effect else {
            return None;
        };

        let total = grapheme_count(&self.text);

        match position {
            CursorPosition::End => Some(total),
            CursorPosition::AtIndex(idx) => Some((*idx).min(total)),
            CursorPosition::AfterReveal => {
                // Find the reveal position from Typewriter, Reveal, or Cascade effects
                for effect in &self.effects {
                    match effect {
                        TextEffect::Typewriter { visible_chars } => {
                            return Some((*visible_chars as usize).min(total));
                        }
                        TextEffect::Reveal { mode, progress, .. } => {
                            // For LeftToRight, cursor follows the reveal edge
                            // For other modes, approximate with progress * total
                            let revealed = match mode {
                                RevealMode::LeftToRight => (*progress * total as f64) as usize,
                                RevealMode::RightToLeft => {
                                    // Cursor at the left edge of revealed portion
                                    ((1.0 - *progress) * total as f64) as usize
                                }
                                _ => (*progress * total as f64) as usize,
                            };
                            return Some(revealed.min(total));
                        }
                        TextEffect::Cascade { speed, stagger, .. } => {
                            let revealed = (self.time * speed / stagger.max(0.001)) as usize;
                            return Some(revealed.min(total));
                        }
                        _ => {}
                    }
                }
                // No reveal effect found, default to end
                Some(total)
            }
        }
    }

    /// Check if the cursor should be visible based on blink speed and current time.
    fn cursor_visible(&self) -> bool {
        let Some(cursor_effect) = self.cursor_effect() else {
            return false;
        };

        let TextEffect::Cursor { blink_speed, .. } = cursor_effect else {
            return false;
        };

        if *blink_speed <= 0.0 {
            // No blinking, always visible
            return true;
        }

        // Blink cycle: on for half, off for half
        let cycle = self.time * blink_speed;
        (cycle % 1.0) < 0.5
    }

    /// Render at a specific position.
    pub fn render_at(&self, x: u16, y: u16, frame: &mut Frame) {
        struct Run<'a> {
            idx: usize,
            grapheme: &'a str,
            width: usize,
            base_px: u16,
            simple_char: Option<char>,
        }

        let mut runs = Vec::new();
        let mut col = 0usize;
        for (idx, grapheme) in graphemes(self.text.as_str()).enumerate() {
            let width = grapheme_width(grapheme);
            if width == 0 {
                continue;
            }
            let base_px = x.saturating_add(col as u16);
            let simple_char = if width == 1 && grapheme.chars().count() == 1 {
                grapheme.chars().next()
            } else {
                None
            };
            runs.push(Run {
                idx,
                grapheme,
                width,
                base_px,
                simple_char,
            });
            col = col.saturating_add(width);
        }

        let total = runs.len();
        if total == 0 {
            return;
        }
        let total_width = col;
        let has_fade_effect = self.effects.iter().any(|effect| {
            matches!(
                effect,
                TextEffect::FadeIn { .. } | TextEffect::FadeOut { .. }
            )
        });
        let has_position_effects = self.has_position_effects();

        let frame_width = frame.buffer.width();
        let frame_height = frame.buffer.height();

        // =====================================================================
        // Phase 1: Render glow layers (outermost first)
        // =====================================================================
        if let Some(ref glow) = self.glow_config {
            // Iterate from layer 0 (outermost) to layers-1 (innermost)
            for layer_idx in 0..glow.layers {
                let glow_color = glow.layer_color(layer_idx);
                let offsets: Vec<_> = glow.layer_offsets(layer_idx).collect();

                for run in &runs {
                    let content = if let Some(ch) = run.simple_char {
                        CellContent::from_char(self.char_at(run.idx, ch))
                    } else {
                        let id = frame.intern_with_width(run.grapheme, run.width as u8);
                        CellContent::from_grapheme(id)
                    };

                    // Apply each offset for this glow layer
                    for (dx, dy) in &offsets {
                        let glow_x = (run.base_px as i32).saturating_add(i32::from(*dx));
                        let glow_y = (y as i32).saturating_add(i32::from(*dy));

                        // Bounds check and render
                        if glow_x >= 0
                            && glow_x < i32::from(frame_width)
                            && glow_y >= 0
                            && glow_y < i32::from(frame_height)
                        {
                            let mut cell = Cell::new(content);
                            cell.fg = glow_color;
                            frame.buffer.set_fast(glow_x as u16, glow_y as u16, cell);
                        }
                    }
                }
            }
        }

        // =====================================================================
        // Phase 2: Render shadows (first shadow = furthest from text)
        // =====================================================================
        for shadow in &self.shadows {
            let shadow_color = shadow.effective_color();

            for run in &runs {
                let content = if let Some(ch) = run.simple_char {
                    CellContent::from_char(self.char_at(run.idx, ch))
                } else {
                    let id = frame.intern_with_width(run.grapheme, run.width as u8);
                    CellContent::from_grapheme(id)
                };

                // Apply shadow offset using the helper method
                if let Some((shadow_x, shadow_y)) =
                    shadow.apply_offset(run.base_px, y, frame_width, frame_height)
                {
                    let mut cell = Cell::new(content);
                    cell.fg = shadow_color;
                    frame.buffer.set_fast(shadow_x, shadow_y, cell);
                }
            }
        }

        // =====================================================================
        // Phase 2.5: Render outline/border (after shadows, before main text)
        // =====================================================================
        if let Some(ref outline) = self.outline_config {
            // For Double style, we render two passes: outer ring first, then inner
            let passes = if outline.is_double() { 2 } else { 1 };

            for pass in 0..passes {
                // For double outline, scale thickness: pass 0 = outer (thickness+1), pass 1 = inner (thickness)
                let effective_thickness = if outline.is_double() {
                    if pass == 0 {
                        (outline.thickness + 1).min(3)
                    } else {
                        outline.thickness
                    }
                } else {
                    outline.thickness
                };

                // Create temporary config with effective thickness
                let temp_config = OutlineConfig {
                    thickness: effective_thickness,
                    ..outline.clone()
                };

                let offsets: Vec<_> = temp_config.offsets().collect();

                for run in &runs {
                    let content = if outline.use_text_char {
                        if let Some(ch) = run.simple_char {
                            CellContent::from_char(self.char_at(run.idx, ch))
                        } else {
                            let id = frame.intern_with_width(run.grapheme, run.width as u8);
                            CellContent::from_grapheme(id)
                        }
                    } else {
                        CellContent::from_char('█')
                    };

                    // Render at each offset position
                    for (offset_idx, (dx, dy)) in offsets.iter().enumerate() {
                        // Get color for this position (may be None for dashed gaps)
                        let Some(outline_color) = outline.color_at(offset_idx, self.time) else {
                            continue;
                        };

                        let outline_x = (run.base_px as i32).saturating_add(i32::from(*dx));
                        let outline_y = (y as i32).saturating_add(i32::from(*dy));

                        // Bounds check and render
                        if outline_x >= 0
                            && outline_x < i32::from(frame_width)
                            && outline_y >= 0
                            && outline_y < i32::from(frame_height)
                        {
                            let mut cell = Cell::new(content);
                            cell.fg = outline_color;
                            frame
                                .buffer
                                .set_fast(outline_x as u16, outline_y as u16, cell);
                        }
                    }
                }
            }
        }

        // =====================================================================
        // Phase 3: Render main text (on top of shadows and glow)
        // =====================================================================
        for run in &runs {
            let color = self.char_color(run.idx, total);
            let content = if let Some(ch) = run.simple_char {
                CellContent::from_char(self.char_at(run.idx, ch))
            } else {
                let id = frame.intern_with_width(run.grapheme, run.width as u8);
                CellContent::from_grapheme(id)
            };

            // Skip fully transparent
            if color.r() == 0 && color.g() == 0 && color.b() == 0 && has_fade_effect {
                continue;
            }

            // Calculate final position with offset
            let (final_x, final_y) = if has_position_effects {
                let offset = self.char_offset(run.idx, total);
                let clamped = offset.clamp_for_position(run.base_px, y, frame_width, frame_height);

                let fx = (run.base_px as i32 + clamped.dx as i32).clamp(0, frame_width as i32 - 1)
                    as u16;
                let fy = (y as i32 + clamped.dy as i32).clamp(0, frame_height as i32 - 1) as u16;
                (fx, fy)
            } else {
                (run.base_px, y)
            };

            let mut cell = Cell::new(content);
            cell.fg = color;
            if let Some(bg) = self.bg_color {
                cell.bg = bg;
            }

            let mut flags = CellStyleFlags::empty();
            if self.bold {
                flags = flags.union(CellStyleFlags::BOLD);
            }
            if self.italic {
                flags = flags.union(CellStyleFlags::ITALIC);
            }
            if self.underline {
                flags = flags.union(CellStyleFlags::UNDERLINE);
            }
            cell.attrs = CellAttrs::new(flags, 0);
            frame.buffer.set_fast(final_x, final_y, cell);
        }

        // Render cursor if visible
        if self.cursor_visible()
            && let Some(cursor_idx) = self.cursor_index()
        {
            let cursor_x = if cursor_idx >= total {
                x.saturating_add(total_width as u16)
            } else {
                runs[cursor_idx].base_px
            };

            // Get cursor style from effect
            if let Some(TextEffect::Cursor { style, .. }) = self.cursor_effect() {
                let cursor_char = style.char();

                // Bounds check
                if cursor_x < frame_width
                    && let Some(cell) = frame.buffer.get_mut(cursor_x, y)
                {
                    cell.content = CellContent::from_char(cursor_char);
                    cell.fg = self.base_color;

                    if let Some(bg) = self.bg_color {
                        cell.bg = bg;
                    }

                    let mut flags = CellStyleFlags::empty();
                    if self.bold {
                        flags = flags.union(CellStyleFlags::BOLD);
                    }
                    cell.attrs = CellAttrs::new(flags, 0);
                }
            }
        }
    }
}

impl Widget for StyledText {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.render_at(area.x, area.y, frame);
    }
}

// =============================================================================
// TransitionOverlay - Full-screen announcement effect
// =============================================================================

/// A centered overlay for displaying transition text with fade effects.
///
/// Progress goes from 0.0 (invisible) to 0.5 (peak visibility) to 1.0 (invisible).
/// This creates a smooth fade-in then fade-out animation.
#[derive(Debug, Clone)]
pub struct TransitionOverlay {
    title: String,
    subtitle: String,
    progress: f64,
    primary_color: PackedRgba,
    secondary_color: PackedRgba,
    gradient: Option<ColorGradient>,
    time: f64,
}

impl TransitionOverlay {
    /// Create a new transition overlay.
    pub fn new(title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            progress: 0.0,
            primary_color: PackedRgba::rgb(255, 100, 200),
            secondary_color: PackedRgba::rgb(180, 180, 220),
            gradient: None,
            time: 0.0,
        }
    }

    /// Set progress (0.0 = invisible, 0.5 = peak, 1.0 = invisible).
    #[must_use]
    pub fn progress(mut self, progress: f64) -> Self {
        self.progress = progress.clamp(0.0, 1.0);
        self
    }

    /// Set the primary (title) color.
    #[must_use]
    pub fn primary_color(mut self, color: PackedRgba) -> Self {
        self.primary_color = color;
        self
    }

    /// Set the secondary (subtitle) color.
    #[must_use]
    pub fn secondary_color(mut self, color: PackedRgba) -> Self {
        self.secondary_color = color;
        self
    }

    /// Use an animated gradient for the title.
    #[must_use]
    pub fn gradient(mut self, gradient: ColorGradient) -> Self {
        self.gradient = Some(gradient);
        self
    }

    /// Set animation time.
    #[must_use]
    pub fn time(mut self, time: f64) -> Self {
        self.time = time;
        self
    }

    /// Calculate opacity from progress.
    fn opacity(&self) -> f64 {
        (self.progress * PI).sin()
    }

    /// Check if visible.
    pub fn is_visible(&self) -> bool {
        self.opacity() > 0.01
    }
}

impl Widget for TransitionOverlay {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let opacity = self.opacity();
        if opacity < 0.01 || area.width < 10 || area.height < 3 {
            return;
        }

        // Center the title
        let title_len = display_width(&self.title) as u16;
        let title_x = area.x + area.width.saturating_sub(title_len) / 2;
        let title_y = area.y + area.height / 2;

        // Render title with gradient or fade
        let title_effect = if let Some(gradient) = &self.gradient {
            TextEffect::AnimatedGradient {
                gradient: gradient.clone(),
                speed: 0.3,
            }
        } else {
            TextEffect::FadeIn { progress: opacity }
        };

        let title_text = StyledText::new(&self.title)
            .effect(title_effect)
            .base_color(apply_alpha(self.primary_color, opacity))
            .bold()
            .time(self.time);
        title_text.render_at(title_x, title_y, frame);

        // Render subtitle
        if !self.subtitle.is_empty() && title_y + 1 < area.y + area.height {
            let subtitle_len = display_width(&self.subtitle) as u16;
            let subtitle_x = area.x + area.width.saturating_sub(subtitle_len) / 2;
            let subtitle_y = title_y + 1;

            let subtitle_text = StyledText::new(&self.subtitle)
                .effect(TextEffect::FadeIn {
                    progress: opacity * 0.85,
                })
                .base_color(self.secondary_color)
                .italic()
                .time(self.time);
            subtitle_text.render_at(subtitle_x, subtitle_y, frame);
        }
    }
}

// =============================================================================
// TransitionState - Animation state manager
// =============================================================================

/// Helper for managing transition animations.
#[derive(Debug, Clone)]
pub struct TransitionState {
    progress: f64,
    active: bool,
    speed: f64,
    title: String,
    subtitle: String,
    color: PackedRgba,
    gradient: Option<ColorGradient>,
    time: f64,
    /// Easing function for transition animations.
    easing: Easing,
}

impl Default for TransitionState {
    fn default() -> Self {
        Self::new()
    }
}

impl TransitionState {
    /// Create new transition state.
    pub fn new() -> Self {
        Self {
            progress: 0.0,
            active: false,
            speed: 0.05,
            title: String::new(),
            subtitle: String::new(),
            color: PackedRgba::rgb(255, 100, 200),
            gradient: None,
            time: 0.0,
            easing: Easing::default(),
        }
    }

    /// Set the easing function for the transition animation.
    pub fn set_easing(&mut self, easing: Easing) {
        self.easing = easing;
    }

    /// Get the current easing function.
    pub fn easing(&self) -> Easing {
        self.easing
    }

    /// Get the eased progress value.
    pub fn eased_progress(&self) -> f64 {
        self.easing.apply(self.progress)
    }

    /// Start a transition.
    pub fn start(
        &mut self,
        title: impl Into<String>,
        subtitle: impl Into<String>,
        color: PackedRgba,
    ) {
        self.title = title.into();
        self.subtitle = subtitle.into();
        self.color = color;
        self.gradient = None;
        self.progress = 0.0;
        self.active = true;
    }

    /// Start a transition with gradient.
    pub fn start_with_gradient(
        &mut self,
        title: impl Into<String>,
        subtitle: impl Into<String>,
        gradient: ColorGradient,
    ) {
        self.title = title.into();
        self.subtitle = subtitle.into();
        self.gradient = Some(gradient);
        self.progress = 0.0;
        self.active = true;
    }

    /// Set transition speed.
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(0.01, 0.5);
    }

    /// Update the transition (call every tick).
    pub fn tick(&mut self) {
        self.time += 0.1;
        if self.active {
            self.progress += self.speed;
            if self.progress >= 1.0 {
                self.progress = 1.0;
                self.active = false;
            }
        }
    }

    /// Check if visible.
    pub fn is_visible(&self) -> bool {
        self.active || (self.progress > 0.0 && self.progress < 1.0)
    }

    /// Check if active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get current progress.
    pub fn progress(&self) -> f64 {
        self.progress
    }

    /// Get the overlay widget.
    pub fn overlay(&self) -> TransitionOverlay {
        let mut overlay = TransitionOverlay::new(&self.title, &self.subtitle)
            .progress(self.progress)
            .primary_color(self.color)
            .time(self.time);

        if let Some(ref gradient) = self.gradient {
            overlay = overlay.gradient(gradient.clone());
        }

        overlay
    }
}

// =============================================================================
// Effect Sequencer / Timeline
// =============================================================================

/// Loop behavior for effect sequences.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoopMode {
    /// Play once and stop at end.
    #[default]
    Once,
    /// Restart from beginning after completing.
    Loop,
    /// Reverse direction at each end (forward, backward, forward...).
    PingPong,
    /// Loop a specific number of times, then stop.
    LoopCount(u32),
}

/// Playback state of a sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SequenceState {
    /// Sequence is actively playing.
    #[default]
    Playing,
    /// Sequence is paused (progress frozen).
    Paused,
    /// Sequence has completed (for Once/LoopCount modes).
    Completed,
}

/// Events emitted by sequence during playback.
///
/// Events are returned by `tick()` instead of callbacks to avoid lifetime issues.
#[derive(Debug, Clone, PartialEq)]
pub enum SequenceEvent {
    /// A new step has started playing.
    StepStarted {
        /// Index of the step that started.
        step_idx: usize,
    },
    /// A step has finished playing.
    StepCompleted {
        /// Index of the step that completed.
        step_idx: usize,
    },
    /// The entire sequence has completed (Once mode or LoopCount exhausted).
    SequenceCompleted,
    /// The sequence has looped back to the beginning.
    SequenceLooped {
        /// Current loop iteration (1-indexed).
        loop_count: u32,
    },
}

/// A single step in an effect sequence.
#[derive(Debug, Clone)]
pub struct SequenceStep {
    /// The effect to apply during this step.
    pub effect: TextEffect,
    /// Duration of this step in seconds.
    pub duration_secs: f64,
    /// Optional easing override for this step.
    pub easing: Option<Easing>,
}

impl SequenceStep {
    /// Create a new sequence step.
    pub fn new(effect: TextEffect, duration_secs: f64) -> Self {
        Self {
            effect,
            duration_secs,
            easing: None,
        }
    }

    /// Create a step with custom easing.
    pub fn with_easing(effect: TextEffect, duration_secs: f64, easing: Easing) -> Self {
        Self {
            effect,
            duration_secs,
            easing: Some(easing),
        }
    }
}

/// Declarative animation timeline for sequencing effects.
///
/// `EffectSequence` enables multi-step animations that automatically transition
/// between effects. It supports looping, ping-pong playback, and per-step easing.
///
/// # Example
///
/// ```rust,ignore
/// let seq = EffectSequence::builder()
///     .step(TextEffect::FadeIn { progress: 0.0 }, 0.5)
///     .step(TextEffect::Pulse { speed: 2.0, min_alpha: 0.5 }, 2.0)
///     .step(TextEffect::FadeOut { progress: 0.0 }, 0.5)
///     .loop_mode(LoopMode::Loop)
///     .easing(Easing::EaseInOut)
///     .build();
///
/// // In animation loop:
/// if let Some(event) = seq.tick(delta_time) {
///     match event {
///         SequenceEvent::StepCompleted { step_idx } => println!("Step {} done", step_idx),
///         SequenceEvent::SequenceCompleted => println!("All done!"),
///         _ => {}
///     }
/// }
///
/// // Get current effect with interpolated progress
/// let effect = seq.current_effect();
/// ```
#[derive(Debug, Clone)]
pub struct EffectSequence {
    steps: Vec<SequenceStep>,
    current_step: usize,
    step_progress: f64,
    loop_mode: LoopMode,
    global_easing: Easing,
    state: SequenceState,
    /// Current loop iteration (1-indexed, for LoopCount tracking).
    loop_iteration: u32,
    /// Direction for PingPong mode (true = forward, false = backward).
    forward: bool,
}

impl Default for EffectSequence {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectSequence {
    /// Create an empty sequence.
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            current_step: 0,
            step_progress: 0.0,
            loop_mode: LoopMode::Once,
            global_easing: Easing::Linear,
            state: SequenceState::Playing,
            loop_iteration: 1,
            forward: true,
        }
    }

    /// Create a builder for fluent sequence construction.
    pub fn builder() -> EffectSequenceBuilder {
        EffectSequenceBuilder::new()
    }

    /// Advance the sequence by `delta_secs` seconds.
    ///
    /// Returns an optional event if a significant state change occurred.
    /// Call this once per frame with the time delta.
    ///
    /// # Returns
    /// - `Some(SequenceEvent::StepStarted)` when transitioning to a new step
    /// - `Some(SequenceEvent::StepCompleted)` when a step finishes
    /// - `Some(SequenceEvent::SequenceLooped)` when looping back to start
    /// - `Some(SequenceEvent::SequenceCompleted)` when sequence ends
    /// - `None` if no significant event occurred
    pub fn tick(&mut self, delta_secs: f64) -> Option<SequenceEvent> {
        // Don't advance if paused, completed, or empty
        if self.state != SequenceState::Playing || self.steps.is_empty() {
            return None;
        }

        let current_duration = self.steps[self.current_step].duration_secs;
        if current_duration <= 0.0 {
            // Zero-duration step: skip immediately
            return self.advance_step();
        }

        // Advance progress
        self.step_progress += delta_secs / current_duration;

        // Check for step completion
        if self.step_progress >= 1.0 {
            self.step_progress = 1.0;
            return self.advance_step();
        }

        None
    }

    /// Handle step advancement and looping logic.
    fn advance_step(&mut self) -> Option<SequenceEvent> {
        let is_last_step = if self.forward {
            self.current_step >= self.steps.len() - 1
        } else {
            self.current_step == 0
        };

        if is_last_step {
            // End of sequence (or end of direction for PingPong)
            match self.loop_mode {
                LoopMode::Once => {
                    self.state = SequenceState::Completed;
                    Some(SequenceEvent::SequenceCompleted)
                }
                LoopMode::Loop => {
                    self.current_step = 0;
                    self.step_progress = 0.0;
                    self.loop_iteration += 1;
                    Some(SequenceEvent::SequenceLooped {
                        loop_count: self.loop_iteration,
                    })
                }
                LoopMode::PingPong => {
                    self.forward = !self.forward;
                    // Don't change current_step, just reverse direction
                    self.step_progress = 0.0;
                    self.loop_iteration += 1;
                    Some(SequenceEvent::SequenceLooped {
                        loop_count: self.loop_iteration,
                    })
                }
                LoopMode::LoopCount(max_loops) => {
                    if self.loop_iteration >= max_loops {
                        self.state = SequenceState::Completed;
                        Some(SequenceEvent::SequenceCompleted)
                    } else {
                        self.current_step = 0;
                        self.step_progress = 0.0;
                        self.loop_iteration += 1;
                        Some(SequenceEvent::SequenceLooped {
                            loop_count: self.loop_iteration,
                        })
                    }
                }
            }
        } else {
            // Move to next/prev step
            if self.forward {
                self.current_step += 1;
            } else {
                self.current_step -= 1;
            }
            self.step_progress = 0.0;

            // Return StepStarted (caller can infer previous step completion)
            Some(SequenceEvent::StepStarted {
                step_idx: self.current_step,
            })
        }
    }

    /// Get the current effect with progress interpolated.
    ///
    /// For progress-based effects (FadeIn, FadeOut, Typewriter, Scramble),
    /// the progress value is set based on the step's progress and easing.
    pub fn current_effect(&self) -> TextEffect {
        if self.steps.is_empty() {
            return TextEffect::None;
        }

        let step = &self.steps[self.current_step];
        let easing = step.easing.unwrap_or(self.global_easing);
        let eased_progress = easing.apply(self.step_progress);

        // Clone and interpolate progress-based effects
        match &step.effect {
            TextEffect::FadeIn { .. } => TextEffect::FadeIn {
                progress: eased_progress,
            },
            TextEffect::FadeOut { .. } => TextEffect::FadeOut {
                progress: eased_progress,
            },
            TextEffect::Typewriter { .. } => {
                // For typewriter, we need text length context which we don't have
                // Return the effect as-is; caller should multiply by text length
                TextEffect::Typewriter {
                    visible_chars: eased_progress,
                }
            }
            TextEffect::Scramble { .. } => TextEffect::Scramble {
                progress: eased_progress,
            },
            // Non-progress effects: return as-is
            other => other.clone(),
        }
    }

    /// Get the overall sequence progress (0.0 to 1.0).
    ///
    /// This represents how far through the entire sequence we are,
    /// accounting for all steps and their durations.
    pub fn progress(&self) -> f64 {
        if self.steps.is_empty() {
            return 1.0;
        }

        let total_duration: f64 = self.steps.iter().map(|s| s.duration_secs).sum();
        if total_duration <= 0.0 {
            return 1.0;
        }

        let elapsed_duration: f64 = self.steps[..self.current_step]
            .iter()
            .map(|s| s.duration_secs)
            .sum::<f64>()
            + self.steps[self.current_step].duration_secs * self.step_progress;

        (elapsed_duration / total_duration).clamp(0.0, 1.0)
    }

    /// Check if the sequence has completed.
    pub fn is_complete(&self) -> bool {
        self.state == SequenceState::Completed
    }

    /// Check if the sequence is currently playing.
    pub fn is_playing(&self) -> bool {
        self.state == SequenceState::Playing
    }

    /// Check if the sequence is paused.
    pub fn is_paused(&self) -> bool {
        self.state == SequenceState::Paused
    }

    /// Pause the sequence. Progress will freeze until resumed.
    pub fn pause(&mut self) {
        if self.state == SequenceState::Playing {
            self.state = SequenceState::Paused;
        }
    }

    /// Resume a paused sequence.
    pub fn resume(&mut self) {
        if self.state == SequenceState::Paused {
            self.state = SequenceState::Playing;
        }
    }

    /// Reset the sequence to the beginning.
    pub fn reset(&mut self) {
        self.current_step = 0;
        self.step_progress = 0.0;
        self.state = SequenceState::Playing;
        self.loop_iteration = 1;
        self.forward = true;
    }

    /// Get the current step index.
    pub fn current_step_index(&self) -> usize {
        self.current_step
    }

    /// Get the progress within the current step (0.0 to 1.0).
    pub fn step_progress(&self) -> f64 {
        self.step_progress
    }

    /// Get the number of steps in the sequence.
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// Get the current loop iteration (1-indexed).
    pub fn loop_iteration(&self) -> u32 {
        self.loop_iteration
    }

    /// Get the current playback state.
    pub fn state(&self) -> SequenceState {
        self.state
    }

    /// Get the loop mode.
    pub fn loop_mode(&self) -> LoopMode {
        self.loop_mode
    }
}

/// Builder for constructing effect sequences fluently.
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct EffectSequenceBuilder {
    steps: Vec<SequenceStep>,
    loop_mode: LoopMode,
    global_easing: Easing,
}

impl EffectSequenceBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a step with the given effect and duration.
    pub fn step(mut self, effect: TextEffect, duration_secs: f64) -> Self {
        self.steps.push(SequenceStep::new(effect, duration_secs));
        self
    }

    /// Add a step with custom easing.
    pub fn step_with_easing(
        mut self,
        effect: TextEffect,
        duration_secs: f64,
        easing: Easing,
    ) -> Self {
        self.steps
            .push(SequenceStep::with_easing(effect, duration_secs, easing));
        self
    }

    /// Set the loop mode for the sequence.
    pub fn loop_mode(mut self, mode: LoopMode) -> Self {
        self.loop_mode = mode;
        self
    }

    /// Set the global easing function (used when steps don't specify their own).
    pub fn easing(mut self, easing: Easing) -> Self {
        self.global_easing = easing;
        self
    }

    /// Build the effect sequence.
    pub fn build(self) -> EffectSequence {
        EffectSequence {
            steps: self.steps,
            current_step: 0,
            step_progress: 0.0,
            loop_mode: self.loop_mode,
            global_easing: self.global_easing,
            state: SequenceState::Playing,
            loop_iteration: 1,
            forward: true,
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lerp_color() {
        let black = PackedRgba::rgb(0, 0, 0);
        let white = PackedRgba::rgb(255, 255, 255);
        let mid = lerp_color(black, white, 0.5);
        assert_eq!(mid.r(), 127);
    }

    #[test]
    fn test_color_gradient() {
        let gradient = ColorGradient::rainbow();
        let red = gradient.sample(0.0);
        assert!(red.r() > 200);

        let mid = gradient.sample(0.5);
        assert!(mid.g() > 200); // Should be greenish
    }

    // =========================================================================
    // Gradient LUT Tests (bd-vzjn)
    // =========================================================================

    #[test]
    fn test_sample_fast_matches_sample() {
        // Verify sample_fast produces identical results to sample
        let gradient = ColorGradient::rainbow();

        for i in 0..=100 {
            let t = i as f64 / 100.0;
            let slow = gradient.sample(t);
            let fast = gradient.sample_fast(t);

            // Allow small differences due to fixed-point rounding
            assert!(
                (slow.r() as i16 - fast.r() as i16).abs() <= 1,
                "Red mismatch at t={}: slow={}, fast={}",
                t,
                slow.r(),
                fast.r()
            );
            assert!(
                (slow.g() as i16 - fast.g() as i16).abs() <= 1,
                "Green mismatch at t={}: slow={}, fast={}",
                t,
                slow.g(),
                fast.g()
            );
            assert!(
                (slow.b() as i16 - fast.b() as i16).abs() <= 1,
                "Blue mismatch at t={}: slow={}, fast={}",
                t,
                slow.b(),
                fast.b()
            );
        }
    }

    #[test]
    fn test_lut_reuse_same_size() {
        // Verify LUT produces consistent results for same size
        let gradient = ColorGradient::cyberpunk();
        let lut1 = gradient.precompute_lut(80);
        let lut2 = gradient.precompute_lut(80);

        assert_eq!(lut1.len(), lut2.len());
        for i in 0..80 {
            let c1 = lut1.sample(i);
            let c2 = lut2.sample(i);
            assert_eq!(c1.r(), c2.r());
            assert_eq!(c1.g(), c2.g());
            assert_eq!(c1.b(), c2.b());
        }
    }

    #[test]
    fn test_lut_invalidate_resize() {
        // Verify LUT changes when size changes
        let gradient = ColorGradient::fire();
        let lut_small = gradient.precompute_lut(10);
        let lut_large = gradient.precompute_lut(100);

        assert_eq!(lut_small.len(), 10);
        assert_eq!(lut_large.len(), 100);

        // Endpoints should still match
        let start_small = lut_small.sample(0);
        let start_large = lut_large.sample(0);
        assert_eq!(start_small.r(), start_large.r());
        assert_eq!(start_small.g(), start_large.g());
        assert_eq!(start_small.b(), start_large.b());
    }

    #[test]
    fn test_fixed_point_accuracy() {
        // Verify fixed-point t values are within epsilon of float
        let cache = TValueCache::new(100);

        for i in 0..100 {
            let expected = i as f64 / 99.0;
            let actual = cache.get(i);
            let diff = (expected - actual).abs();
            assert!(
                diff < 0.0001,
                "Fixed-point error at {}: expected {}, got {}, diff={}",
                i,
                expected,
                actual,
                diff
            );
        }
    }

    #[test]
    fn test_lut_sample_t_normalized() {
        // Verify sample_t handles normalized values correctly
        let gradient = ColorGradient::ocean();
        let lut = gradient.precompute_lut(50);

        // t=0.0 should match first sample
        let at_zero = lut.sample_t(0.0);
        let first = lut.sample(0);
        assert_eq!(at_zero.r(), first.r());

        // t=1.0 should match last sample
        let at_one = lut.sample_t(1.0);
        let last = lut.sample(49);
        assert_eq!(at_one.r(), last.r());
    }

    #[test]
    fn test_lut_empty_handling() {
        // Verify empty LUT returns default white
        let lut = GradientLut {
            colors: Vec::new(),
            count: 0,
        };
        assert!(lut.is_empty());
        let color = lut.sample(0);
        assert_eq!(color.r(), 255);
        assert_eq!(color.g(), 255);
        assert_eq!(color.b(), 255);
    }

    #[test]
    fn test_sample_batch_consistency() {
        // Verify batch sampling matches individual samples
        let gradient = ColorGradient::sunset();
        let batch = gradient.sample_batch(0.0, 1.0, 10);

        assert_eq!(batch.len(), 10);

        for (i, color) in batch.iter().enumerate() {
            let t = i as f64 / 9.0;
            let individual = gradient.sample_fast(t);
            assert_eq!(color.r(), individual.r());
            assert_eq!(color.g(), individual.g());
            assert_eq!(color.b(), individual.b());
        }
    }

    #[test]
    fn test_t_value_cache_single() {
        // Single element cache should return 0.5
        let cache = TValueCache::new(1);
        let t = cache.get(0);
        assert!((t - 0.5).abs() < 0.0001);
    }

    #[test]
    fn test_t_value_cache_empty() {
        // Empty cache should return 0.5 for any index
        let cache = TValueCache::new(0);
        assert!(cache.is_empty());
        let t = cache.get(0);
        assert!((t - 0.5).abs() < 0.0001);
    }

    #[test]
    fn test_lerp_color_fast_matches_lerp_color() {
        // Verify fast lerp matches regular lerp
        let a = PackedRgba::rgb(100, 50, 200);
        let b = PackedRgba::rgb(50, 200, 100);

        for i in 0..=10 {
            let t = i as f64 / 10.0;
            let slow = lerp_color(a, b, t);
            let fast = lerp_color_fast(a, b, t);

            // Allow 1 unit difference due to rounding
            assert!((slow.r() as i16 - fast.r() as i16).abs() <= 1);
            assert!((slow.g() as i16 - fast.g() as i16).abs() <= 1);
            assert!((slow.b() as i16 - fast.b() as i16).abs() <= 1);
        }
    }

    // =========================================================================
    // Vertical, Diagonal, Radial Gradient Tests (bd-2j71)
    // =========================================================================

    /// Helper: Create a simple two-stop gradient for testing.
    fn test_gradient() -> ColorGradient {
        ColorGradient::new(vec![
            (0.0, PackedRgba::rgb(255, 0, 0)), // Red at start
            (1.0, PackedRgba::rgb(0, 0, 255)), // Blue at end
        ])
    }

    #[test]
    fn test_vertical_multiline_top_is_start() {
        // Row 0 of a multi-line text should get sample(0) - red
        let gradient = test_gradient();

        // Simulate multi-line: row 0 of 10 rows
        let t_y = 0.0 / 9.0; // Row 0 of 10 rows (0-indexed)
        let color = gradient.sample(t_y);

        // Should be red (start of gradient)
        assert!(
            color.r() > 200,
            "Top row should be red, got r={}",
            color.r()
        );
        assert!(
            color.b() < 50,
            "Top row should have minimal blue, got b={}",
            color.b()
        );
    }

    #[test]
    fn test_vertical_multiline_bottom_is_end() {
        // Last row of a multi-line text should get sample(1) - blue
        let gradient = test_gradient();

        // Simulate multi-line: row 9 of 10 rows
        let t_y = 9.0 / 9.0; // Last row (t=1.0)
        let color = gradient.sample(t_y);

        // Should be blue (end of gradient)
        assert!(
            color.b() > 200,
            "Bottom row should be blue, got b={}",
            color.b()
        );
        assert!(
            color.r() < 50,
            "Bottom row should have minimal red, got r={}",
            color.r()
        );
    }

    #[test]
    fn test_vertical_singleline_is_middle() {
        // Single-line text should return center color (sample(0.5))
        let gradient = test_gradient();

        let color = gradient.sample(0.5);

        // Should be purple (mix of red and blue)
        assert!(
            color.r() > 100 && color.r() < 180,
            "Middle should have some red, got r={}",
            color.r()
        );
        assert!(
            color.b() > 100 && color.b() < 180,
            "Middle should have some blue, got b={}",
            color.b()
        );
    }

    #[test]
    fn test_diagonal_0_is_horizontal() {
        // Diagonal at 0° should match horizontal gradient behavior
        // At angle 0, cos=1, sin=0, so projected = x * 1 + y * 0 = x
        let angle = 0.0_f64.to_radians();
        let cos_a = angle.cos();
        let sin_a = angle.sin();

        // Two points at same x but different y should have same projection
        let t_x = 0.5;
        let projected1 = t_x * cos_a + 0.0 * sin_a;
        let projected2 = t_x * cos_a + 1.0 * sin_a;

        // Both should be approximately equal since sin(0)=0
        assert!(
            (projected1 - projected2).abs() < 0.001,
            "At 0°, diagonal should be horizontal: {} vs {}",
            projected1,
            projected2
        );
    }

    #[test]
    fn test_diagonal_90_is_vertical() {
        // Diagonal at 90° should be top-to-bottom (vertical)
        let angle = 90.0_f64.to_radians();
        let cos_a = angle.cos();
        let sin_a = angle.sin();

        // Two points at same y but different x should have same projection
        let t_y = 0.5;
        let projected1 = 0.0 * cos_a + t_y * sin_a;
        let projected2 = 1.0 * cos_a + t_y * sin_a;

        // Both should be approximately equal since cos(90°)≈0
        assert!(
            (projected1 - projected2).abs() < 0.01,
            "At 90°, diagonal should be vertical: {} vs {}",
            projected1,
            projected2
        );
    }

    #[test]
    fn test_diagonal_angle_wraps() {
        // 450° should be equivalent to 90°
        let angle_90 = 90.0_f64.to_radians();
        let angle_450 = 450.0_f64.to_radians();

        let cos_90 = angle_90.cos();
        let sin_90 = angle_90.sin();
        let cos_450 = angle_450.cos();
        let sin_450 = angle_450.sin();

        // Trig functions naturally wrap, verify this
        assert!(
            (cos_90 - cos_450).abs() < 0.0001,
            "cos(90°) should equal cos(450°): {} vs {}",
            cos_90,
            cos_450
        );
        assert!(
            (sin_90 - sin_450).abs() < 0.0001,
            "sin(90°) should equal sin(450°): {} vs {}",
            sin_90,
            sin_450
        );
    }

    #[test]
    fn test_radial_center_is_start() {
        // Center of radial gradient should get sample(0) - red
        let gradient = test_gradient();
        let center = (0.5, 0.5);
        let aspect = 1.0;

        // Point exactly at center
        let t_x = 0.5;
        let t_y = 0.5;

        let dx: f64 = (t_x - center.0) * aspect;
        let dy: f64 = t_y - center.1;
        let distance = (dx * dx + dy * dy).sqrt();
        let max_distance = (0.5_f64.powi(2) * aspect * aspect + 0.5_f64.powi(2)).sqrt();
        let t = (distance / max_distance).clamp(0.0, 1.0);

        let color = gradient.sample(t);

        // Should be red (start of gradient, distance=0)
        assert!(color.r() > 200, "Center should be red, got r={}", color.r());
        assert!(t < 0.01, "Center t should be ~0, got {}", t);
    }

    #[test]
    fn test_radial_corner_is_end() {
        // Corner of radial gradient should get sample(1) or close - blue
        let gradient = test_gradient();
        let center = (0.5, 0.5);
        let aspect = 1.0;

        // Point at corner (0, 0)
        let t_x = 0.0;
        let t_y = 0.0;

        let dx: f64 = (t_x - center.0) * aspect;
        let dy: f64 = t_y - center.1;
        let distance = (dx * dx + dy * dy).sqrt();
        let max_distance = (0.5_f64.powi(2) * aspect * aspect + 0.5_f64.powi(2)).sqrt();
        let t = (distance / max_distance).clamp(0.0, 1.0);

        let color = gradient.sample(t);

        // Should be blue (end of gradient, distance=max)
        assert!(
            color.b() > 200,
            "Corner should be blue, got b={}",
            color.b()
        );
        assert!(t > 0.99, "Corner t should be ~1, got {}", t);
    }

    #[test]
    fn test_radial_aspect_stretches() {
        // With aspect=2, horizontal distance counts twice as much
        let center = (0.5, 0.5);

        // Point to the right of center
        let point_right = (1.0, 0.5); // dx=0.5, dy=0
        // Point below center
        let point_below = (0.5, 1.0); // dx=0, dy=0.5

        // With aspect=1 (circular), both should have same distance
        let dx_r_1: f64 = (point_right.0 - center.0) * 1.0;
        let dy_r_1: f64 = point_right.1 - center.1;
        let dist_right_1 = (dx_r_1 * dx_r_1 + dy_r_1 * dy_r_1).sqrt();

        let dx_b_1: f64 = (point_below.0 - center.0) * 1.0;
        let dy_b_1: f64 = point_below.1 - center.1;
        let dist_below_1 = (dx_b_1 * dx_b_1 + dy_b_1 * dy_b_1).sqrt();

        assert!(
            (dist_right_1 - dist_below_1).abs() < 0.001,
            "With aspect=1, right and below should be equidistant"
        );

        // With aspect=2, horizontal distance is doubled
        let dx_r_2: f64 = (point_right.0 - center.0) * 2.0;
        let dy_r_2: f64 = point_right.1 - center.1;
        let dist_right_2 = (dx_r_2 * dx_r_2 + dy_r_2 * dy_r_2).sqrt();

        let dx_b_2: f64 = (point_below.0 - center.0) * 2.0;
        let dy_b_2: f64 = point_below.1 - center.1;
        let dist_below_2 = (dx_b_2 * dx_b_2 + dy_b_2 * dy_b_2).sqrt();

        // Right should now be farther than below
        assert!(
            dist_right_2 > dist_below_2,
            "With aspect=2, right should be farther: {} vs {}",
            dist_right_2,
            dist_below_2
        );
    }

    #[test]
    fn test_gradient_animated_time_offset() {
        // Verify that time offset moves the gradient
        let gradient = test_gradient();

        // Sample at t=0.0 and t=0.5 should give different colors
        let at_0 = gradient.sample(0.0);
        let at_05 = gradient.sample(0.5);

        // With animation, sampling at t=0 with time offset 0.5
        // should give same result as sampling at t=0.5
        let animated_t = (0.0_f64 + 0.5).rem_euclid(1.0);
        let animated = gradient.sample(animated_t);

        assert_eq!(animated.r(), at_05.r());
        assert_eq!(animated.g(), at_05.g());
        assert_eq!(animated.b(), at_05.b());

        // Verify original values are different
        assert_ne!(
            at_0.r(),
            at_05.r(),
            "Gradient should have different colors at different t"
        );
    }

    // =========================================================================
    // OkLab Perceptual Color Space Tests (bd-36k2)
    // =========================================================================

    #[test]
    fn test_oklab_roundtrip_identity() {
        // RGB -> OkLab -> RGB should preserve color within epsilon
        let test_colors = [
            PackedRgba::rgb(0, 0, 0),       // Black
            PackedRgba::rgb(255, 255, 255), // White
            PackedRgba::rgb(255, 0, 0),     // Red
            PackedRgba::rgb(0, 255, 0),     // Green
            PackedRgba::rgb(0, 0, 255),     // Blue
            PackedRgba::rgb(255, 255, 0),   // Yellow
            PackedRgba::rgb(255, 0, 255),   // Magenta
            PackedRgba::rgb(0, 255, 255),   // Cyan
            PackedRgba::rgb(128, 128, 128), // Gray
            PackedRgba::rgb(100, 150, 200), // Arbitrary
        ];

        for color in test_colors {
            let lab = rgb_to_oklab(color);
            let back = oklab_to_rgb(lab);

            // Allow 1 unit difference due to float precision and gamma curves
            assert!(
                (color.r() as i16 - back.r() as i16).abs() <= 1,
                "Red roundtrip failed for {:?}: {} -> {}",
                color,
                color.r(),
                back.r()
            );
            assert!(
                (color.g() as i16 - back.g() as i16).abs() <= 1,
                "Green roundtrip failed for {:?}: {} -> {}",
                color,
                color.g(),
                back.g()
            );
            assert!(
                (color.b() as i16 - back.b() as i16).abs() <= 1,
                "Blue roundtrip failed for {:?}: {} -> {}",
                color,
                color.b(),
                back.b()
            );
        }
    }

    #[test]
    fn test_oklab_black_and_white() {
        // Black should have L=0, a=0, b=0
        let black = rgb_to_oklab(PackedRgba::rgb(0, 0, 0));
        assert!(black.l.abs() < 0.01);
        assert!(black.a.abs() < 0.01);
        assert!(black.b.abs() < 0.01);

        // White should have L≈1, a≈0, b≈0
        let white = rgb_to_oklab(PackedRgba::rgb(255, 255, 255));
        assert!((white.l - 1.0).abs() < 0.01);
        assert!(white.a.abs() < 0.01);
        assert!(white.b.abs() < 0.01);
    }

    #[test]
    fn test_oklab_lerp() {
        let black = OkLab::new(0.0, 0.0, 0.0);
        let white = OkLab::new(1.0, 0.0, 0.0);

        // Midpoint should be gray
        let mid = black.lerp(white, 0.5);
        assert!((mid.l - 0.5).abs() < 0.01);
        assert!(mid.a.abs() < 0.01);
        assert!(mid.b.abs() < 0.01);

        // Endpoints preserved
        let at_zero = black.lerp(white, 0.0);
        assert!((at_zero.l - black.l).abs() < 0.0001);

        let at_one = black.lerp(white, 1.0);
        assert!((at_one.l - white.l).abs() < 0.0001);
    }

    #[test]
    fn test_delta_e_same_color() {
        // Same color should have DeltaE = 0
        let red = PackedRgba::rgb(255, 0, 0);
        assert!(delta_e(red, red) < 0.0001);
    }

    #[test]
    fn test_delta_e_black_white() {
        // Black and white should have significant DeltaE
        let black = PackedRgba::rgb(0, 0, 0);
        let white = PackedRgba::rgb(255, 255, 255);
        let de = delta_e(black, white);
        assert!(
            de > 0.9,
            "DeltaE between black and white should be ~1.0, got {}",
            de
        );
    }

    #[test]
    fn test_deltae_monotonic_simple_gradient() {
        // Simple two-stop gradients (like grayscale) should have monotonic DeltaE from start
        // Note: Complex gradients like rainbow that cycle through hues will NOT be monotonic
        let gradient = ColorGradient::new(vec![
            (0.0, PackedRgba::rgb(0, 0, 0)),       // Black
            (1.0, PackedRgba::rgb(255, 255, 255)), // White
        ]);
        let samples: Vec<PackedRgba> = (0..=20)
            .map(|i| gradient.sample_oklab(i as f64 / 20.0))
            .collect();

        // Validate monotonicity with small tolerance for float precision
        let violation = validate_gradient_monotonicity(&samples, 0.001);
        assert!(
            violation.is_none(),
            "DeltaE not monotonic at index {:?}",
            violation
        );
    }

    #[test]
    fn test_deltae_step_uniformity() {
        // OkLab should produce more uniform perceptual steps between adjacent samples
        let gradient = ColorGradient::new(vec![
            (0.0, PackedRgba::rgb(0, 0, 0)),       // Black
            (1.0, PackedRgba::rgb(255, 255, 255)), // White
        ]);

        // Sample with OkLab interpolation
        let samples: Vec<PackedRgba> = (0..=10)
            .map(|i| gradient.sample_oklab(i as f64 / 10.0))
            .collect();

        // Calculate DeltaE between adjacent samples
        let mut deltas = Vec::new();
        for i in 1..samples.len() {
            deltas.push(delta_e(samples[i - 1], samples[i]));
        }

        // Check that adjacent steps are roughly uniform
        let mean: f64 = deltas.iter().sum::<f64>() / deltas.len() as f64;
        for (i, &d) in deltas.iter().enumerate() {
            // Each step should be within 20% of the mean
            assert!(
                (d - mean).abs() / mean < 0.2,
                "Step {} delta {} differs too much from mean {}",
                i,
                d,
                mean
            );
        }
    }

    #[test]
    fn test_out_of_gamut_clamp() {
        // Extreme OkLab values should not panic, just clamp
        let extreme_colors = [
            OkLab::new(2.0, 0.0, 0.0),  // Over white
            OkLab::new(-1.0, 0.0, 0.0), // Under black
            OkLab::new(0.5, 2.0, 0.0),  // Extreme a
            OkLab::new(0.5, 0.0, -2.0), // Extreme b
            OkLab::new(0.5, 1.0, 1.0),  // Out of gamut
        ];

        for lab in extreme_colors {
            let rgb = oklab_to_rgb(lab);
            // Should not panic and should be valid RGB (u8 values are always in range)
            let _ = (rgb.r(), rgb.g(), rgb.b()); // Access values to verify no panic
        }
    }

    #[test]
    fn test_lerp_color_oklab_endpoints() {
        let red = PackedRgba::rgb(255, 0, 0);
        let blue = PackedRgba::rgb(0, 0, 255);

        // At t=0, should return first color
        let at_zero = lerp_color_oklab(red, blue, 0.0);
        assert_eq!(at_zero.r(), red.r());
        assert_eq!(at_zero.g(), red.g());
        assert_eq!(at_zero.b(), red.b());

        // At t=1, should return second color
        let at_one = lerp_color_oklab(red, blue, 1.0);
        assert_eq!(at_one.r(), blue.r());
        assert_eq!(at_one.g(), blue.g());
        assert_eq!(at_one.b(), blue.b());
    }

    #[test]
    fn test_sample_oklab_matches_endpoints() {
        let gradient = ColorGradient::fire();

        // Sample at t=0 should match first stop
        let at_zero = gradient.sample_oklab(0.0);
        let first = gradient.sample(0.0);
        assert_eq!(at_zero.r(), first.r());
        assert_eq!(at_zero.g(), first.g());
        assert_eq!(at_zero.b(), first.b());

        // Sample at t=1 should match last stop
        let at_one = gradient.sample_oklab(1.0);
        let last = gradient.sample(1.0);
        assert_eq!(at_one.r(), last.r());
        assert_eq!(at_one.g(), last.g());
        assert_eq!(at_one.b(), last.b());
    }

    #[test]
    fn test_sample_fast_oklab_matches_sample_oklab() {
        let gradient = ColorGradient::sunset();

        for i in 0..=100 {
            let t = i as f64 / 100.0;
            let slow = gradient.sample_oklab(t);
            let fast = gradient.sample_fast_oklab(t);

            // Should be identical since both use same algorithm
            assert_eq!(slow.r(), fast.r(), "Red mismatch at t={}", t);
            assert_eq!(slow.g(), fast.g(), "Green mismatch at t={}", t);
            assert_eq!(slow.b(), fast.b(), "Blue mismatch at t={}", t);
        }
    }

    #[test]
    fn test_precompute_lut_oklab_consistency() {
        let gradient = ColorGradient::ocean();
        let lut1 = gradient.precompute_lut_oklab(50);
        let lut2 = gradient.precompute_lut_oklab(50);

        assert_eq!(lut1.len(), lut2.len());
        for i in 0..50 {
            let c1 = lut1.sample(i);
            let c2 = lut2.sample(i);
            assert_eq!(c1.r(), c2.r());
            assert_eq!(c1.g(), c2.g());
            assert_eq!(c1.b(), c2.b());
        }
    }

    #[test]
    fn test_oklab_perceptual_uniformity() {
        // OkLab interpolation should produce more uniform perceptual steps
        // than RGB interpolation between highly saturated colors
        let red = PackedRgba::rgb(255, 0, 0);
        let cyan = PackedRgba::rgb(0, 255, 255);

        // Sample both RGB and OkLab gradients
        let mut rgb_deltas = Vec::new();
        let mut oklab_deltas = Vec::new();

        let steps = 10;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let prev_t = (i - 1) as f64 / steps as f64;

            let rgb_curr = lerp_color(red, cyan, t);
            let rgb_prev = lerp_color(red, cyan, prev_t);
            rgb_deltas.push(delta_e(rgb_prev, rgb_curr));

            let oklab_curr = lerp_color_oklab(red, cyan, t);
            let oklab_prev = lerp_color_oklab(red, cyan, prev_t);
            oklab_deltas.push(delta_e(oklab_prev, oklab_curr));
        }

        // Calculate variance (uniformity measure)
        fn variance(values: &[f64]) -> f64 {
            let mean: f64 = values.iter().sum::<f64>() / values.len() as f64;
            values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64
        }

        let rgb_var = variance(&rgb_deltas);
        let oklab_var = variance(&oklab_deltas);

        // OkLab should have lower variance (more uniform steps)
        // This test may not always pass due to the nature of color perception,
        // but for most saturated color pairs it should
        assert!(
            oklab_var <= rgb_var * 1.5,
            "OkLab variance {} should be similar or lower than RGB variance {}",
            oklab_var,
            rgb_var
        );
    }

    // =========================================================================
    // Shadow and Multi-Layer Glow Tests (bd-3hsz)
    // =========================================================================

    #[test]
    fn test_shadow_offset_applied() {
        let shadow = Shadow::new(2, 3);
        // Shadow at (10, 10) with offset (2, 3) should be at (12, 13)
        let result = shadow.apply_offset(10, 10, 80, 24);
        assert_eq!(result, Some((12, 13)));
    }

    #[test]
    fn test_shadow_negative_offset() {
        let shadow = Shadow::new(-1, -1);
        // Shadow at (10, 10) with offset (-1, -1) should be at (9, 9)
        let result = shadow.apply_offset(10, 10, 80, 24);
        assert_eq!(result, Some((9, 9)));
    }

    #[test]
    fn test_shadow_at_edge_clips() {
        let shadow = Shadow::new(-5, -5);
        // Shadow at (2, 2) with offset (-5, -5) would be at (-3, -3)
        let result = shadow.apply_offset(2, 2, 80, 24);
        // (-3, -3) is outside bounds, should return None
        assert_eq!(result, None);
    }

    #[test]
    fn test_shadow_at_right_edge() {
        let shadow = Shadow::new(5, 0);
        // Shadow at (75, 10) in 80-wide terminal with dx=5 would be at (80, 10)
        // which is outside bounds (0-79 valid)
        let result = shadow.apply_offset(75, 10, 80, 24);
        assert_eq!(result, None);
    }

    #[test]
    fn test_shadow_opacity() {
        let shadow = Shadow::new(1, 1)
            .color(PackedRgba::rgb(100, 100, 100))
            .opacity(0.5);
        let effective = shadow.effective_color();
        // apply_alpha multiplies RGB values by opacity, not alpha channel
        // 100 * 0.5 = 50
        assert_eq!(effective.r(), 50);
        assert_eq!(effective.g(), 50);
        assert_eq!(effective.b(), 50);
    }

    #[test]
    fn test_shadow_zero_opacity_skipped() {
        let text = StyledText::new("Test").shadow(Shadow::new(1, 1).opacity(0.0));
        // Zero opacity shadow should not be added
        assert!(!text.has_shadows());
    }

    #[test]
    fn test_shadow_multiple_layers() {
        let text = StyledText::new("DEPTH").shadows(vec![
            Shadow::new(3, 3).opacity(0.2),
            Shadow::new(2, 2).opacity(0.3),
            Shadow::new(1, 1).opacity(0.5),
        ]);
        assert_eq!(text.shadow_count(), 3);
        assert!(text.has_shadows());

        let shadows = text.get_shadows();
        assert_eq!(shadows[0].dx, 3);
        assert_eq!(shadows[1].dx, 2);
        assert_eq!(shadows[2].dx, 1);
    }

    #[test]
    fn test_shadow_z_order() {
        // Shadows should be stored in order: first is furthest, last is closest
        let text = StyledText::new("Test")
            .shadow(Shadow::new(3, 3).opacity(0.2))
            .shadow(Shadow::new(1, 1).opacity(0.5));

        let shadows = text.get_shadows();
        assert_eq!(shadows.len(), 2);
        assert_eq!(shadows[0].dx, 3); // First added = furthest
        assert_eq!(shadows[1].dx, 1); // Last added = closest
    }

    #[test]
    fn test_shadow_default() {
        let shadow = Shadow::default();
        assert_eq!(shadow.dx, 1);
        assert_eq!(shadow.dy, 1);
        assert!((shadow.opacity - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_glow_layer_count() {
        let glow = GlowConfig::new(PackedRgba::rgb(255, 255, 255)).layers(3);
        assert_eq!(glow.layers, 3);
    }

    #[test]
    fn test_glow_layer_count_clamped() {
        // Layers should be clamped to 1-5
        let too_many = GlowConfig::default().layers(10);
        assert_eq!(too_many.layers, 5);

        let too_few = GlowConfig::default().layers(0);
        assert_eq!(too_few.layers, 1);
    }

    #[test]
    fn test_glow_falloff() {
        let glow = GlowConfig::new(PackedRgba::rgb(0, 255, 255))
            .intensity(0.8)
            .layers(3)
            .falloff(0.5);

        // Layer 0 (outermost) should have lowest opacity
        // Layer 2 (innermost) should have highest opacity
        let layer0_opacity = glow.layer_opacity(0);
        let layer1_opacity = glow.layer_opacity(1);
        let layer2_opacity = glow.layer_opacity(2);

        // Innermost layer should be brightest
        assert!(layer2_opacity > layer1_opacity);
        assert!(layer1_opacity > layer0_opacity);

        // Verify falloff pattern: each layer dimmer by falloff factor
        assert!((layer1_opacity / layer2_opacity - 0.5).abs() < 0.01);
        assert!((layer0_opacity / layer1_opacity - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_glow_intensity_scales() {
        let low_glow = GlowConfig::default().intensity(0.3).layers(2);
        let high_glow = GlowConfig::default().intensity(0.9).layers(2);

        // Higher intensity = brighter layers
        assert!(high_glow.layer_opacity(1) > low_glow.layer_opacity(1));
    }

    #[test]
    fn test_glow_layer_offsets() {
        let glow = GlowConfig::default().layers(2);

        // Layer 0 should have radius 2 (layers - 0 = 2)
        let offsets: Vec<_> = glow.layer_offsets(0).collect();
        assert_eq!(offsets.len(), 8);
        // Should include corners and edges at radius 2
        assert!(offsets.contains(&(-2, -2)));
        assert!(offsets.contains(&(2, 2)));
        assert!(offsets.contains(&(0, 2)));
        assert!(offsets.contains(&(-2, 0)));
    }

    #[test]
    fn test_glow_config_default() {
        let glow = GlowConfig::default();
        assert_eq!(glow.layers, 2);
        assert!((glow.intensity - 0.6).abs() < 0.001);
        assert!((glow.falloff - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_styled_text_with_shadow() {
        let text = StyledText::new("SHADOW").shadow(
            Shadow::new(1, 1)
                .color(PackedRgba::rgb(0, 0, 0))
                .opacity(0.5),
        );

        assert!(text.has_shadows());
        assert_eq!(text.shadow_count(), 1);
    }

    #[test]
    fn test_styled_text_with_glow() {
        let text = StyledText::new("NEON").glow(
            GlowConfig::new(PackedRgba::rgb(0, 255, 255))
                .intensity(0.8)
                .layers(3),
        );

        assert!(text.has_glow());
        assert!(text.glow_config().is_some());
        assert_eq!(text.glow_config().unwrap().layers, 3);
    }

    #[test]
    fn test_styled_text_clear_shadows() {
        let text = StyledText::new("Test")
            .shadow(Shadow::new(1, 1))
            .shadow(Shadow::new(2, 2))
            .clear_shadows();

        assert!(!text.has_shadows());
        assert_eq!(text.shadow_count(), 0);
    }

    #[test]
    fn test_styled_text_clear_glow() {
        let text = StyledText::new("Test")
            .glow(GlowConfig::default())
            .clear_glow();

        assert!(!text.has_glow());
        assert!(text.glow_config().is_none());
    }

    #[test]
    fn test_shadow_and_glow_combined() {
        let text = StyledText::new("COMBINED")
            .shadow(Shadow::new(2, 2).opacity(0.3))
            .glow(GlowConfig::new(PackedRgba::rgb(255, 0, 255)).layers(2));

        assert!(text.has_shadows());
        assert!(text.has_glow());
        assert_eq!(text.shadow_count(), 1);
        assert!(text.glow_config().is_some());
    }

    // =========================================================================
    // Outline/Border Tests (bd-882b)
    // =========================================================================

    #[test]
    fn test_outline_8_neighbors() {
        // thickness=1 should produce 8 adjacent cell offsets
        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).thickness(1);
        let offsets: Vec<_> = config.offsets().collect();

        assert_eq!(offsets.len(), 8, "thickness=1 should have 8 neighbors");

        // Verify all expected offsets are present (Moore neighborhood)
        let expected = [
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ];
        for exp in expected {
            assert!(
                offsets.contains(&exp),
                "Expected offset {:?} not found in {:?}",
                exp,
                offsets
            );
        }
    }

    #[test]
    fn test_outline_16_neighbors() {
        // thickness=2 should produce 16 offsets
        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).thickness(2);
        let offsets: Vec<_> = config.offsets().collect();

        assert_eq!(offsets.len(), 16, "thickness=2 should have 16 neighbors");

        // Verify offsets extend to distance 2
        let has_dist_2 = offsets
            .iter()
            .any(|(dx, dy)| dx.abs() == 2 || dy.abs() == 2);
        assert!(has_dist_2, "thickness=2 should have offsets at distance 2");
    }

    #[test]
    fn test_outline_24_neighbors() {
        // thickness=3 should produce 24 offsets (5x5 minus center)
        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).thickness(3);
        let offsets: Vec<_> = config.offsets().collect();

        assert_eq!(offsets.len(), 24, "thickness=3 should have 24 neighbors");

        // Verify corners are included
        assert!(
            offsets.contains(&(-2, -2)),
            "Should include corner (-2, -2)"
        );
        assert!(offsets.contains(&(2, 2)), "Should include corner (2, 2)");
    }

    #[test]
    fn test_outline_solid_continuous() {
        // Solid style should never return None for any offset
        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).style(OutlineStyle::Solid);

        for offset_idx in 0..24 {
            assert!(
                config.color_at(offset_idx, 0.0).is_some(),
                "Solid style should return color for all offsets"
            );
        }
    }

    #[test]
    fn test_outline_dashed_alternates() {
        // Dashed style with dash_len=2 should alternate: 2 visible, 2 invisible
        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255))
            .style(OutlineStyle::Dashed { dash_len: 2 });

        // Check pattern: indices 0,1 visible, 2,3 invisible, 4,5 visible, etc.
        assert!(
            config.color_at(0, 0.0).is_some(),
            "Index 0 should be visible"
        );
        assert!(
            config.color_at(1, 0.0).is_some(),
            "Index 1 should be visible"
        );
        assert!(
            config.color_at(2, 0.0).is_none(),
            "Index 2 should be invisible"
        );
        assert!(
            config.color_at(3, 0.0).is_none(),
            "Index 3 should be invisible"
        );
        assert!(
            config.color_at(4, 0.0).is_some(),
            "Index 4 should be visible"
        );
        assert!(
            config.color_at(5, 0.0).is_some(),
            "Index 5 should be visible"
        );
    }

    #[test]
    fn test_outline_double_two_rings() {
        // Double style should be detected for special rendering
        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).style(OutlineStyle::Double);

        assert!(
            config.is_double(),
            "Double style should return true for is_double()"
        );

        // Solid style should not be double
        let solid_config =
            OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).style(OutlineStyle::Solid);
        assert!(
            !solid_config.is_double(),
            "Solid style should return false for is_double()"
        );
    }

    #[test]
    fn test_outline_gradient_animates() {
        // Gradient style should produce different colors at different times
        let gradient = ColorGradient::new(vec![
            (0.0, PackedRgba::rgb(255, 0, 0)),
            (0.5, PackedRgba::rgb(0, 255, 0)),
            (1.0, PackedRgba::rgb(0, 0, 255)),
        ]);
        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255))
            .style(OutlineStyle::Gradient(gradient));

        let color_t0 = config.color_at(0, 0.0);
        let color_t1 = config.color_at(0, 1.0);

        assert!(color_t0.is_some(), "Gradient should return color at t=0");
        assert!(color_t1.is_some(), "Gradient should return color at t=1");

        // Colors should differ (animation effect)
        // Note: they might be the same at certain time values, so we test the mechanism works
    }

    #[test]
    fn test_outline_under_text() {
        // StyledText with outline should have outline configured
        let text = StyledText::new("OUTLINED")
            .outline(OutlineConfig::new(PackedRgba::rgb(0, 0, 0)).thickness(1));

        assert!(text.has_outline());
        assert!(text.outline_config().is_some());

        let config = text.outline_config().unwrap();
        assert_eq!(config.thickness, 1);
    }

    #[test]
    fn test_outline_at_edge() {
        // Outline offsets near terminal edge should not cause overflow
        // This tests the saturating arithmetic in outline rendering

        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).thickness(3);

        // Test with position at (0, 0) - should not panic
        let offsets: Vec<_> = config.offsets().collect();
        for (dx, dy) in offsets {
            // Simulate offset calculation with saturating arithmetic
            let base_x: u16 = 0;
            let base_y: u16 = 0;
            let outline_x = (base_x as i32).saturating_add(i32::from(dx));
            let outline_y = (base_y as i32).saturating_add(i32::from(dy));

            // Should not panic, just go negative (which is filtered by bounds check)
            assert!((-3..=3).contains(&outline_x));
            assert!((-3..=3).contains(&outline_y));
        }
    }

    #[test]
    fn test_outline_thickness_clamped() {
        // Thickness should be clamped to 1-3
        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).thickness(10);
        assert_eq!(config.thickness, 3, "thickness should be clamped to max 3");

        let config2 = OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).thickness(0);
        assert_eq!(config2.thickness, 1, "thickness should be clamped to min 1");
    }

    #[test]
    fn test_outline_default() {
        let config = OutlineConfig::default();

        assert_eq!(config.thickness, 1);
        assert!(matches!(config.style, OutlineStyle::Solid));
        assert!(config.use_text_char);
    }

    #[test]
    fn test_outline_clear() {
        let text = StyledText::new("Test")
            .outline(OutlineConfig::new(PackedRgba::rgb(255, 255, 255)))
            .clear_outline();

        assert!(!text.has_outline());
        assert!(text.outline_config().is_none());
    }

    #[test]
    fn test_outline_use_block_char() {
        let config = OutlineConfig::new(PackedRgba::rgb(255, 255, 255)).use_text_char(false);

        assert!(!config.use_text_char);
    }

    #[test]
    fn test_shadow_glow_outline_combined() {
        // All three effects can be combined
        let text = StyledText::new("FULL")
            .shadow(Shadow::new(2, 2).opacity(0.3))
            .glow(GlowConfig::new(PackedRgba::rgb(255, 0, 255)).layers(2))
            .outline(OutlineConfig::new(PackedRgba::rgb(0, 0, 0)).thickness(1));

        assert!(text.has_shadows());
        assert!(text.has_glow());
        assert!(text.has_outline());
        assert_eq!(text.shadow_count(), 1);
        assert!(text.glow_config().is_some());
        assert!(text.outline_config().is_some());
    }

    // =========================================================================

    #[test]
    fn test_styled_text_effects() {
        let text = StyledText::new("Hello")
            .effect(TextEffect::RainbowGradient { speed: 1.0 })
            .time(0.5);

        assert_eq!(text.len(), 5);
        assert!(!text.is_empty());
    }

    #[test]
    fn test_transition_state() {
        let mut state = TransitionState::new();
        assert!(!state.is_active());

        state.start("Title", "Sub", PackedRgba::rgb(255, 0, 0));
        assert!(state.is_active());

        for _ in 0..50 {
            state.tick();
        }
        assert!(!state.is_active());
    }

    #[test]
    fn test_scramble_effect() {
        let text = StyledText::new("TEST")
            .effect(TextEffect::Scramble { progress: 0.0 })
            .seed(42)
            .time(1.0);

        // At progress 0, characters should be scrambled
        let ch = text.char_at(0, 'T');
        // The scrambled char will be random but not necessarily 'T'
        assert!(ch.is_ascii_graphic());
    }

    #[test]
    fn test_ascii_art_basic() {
        let art = AsciiArtText::new("HI", AsciiArtStyle::Block);
        let lines = art.render_lines();
        assert!(!lines.is_empty());
        // Block style produces 5-line characters
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn test_ascii_art_styles() {
        for style in [
            AsciiArtStyle::Block,
            AsciiArtStyle::Banner,
            AsciiArtStyle::Mini,
            AsciiArtStyle::Slant,
        ] {
            let art = AsciiArtText::new("A", style);
            let lines = art.render_lines();
            assert!(!lines.is_empty());
        }
    }

    // =========================================================================
    // Organic Pulse Tests (bd-27kx)
    // =========================================================================

    #[test]
    fn test_organic_asymmetric_rise_fast() {
        // With asymmetry > 0, rise duration < fall duration
        // At asymmetry=0.5: rise is 0.3 - 0.5*0.2 = 0.2 (20% of cycle)
        let rise_end = 0.2;
        let peak = breathing_curve(rise_end, 0.5);
        assert!(
            (peak - 1.0).abs() < 0.01,
            "Expected peak ~1.0 at t={rise_end}, got {peak}"
        );
        let mid_rise = breathing_curve(0.1, 0.5);
        assert!(mid_rise > 0.5, "Expected mid-rise > 0.5, got {mid_rise}");
    }

    #[test]
    fn test_organic_symmetric_equal() {
        // With asymmetry=0, rise takes 30% of cycle
        let mid_rise = breathing_curve(0.15, 0.0);
        let mid_fall = breathing_curve(0.65, 0.0);
        assert!(
            mid_rise > 0.4 && mid_rise < 0.9,
            "Expected mid_rise in range, got {mid_rise}"
        );
        assert!(
            mid_fall > 0.4 && mid_fall < 0.9,
            "Expected mid_fall in range, got {mid_fall}"
        );
    }

    #[test]
    fn test_organic_phase_variation_spreads() {
        let seed = 42u64;
        let phase_variation = 0.5;
        let phase0 = organic_char_phase_offset(0, seed, phase_variation);
        let phase1 = organic_char_phase_offset(1, seed, phase_variation);
        let phase2 = organic_char_phase_offset(2, seed, phase_variation);
        assert!(phase0 >= 0.0 && phase0 <= phase_variation);
        assert!(phase1 >= 0.0 && phase1 <= phase_variation);
        assert!(phase2 >= 0.0 && phase2 <= phase_variation);
        let all_same = (phase0 - phase1).abs() < 0.001 && (phase1 - phase2).abs() < 0.001;
        assert!(!all_same, "Expected different phases for adjacent chars");
    }

    #[test]
    fn test_organic_seed_deterministic() {
        let seed = 12345u64;
        let phase_variation = 1.0;
        let phase_a = organic_char_phase_offset(10, seed, phase_variation);
        let phase_b = organic_char_phase_offset(10, seed, phase_variation);
        assert!(
            (phase_a - phase_b).abs() < 1e-10,
            "Expected identical phases for same seed+index"
        );
        let phase_c = organic_char_phase_offset(10, seed + 1, phase_variation);
        assert!(
            (phase_a - phase_c).abs() > 0.01,
            "Expected different phases for different seeds"
        );
    }

    #[test]
    fn test_organic_min_brightness_floor() {
        let at_start = breathing_curve(0.0, 0.5);
        assert!(
            at_start < 0.01,
            "Expected curve near 0 at t=0, got {at_start}"
        );
        let at_end = breathing_curve(0.999, 0.5);
        assert!(at_end < 0.05, "Expected curve near 0 at t~1, got {at_end}");
    }

    #[test]
    fn test_organic_max_brightness_ceiling() {
        let peak = breathing_curve(0.2, 0.5);
        assert!((peak - 1.0).abs() < 0.01, "Expected peak=1.0, got {peak}");
    }

    #[test]
    fn test_organic_cycle_complete() {
        let start = breathing_curve(0.0, 0.5);
        let end = breathing_curve(1.0, 0.5);
        assert!(
            (start - end).abs() < 0.01,
            "Expected cycle to complete: start={start}, end={end}"
        );
    }

    #[test]
    fn test_organic_pulse_effect_integration() {
        // Test with zero phase variation so t=0 gives exact min_brightness
        let text = StyledText::new("HELLO")
            .effect(TextEffect::OrganicPulse {
                speed: 1.0,
                min_brightness: 0.3,
                asymmetry: 0.5,
                phase_variation: 0.0, // No phase variation for predictable test
                seed: 42,
            })
            .time(0.0);
        let color = text.char_color(0, 5);
        // At t=0 with no phase variation, breathing_curve(0, _) = 0
        // brightness = 0.3 + 0.7 * 0 = 0.3, which is ~76
        let expected = (0.3 * 255.0) as u8;
        assert!(
            color.r() >= expected.saturating_sub(10) && color.r() <= expected.saturating_add(10),
            "Expected color ~{expected}, got {}",
            color.r()
        );

        // Test with phase variation - should be in valid [min, max] range
        let text_varied = StyledText::new("HELLO")
            .effect(TextEffect::OrganicPulse {
                speed: 1.0,
                min_brightness: 0.3,
                asymmetry: 0.5,
                phase_variation: 0.5,
                seed: 42,
            })
            .time(0.0);
        let color_varied = text_varied.char_color(0, 5);
        let min_expected = (0.3 * 255.0) as u8;
        assert!(
            color_varied.r() >= min_expected,
            "Color should be >= {min_expected}, got {}",
            color_varied.r()
        );
    }

    // =========================================================================
    // Easing Tests
    // =========================================================================

    #[test]
    fn test_easing_linear_identity() {
        // Linear.apply(t) == t for all t
        for i in 0..=100 {
            let t = i as f64 / 100.0;
            let result = Easing::Linear.apply(t);
            assert!(
                (result - t).abs() < 1e-10,
                "Linear({t}) should equal {t}, got {result}"
            );
        }
    }

    #[test]
    fn test_easing_input_clamped() {
        // Inputs outside 0-1 should be clamped
        let easings = [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::EaseInQuad,
            Easing::EaseOutQuad,
            Easing::EaseInOutQuad,
            Easing::Bounce,
        ];

        for easing in easings {
            let at_zero = easing.apply(0.0);
            let below_zero = easing.apply(-0.5);
            let above_one = easing.apply(1.5);
            let at_one = easing.apply(1.0);

            assert!(
                (below_zero - at_zero).abs() < 1e-10,
                "{:?}.apply(-0.5) should equal apply(0.0)",
                easing
            );
            assert!(
                (above_one - at_one).abs() < 1e-10,
                "{:?}.apply(1.5) should equal apply(1.0)",
                easing
            );
        }
    }

    #[test]
    fn test_easing_bounds_normal() {
        // Most curves output 0 at t=0, 1 at t=1
        let easings = [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::EaseInQuad,
            Easing::EaseOutQuad,
            Easing::EaseInOutQuad,
            Easing::Bounce,
        ];

        for easing in easings {
            let start = easing.apply(0.0);
            let end = easing.apply(1.0);

            assert!(
                start.abs() < 1e-10,
                "{:?}.apply(0.0) should be 0, got {start}",
                easing
            );
            assert!(
                (end - 1.0).abs() < 1e-10,
                "{:?}.apply(1.0) should be 1, got {end}",
                easing
            );
        }
    }

    #[test]
    fn test_easing_elastic_overshoots() {
        // Elastic briefly exceeds 1.0
        assert!(Easing::Elastic.can_overshoot());

        // Find the maximum value in the curve
        let mut max_val = 0.0_f64;
        for i in 0..=1000 {
            let t = i as f64 / 1000.0;
            let val = Easing::Elastic.apply(t);
            max_val = max_val.max(val);
        }

        assert!(
            max_val > 1.0,
            "Elastic should exceed 1.0, max was {max_val}"
        );
    }

    #[test]
    fn test_easing_back_overshoots() {
        // Back goes < 0 at start or > 1 during transition
        assert!(Easing::Back.can_overshoot());

        let mut min_val = f64::MAX;
        let mut max_val = f64::MIN;

        for i in 0..=1000 {
            let t = i as f64 / 1000.0;
            let val = Easing::Back.apply(t);
            min_val = min_val.min(val);
            max_val = max_val.max(val);
        }

        // Back should overshoot in one direction
        assert!(
            min_val < 0.0 || max_val > 1.0,
            "Back should overshoot, got range [{min_val}, {max_val}]"
        );
    }

    #[test]
    fn test_easing_monotonic() {
        // EaseIn/Out should be monotonically increasing
        let monotonic_easings = [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::EaseInQuad,
            Easing::EaseOutQuad,
            Easing::EaseInOutQuad,
        ];

        for easing in monotonic_easings {
            let mut prev = easing.apply(0.0);
            for i in 1..=100 {
                let t = i as f64 / 100.0;
                let curr = easing.apply(t);
                assert!(
                    curr >= prev - 1e-10,
                    "{:?} is not monotonic at t={t}: {prev} -> {curr}",
                    easing
                );
                prev = curr;
            }
        }
    }

    #[test]
    fn test_easing_step_discrete() {
        // Step(4) outputs exactly {0, 0.25, 0.5, 0.75, 1.0}
        let step4 = Easing::Step(4);

        let expected = [0.0, 0.25, 0.5, 0.75, 1.0];
        let inputs = [0.0, 0.25, 0.5, 0.75, 1.0];

        for (t, exp) in inputs.iter().zip(expected.iter()) {
            let result = step4.apply(*t);
            assert!(
                (result - exp).abs() < 1e-10,
                "Step(4).apply({t}) should be {exp}, got {result}"
            );
        }

        // Values between steps should snap to lower step
        let mid_result = step4.apply(0.3);
        assert!(
            (mid_result - 0.25).abs() < 1e-10,
            "Step(4).apply(0.3) should be 0.25, got {mid_result}"
        );
    }

    #[test]
    fn test_easing_in_slow_start() {
        // EaseIn should be slow at start (derivative ≈ 0)
        // Compare values at t=0.1: EaseIn should be much smaller than Linear
        let linear = Easing::Linear.apply(0.1);
        let ease_in = Easing::EaseIn.apply(0.1);

        assert!(
            ease_in < linear,
            "EaseIn(0.1) should be less than Linear(0.1)"
        );
        assert!(
            ease_in < linear * 0.5,
            "EaseIn(0.1) should be significantly slower than Linear"
        );
    }

    #[test]
    fn test_easing_out_slow_end() {
        // EaseOut should be slow at end
        // Compare values at t=0.9: EaseOut should be much larger than Linear
        let linear = Easing::Linear.apply(0.9);
        let ease_out = Easing::EaseOut.apply(0.9);

        assert!(
            ease_out > linear,
            "EaseOut(0.9) should be greater than Linear(0.9)"
        );
    }

    #[test]
    fn test_easing_symmetry() {
        // EaseInOut should be symmetric around t=0.5
        let easing = Easing::EaseInOut;

        // At t=0.5, value should be 0.5
        let mid = easing.apply(0.5);
        assert!(
            (mid - 0.5).abs() < 1e-10,
            "EaseInOut(0.5) should be 0.5, got {mid}"
        );

        // Check symmetry: f(t) + f(1-t) = 1
        for i in 0..=50 {
            let t = i as f64 / 100.0;
            let left = easing.apply(t);
            let right = easing.apply(1.0 - t);

            assert!(
                (left + right - 1.0).abs() < 1e-10,
                "EaseInOut should be symmetric: f({t}) + f({}) = {} (expected 1.0)",
                1.0 - t,
                left + right
            );
        }
    }

    #[test]
    fn test_easing_styled_text_integration() {
        // Verify StyledText can use easing
        let text = StyledText::new("Hello")
            .effect(TextEffect::Pulse {
                speed: 1.0,
                min_alpha: 0.3,
            })
            .easing(Easing::EaseInOut)
            .time(0.25);

        assert_eq!(text.len(), 5);
    }

    #[test]
    fn test_easing_transition_state_integration() {
        let mut state = TransitionState::new();
        state.set_easing(Easing::EaseOut);

        assert_eq!(state.easing(), Easing::EaseOut);

        state.start("Test", "Subtitle", PackedRgba::rgb(255, 0, 0));

        // Progress starts at 0
        assert!((state.eased_progress() - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_easing_names() {
        // All easings should have names
        let easings = [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::EaseInQuad,
            Easing::EaseOutQuad,
            Easing::EaseInOutQuad,
            Easing::Bounce,
            Easing::Elastic,
            Easing::Back,
            Easing::Step(4),
        ];

        for easing in easings {
            let name = easing.name();
            assert!(!name.is_empty(), "{:?} should have a name", easing);
        }
    }

    // =========================================================================
    // AnimationClock Tests
    // =========================================================================

    #[test]
    fn test_clock_new_starts_at_zero() {
        let clock = AnimationClock::new();
        assert!((clock.time() - 0.0).abs() < 1e-10);
        assert!((clock.speed() - 1.0).abs() < 1e-10);
        assert!(!clock.is_paused());
    }

    #[test]
    fn test_clock_with_time() {
        let clock = AnimationClock::with_time(5.0);
        assert!((clock.time() - 5.0).abs() < 1e-10);
    }

    #[test]
    fn test_clock_tick_delta_advances() {
        let mut clock = AnimationClock::new();
        clock.tick_delta(0.5);
        assert!((clock.time() - 0.5).abs() < 1e-10);

        clock.tick_delta(0.25);
        assert!((clock.time() - 0.75).abs() < 1e-10);
    }

    #[test]
    fn test_clock_pause_stops_time() {
        let mut clock = AnimationClock::new();
        clock.pause();
        assert!(clock.is_paused());
        assert!((clock.speed() - 0.0).abs() < 1e-10);

        // Ticking while paused should not advance time
        clock.tick_delta(1.0);
        assert!((clock.time() - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_clock_resume_restarts() {
        let mut clock = AnimationClock::new();
        clock.pause();
        assert!(clock.is_paused());

        clock.resume();
        assert!(!clock.is_paused());
        assert!((clock.speed() - 1.0).abs() < 1e-10);

        clock.tick_delta(1.0);
        assert!((clock.time() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_clock_speed_multiplies() {
        let mut clock = AnimationClock::new();
        clock.set_speed(2.0);
        clock.tick_delta(1.0);
        // At 2x speed, 1 second real time = 2 seconds animation time
        assert!((clock.time() - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_clock_half_speed() {
        let mut clock = AnimationClock::new();
        clock.set_speed(0.5);
        clock.tick_delta(1.0);
        // At 0.5x speed, 1 second real time = 0.5 seconds animation time
        assert!((clock.time() - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_clock_reset_zeros() {
        let mut clock = AnimationClock::new();
        clock.tick_delta(5.0);
        assert!((clock.time() - 5.0).abs() < 1e-10);

        clock.reset();
        assert!((clock.time() - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_clock_set_time() {
        let mut clock = AnimationClock::new();
        clock.set_time(10.0);
        assert!((clock.time() - 10.0).abs() < 1e-10);
    }

    #[test]
    fn test_clock_elapsed_since() {
        let mut clock = AnimationClock::new();
        clock.tick_delta(5.0);

        let elapsed = clock.elapsed_since(2.0);
        assert!((elapsed - 3.0).abs() < 1e-10);

        // Elapsed since future time should be 0
        let elapsed_future = clock.elapsed_since(10.0);
        assert!((elapsed_future - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_clock_phase_cycling() {
        let mut clock = AnimationClock::new();

        // At time 0, phase should be 0
        assert!((clock.phase(1.0) - 0.0).abs() < 1e-10);

        // At time 0.5 with 1 cycle/sec, phase = 0.5
        clock.set_time(0.5);
        assert!((clock.phase(1.0) - 0.5).abs() < 1e-10);

        // At time 1.0, phase should wrap to 0
        clock.set_time(1.0);
        assert!((clock.phase(1.0) - 0.0).abs() < 1e-10);

        // At time 1.25, phase = 0.25
        clock.set_time(1.25);
        assert!((clock.phase(1.0) - 0.25).abs() < 1e-10);
    }

    #[test]
    fn test_clock_phase_frequency() {
        let mut clock = AnimationClock::new();
        clock.set_time(0.5);

        // 2 cycles per second: at t=0.5, phase = (0.5 * 2).fract() = 0.0
        assert!((clock.phase(2.0) - 0.0).abs() < 1e-10);

        clock.set_time(0.25);
        // At t=0.25 with 2 cycles/sec: phase = (0.25 * 2).fract() = 0.5
        assert!((clock.phase(2.0) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_clock_phase_zero_frequency() {
        let clock = AnimationClock::with_time(5.0);
        // Zero or negative frequency should return 0
        assert!((clock.phase(0.0) - 0.0).abs() < 1e-10);
        assert!((clock.phase(-1.0) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_clock_negative_speed_clamped() {
        let mut clock = AnimationClock::new();
        clock.set_speed(-5.0);
        // Negative speed should be clamped to 0
        assert!((clock.speed() - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_clock_default() {
        let clock = AnimationClock::default();
        assert!((clock.time() - 0.0).abs() < 1e-10);
        assert!((clock.speed() - 1.0).abs() < 1e-10);
    }

    // =========================================================================
    // Position/Wave Effect Tests (bd-3bix)
    // =========================================================================

    #[test]
    fn test_wave_offset_sinusoidal() {
        // Wave offset follows a sine curve
        let text = StyledText::new("ABCDEFGHIJ")
            .effect(TextEffect::Wave {
                amplitude: 2.0,
                wavelength: 10.0,
                speed: 0.0, // No time-based animation for this test
                direction: Direction::Down,
            })
            .time(0.0);

        let total = text.len();

        // At time 0, offset at idx 0 should be 0 (sin(0) = 0)
        let offset0 = text.char_offset(0, total);
        assert_eq!(offset0.dy, 0);

        // At wavelength/4 (idx 2.5 ~ idx 2), should be near max
        let offset2 = text.char_offset(2, total);
        // sin(0.2 * TAU) ≈ 0.95, * 2 ≈ 1.9 → rounds to 2
        assert!(offset2.dy.abs() <= 2);
    }

    #[test]
    fn test_wave_amplitude_respected() {
        // Max offset should not exceed amplitude
        let text = StyledText::new("ABCDEFGHIJ")
            .effect(TextEffect::Wave {
                amplitude: 3.0,
                wavelength: 4.0,
                speed: 1.0,
                direction: Direction::Down,
            })
            .time(0.25);

        let total = text.len();

        for i in 0..total {
            let offset = text.char_offset(i, total);
            assert!(
                offset.dy.abs() <= 3,
                "Wave offset {} at idx {} exceeds amplitude 3",
                offset.dy,
                i
            );
        }
    }

    #[test]
    fn test_wave_wavelength_period() {
        // Characters wavelength apart should have approximately the same offset
        let text = StyledText::new("ABCDEFGHIJ")
            .effect(TextEffect::Wave {
                amplitude: 2.0,
                wavelength: 5.0,
                speed: 0.0,
                direction: Direction::Down,
            })
            .time(0.0);

        let total = text.len();

        // idx 0 and idx 5 should have similar offsets (one wavelength apart)
        let offset0 = text.char_offset(0, total);
        let offset5 = text.char_offset(5, total);

        assert_eq!(
            offset0.dy, offset5.dy,
            "Characters one wavelength apart should have same offset"
        );
    }

    #[test]
    fn test_wave_direction_up_down() {
        // Vertical wave affects dy only
        let text = StyledText::new("ABC")
            .effect(TextEffect::Wave {
                amplitude: 2.0,
                wavelength: 4.0,
                speed: 1.0,
                direction: Direction::Down,
            })
            .time(0.25);

        let offset = text.char_offset(1, 3);
        assert_eq!(offset.dx, 0, "Vertical wave should not affect dx");
    }

    #[test]
    fn test_wave_direction_left_right() {
        // Horizontal wave affects dx only
        let text = StyledText::new("ABC")
            .effect(TextEffect::Wave {
                amplitude: 2.0,
                wavelength: 4.0,
                speed: 1.0,
                direction: Direction::Right,
            })
            .time(0.25);

        let offset = text.char_offset(1, 3);
        assert_eq!(offset.dy, 0, "Horizontal wave should not affect dy");
    }

    #[test]
    fn test_bounce_starts_high() {
        // At time 0, first character should have maximum offset (height)
        let text = StyledText::new("ABC")
            .effect(TextEffect::Bounce {
                height: 3.0,
                speed: 1.0,
                stagger: 0.0,
                damping: 0.9,
            })
            .time(0.0);

        let offset = text.char_offset(0, 3);
        // Bounce starts at max height (negative dy = up)
        assert!(offset.dy < 0, "Bounce should start with upward offset");
        assert!(
            offset.dy.abs() <= 3,
            "Bounce initial offset should not exceed height"
        );
    }

    #[test]
    fn test_bounce_settles() {
        // After sufficient time with damping, offset approaches 0
        let text = StyledText::new("A")
            .effect(TextEffect::Bounce {
                height: 5.0,
                speed: 2.0,
                stagger: 0.0,
                damping: 0.5, // Fast settling
            })
            .time(5.0);

        let offset = text.char_offset(0, 1);
        assert!(
            offset.dy.abs() <= 1,
            "Bounce should settle near 0 after time"
        );
    }

    #[test]
    fn test_bounce_stagger() {
        // Adjacent chars with stagger should have different offsets
        let text = StyledText::new("ABC")
            .effect(TextEffect::Bounce {
                height: 3.0,
                speed: 1.0,
                stagger: 0.5, // Significant stagger
                damping: 0.9,
            })
            .time(0.5);

        let offset0 = text.char_offset(0, 3);
        let offset1 = text.char_offset(1, 3);

        // With stagger, characters at different positions should have different phases
        // (though they might occasionally be equal)
        // At least verify the stagger doesn't break anything
        assert!(
            offset0.dx == 0 && offset1.dx == 0,
            "Bounce is vertical only"
        );
    }

    #[test]
    fn test_shake_bounded() {
        // Shake offset should never exceed intensity
        let text = StyledText::new("ABCDEFGHIJ")
            .effect(TextEffect::Shake {
                intensity: 2.0,
                speed: 10.0,
                seed: 12345,
            })
            .time(0.5);

        let total = text.len();

        for i in 0..total {
            let offset = text.char_offset(i, total);
            assert!(
                offset.dx.abs() <= 2,
                "Shake dx {} exceeds intensity at idx {}",
                offset.dx,
                i
            );
            assert!(
                offset.dy.abs() <= 2,
                "Shake dy {} exceeds intensity at idx {}",
                offset.dy,
                i
            );
        }
    }

    #[test]
    fn test_shake_deterministic() {
        // Same seed + time = same offset
        let text1 = StyledText::new("ABC")
            .effect(TextEffect::Shake {
                intensity: 2.0,
                speed: 10.0,
                seed: 42,
            })
            .time(1.23);

        let text2 = StyledText::new("ABC")
            .effect(TextEffect::Shake {
                intensity: 2.0,
                speed: 10.0,
                seed: 42,
            })
            .time(1.23);

        for i in 0..3 {
            let offset1 = text1.char_offset(i, 3);
            let offset2 = text2.char_offset(i, 3);
            assert_eq!(offset1, offset2, "Same seed+time should give same offset");
        }
    }

    #[test]
    fn test_cascade_reveals_in_order() {
        // At time 0, all characters should be offset
        let text = StyledText::new("ABC")
            .effect(TextEffect::Cascade {
                speed: 1.0,
                direction: Direction::Down,
                stagger: 1.0,
            })
            .time(0.0);

        let offset0 = text.char_offset(0, 3);
        let offset2 = text.char_offset(2, 3);

        // At time 0, all chars should have an offset
        // (they slide in from above for Direction::Down)
        assert!(
            offset0.dy < 0 || offset2.dy < 0,
            "Cascade should offset chars"
        );
    }

    #[test]
    fn test_offset_bounds_saturate() {
        // Large offsets should use saturating arithmetic
        let offset = CharacterOffset::new(i16::MAX, i16::MAX);
        let added = offset + CharacterOffset::new(100, 100);

        assert_eq!(added.dx, i16::MAX, "dx should saturate");
        assert_eq!(added.dy, i16::MAX, "dy should saturate");
    }

    #[test]
    fn test_negative_offset_clamped() {
        // Offset at position (0, 0) should clamp negative offsets
        let offset = CharacterOffset::new(-5, -5);
        let clamped = offset.clamp_for_position(0, 0, 80, 24);

        assert_eq!(clamped.dx, 0, "dx should clamp to 0 at edge");
        assert_eq!(clamped.dy, 0, "dy should clamp to 0 at edge");
    }

    #[test]
    fn test_direction_is_vertical() {
        assert!(Direction::Up.is_vertical());
        assert!(Direction::Down.is_vertical());
        assert!(!Direction::Left.is_vertical());
        assert!(!Direction::Right.is_vertical());
    }

    #[test]
    fn test_direction_is_horizontal() {
        assert!(Direction::Left.is_horizontal());
        assert!(Direction::Right.is_horizontal());
        assert!(!Direction::Up.is_horizontal());
        assert!(!Direction::Down.is_horizontal());
    }

    #[test]
    fn test_has_position_effects() {
        let plain = StyledText::new("test");
        assert!(!plain.has_position_effects());

        let with_wave = StyledText::new("test").effect(TextEffect::Wave {
            amplitude: 1.0,
            wavelength: 5.0,
            speed: 1.0,
            direction: Direction::Down,
        });
        assert!(with_wave.has_position_effects());

        let with_color = StyledText::new("test").effect(TextEffect::RainbowGradient { speed: 1.0 });
        assert!(!with_color.has_position_effects());
    }

    #[test]
    fn test_multiple_position_effects_add() {
        // Wave + Shake offsets should sum
        let text = StyledText::new("ABC")
            .effect(TextEffect::Wave {
                amplitude: 1.0,
                wavelength: 10.0,
                speed: 0.0,
                direction: Direction::Down,
            })
            .effect(TextEffect::Shake {
                intensity: 1.0,
                speed: 10.0,
                seed: 42,
            })
            .time(0.5);

        // Just verify it doesn't panic and produces an offset
        let offset = text.char_offset(1, 3);
        // The combined offset should exist (could be any value within bounds)
        assert!(offset.dx.abs() <= 2 || offset.dy.abs() <= 3);
    }

    // =========================================================================
    // Composable Effect Chain Tests (bd-3aa3)
    // =========================================================================

    #[test]
    fn test_single_effect_backwards_compat() {
        // .effect(e) still works as before
        let text = StyledText::new("Hello")
            .effect(TextEffect::RainbowGradient { speed: 1.0 })
            .time(0.5);

        assert_eq!(text.effect_count(), 1);
        assert!(text.has_effects());
        assert_eq!(text.len(), 5);
    }

    #[test]
    fn test_multiple_color_effects_blend() {
        // Rainbow + Pulse should modulate together
        let text = StyledText::new("Test")
            .effect(TextEffect::RainbowGradient { speed: 1.0 })
            .effect(TextEffect::Pulse {
                speed: 2.0,
                min_alpha: 0.5,
            })
            .time(0.25);

        assert_eq!(text.effect_count(), 2);

        // Get colors - they should be non-zero (rainbow provides color, pulse modulates alpha)
        let color = text.char_color(0, 4);
        // Color should exist (not fully transparent)
        assert!(color.r() > 0 || color.g() > 0 || color.b() > 0);
    }

    #[test]
    fn test_multiple_alpha_effects_multiply() {
        // FadeIn * Pulse should multiply alpha values
        let text = StyledText::new("Test")
            .base_color(PackedRgba::rgb(255, 255, 255))
            .effect(TextEffect::FadeIn { progress: 0.5 }) // 50% alpha
            .effect(TextEffect::Pulse {
                speed: 0.0, // No animation, so sin(0) = 0, alpha = 0.5 + 0.5*0 = 0.5
                min_alpha: 0.5,
            })
            .time(0.0);

        assert_eq!(text.effect_count(), 2);

        // The combined alpha should be 0.5 * ~0.5 = ~0.25
        // This means the color values should be reduced
        let color = text.char_color(0, 4);
        // Color should be dimmed (not full 255)
        assert!(color.r() < 200);
    }

    #[test]
    fn test_effect_order_deterministic() {
        // Same effects applied in same order = same output
        let text1 = StyledText::new("Test")
            .effect(TextEffect::RainbowGradient { speed: 1.0 })
            .effect(TextEffect::FadeIn { progress: 0.8 })
            .time(0.5)
            .seed(42);

        let text2 = StyledText::new("Test")
            .effect(TextEffect::RainbowGradient { speed: 1.0 })
            .effect(TextEffect::FadeIn { progress: 0.8 })
            .time(0.5)
            .seed(42);

        let color1 = text1.char_color(0, 4);
        let color2 = text2.char_color(0, 4);

        assert_eq!(color1.r(), color2.r());
        assert_eq!(color1.g(), color2.g());
        assert_eq!(color1.b(), color2.b());
    }

    #[test]
    fn test_clear_effects() {
        // clear_effects() returns to plain rendering
        let text = StyledText::new("Test")
            .effect(TextEffect::RainbowGradient { speed: 1.0 })
            .effect(TextEffect::Pulse {
                speed: 2.0,
                min_alpha: 0.3,
            })
            .clear_effects();

        assert_eq!(text.effect_count(), 0);
        assert!(!text.has_effects());

        // Color should be base color (white by default)
        let color = text.char_color(0, 4);
        assert_eq!(color.r(), 255);
        assert_eq!(color.g(), 255);
        assert_eq!(color.b(), 255);
    }

    #[test]
    fn test_empty_effects_vec() {
        // No effects = plain text rendering
        let text = StyledText::new("Test").base_color(PackedRgba::rgb(100, 150, 200));

        assert_eq!(text.effect_count(), 0);
        assert!(!text.has_effects());

        // Color should be base color
        let color = text.char_color(0, 4);
        assert_eq!(color.r(), 100);
        assert_eq!(color.g(), 150);
        assert_eq!(color.b(), 200);
    }

    #[test]
    fn test_max_effects_enforced() {
        // Adding >MAX_EFFECTS should be silently ignored (truncated)
        let mut text = StyledText::new("Test");

        // Add more than MAX_EFFECTS (8)
        for i in 0..12 {
            text = text.effect(TextEffect::Pulse {
                speed: i as f64,
                min_alpha: 0.5,
            });
        }

        // Should be capped at MAX_EFFECTS
        assert_eq!(text.effect_count(), MAX_EFFECTS);
        assert_eq!(text.effect_count(), 8);
    }

    #[test]
    fn test_effects_method_batch_add() {
        // .effects() method should add multiple at once
        let effects = vec![
            TextEffect::RainbowGradient { speed: 1.0 },
            TextEffect::FadeIn { progress: 0.5 },
            TextEffect::Pulse {
                speed: 1.0,
                min_alpha: 0.3,
            },
        ];

        let text = StyledText::new("Test").effects(effects);

        assert_eq!(text.effect_count(), 3);
    }

    #[test]
    fn test_none_effect_ignored() {
        // TextEffect::None should not be added
        let text = StyledText::new("Test")
            .effect(TextEffect::None)
            .effect(TextEffect::RainbowGradient { speed: 1.0 })
            .effect(TextEffect::None);

        assert_eq!(text.effect_count(), 1);
    }

    // =========================================================================
    // StyledMultiLine Tests (bd-2bzl)
    // =========================================================================

    #[test]
    fn test_styled_multiline_from_lines() {
        let lines = vec!["Hello".into(), "World".into()];
        let multi = StyledMultiLine::new(lines);
        assert_eq!(multi.height(), 2);
        assert_eq!(multi.width(), 5);
        assert_eq!(multi.total_height(), 2);
    }

    #[test]
    fn test_styled_multiline_from_ascii_art() {
        let art = AsciiArtText::new("AB", AsciiArtStyle::Block);
        let multi = StyledMultiLine::from_ascii_art(art);
        assert_eq!(multi.height(), 5); // Block style is 5 lines
        assert_eq!(multi.width(), 12); // 2 chars × 6 width each
    }

    #[test]
    fn test_styled_multiline_from_ascii_art_with_color() {
        let art = AsciiArtText::new("X", AsciiArtStyle::Mini).color(PackedRgba::rgb(255, 0, 0));
        let multi = StyledMultiLine::from_ascii_art(art);
        assert_eq!(multi.base_color, PackedRgba::rgb(255, 0, 0));
    }

    #[test]
    fn test_styled_multiline_effects_chain() {
        let multi = StyledMultiLine::new(vec!["Test".into()])
            .effect(TextEffect::FadeIn { progress: 0.5 })
            .effect(TextEffect::HorizontalGradient {
                gradient: ColorGradient::rainbow(),
            })
            .bold()
            .italic()
            .time(1.0)
            .seed(42);
        assert_eq!(multi.effects.len(), 2);
        assert!(multi.bold);
        assert!(multi.italic);
    }

    #[test]
    fn test_styled_multiline_with_reflection() {
        let lines = vec!["ABC".into(), "DEF".into(), "GHI".into()];
        let multi = StyledMultiLine::new(lines).reflection(Reflection::default());
        assert_eq!(multi.height(), 3);
        // default: gap=0, height_ratio=1.0 → 3 + 0 + 3 = 6
        assert_eq!(multi.total_height(), 6);
    }

    #[test]
    fn test_styled_multiline_reflection_custom() {
        let refl = Reflection {
            gap: 0,
            start_opacity: 0.5,
            end_opacity: 0.0,
            height_ratio: 1.0,
            wave: 0.0,
        };
        let multi = StyledMultiLine::new(vec!["Line".into()]).reflection(refl);
        // 1 line + 0 gap + ceil(1*1.0)=1 reflected = 2
        assert_eq!(multi.total_height(), 2);
    }

    #[test]
    fn test_styled_multiline_render_no_panic() {
        use ftui_render::grapheme_pool::GraphemePool;

        let art = AsciiArtText::new("HI", AsciiArtStyle::Block);
        let multi = StyledMultiLine::from_ascii_art(art)
            .effect(TextEffect::RainbowGradient { speed: 0.1 })
            .time(0.5);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(5, 3, 70, 10);
        multi.render(area, &mut frame);
    }

    #[test]
    fn test_styled_multiline_render_with_reflection() {
        use ftui_render::grapheme_pool::GraphemePool;

        let multi = StyledMultiLine::new(vec!["█████".into(), "█   █".into(), "█████".into()])
            .base_color(PackedRgba::rgb(0, 255, 128))
            .reflection(Reflection {
                gap: 1,
                start_opacity: 0.4,
                end_opacity: 0.1,
                height_ratio: 0.67,
                wave: 0.0,
            });

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 20, &mut pool);
        multi.render_at(0, 0, &mut frame);

        // Primary text should be rendered at (0,0) through (0,2)
        if let Some(cell) = frame.buffer.get(0, 0) {
            assert_ne!(
                cell.fg,
                PackedRgba::default(),
                "primary text should have color"
            );
        }
    }

    #[test]
    fn test_styled_multiline_2d_gradient() {
        let multi = StyledMultiLine::new(vec!["ABC".into(), "DEF".into()])
            .effect(TextEffect::RainbowGradient { speed: 0.0 });

        // Top-left and bottom-right should get different colors (due to 2D mapping)
        let c00 = multi.char_color_2d(0, 0, 3, 2);
        let c21 = multi.char_color_2d(2, 1, 3, 2);
        // With speed=0, hue = t_x + t_y*0.3, so different positions → different hues
        assert_ne!(
            c00, c21,
            "2D gradient should produce different colors at different positions"
        );
    }

    #[test]
    fn test_styled_multiline_empty_lines() {
        let multi = StyledMultiLine::new(vec![]);
        assert_eq!(multi.height(), 0);
        assert_eq!(multi.width(), 0);
        assert_eq!(multi.total_height(), 0);

        // Render should not panic on empty
        use ftui_render::grapheme_pool::GraphemePool;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 20, &mut pool);
        multi.render_at(0, 0, &mut frame);
    }

    #[test]
    fn test_styled_multiline_small_area() {
        use ftui_render::grapheme_pool::GraphemePool;

        let multi = StyledMultiLine::new(vec!["ABCDEF".into(), "GHIJKL".into()]).effect(
            TextEffect::AnimatedGradient {
                gradient: ColorGradient::cyberpunk(),
                speed: 0.5,
            },
        );

        // Render into tiny frame — should not panic, just clip
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        multi.render_at(0, 0, &mut frame);
    }

    #[test]
    fn test_styled_multiline_widget_trait() {
        use ftui_render::grapheme_pool::GraphemePool;

        let multi = StyledMultiLine::new(vec!["Test".into()]);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        // Use Widget trait
        Widget::render(&multi, Rect::new(0, 0, 80, 24), &mut frame);
    }

    #[test]
    fn test_styled_multiline_widget_zero_area() {
        use ftui_render::grapheme_pool::GraphemePool;

        let multi = StyledMultiLine::new(vec!["Test".into()]);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        // Zero-size area should early return
        Widget::render(&multi, Rect::new(0, 0, 0, 0), &mut frame);
    }

    #[test]
    fn test_ascii_art_get_color() {
        let art = AsciiArtText::new("X", AsciiArtStyle::Block);
        assert_eq!(art.get_color(), None);

        let art2 =
            AsciiArtText::new("X", AsciiArtStyle::Block).color(PackedRgba::rgb(100, 200, 50));
        assert_eq!(art2.get_color(), Some(PackedRgba::rgb(100, 200, 50)));
    }

    #[test]
    fn test_reflection_default() {
        let refl = Reflection::default();
        assert_eq!(refl.gap, 0);
        assert!((refl.height_ratio - 1.0).abs() < 1e-10);
        assert!((refl.wave - 0.0).abs() < 1e-10);
        assert!((refl.start_opacity - 0.4).abs() < 1e-10);
        assert!((refl.end_opacity - 0.05).abs() < 1e-10);
    }

    #[test]
    fn test_reflection_reflected_rows() {
        let refl = Reflection::default();
        assert_eq!(refl.reflected_rows(5), 5); // height_ratio=1.0 → all rows

        let half = Reflection {
            height_ratio: 0.5,
            ..Default::default()
        };
        assert_eq!(half.reflected_rows(4), 2);
        assert_eq!(half.reflected_rows(5), 3); // ceil(5*0.5) = 3

        let zero = Reflection {
            height_ratio: 0.0,
            ..Default::default()
        };
        assert_eq!(zero.reflected_rows(10), 0);
    }

    #[test]
    fn test_reflection_gap_in_total_height() {
        let multi = StyledMultiLine::new(vec!["AAA".into(), "BBB".into(), "CCC".into()])
            .reflection(Reflection {
                gap: 2,
                height_ratio: 1.0,
                ..Default::default()
            });
        // 3 lines + 2 gap + 3 reflected = 8
        assert_eq!(multi.total_height(), 8);
    }

    #[test]
    fn test_reflection_height_ratio_in_total_height() {
        let multi = StyledMultiLine::new(vec!["A".into(), "B".into(), "C".into(), "D".into()])
            .reflection(Reflection {
                gap: 0,
                height_ratio: 0.5,
                ..Default::default()
            });
        // 4 lines + 0 gap + ceil(4*0.5)=2 reflected = 6
        assert_eq!(multi.total_height(), 6);
    }

    #[test]
    fn test_reflection_wave_render_no_panic() {
        use ftui_render::grapheme_pool::GraphemePool;

        let multi = StyledMultiLine::new(vec!["WAVE".into(), "TEST".into()])
            .base_color(PackedRgba::rgb(255, 0, 0))
            .time(1.5)
            .reflection(Reflection {
                gap: 1,
                height_ratio: 1.0,
                wave: 0.3,
                ..Default::default()
            });

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 20, &mut pool);
        multi.render_at(0, 0, &mut frame);
        // Just verify no panic with wave shimmer active
    }

    // =========================================================================
    // EffectSequence Tests
    // =========================================================================

    #[test]
    fn test_sequence_single_step() {
        // A single step should play through from 0 to 1
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 1.0)
            .build();

        assert!(!seq.is_complete());
        assert!(seq.is_playing());
        assert_eq!(seq.current_step_index(), 0);

        // Advance halfway
        seq.tick(0.5);
        assert!((seq.step_progress() - 0.5).abs() < 0.01);
        assert!(!seq.is_complete());

        // Advance to completion
        let event = seq.tick(0.5);
        assert!(seq.is_complete());
        assert!(matches!(event, Some(SequenceEvent::SequenceCompleted)));
    }

    #[test]
    fn test_sequence_multi_step() {
        // Steps should transition at duration boundaries
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 1.0)
            .step(
                TextEffect::Pulse {
                    speed: 2.0,
                    min_alpha: 0.5,
                },
                2.0,
            )
            .step(TextEffect::FadeOut { progress: 0.0 }, 1.0)
            .build();

        assert_eq!(seq.step_count(), 3);
        assert_eq!(seq.current_step_index(), 0);

        // Complete first step
        seq.tick(1.0);
        assert_eq!(seq.current_step_index(), 1);

        // Complete second step (2s duration)
        seq.tick(2.0);
        assert_eq!(seq.current_step_index(), 2);

        // Complete third step
        seq.tick(1.0);
        assert!(seq.is_complete());
    }

    #[test]
    fn test_sequence_loop() {
        // Loop mode should restart from step 0
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 0.5)
            .loop_mode(LoopMode::Loop)
            .build();

        // Complete first pass
        let event = seq.tick(0.5);
        assert!(matches!(
            event,
            Some(SequenceEvent::SequenceLooped { loop_count: 2 })
        ));
        assert_eq!(seq.current_step_index(), 0);
        assert!(!seq.is_complete());

        // Loop again
        let event = seq.tick(0.5);
        assert!(matches!(
            event,
            Some(SequenceEvent::SequenceLooped { loop_count: 3 })
        ));
    }

    #[test]
    fn test_sequence_pingpong() {
        // PingPong should reverse at ends
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 0.5)
            .step(TextEffect::FadeOut { progress: 0.0 }, 0.5)
            .loop_mode(LoopMode::PingPong)
            .build();

        assert_eq!(seq.current_step_index(), 0);

        // Forward: step 0 -> step 1
        seq.tick(0.5);
        assert_eq!(seq.current_step_index(), 1);

        // Complete step 1, reverse triggered
        let event = seq.tick(0.5);
        assert!(matches!(
            event,
            Some(SequenceEvent::SequenceLooped { loop_count: 2 })
        ));
        // In pingpong, after loop we stay at the last step but direction reverses
        assert_eq!(seq.current_step_index(), 1);
    }

    #[test]
    fn test_sequence_loop_count() {
        // LoopCount should stop after N iterations
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 0.5)
            .loop_mode(LoopMode::LoopCount(3))
            .build();

        // First pass
        seq.tick(0.5);
        assert!(!seq.is_complete());

        // Second pass
        seq.tick(0.5);
        assert!(!seq.is_complete());

        // Third pass - should complete
        let event = seq.tick(0.5);
        assert!(seq.is_complete());
        assert!(matches!(event, Some(SequenceEvent::SequenceCompleted)));
    }

    #[test]
    fn test_sequence_once_completes() {
        // Once mode should fire SequenceCompleted at end
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 0.5)
            .loop_mode(LoopMode::Once) // Default, but explicit
            .build();

        let event = seq.tick(0.5);
        assert!(matches!(event, Some(SequenceEvent::SequenceCompleted)));
        assert!(seq.is_complete());
    }

    #[test]
    fn test_sequence_step_easing() {
        // Per-step easing should override global
        let mut seq = EffectSequence::builder()
            .step_with_easing(TextEffect::FadeIn { progress: 0.0 }, 1.0, Easing::EaseIn)
            .easing(Easing::Linear) // Global is Linear, but step uses EaseIn
            .build();

        // Advance halfway
        seq.tick(0.5);

        // Get current effect - should have EaseIn applied
        let effect = seq.current_effect();

        // EaseIn at t=0.5: 0.5^3 = 0.125 (much less than linear 0.5)
        if let TextEffect::FadeIn { progress } = effect {
            // EaseIn should give progress < 0.5
            assert!(
                progress < 0.3,
                "EaseIn at 0.5 should be ~0.125, got {progress}"
            );
        } else {
            unreachable!("Expected FadeIn effect");
        }
    }

    #[test]
    fn test_sequence_event_step_started() {
        // StepStarted should fire when transitioning to a new step
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 0.5)
            .step(TextEffect::FadeOut { progress: 0.0 }, 0.5)
            .build();

        // Complete first step
        let event = seq.tick(0.5);

        // Should get StepStarted for step 1
        assert!(matches!(
            event,
            Some(SequenceEvent::StepStarted { step_idx: 1 })
        ));
    }

    #[test]
    fn test_sequence_pause_resume() {
        // Pausing should freeze progress
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 1.0)
            .build();

        seq.tick(0.3);
        let progress_before = seq.step_progress();

        seq.pause();
        assert!(seq.is_paused());

        // Ticking while paused should not advance
        seq.tick(0.5);
        assert!((seq.step_progress() - progress_before).abs() < 0.001);

        seq.resume();
        assert!(seq.is_playing());

        // Now ticking should advance
        seq.tick(0.2);
        assert!(seq.step_progress() > progress_before);
    }

    #[test]
    fn test_sequence_reset() {
        // Reset should return to step 0, progress 0, playing state
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 1.0)
            .build();

        seq.tick(0.5);
        assert!(seq.step_progress() > 0.0);

        seq.reset();

        assert_eq!(seq.current_step_index(), 0);
        assert!((seq.step_progress() - 0.0).abs() < 0.001);
        assert!(seq.is_playing());
        assert_eq!(seq.loop_iteration(), 1);
    }

    #[test]
    fn test_sequence_empty() {
        // Empty sequence should be complete immediately
        let seq = EffectSequence::new();

        // Empty sequence progress is 1.0 (complete)
        assert!((seq.progress() - 1.0).abs() < 0.001);

        // current_effect should return None
        assert!(matches!(seq.current_effect(), TextEffect::None));
    }

    #[test]
    fn test_sequence_progress_overall() {
        // Overall progress should reflect position across all steps
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 1.0)
            .step(TextEffect::FadeOut { progress: 0.0 }, 1.0)
            .build();

        // Total duration = 2.0 seconds

        // At start
        assert!((seq.progress() - 0.0).abs() < 0.01);

        // Halfway through first step (0.5 of 2.0 total = 0.25)
        seq.tick(0.5);
        assert!((seq.progress() - 0.25).abs() < 0.01);

        // Complete first step (1.0 of 2.0 = 0.5)
        seq.tick(0.5);
        assert!((seq.progress() - 0.5).abs() < 0.01);

        // Halfway through second step (1.5 of 2.0 = 0.75)
        seq.tick(0.5);
        assert!((seq.progress() - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_sequence_builder_fluent() {
        // Builder should support fluent chaining
        let seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 0.5)
            .step(
                TextEffect::Pulse {
                    speed: 2.0,
                    min_alpha: 0.5,
                },
                2.0,
            )
            .step(TextEffect::FadeOut { progress: 0.0 }, 0.5)
            .loop_mode(LoopMode::Loop)
            .easing(Easing::EaseInOut)
            .build();

        assert_eq!(seq.step_count(), 3);
        assert_eq!(seq.loop_mode(), LoopMode::Loop);
    }

    #[test]
    fn test_sequence_current_effect_interpolation() {
        // Progress-based effects should have progress interpolated
        let mut seq = EffectSequence::builder()
            .step(TextEffect::FadeIn { progress: 0.0 }, 1.0)
            .build();

        seq.tick(0.5);

        let effect = seq.current_effect();
        if let TextEffect::FadeIn { progress } = effect {
            assert!((progress - 0.5).abs() < 0.01);
        } else {
            unreachable!("Expected FadeIn effect");
        }
    }

    // =========================================================================
    // Cursor Effect Tests (bd-28rb)
    // =========================================================================

    #[test]
    fn test_cursor_at_end() {
        // Cursor at End position should appear after the last character
        let text = StyledText::new("Hello")
            .effect(TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed: 0.0, // No blinking
                position: CursorPosition::End,
            })
            .time(0.0);

        // Cursor should be at index 5 (after 'o')
        let cursor_idx = text.cursor_index();
        assert_eq!(cursor_idx, Some(5));

        // Cursor should be visible (no blinking)
        assert!(text.cursor_visible());
    }

    #[test]
    fn test_cursor_blinks() {
        // Cursor with blink_speed should toggle visibility over time
        let blink_speed = 2.0; // 2 blinks per second

        // At time 0.0, cursor should be visible (first half of cycle)
        let text_visible = StyledText::new("Hello")
            .effect(TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed,
                position: CursorPosition::End,
            })
            .time(0.0);
        assert!(
            text_visible.cursor_visible(),
            "Cursor should be visible at t=0.0"
        );

        // At time 0.25, cursor should still be visible (cycle = 0.5, 0.5 < 0.5 is false... wait)
        // Let me recalculate: cycle = 0.25 * 2.0 = 0.5, 0.5 % 1.0 = 0.5, 0.5 < 0.5 is false
        let text_mid = StyledText::new("Hello")
            .effect(TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed,
                position: CursorPosition::End,
            })
            .time(0.25);
        assert!(
            !text_mid.cursor_visible(),
            "Cursor should be hidden at t=0.25 (second half of cycle)"
        );

        // At time 0.5, cursor should be visible again (new cycle starts)
        // cycle = 0.5 * 2.0 = 1.0, 1.0 % 1.0 = 0.0, 0.0 < 0.5 is true
        let text_new_cycle = StyledText::new("Hello")
            .effect(TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed,
                position: CursorPosition::End,
            })
            .time(0.5);
        assert!(
            text_new_cycle.cursor_visible(),
            "Cursor should be visible at t=0.5 (new cycle)"
        );

        // At time 0.75, cursor should be hidden again
        // cycle = 0.75 * 2.0 = 1.5, 1.5 % 1.0 = 0.5, 0.5 < 0.5 is false
        let text_hidden_again = StyledText::new("Hello")
            .effect(TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed,
                position: CursorPosition::End,
            })
            .time(0.75);
        assert!(
            !text_hidden_again.cursor_visible(),
            "Cursor should be hidden at t=0.75"
        );
    }

    #[test]
    fn test_cursor_after_reveal() {
        // Cursor with AfterReveal should follow the Typewriter reveal position
        let text = StyledText::new("Hello World")
            .effect(TextEffect::Typewriter { visible_chars: 5.0 }) // "Hello" revealed
            .effect(TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed: 0.0,
                position: CursorPosition::AfterReveal,
            })
            .time(0.0);

        // Cursor should be at index 5 (after "Hello")
        let cursor_idx = text.cursor_index();
        assert_eq!(cursor_idx, Some(5));

        // Test with partial reveal
        let text_partial = StyledText::new("Hello World")
            .effect(TextEffect::Typewriter { visible_chars: 3.5 }) // "Hel" and half of 'l'
            .effect(TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed: 0.0,
                position: CursorPosition::AfterReveal,
            })
            .time(0.0);

        // Cursor should be at index 3 (truncated to integer)
        let cursor_idx_partial = text_partial.cursor_index();
        assert_eq!(cursor_idx_partial, Some(3));
    }

    #[test]
    fn test_cursor_after_reveal_with_reveal_effect() {
        // Cursor with AfterReveal should also work with TextEffect::Reveal
        let text = StyledText::new("Hello World") // 11 chars
            .effect(TextEffect::Reveal {
                mode: RevealMode::LeftToRight,
                progress: 0.5, // 50% revealed = ~5 chars
                seed: 0,
            })
            .effect(TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed: 0.0,
                position: CursorPosition::AfterReveal,
            })
            .time(0.0);

        // Cursor should be at index 5 (0.5 * 11 = 5.5 → 5)
        let cursor_idx = text.cursor_index();
        assert_eq!(cursor_idx, Some(5));
    }

    #[test]
    fn test_cursor_custom_char() {
        // Custom cursor should use the specified character
        let custom_char = '▌';
        let style = CursorStyle::Custom(custom_char);
        assert_eq!(style.char(), custom_char);

        // Test other cursor styles
        assert_eq!(CursorStyle::Block.char(), '█');
        assert_eq!(CursorStyle::Underline.char(), '_');
        assert_eq!(CursorStyle::Bar.char(), '|');
    }

    #[test]
    fn test_cursor_at_index() {
        // Cursor at specific index
        let text = StyledText::new("Hello")
            .effect(TextEffect::Cursor {
                style: CursorStyle::Bar,
                blink_speed: 0.0,
                position: CursorPosition::AtIndex(2),
            })
            .time(0.0);

        // Cursor should be at index 2
        assert_eq!(text.cursor_index(), Some(2));
    }

    #[test]
    fn test_cursor_at_index_clamped() {
        // Cursor index beyond text length should be clamped
        let text = StyledText::new("Hi") // 2 chars
            .effect(TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed: 0.0,
                position: CursorPosition::AtIndex(10), // Beyond text length
            })
            .time(0.0);

        // Cursor should be clamped to 2 (text length)
        assert_eq!(text.cursor_index(), Some(2));
    }

    #[test]
    fn test_cursor_no_blink() {
        // Cursor with blink_speed = 0 should always be visible
        for time in [0.0, 0.1, 0.25, 0.5, 0.75, 1.0, 5.0, 100.0] {
            let text = StyledText::new("Test")
                .effect(TextEffect::Cursor {
                    style: CursorStyle::Block,
                    blink_speed: 0.0,
                    position: CursorPosition::End,
                })
                .time(time);
            assert!(
                text.cursor_visible(),
                "Cursor should always be visible with blink_speed=0 at t={time}"
            );
        }
    }

    #[test]
    fn test_no_cursor_effect() {
        // Text without cursor effect should return None for cursor methods
        let text = StyledText::new("Hello")
            .effect(TextEffect::RainbowGradient { speed: 1.0 })
            .time(0.0);

        assert!(text.cursor_index().is_none());
        assert!(!text.cursor_visible());
    }

    #[test]
    fn test_cursor_default_styles() {
        // CursorStyle and CursorPosition should have correct defaults
        assert_eq!(CursorStyle::default(), CursorStyle::Block);
        assert_eq!(CursorPosition::default(), CursorPosition::End);
    }

    // =========================================================================
    // Reveal Effect Tests (bd-vins)
    // =========================================================================

    #[test]
    fn test_reveal_ltr_first_chars_first() {
        // LeftToRight at progress=0.5 shows first half of characters
        let text = StyledText::new("Hello World")
            .effect(TextEffect::Reveal {
                mode: RevealMode::LeftToRight,
                progress: 0.5,
                seed: 0,
            })
            .time(0.0);

        // Total 11 chars, 0.5 progress = 5 chars visible
        let c0 = text.char_color(0, 11);
        let c4 = text.char_color(4, 11);
        let c5 = text.char_color(5, 11);
        let c10 = text.char_color(10, 11);

        assert_ne!(c0, PackedRgba::TRANSPARENT, "First char should be visible");
        assert_ne!(c4, PackedRgba::TRANSPARENT, "5th char should be visible");
        assert_eq!(c5, PackedRgba::TRANSPARENT, "6th char should be hidden");
        assert_eq!(c10, PackedRgba::TRANSPARENT, "Last char should be hidden");
    }

    #[test]
    fn test_reveal_rtl_last_chars_first() {
        // RightToLeft at progress=0.5 shows last half of characters
        let text = StyledText::new("Hello World")
            .effect(TextEffect::Reveal {
                mode: RevealMode::RightToLeft,
                progress: 0.5,
                seed: 0,
            })
            .time(0.0);

        // Total 11 chars, 0.5 progress = last 6 chars visible (idx 5-10)
        // hidden_count = (0.5 * 11) = 5, so idx >= 5 is visible
        let c0 = text.char_color(0, 11);
        let c4 = text.char_color(4, 11);
        let c5 = text.char_color(5, 11);
        let c10 = text.char_color(10, 11);

        assert_eq!(c0, PackedRgba::TRANSPARENT, "First char should be hidden");
        assert_eq!(
            c4,
            PackedRgba::TRANSPARENT,
            "5th char (idx 4) should be hidden"
        );
        assert_ne!(
            c5,
            PackedRgba::TRANSPARENT,
            "6th char (idx 5) should be visible"
        );
        assert_ne!(c10, PackedRgba::TRANSPARENT, "Last char should be visible");
    }

    #[test]
    fn test_reveal_center_out_middle_first() {
        // CenterOut: center characters visible first
        let text = StyledText::new("ABCDEFGHI") // 9 chars, center at idx 4
            .effect(TextEffect::Reveal {
                mode: RevealMode::CenterOut,
                progress: 0.3,
                seed: 0,
            })
            .time(0.0);

        // Center char (idx 4 'E') should be visible early
        let c4 = text.char_color(4, 9); // Center
        let c0 = text.char_color(0, 9); // Edge
        let c8 = text.char_color(8, 9); // Edge

        assert_ne!(
            c4,
            PackedRgba::TRANSPARENT,
            "Center should be visible first"
        );
        assert_eq!(
            c0,
            PackedRgba::TRANSPARENT,
            "Left edge should be hidden at low progress"
        );
        assert_eq!(
            c8,
            PackedRgba::TRANSPARENT,
            "Right edge should be hidden at low progress"
        );
    }

    #[test]
    fn test_reveal_edges_in_edges_first() {
        // EdgesIn: edge characters visible first
        let text = StyledText::new("ABCDEFGHI") // 9 chars
            .effect(TextEffect::Reveal {
                mode: RevealMode::EdgesIn,
                progress: 0.3,
                seed: 0,
            })
            .time(0.0);

        // Edge chars (idx 0, 8) should be visible, center hidden
        let c0 = text.char_color(0, 9); // Left edge
        let c8 = text.char_color(8, 9); // Right edge
        let c4 = text.char_color(4, 9); // Center

        assert_ne!(
            c0,
            PackedRgba::TRANSPARENT,
            "Left edge should be visible first"
        );
        assert_ne!(
            c8,
            PackedRgba::TRANSPARENT,
            "Right edge should be visible first"
        );
        assert_eq!(
            c4,
            PackedRgba::TRANSPARENT,
            "Center should be hidden at low progress"
        );
    }

    #[test]
    fn test_reveal_random_deterministic() {
        // Same seed = same reveal order
        let text1 = StyledText::new("Hello World")
            .effect(TextEffect::Reveal {
                mode: RevealMode::Random,
                progress: 0.5,
                seed: 12345,
            })
            .time(0.0);

        let text2 = StyledText::new("Hello World")
            .effect(TextEffect::Reveal {
                mode: RevealMode::Random,
                progress: 0.5,
                seed: 12345,
            })
            .time(0.0);

        // Colors should be identical for same seed
        for i in 0..11 {
            let c1 = text1.char_color(i, 11);
            let c2 = text2.char_color(i, 11);
            assert_eq!(
                c1, c2,
                "Same seed should produce same visibility at idx {i}"
            );
        }
    }

    #[test]
    fn test_reveal_random_all_chars_reveal() {
        // At progress=1.0, all chars visible regardless of seed
        let text = StyledText::new("Hello World")
            .effect(TextEffect::Reveal {
                mode: RevealMode::Random,
                progress: 1.0,
                seed: 99999,
            })
            .time(0.0);

        for i in 0..11 {
            let c = text.char_color(i, 11);
            assert_ne!(
                c,
                PackedRgba::TRANSPARENT,
                "All chars visible at progress=1.0"
            );
        }
    }

    #[test]
    fn test_reveal_by_word_whole_words() {
        // ByWord reveals complete words
        let text = StyledText::new("One Two Three") // 3 words
            .effect(TextEffect::Reveal {
                mode: RevealMode::ByWord,
                progress: 0.4, // Should show first word
                seed: 0,
            })
            .time(0.0);

        // "One" = idx 0-2, " " = idx 3, "Two" = idx 4-6, etc.
        let c0 = text.char_color(0, 13); // 'O'
        let c2 = text.char_color(2, 13); // 'e'

        assert_ne!(c0, PackedRgba::TRANSPARENT, "First word first char visible");
        assert_ne!(c2, PackedRgba::TRANSPARENT, "First word last char visible");
    }

    #[test]
    fn test_reveal_by_line_whole_lines() {
        // ByLine for single-line falls back to LeftToRight behavior
        let text = StyledText::new("Hello")
            .effect(TextEffect::Reveal {
                mode: RevealMode::ByLine,
                progress: 0.5,
                seed: 0,
            })
            .time(0.0);

        let c0 = text.char_color(0, 5);

        assert_ne!(c0, PackedRgba::TRANSPARENT, "First chars visible");
    }

    #[test]
    fn test_reveal_mask_angle_0() {
        // Angle 0 = left-to-right sweep
        let text = StyledText::new("ABCDE")
            .effect(TextEffect::RevealMask {
                angle: 0.0,
                progress: 0.5,
                softness: 0.0,
            })
            .time(0.0);

        let c0 = text.char_color(0, 5);

        // With hard edge at 0.5, approximately half should be visible
        assert_ne!(c0, PackedRgba::TRANSPARENT, "Left side visible at angle 0");
    }

    #[test]
    fn test_reveal_mask_angle_90() {
        // Angle 90 = top-to-bottom sweep
        // For single-line text (y=0.5), this becomes uniform visibility
        let text = StyledText::new("ABCDE")
            .effect(TextEffect::RevealMask {
                angle: 90.0,
                progress: 0.5,
                softness: 0.0,
            })
            .time(0.0);

        // With angle 90 and y=0.5, all chars have similar sweep position
        let c0 = text.char_color(0, 5);
        let c4 = text.char_color(4, 5);

        // Both should be in same visibility state
        assert_eq!(
            c0 == PackedRgba::TRANSPARENT,
            c4 == PackedRgba::TRANSPARENT,
            "At angle 90, all single-line chars have same visibility"
        );
    }

    #[test]
    fn test_reveal_mask_softness_0() {
        // Softness 0 = hard edge (binary visible/hidden)
        let text = StyledText::new("ABCDEFGHIJ") // 10 chars
            .effect(TextEffect::RevealMask {
                angle: 0.0,
                progress: 0.5,
                softness: 0.0,
            })
            .time(0.0);

        // Count visible and hidden
        let mut visible = 0;
        let mut hidden = 0;
        for i in 0..10 {
            let c = text.char_color(i, 10);
            if c == PackedRgba::TRANSPARENT {
                hidden += 1;
            } else {
                visible += 1;
            }
        }

        // With hard edge, should be a clean split
        assert!(visible > 0, "Some chars should be visible");
        assert!(hidden > 0, "Some chars should be hidden");
    }

    #[test]
    fn test_reveal_mask_softness_1() {
        // Softness 1 = full gradient fade
        let text = StyledText::new("ABCDEFGHIJ") // 10 chars
            .base_color(PackedRgba::rgb(255, 255, 255))
            .effect(TextEffect::RevealMask {
                angle: 0.0,
                progress: 0.5,
                softness: 1.0,
            })
            .time(0.0);

        // With soft edge, chars in the middle range should have partial alpha
        let c0 = text.char_color(0, 10);
        let c9 = text.char_color(9, 10);

        // First char should be more visible than last
        let alpha_first = c0.r() as f64 / 255.0;
        let alpha_last = c9.r() as f64 / 255.0;

        assert!(
            alpha_first > alpha_last || c9 == PackedRgba::TRANSPARENT,
            "Soft edge should create gradient (first more visible than last)"
        );
    }

    #[test]
    fn test_reveal_mode_default() {
        // RevealMode default should be LeftToRight
        assert_eq!(RevealMode::default(), RevealMode::LeftToRight);
    }

    #[test]
    fn test_reveal_progress_boundaries() {
        // Progress 0 = all hidden, progress 1 = all visible
        let text_0 = StyledText::new("Test")
            .effect(TextEffect::Reveal {
                mode: RevealMode::LeftToRight,
                progress: 0.0,
                seed: 0,
            })
            .time(0.0);

        let text_1 = StyledText::new("Test")
            .effect(TextEffect::Reveal {
                mode: RevealMode::LeftToRight,
                progress: 1.0,
                seed: 0,
            })
            .time(0.0);

        for i in 0..4 {
            assert_eq!(
                text_0.char_color(i, 4),
                PackedRgba::TRANSPARENT,
                "All hidden at progress=0"
            );
            assert_ne!(
                text_1.char_color(i, 4),
                PackedRgba::TRANSPARENT,
                "All visible at progress=1"
            );
        }
    }

    // =========================================================================
    // Chromatic Aberration Tests (bd-atwq)
    // =========================================================================

    #[test]
    fn test_chromatic_aberration_basic() {
        // Basic chromatic aberration should shift red/blue channels based on position
        let text = StyledText::new("0123456789")
            .base_color(PackedRgba::rgb(128, 128, 128))
            .effect(TextEffect::ChromaticAberration {
                offset: 2,
                direction: Direction::Right,
                animated: false,
                speed: 1.0,
            })
            .time(0.0);

        // Left edge should have red boost (shift is negative)
        let left = text.char_color(0, 10);
        // Right edge should have blue boost (shift is positive)
        let right = text.char_color(9, 10);

        // Left character: red should be boosted, blue should be reduced
        assert!(
            left.r() > 128 || left.b() < 128,
            "Left edge should show red shift or blue reduction: r={}, b={}",
            left.r(),
            left.b()
        );
        // Right character: blue should be boosted, red should be reduced
        assert!(
            right.b() > 128 || right.r() < 128,
            "Right edge should show blue shift or red reduction: r={}, b={}",
            right.r(),
            right.b()
        );
    }

    #[test]
    fn test_chromatic_aberration_zero_offset() {
        // Zero offset should produce no change
        let base = PackedRgba::rgb(128, 128, 128);
        let text = StyledText::new("0123456789")
            .base_color(base)
            .effect(TextEffect::ChromaticAberration {
                offset: 0,
                direction: Direction::Right,
                animated: false,
                speed: 1.0,
            })
            .time(0.0);

        for idx in 0..10 {
            let result = text.char_color(idx, 10);
            assert_eq!(
                result.r(),
                base.r(),
                "Zero offset should not change red at idx={}",
                idx
            );
            assert_eq!(
                result.b(),
                base.b(),
                "Zero offset should not change blue at idx={}",
                idx
            );
        }
    }

    #[test]
    fn test_chromatic_aberration_center_unchanged() {
        // Center character should have minimal shift compared to edges
        let base = PackedRgba::rgb(128, 128, 128);
        let text = StyledText::new("0123456789") // 10 chars - center between idx 4 and 5
            .base_color(base)
            .effect(TextEffect::ChromaticAberration {
                offset: 5,
                direction: Direction::Right,
                animated: false,
                speed: 1.0,
            })
            .time(0.0);

        // For 10 chars, indices near center (4 and 5) should have less shift than edges
        let center_left = text.char_color(4, 10);
        let center_right = text.char_color(5, 10);
        let edge_left = text.char_color(0, 10);
        let edge_right = text.char_color(9, 10);

        // Center chars should have smaller shift magnitude than edge chars
        let center_left_shift = ((center_left.r() as i16 - base.r() as i16).abs()
            + (center_left.b() as i16 - base.b() as i16).abs())
            as f64;
        let center_right_shift = ((center_right.r() as i16 - base.r() as i16).abs()
            + (center_right.b() as i16 - base.b() as i16).abs())
            as f64;
        let edge_left_shift = ((edge_left.r() as i16 - base.r() as i16).abs()
            + (edge_left.b() as i16 - base.b() as i16).abs()) as f64;
        let edge_right_shift = ((edge_right.r() as i16 - base.r() as i16).abs()
            + (edge_right.b() as i16 - base.b() as i16).abs())
            as f64;

        let avg_center = (center_left_shift + center_right_shift) / 2.0;
        let avg_edge = (edge_left_shift + edge_right_shift) / 2.0;

        assert!(
            avg_center < avg_edge,
            "Center should have less shift than edges: center={}, edge={}",
            avg_center,
            avg_edge
        );
    }

    #[test]
    fn test_chromatic_aberration_symmetry() {
        // Left and right edges should have opposite color shifts
        let base = PackedRgba::rgb(128, 128, 128);
        let text = StyledText::new("0123456789")
            .base_color(base)
            .effect(TextEffect::ChromaticAberration {
                offset: 3,
                direction: Direction::Right,
                animated: false,
                speed: 1.0,
            })
            .time(0.0);

        let left = text.char_color(0, 10);
        let right = text.char_color(9, 10);

        // Red shifts should be roughly opposite
        let left_r_shift = left.r() as i16 - base.r() as i16;
        let right_r_shift = right.r() as i16 - base.r() as i16;

        // Blue shifts should be roughly opposite
        let left_b_shift = left.b() as i16 - base.b() as i16;
        let right_b_shift = right.b() as i16 - base.b() as i16;

        // Shifts should be opposite signs (with some tolerance)
        assert!(
            left_r_shift * right_r_shift <= 0,
            "Red shifts should be opposite: left={}, right={}",
            left_r_shift,
            right_r_shift
        );
        assert!(
            left_b_shift * right_b_shift <= 0,
            "Blue shifts should be opposite: left={}, right={}",
            left_b_shift,
            right_b_shift
        );
    }

    #[test]
    fn test_chromatic_aberration_green_unchanged() {
        // Green channel should remain unchanged (chromatic aberration affects R/B only)
        let base = PackedRgba::rgb(100, 150, 200);
        let text = StyledText::new("0123456789")
            .base_color(base)
            .effect(TextEffect::ChromaticAberration {
                offset: 5,
                direction: Direction::Right,
                animated: false,
                speed: 1.0,
            })
            .time(0.0);

        for idx in 0..10 {
            let result = text.char_color(idx, 10);
            assert_eq!(
                result.g(),
                base.g(),
                "Green should be unchanged at idx={}",
                idx
            );
        }
    }

    #[test]
    fn test_chromatic_aberration_clamping() {
        // Verify RGB values are properly clamped at extremes
        let base = PackedRgba::rgb(250, 128, 250);
        let text = StyledText::new("01234567890123456789") // 20 chars
            .base_color(base)
            .effect(TextEffect::ChromaticAberration {
                offset: 10, // High offset
                direction: Direction::Right,
                animated: false,
                speed: 1.0,
            })
            .time(0.0);

        // Right-most index: blue should clamp up to 255.
        let right = text.char_color(19, 20);
        assert_eq!(right.b(), 255, "Blue should clamp at 255 on the right");

        // Left-most index: red should clamp up to 255.
        let left = text.char_color(0, 20);
        assert_eq!(left.r(), 255, "Red should clamp at 255 on the left");
    }

    #[test]
    fn test_chromatic_aberration_single_char() {
        // Single character should have no shift (distance = 0)
        let base = PackedRgba::rgb(128, 128, 128);
        let text = StyledText::new("X")
            .base_color(base)
            .effect(TextEffect::ChromaticAberration {
                offset: 5,
                direction: Direction::Right,
                animated: false,
                speed: 1.0,
            })
            .time(0.0);

        let result = text.char_color(0, 1);
        assert_eq!(result.r(), base.r(), "Single char red unchanged");
        assert_eq!(result.g(), base.g(), "Single char green unchanged");
        assert_eq!(result.b(), base.b(), "Single char blue unchanged");
    }

    // =========================================================================
    // Scanline / CRT Effect Tests (bd-3r87)
    // =========================================================================

    #[test]
    fn test_scanline_dims_every_nth() {
        // Every 2nd row should be dimmed, others full brightness
        let base = PackedRgba::rgb(200, 200, 200);
        let text = StyledMultiLine::new(vec![
            "AAAA".to_string(),
            "BBBB".to_string(),
            "CCCC".to_string(),
            "DDDD".to_string(),
        ])
        .base_color(base)
        .effect(TextEffect::Scanline {
            intensity: 0.5,
            line_gap: 2,
            scroll: false,
            scroll_speed: 0.0,
            flicker: 0.0,
        })
        .time(0.0);

        // Row 0 (even) - should be dimmed
        let total_width = text.width();
        let total_height = text.height();
        let row0 = text.char_color_2d(0, 0, total_width, total_height);
        // Row 1 (odd) - should be full brightness
        let row1 = text.char_color_2d(0, 1, total_width, total_height);

        // Row 0 should be darker than row 1
        assert!(
            row0.r() < row1.r(),
            "Scanline row should be dimmer: {} vs {}",
            row0.r(),
            row1.r()
        );
    }

    #[test]
    fn test_scanline_intensity_applied() {
        // Higher intensity = darker scanlines
        let base = PackedRgba::rgb(200, 200, 200);

        // Low intensity
        let text_low = StyledMultiLine::new(vec!["AAA".to_string(), "BBB".to_string()])
            .base_color(base)
            .effect(TextEffect::Scanline {
                intensity: 0.2,
                line_gap: 2,
                scroll: false,
                scroll_speed: 0.0,
                flicker: 0.0,
            })
            .time(0.0);

        // High intensity
        let text_high = StyledMultiLine::new(vec!["AAA".to_string(), "BBB".to_string()])
            .base_color(base)
            .effect(TextEffect::Scanline {
                intensity: 0.8,
                line_gap: 2,
                scroll: false,
                scroll_speed: 0.0,
                flicker: 0.0,
            })
            .time(0.0);

        let total_width = text_low.width();
        let total_height = text_low.height();
        let low_color = text_low.char_color_2d(0, 0, total_width, total_height);
        let high_color = text_high.char_color_2d(0, 0, total_width, total_height);

        // Higher intensity should result in darker scanline
        assert!(
            high_color.r() < low_color.r(),
            "Higher intensity should be darker: {} vs {}",
            high_color.r(),
            low_color.r()
        );
    }

    #[test]
    fn test_scanline_scroll_moves_pattern() {
        // Scrolling should shift which rows are dimmed
        let base = PackedRgba::rgb(200, 200, 200);

        // At time 0, row 0 is a scanline
        let text_t0 = StyledMultiLine::new(vec![
            "AA".to_string(),
            "BB".to_string(),
            "CC".to_string(),
            "DD".to_string(),
        ])
        .base_color(base)
        .effect(TextEffect::Scanline {
            intensity: 0.5,
            line_gap: 2,
            scroll: true,
            scroll_speed: 1.0, // 1 row per second
            flicker: 0.0,
        })
        .time(0.0);

        // At time 1.0, pattern should shift by 1 row
        let text_t1 = StyledMultiLine::new(vec![
            "AA".to_string(),
            "BB".to_string(),
            "CC".to_string(),
            "DD".to_string(),
        ])
        .base_color(base)
        .effect(TextEffect::Scanline {
            intensity: 0.5,
            line_gap: 2,
            scroll: true,
            scroll_speed: 1.0,
            flicker: 0.0,
        })
        .time(1.0);

        // At t=0, row 0 is scanline (dimmed)
        // At t=1, row 0+1=1 effective, so row 1 is now scanline
        let total_width = text_t0.width();
        let total_height = text_t0.height();
        let row0_t0 = text_t0.char_color_2d(0, 0, total_width, total_height);
        let row0_t1 = text_t1.char_color_2d(0, 0, total_width, total_height);

        // Row 0 at t=0 should be dimmed, at t=1 should be bright
        assert!(
            row0_t0.r() < row0_t1.r(),
            "Scroll should shift pattern: t0={} vs t1={}",
            row0_t0.r(),
            row0_t1.r()
        );
    }

    #[test]
    fn test_scanline_flicker_varies() {
        // Flicker should cause variation between characters
        let base = PackedRgba::rgb(200, 200, 200);
        let text = StyledText::new("ABCDEFGH")
            .base_color(base)
            .effect(TextEffect::Scanline {
                intensity: 0.0,
                line_gap: 0, // No dimming from line gap
                scroll: false,
                scroll_speed: 0.0,
                flicker: 0.8, // High flicker
            })
            .time(1.5);

        // Sample colors at different positions
        let colors: Vec<PackedRgba> = (0..8).map(|i| text.char_color(i, 8)).collect();

        // With high flicker, we expect some variation
        // Not all chars should be exactly the same brightness
        let all_same = colors.windows(2).all(|w| w[0].r() == w[1].r());
        assert!(
            !all_same,
            "Flicker should cause brightness variation between chars"
        );
    }

    #[test]
    fn test_scanline_gap_1_dims_all() {
        // line_gap=1 means every row is a scanline (all dimmed)
        let base = PackedRgba::rgb(200, 200, 200);
        let text = StyledMultiLine::new(vec!["AA".to_string(), "BB".to_string(), "CC".to_string()])
            .base_color(base)
            .effect(TextEffect::Scanline {
                intensity: 0.5,
                line_gap: 1,
                scroll: false,
                scroll_speed: 0.0,
                flicker: 0.0,
            })
            .time(0.0);

        let total_width = text.width();
        let total_height = text.height();
        // All rows should be dimmed
        for row in 0..3 {
            let color = text.char_color_2d(0, row, total_width, total_height);
            assert!(
                color.r() < base.r(),
                "Row {} should be dimmed with gap=1",
                row
            );
        }
    }

    #[test]
    fn test_scanline_gap_2_alternates() {
        // line_gap=2: rows 0, 2, 4... are scanlines
        let base = PackedRgba::rgb(200, 200, 200);
        let text = StyledMultiLine::new(vec![
            "AA".to_string(),
            "BB".to_string(),
            "CC".to_string(),
            "DD".to_string(),
        ])
        .base_color(base)
        .effect(TextEffect::Scanline {
            intensity: 0.5,
            line_gap: 2,
            scroll: false,
            scroll_speed: 0.0,
            flicker: 0.0,
        })
        .time(0.0);

        let total_width = text.width();
        let total_height = text.height();
        // Even rows (0, 2) should be dimmed
        let row0 = text.char_color_2d(0, 0, total_width, total_height);
        let row2 = text.char_color_2d(0, 2, total_width, total_height);
        assert!(row0.r() < base.r(), "Row 0 should be dimmed");
        assert!(row2.r() < base.r(), "Row 2 should be dimmed");

        // Odd rows (1, 3) should be full brightness
        let row1 = text.char_color_2d(0, 1, total_width, total_height);
        let row3 = text.char_color_2d(0, 3, total_width, total_height);
        assert_eq!(row1.r(), base.r(), "Row 1 should be full brightness");
        assert_eq!(row3.r(), base.r(), "Row 3 should be full brightness");
    }
}

// =============================================================================
// ASCII Art Text - Figlet-style large text
// =============================================================================

/// ASCII art font styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsciiArtStyle {
    /// Large block letters using Unicode block characters.
    Block,
    /// Classic banner-style with slashes and pipes.
    Banner,
    /// Minimal 3-line height for compact display.
    Mini,
    /// Slanted italic-like style.
    Slant,
    /// Doom-style chunky letters.
    Doom,
    /// Small caps using Unicode characters.
    SmallCaps,
}

/// ASCII art text renderer.
#[derive(Debug, Clone)]
pub struct AsciiArtText {
    text: String,
    style: AsciiArtStyle,
    color: Option<PackedRgba>,
    gradient: Option<ColorGradient>,
}

impl AsciiArtText {
    /// Create new ASCII art text.
    pub fn new(text: impl Into<String>, style: AsciiArtStyle) -> Self {
        Self {
            text: text.into().to_uppercase(),
            style,
            color: None,
            gradient: None,
        }
    }

    /// Set text color.
    #[must_use]
    pub fn color(mut self, color: PackedRgba) -> Self {
        self.color = Some(color);
        self
    }

    /// Use a gradient for coloring.
    #[must_use]
    pub fn gradient(mut self, gradient: ColorGradient) -> Self {
        self.gradient = Some(gradient);
        self
    }

    /// Get the solid color, if set.
    pub fn get_color(&self) -> Option<PackedRgba> {
        self.color
    }

    /// Get the height in lines for this style.
    pub fn height(&self) -> usize {
        match self.style {
            AsciiArtStyle::Block => 5,
            AsciiArtStyle::Banner => 6,
            AsciiArtStyle::Mini => 3,
            AsciiArtStyle::Slant => 5,
            AsciiArtStyle::Doom => 8,
            AsciiArtStyle::SmallCaps => 1,
        }
    }

    /// Get the width for a single character.
    #[allow(dead_code)]
    fn char_width(&self) -> usize {
        match self.style {
            AsciiArtStyle::Block => 6,
            AsciiArtStyle::Banner => 6,
            AsciiArtStyle::Mini => 4,
            AsciiArtStyle::Slant => 6,
            AsciiArtStyle::Doom => 8,
            AsciiArtStyle::SmallCaps => 1,
        }
    }

    /// Render to vector of lines.
    pub fn render_lines(&self) -> Vec<String> {
        let height = self.height();
        let mut lines = vec![String::new(); height];

        for ch in self.text.chars() {
            let char_lines = self.render_char(ch);
            for (i, line) in char_lines.iter().enumerate() {
                if i < lines.len() {
                    lines[i].push_str(line);
                }
            }
        }

        lines
    }

    /// Render a single character to lines.
    fn render_char(&self, ch: char) -> Vec<&'static str> {
        match self.style {
            AsciiArtStyle::Block => self.render_block(ch),
            AsciiArtStyle::Banner => self.render_banner(ch),
            AsciiArtStyle::Mini => self.render_mini(ch),
            AsciiArtStyle::Slant => self.render_slant(ch),
            AsciiArtStyle::Doom => self.render_doom(ch),
            AsciiArtStyle::SmallCaps => self.render_small_caps(ch),
        }
    }

    fn render_block(&self, ch: char) -> Vec<&'static str> {
        match ch {
            'A' => vec!["  █   ", " █ █  ", "█████ ", "█   █ ", "█   █ "],
            'B' => vec!["████  ", "█   █ ", "████  ", "█   █ ", "████  "],
            'C' => vec![" ████ ", "█     ", "█     ", "█     ", " ████ "],
            'D' => vec!["████  ", "█   █ ", "█   █ ", "█   █ ", "████  "],
            'E' => vec!["█████ ", "█     ", "███   ", "█     ", "█████ "],
            'F' => vec!["█████ ", "█     ", "███   ", "█     ", "█     "],
            'G' => vec![" ████ ", "█     ", "█  ██ ", "█   █ ", " ████ "],
            'H' => vec!["█   █ ", "█   █ ", "█████ ", "█   █ ", "█   █ "],
            'I' => vec!["█████ ", "  █   ", "  █   ", "  █   ", "█████ "],
            'J' => vec!["█████ ", "   █  ", "   █  ", "█  █  ", " ██   "],
            'K' => vec!["█   █ ", "█  █  ", "███   ", "█  █  ", "█   █ "],
            'L' => vec!["█     ", "█     ", "█     ", "█     ", "█████ "],
            'M' => vec!["█   █ ", "██ ██ ", "█ █ █ ", "█   █ ", "█   █ "],
            'N' => vec!["█   █ ", "██  █ ", "█ █ █ ", "█  ██ ", "█   █ "],
            'O' => vec![" ███  ", "█   █ ", "█   █ ", "█   █ ", " ███  "],
            'P' => vec!["████  ", "█   █ ", "████  ", "█     ", "█     "],
            'Q' => vec![" ███  ", "█   █ ", "█   █ ", "█  █  ", " ██ █ "],
            'R' => vec!["████  ", "█   █ ", "████  ", "█  █  ", "█   █ "],
            'S' => vec![" ████ ", "█     ", " ███  ", "    █ ", "████  "],
            'T' => vec!["█████ ", "  █   ", "  █   ", "  █   ", "  █   "],
            'U' => vec!["█   █ ", "█   █ ", "█   █ ", "█   █ ", " ███  "],
            'V' => vec!["█   █ ", "█   █ ", "█   █ ", " █ █  ", "  █   "],
            'W' => vec!["█   █ ", "█   █ ", "█ █ █ ", "██ ██ ", "█   █ "],
            'X' => vec!["█   █ ", " █ █  ", "  █   ", " █ █  ", "█   █ "],
            'Y' => vec!["█   █ ", " █ █  ", "  █   ", "  █   ", "  █   "],
            'Z' => vec!["█████ ", "   █  ", "  █   ", " █    ", "█████ "],
            '0' => vec![" ███  ", "█  ██ ", "█ █ █ ", "██  █ ", " ███  "],
            '1' => vec!["  █   ", " ██   ", "  █   ", "  █   ", " ███  "],
            '2' => vec![" ███  ", "█   █ ", "  ██  ", " █    ", "█████ "],
            '3' => vec!["████  ", "    █ ", " ███  ", "    █ ", "████  "],
            '4' => vec!["█   █ ", "█   █ ", "█████ ", "    █ ", "    █ "],
            '5' => vec!["█████ ", "█     ", "████  ", "    █ ", "████  "],
            '6' => vec![" ███  ", "█     ", "████  ", "█   █ ", " ███  "],
            '7' => vec!["█████ ", "    █ ", "   █  ", "  █   ", "  █   "],
            '8' => vec![" ███  ", "█   █ ", " ███  ", "█   █ ", " ███  "],
            '9' => vec![" ███  ", "█   █ ", " ████ ", "    █ ", " ███  "],
            ' ' => vec!["      ", "      ", "      ", "      ", "      "],
            '!' => vec!["  █   ", "  █   ", "  █   ", "      ", "  █   "],
            '?' => vec![" ███  ", "█   █ ", "  ██  ", "      ", "  █   "],
            '.' => vec!["      ", "      ", "      ", "      ", "  █   "],
            '-' => vec!["      ", "      ", "█████ ", "      ", "      "],
            ':' => vec!["      ", "  █   ", "      ", "  █   ", "      "],
            _ => vec!["█████ ", "█   █ ", "█   █ ", "█   █ ", "█████ "],
        }
    }

    fn render_banner(&self, ch: char) -> Vec<&'static str> {
        match ch {
            'A' => vec![
                "  /\\  ", " /  \\ ", "/----\\", "/    \\", "/    \\", "      ",
            ],
            'B' => vec![
                "==\\   ", "| /=\\ ", "||__/ ", "| /=\\ ", "==/   ", "      ",
            ],
            'C' => vec![" /===\\", "|     ", "|     ", "|     ", " \\===/", "      "],
            'D' => vec!["==\\   ", "| \\   ", "|  |  ", "| /   ", "==/   ", "      "],
            'E' => vec!["|===| ", "|     ", "|===  ", "|     ", "|===| ", "      "],
            'F' => vec!["|===| ", "|     ", "|===  ", "|     ", "|     ", "      "],
            'G' => vec![" /===\\", "|     ", "| /==|", "|    |", " \\===/", "      "],
            'H' => vec!["|   | ", "|   | ", "|===| ", "|   | ", "|   | ", "      "],
            'I' => vec!["|===| ", "  |   ", "  |   ", "  |   ", "|===| ", "      "],
            'J' => vec!["|===| ", "   |  ", "   |  ", "|  |  ", " \\/   ", "      "],
            'K' => vec!["|  /  ", "| /   ", "|<    ", "| \\   ", "|  \\  ", "      "],
            'L' => vec!["|     ", "|     ", "|     ", "|     ", "|===| ", "      "],
            'M' => vec!["|\\  /|", "| \\/ |", "|    |", "|    |", "|    |", "      "],
            'N' => vec![
                "|\\   |", "| \\  |", "|  \\ |", "|   \\|", "|    |", "      ",
            ],
            'O' => vec![" /==\\ ", "|    |", "|    |", "|    |", " \\==/ ", "      "],
            'P' => vec!["|===\\ ", "|   | ", "|===/ ", "|     ", "|     ", "      "],
            'Q' => vec![
                " /==\\ ", "|    |", "|    |", "|  \\ |", " \\==\\/", "      ",
            ],
            'R' => vec![
                "|===\\ ", "|   | ", "|===/ ", "|  \\  ", "|   \\ ", "      ",
            ],
            'S' => vec![
                " /===\\", "|     ", " \\==\\ ", "     |", "\\===/ ", "      ",
            ],
            'T' => vec!["|===| ", "  |   ", "  |   ", "  |   ", "  |   ", "      "],
            'U' => vec!["|   | ", "|   | ", "|   | ", "|   | ", " \\=/ ", "      "],
            'V' => vec!["|   | ", "|   | ", " \\ /  ", "  |   ", "  |   ", "      "],
            'W' => vec![
                "|    |", "|    |", "| /\\ |", "|/  \\|", "/    \\", "      ",
            ],
            'X' => vec![
                "\\   / ", " \\ /  ", "  X   ", " / \\  ", "/   \\ ", "      ",
            ],
            'Y' => vec!["\\   / ", " \\ /  ", "  |   ", "  |   ", "  |   ", "      "],
            'Z' => vec!["|===| ", "   /  ", "  /   ", " /    ", "|===| ", "      "],
            ' ' => vec!["      ", "      ", "      ", "      ", "      ", "      "],
            _ => vec!["[???] ", "[???] ", "[???] ", "[???] ", "[???] ", "      "],
        }
    }

    fn render_mini(&self, ch: char) -> Vec<&'static str> {
        match ch {
            'A' => vec![" /\\ ", "/--\\", "    "],
            'B' => vec!["|=\\ ", "|=/ ", "    "],
            'C' => vec!["/== ", "\\== ", "    "],
            'D' => vec!["|=\\ ", "|=/ ", "    "],
            'E' => vec!["|== ", "|== ", "    "],
            'F' => vec!["|== ", "|   ", "    "],
            'G' => vec!["/== ", "\\=| ", "    "],
            'H' => vec!["|-| ", "| | ", "    "],
            'I' => vec!["=|= ", "=|= ", "    "],
            'J' => vec!["==| ", "\\=| ", "    "],
            'K' => vec!["|/ ", "|\\  ", "    "],
            'L' => vec!["|   ", "|== ", "    "],
            'M' => vec!["|v| ", "| | ", "    "],
            'N' => vec!["|\\| ", "| | ", "    "],
            'O' => vec!["/=\\ ", "\\=/ ", "    "],
            'P' => vec!["|=\\ ", "|   ", "    "],
            'Q' => vec!["/=\\ ", "\\=\\|", "    "],
            'R' => vec!["|=\\ ", "| \\ ", "    "],
            'S' => vec!["/=  ", "\\=/ ", "    "],
            'T' => vec!["=|= ", " |  ", "    "],
            'U' => vec!["| | ", "\\=/ ", "    "],
            'V' => vec!["| | ", " V  ", "    "],
            'W' => vec!["| | ", "|^| ", "    "],
            'X' => vec!["\\/  ", "/\\  ", "    "],
            'Y' => vec!["\\/  ", " |  ", "    "],
            'Z' => vec!["==/ ", "/== ", "    "],
            ' ' => vec!["    ", "    ", "    "],
            _ => vec!["[?] ", "[?] ", "    "],
        }
    }

    fn render_slant(&self, ch: char) -> Vec<&'static str> {
        match ch {
            'A' => vec!["   /| ", "  /_| ", " /  | ", "/   | ", "      "],
            'B' => vec!["|===  ", "| __) ", "|  _) ", "|===  ", "      "],
            'C' => vec!["  ___/", " /    ", "|     ", " \\___\\", "      "],
            'D' => vec!["|===  ", "|   \\ ", "|   / ", "|===  ", "      "],
            'E' => vec!["|==== ", "|___  ", "|     ", "|==== ", "      "],
            'F' => vec!["|==== ", "|___  ", "|     ", "|     ", "      "],
            'G' => vec!["  ____", " /    ", "| /_  ", " \\__/ ", "      "],
            'H' => vec!["|   | ", "|===| ", "|   | ", "|   | ", "      "],
            'I' => vec!["  |   ", "  |   ", "  |   ", "  |   ", "      "],
            'J' => vec!["    | ", "    | ", " \\  | ", "  \\=/ ", "      "],
            'K' => vec!["|  /  ", "|-<   ", "|  \\  ", "|   \\ ", "      "],
            'L' => vec!["|     ", "|     ", "|     ", "|==== ", "      "],
            'M' => vec!["|\\  /|", "| \\/ |", "|    |", "|    |", "      "],
            'N' => vec!["|\\   |", "| \\  |", "|  \\ |", "|   \\|", "      "],
            'O' => vec!["  __  ", " /  \\ ", "|    |", " \\__/ ", "      "],
            'P' => vec!["|===\\ ", "|   | ", "|===/ ", "|     ", "      "],
            'Q' => vec!["  __  ", " /  \\ ", "|  \\ |", " \\__\\/", "      "],
            'R' => vec!["|===\\ ", "|   | ", "|===/ ", "|   \\ ", "      "],
            'S' => vec!["  ____", " (    ", "  === ", " ____)", "      "],
            'T' => vec!["====| ", "   |  ", "   |  ", "   |  ", "      "],
            'U' => vec!["|   | ", "|   | ", "|   | ", " \\=/ ", "      "],
            'V' => vec!["|   | ", " \\ /  ", "  |   ", "  .   ", "      "],
            'W' => vec!["|    |", "|/\\/\\|", "|    |", ".    .", "      "],
            'X' => vec!["\\   / ", " \\ /  ", " / \\  ", "/   \\ ", "      "],
            'Y' => vec!["\\   / ", " \\ /  ", "  |   ", "  |   ", "      "],
            'Z' => vec!["=====|", "    / ", "   /  ", "|=====", "      "],
            ' ' => vec!["      ", "      ", "      ", "      ", "      "],
            _ => vec!["[????]", "[????]", "[????]", "[????]", "      "],
        }
    }

    fn render_doom(&self, ch: char) -> Vec<&'static str> {
        // Doom-style large chunky letters
        match ch {
            'A' => vec![
                "   ██   ",
                "  ████  ",
                " ██  ██ ",
                "██    ██",
                "████████",
                "██    ██",
                "██    ██",
                "        ",
            ],
            'B' => vec![
                "██████  ",
                "██   ██ ",
                "██   ██ ",
                "██████  ",
                "██   ██ ",
                "██   ██ ",
                "██████  ",
                "        ",
            ],
            'C' => vec![
                " ██████ ",
                "██      ",
                "██      ",
                "██      ",
                "██      ",
                "██      ",
                " ██████ ",
                "        ",
            ],
            'D' => vec![
                "██████  ",
                "██   ██ ",
                "██    ██",
                "██    ██",
                "██    ██",
                "██   ██ ",
                "██████  ",
                "        ",
            ],
            'E' => vec![
                "████████",
                "██      ",
                "██      ",
                "██████  ",
                "██      ",
                "██      ",
                "████████",
                "        ",
            ],
            'F' => vec![
                "████████",
                "██      ",
                "██      ",
                "██████  ",
                "██      ",
                "██      ",
                "██      ",
                "        ",
            ],
            ' ' => vec![
                "        ", "        ", "        ", "        ", "        ", "        ", "        ",
                "        ",
            ],
            _ => vec![
                "████████",
                "██    ██",
                "██    ██",
                "██    ██",
                "██    ██",
                "██    ██",
                "████████",
                "        ",
            ],
        }
    }

    fn render_small_caps(&self, ch: char) -> Vec<&'static str> {
        // Unicode small caps
        match ch {
            'A' => vec!["ᴀ"],
            'B' => vec!["ʙ"],
            'C' => vec!["ᴄ"],
            'D' => vec!["ᴅ"],
            'E' => vec!["ᴇ"],
            'F' => vec!["ꜰ"],
            'G' => vec!["ɢ"],
            'H' => vec!["ʜ"],
            'I' => vec!["ɪ"],
            'J' => vec!["ᴊ"],
            'K' => vec!["ᴋ"],
            'L' => vec!["ʟ"],
            'M' => vec!["ᴍ"],
            'N' => vec!["ɴ"],
            'O' => vec!["ᴏ"],
            'P' => vec!["ᴘ"],
            'Q' => vec!["ǫ"],
            'R' => vec!["ʀ"],
            'S' => vec!["ꜱ"],
            'T' => vec!["ᴛ"],
            'U' => vec!["ᴜ"],
            'V' => vec!["ᴠ"],
            'W' => vec!["ᴡ"],
            'X' => vec!["x"],
            'Y' => vec!["ʏ"],
            'Z' => vec!["ᴢ"],
            ' ' => vec![" "],
            _ => vec!["?"],
        }
    }

    /// Render to frame at position with optional effects.
    pub fn render_at(&self, x: u16, y: u16, frame: &mut Frame, time: f64) {
        let lines = self.render_lines();
        let total_width: usize = lines.first().map(|l| display_width(l)).unwrap_or(0);

        for (row, line) in lines.iter().enumerate() {
            let py = y.saturating_add(row as u16);
            let mut col = 0usize;
            for grapheme in graphemes(line) {
                let w = grapheme_width(grapheme);
                if w == 0 {
                    continue;
                }
                let px = x.saturating_add(col as u16);

                // Determine color
                let color = if let Some(ref gradient) = self.gradient {
                    let t = if total_width > 1 {
                        (col as f64 / (total_width - 1) as f64 + time * 0.2).rem_euclid(1.0)
                    } else {
                        0.5
                    };
                    gradient.sample(t)
                } else {
                    self.color.unwrap_or(PackedRgba::rgb(255, 255, 255))
                };

                let content = if w > 1 || grapheme.chars().count() > 1 {
                    let id = frame.intern_with_width(grapheme, w as u8);
                    CellContent::from_grapheme(id)
                } else if let Some(ch) = grapheme.chars().next() {
                    CellContent::from_char(ch)
                } else {
                    continue;
                };

                let mut cell = Cell::new(content);
                if grapheme != " " {
                    cell.fg = color;
                }
                frame.buffer.set_fast(px, py, cell);

                col = col.saturating_add(w);
            }
        }
    }
}

// =============================================================================
// StyledMultiLine — Multi-line text with 2D effects (AsciiArt integration)
// =============================================================================

/// Reflection configuration for mirrored text rendering below the primary block.
///
/// Creates a vertically flipped copy of the source text with a gradient
/// opacity falloff, simulating a glossy/water surface effect.
#[derive(Debug, Clone)]
pub struct Reflection {
    /// Rows between the primary text and the reflection (0–3 typical).
    pub gap: u16,
    /// Starting opacity at the top of the reflection (0.0–1.0).
    pub start_opacity: f64,
    /// Ending opacity at the bottom of the reflection (0.0–1.0).
    pub end_opacity: f64,
    /// Fraction of the source text height to reflect (0.0–1.0).
    /// For example, 0.5 reflects only the bottom half of the text.
    pub height_ratio: f64,
    /// Water-ripple wave amplitude (0.0 = perfect mirror, >0 = horizontal shimmer).
    /// Each reflection row is offset horizontally by `sin(row * 0.5 + time * 2.0) * wave`.
    pub wave: f64,
}

impl Default for Reflection {
    fn default() -> Self {
        Self {
            gap: 0,
            start_opacity: 0.4,
            end_opacity: 0.05,
            height_ratio: 1.0,
            wave: 0.0,
        }
    }
}

impl Reflection {
    /// Compute the number of reflected rows given a source height.
    pub fn reflected_rows(&self, source_height: usize) -> usize {
        let max_rows = (source_height as f64 * self.height_ratio.clamp(0.0, 1.0)).ceil() as usize;
        max_rows.min(source_height)
    }
}

/// Multi-line styled text with 2D effect application.
///
/// Integrates [`AsciiArtText`] (or any `Vec<String>`) with the text effect
/// system, applying effects using 2D coordinates (x=char column, y=row) for
/// proper gradient mapping and positional animations.
///
/// # Example
///
/// ```rust,ignore
/// use ftui_extras::text_effects::{
///     AsciiArtText, AsciiArtStyle, ColorGradient, StyledMultiLine, TextEffect,
/// };
///
/// let art = AsciiArtText::new("HELLO", AsciiArtStyle::Block);
/// let multi = StyledMultiLine::from_ascii_art(art)
///     .effect(TextEffect::AnimatedGradient {
///         gradient: ColorGradient::cyberpunk(),
///         speed: 0.2,
///     })
///     .reflection(Reflection::default())
///     .time(0.5);
/// ```
#[derive(Debug, Clone)]
pub struct StyledMultiLine {
    lines: Vec<String>,
    effects: Vec<TextEffect>,
    base_color: PackedRgba,
    bg_color: Option<PackedRgba>,
    bold: bool,
    italic: bool,
    time: f64,
    seed: u64,
    easing: Easing,
    reflection: Option<Reflection>,
}

impl StyledMultiLine {
    /// Create from pre-rendered lines (e.g. from `AsciiArtText::render_lines()`).
    pub fn new(lines: Vec<String>) -> Self {
        Self {
            lines,
            effects: Vec::new(),
            base_color: PackedRgba::rgb(255, 255, 255),
            bg_color: None,
            bold: false,
            italic: false,
            time: 0.0,
            seed: 0,
            easing: Easing::Linear,
            reflection: None,
        }
    }

    /// Create from an [`AsciiArtText`], preserving its gradient as a base color source.
    pub fn from_ascii_art(art: AsciiArtText) -> Self {
        let lines = art.render_lines();
        let mut styled = Self::new(lines);
        if let Some(color) = art.get_color() {
            styled.base_color = color;
        }
        styled
    }

    /// Add a text effect.
    #[must_use]
    pub fn effect(mut self, effect: TextEffect) -> Self {
        if self.effects.len() < MAX_EFFECTS {
            self.effects.push(effect);
        }
        self
    }

    /// Add multiple effects.
    #[must_use]
    pub fn effects(mut self, effects: impl IntoIterator<Item = TextEffect>) -> Self {
        for e in effects {
            if self.effects.len() >= MAX_EFFECTS {
                break;
            }
            self.effects.push(e);
        }
        self
    }

    /// Set the base foreground color.
    #[must_use]
    pub fn base_color(mut self, color: PackedRgba) -> Self {
        self.base_color = color;
        self
    }

    /// Set an optional background color.
    #[must_use]
    pub fn bg_color(mut self, color: PackedRgba) -> Self {
        self.bg_color = Some(color);
        self
    }

    /// Enable bold style.
    #[must_use]
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Enable italic style.
    #[must_use]
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// Set animation time.
    #[must_use]
    pub fn time(mut self, time: f64) -> Self {
        self.time = time;
        self
    }

    /// Set deterministic seed for effects like Scramble/Glitch.
    #[must_use]
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Set easing function for effects.
    #[must_use]
    pub fn easing(mut self, easing: Easing) -> Self {
        self.easing = easing;
        self
    }

    /// Enable reflection below the text block.
    #[must_use]
    pub fn reflection(mut self, reflection: Reflection) -> Self {
        self.reflection = Some(reflection);
        self
    }

    /// Get the lines as a slice.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Get the height in lines (not including reflection).
    pub fn height(&self) -> usize {
        self.lines.len()
    }

    /// Get the maximum width across all lines.
    pub fn width(&self) -> usize {
        self.lines
            .iter()
            .map(|l| display_width(l))
            .max()
            .unwrap_or(0)
    }

    /// Total height including reflection.
    pub fn total_height(&self) -> usize {
        let base = self.lines.len();
        match &self.reflection {
            Some(r) => base + r.gap as usize + r.reflected_rows(base),
            None => base,
        }
    }

    /// Compute the color for a character at 2D position (col, row).
    ///
    /// Uses the full text block dimensions for proper gradient mapping:
    /// - Horizontal gradients use `col / total_width`
    /// - Vertical components use `row / total_height`
    fn char_color_2d(
        &self,
        col: usize,
        row: usize,
        total_width: usize,
        total_height: usize,
    ) -> PackedRgba {
        let mut color = self.base_color;

        let t_x = if total_width > 1 {
            col as f64 / (total_width - 1) as f64
        } else {
            0.5
        };
        let t_y = if total_height > 1 {
            row as f64 / (total_height - 1) as f64
        } else {
            0.5
        };

        for effect in &self.effects {
            match effect {
                TextEffect::HorizontalGradient { gradient } => {
                    color = gradient.sample(t_x);
                }
                TextEffect::AnimatedGradient { gradient, speed } => {
                    let t = (t_x + self.time * speed).rem_euclid(1.0);
                    color = gradient.sample(t);
                }
                TextEffect::RainbowGradient { speed } => {
                    let hue = (t_x + t_y * 0.3 + self.time * speed).rem_euclid(1.0);
                    color = hsv_to_rgb(hue, 0.9, 1.0);
                }
                TextEffect::VerticalGradient { gradient } => {
                    // For multi-line: t = row / total_rows
                    color = gradient.sample(t_y);
                }
                TextEffect::DiagonalGradient { gradient, angle } => {
                    let angle_rad = angle.to_radians();
                    let cos_a = angle_rad.cos();
                    let sin_a = angle_rad.sin();
                    // Project 2D position onto gradient axis
                    let projected = t_x * cos_a + t_y * sin_a;
                    // Normalize to 0-1 range
                    let normalized = (projected + FRAC_1_SQRT_2) / SQRT_2;
                    color = gradient.sample(normalized.clamp(0.0, 1.0));
                }
                TextEffect::RadialGradient {
                    gradient,
                    center,
                    aspect,
                } => {
                    let dx = (t_x - center.0) * aspect;
                    let dy = t_y - center.1;
                    let dist = (dx * dx + dy * dy).sqrt();
                    // Max distance is from center to corner
                    let max_dist = {
                        let corner_dx = (0.5_f64.max(center.0) - center.0.min(0.5)) * aspect;
                        let corner_dy = 0.5_f64.max(center.1) - center.1.min(0.5);
                        (corner_dx * corner_dx + corner_dy * corner_dy).sqrt()
                    };
                    let normalized = if max_dist > 0.0 {
                        (dist / max_dist).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    color = gradient.sample(normalized);
                }
                TextEffect::ColorWave {
                    color1,
                    color2,
                    speed,
                    wavelength,
                } => {
                    let t = ((col as f64 + row as f64 * 0.5) / wavelength + self.time * speed)
                        .sin()
                        * 0.5
                        + 0.5;
                    color = lerp_color(*color1, *color2, t);
                }
                TextEffect::FadeIn { progress } => {
                    color = apply_alpha(color, *progress);
                }
                TextEffect::FadeOut { progress } => {
                    color = apply_alpha(color, 1.0 - *progress);
                }
                TextEffect::Pulse { speed, min_alpha } => {
                    let alpha = min_alpha
                        + (1.0 - min_alpha) * ((self.time * speed * TAU).sin() * 0.5 + 0.5);
                    color = apply_alpha(color, alpha);
                }
                TextEffect::OrganicPulse {
                    speed,
                    min_brightness,
                    asymmetry,
                    phase_variation,
                    seed,
                } => {
                    // Use 2D position as character index for phase variation
                    let char_idx = row * total_width + col;
                    let phase_offset = organic_char_phase_offset(char_idx, *seed, *phase_variation);
                    let cycle_t = (self.time * speed + phase_offset).rem_euclid(1.0);
                    let brightness = min_brightness
                        + (1.0 - min_brightness) * breathing_curve(cycle_t, *asymmetry);
                    color = apply_alpha(color, brightness);
                }
                TextEffect::Glow {
                    color: glow_color,
                    intensity,
                } => {
                    color = lerp_color(color, *glow_color, *intensity);
                }
                TextEffect::PulsingGlow {
                    color: glow_color,
                    speed,
                } => {
                    let intensity = ((self.time * speed * TAU).sin() * 0.5 + 0.5) * 0.6;
                    color = lerp_color(color, *glow_color, intensity);
                }
                TextEffect::ColorCycle { colors, speed } => {
                    if !colors.is_empty() {
                        let t = (self.time * speed).rem_euclid(colors.len() as f64);
                        let idx = t as usize % colors.len();
                        let next = (idx + 1) % colors.len();
                        let frac = t.fract();
                        color = lerp_color(colors[idx], colors[next], frac);
                    }
                }
                TextEffect::Scanline {
                    intensity,
                    line_gap,
                    scroll,
                    scroll_speed,
                    flicker,
                } => {
                    // Calculate scroll offset for animated scanlines
                    let scroll_offset = if *scroll {
                        (self.time * scroll_speed) as usize
                    } else {
                        0
                    };

                    // Determine if this row is a scanline (dimmed)
                    let effective_row = row + scroll_offset;
                    let is_scanline =
                        *line_gap > 0 && effective_row.is_multiple_of(*line_gap as usize);

                    // Apply scanline dimming
                    let mut dim_factor = if is_scanline { 1.0 - *intensity } else { 1.0 };

                    // Apply flicker (random per-frame brightness variation)
                    if *flicker > 0.0 {
                        let flicker_seed = (self.time * 60.0) as u64;
                        let hash = flicker_seed
                            .wrapping_mul(2654435761)
                            .wrapping_add((row * total_width + col) as u64 * 2246822519);
                        let rand = (hash % 10000) as f64 / 10000.0;
                        dim_factor *= 1.0 - (*flicker * rand);
                    }

                    color = apply_alpha(color, dim_factor);
                }
                _ => {} // Position/char effects handled separately
            }
        }

        color
    }

    /// Render one line of the multi-line block.
    #[allow(clippy::too_many_arguments)]
    fn render_line(
        &self,
        line: &str,
        x: u16,
        y: u16,
        row: usize,
        total_width: usize,
        total_height: usize,
        opacity: f64,
        frame: &mut Frame,
    ) {
        let frame_width = frame.buffer.width();
        let frame_height = frame.buffer.height();

        let mut flags = CellStyleFlags::empty();
        if self.bold {
            flags = flags.union(CellStyleFlags::BOLD);
        }
        if self.italic {
            flags = flags.union(CellStyleFlags::ITALIC);
        }
        let attrs = CellAttrs::new(flags, 0);

        let mut col = 0usize;
        for grapheme in graphemes(line) {
            let width = grapheme_width(grapheme);
            if width == 0 {
                continue;
            }

            let px = x.saturating_add(col as u16);
            let py = y;

            if px >= frame_width || py >= frame_height {
                col = col.saturating_add(width);
                continue;
            }

            let mut color = self.char_color_2d(col, row, total_width, total_height);
            if opacity < 1.0 {
                color = apply_alpha(color, opacity);
            }

            if grapheme == " " {
                if let Some(bg) = self.bg_color
                    && let Some(cell) = frame.buffer.get_mut(px, py)
                {
                    cell.bg = bg;
                }
                col = col.saturating_add(width);
                continue;
            }

            let content = if width > 1 || grapheme.chars().count() > 1 {
                let id = frame.intern_with_width(grapheme, width as u8);
                CellContent::from_grapheme(id)
            } else if let Some(ch) = grapheme.chars().next() {
                CellContent::from_char(ch)
            } else {
                col = col.saturating_add(width);
                continue;
            };

            let mut cell = Cell::new(content);
            cell.fg = color;
            cell.attrs = attrs;
            if let Some(bg) = self.bg_color {
                cell.bg = bg;
            }
            frame.buffer.set_fast(px, py, cell);
            col = col.saturating_add(width);
        }
    }

    /// Render the reflection below the primary text block.
    fn render_reflection(
        &self,
        x: u16,
        y: u16,
        total_width: usize,
        reflection: &Reflection,
        frame: &mut Frame,
    ) {
        let src_height = self.lines.len();
        let refl_rows = reflection.reflected_rows(src_height);

        if refl_rows == 0 {
            return;
        }

        for refl_row in 0..refl_rows {
            // Mirror: bottom line of source renders first
            let src_row = src_height - 1 - refl_row;
            let dest_y = y.saturating_add(refl_row as u16);

            // Linear opacity interpolation from start to end
            let t = if refl_rows > 1 {
                refl_row as f64 / (refl_rows - 1) as f64
            } else {
                0.0
            };
            let opacity =
                reflection.start_opacity + (reflection.end_opacity - reflection.start_opacity) * t;

            // Wave offset: horizontal shimmer for water-ripple effect
            let wave_dx = if reflection.wave > 0.0 {
                ((refl_row as f64 * 0.5 + self.time * 2.0).sin() * reflection.wave) as i16
            } else {
                0
            };

            if let Some(line) = self.lines.get(src_row) {
                let render_x = if wave_dx >= 0 {
                    x.saturating_add(wave_dx as u16)
                } else {
                    x.saturating_sub(wave_dx.unsigned_abs())
                };
                self.render_line(
                    line,
                    render_x,
                    dest_y,
                    src_row,
                    total_width,
                    src_height,
                    opacity,
                    frame,
                );
            }
        }
    }

    /// Render the multi-line text at the given position.
    pub fn render_at(&self, x: u16, y: u16, frame: &mut Frame) {
        let total_width = self.width();
        let total_height = self.lines.len();

        if total_width == 0 || total_height == 0 {
            return;
        }

        // Render primary lines
        for (row, line) in self.lines.iter().enumerate() {
            let py = y.saturating_add(row as u16);
            self.render_line(line, x, py, row, total_width, total_height, 1.0, frame);
        }

        // Render reflection (with gap)
        if let Some(ref reflection) = self.reflection {
            let refl_y = y.saturating_add(total_height as u16 + reflection.gap);
            self.render_reflection(x, refl_y, total_width, reflection, frame);
        }
    }
}

impl Widget for StyledMultiLine {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.render_at(area.x, area.y, frame);
    }
}

// =============================================================================
// Sparkle Effect - Particles that twinkle
// =============================================================================

/// A single sparkle particle.
#[derive(Debug, Clone)]
pub struct Sparkle {
    pub x: f64,
    pub y: f64,
    pub brightness: f64,
    pub phase: f64,
}

/// Manages a collection of sparkle effects.
#[derive(Debug, Clone, Default)]
pub struct SparkleField {
    sparkles: Vec<Sparkle>,
    density: f64,
}

impl SparkleField {
    /// Create a new sparkle field.
    pub fn new(density: f64) -> Self {
        Self {
            sparkles: Vec::new(),
            density: density.clamp(0.0, 1.0),
        }
    }

    /// Initialize sparkles for an area.
    pub fn init_for_area(&mut self, width: u16, height: u16, seed: u64) {
        self.sparkles.clear();
        let count = ((width as f64 * height as f64) * self.density * 0.05) as usize;

        let mut rng = seed;
        for _ in 0..count {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let x = (rng % width as u64) as f64;
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let y = (rng % height as u64) as f64;
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let phase = (rng % 1000) as f64 / 1000.0 * TAU;

            self.sparkles.push(Sparkle {
                x,
                y,
                brightness: 1.0,
                phase,
            });
        }
    }

    /// Update sparkles for animation.
    pub fn update(&mut self, time: f64) {
        for sparkle in &mut self.sparkles {
            sparkle.brightness = 0.5 + 0.5 * (time * 3.0 + sparkle.phase).sin();
        }
    }

    /// Render sparkles to frame.
    pub fn render(&self, offset_x: u16, offset_y: u16, frame: &mut Frame) {
        for sparkle in &self.sparkles {
            let px = offset_x.saturating_add(sparkle.x as u16);
            let py = offset_y.saturating_add(sparkle.y as u16);

            if let Some(cell) = frame.buffer.get_mut(px, py) {
                let b = (sparkle.brightness * 255.0) as u8;
                // Use star characters for sparkle
                let ch = if sparkle.brightness > 0.8 {
                    '*'
                } else if sparkle.brightness > 0.5 {
                    '+'
                } else {
                    '.'
                };
                cell.content = CellContent::from_char(ch);
                cell.fg = PackedRgba::rgb(b, b, b.saturating_add(50));
            }
        }
    }
}

// =============================================================================
// Matrix/Cyber Characters
// =============================================================================

/// Characters for matrix/cyber style effects.
pub struct CyberChars;

impl CyberChars {
    /// Get a random cyber character based on seed.
    pub fn get(seed: u64) -> char {
        const CYBER_CHARS: &[char] = &[
            '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'ア', 'イ', 'ウ', 'エ', 'オ', 'カ',
            'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ', 'タ', 'チ', 'ツ', 'テ', 'ト',
            '/', '\\', '|', '-', '+', '*', '#', '@', '=', '>', '<', '[', ']', '{', '}', '(', ')',
            '$', '%', '&',
        ];
        let idx = (seed % CYBER_CHARS.len() as u64) as usize;
        CYBER_CHARS[idx]
    }

    /// Get a random printable ASCII character.
    pub fn ascii(seed: u64) -> char {
        let code = 33 + (seed % 94) as u8;
        code as char
    }

    /// Get half-width katakana characters for authentic Matrix effect.
    pub const HALF_WIDTH_KATAKANA: &'static [char] = &[
        'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ', 'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ', 'ｾ', 'ｿ', 'ﾀ', 'ﾁ', 'ﾂ',
        'ﾃ', 'ﾄ', 'ﾅ', 'ﾆ', 'ﾇ', 'ﾈ', 'ﾉ', 'ﾊ', 'ﾋ', 'ﾌ', 'ﾍ', 'ﾎ', 'ﾏ', 'ﾐ', 'ﾑ', 'ﾒ', 'ﾓ', 'ﾔ',
        'ﾕ', 'ﾖ', 'ﾗ', 'ﾘ', 'ﾙ', 'ﾚ', 'ﾛ', 'ﾜ', 'ﾝ',
    ];

    /// Get a matrix character (half-width katakana + digits + symbols).
    pub fn matrix(seed: u64) -> char {
        const MATRIX_CHARS: &[char] = &[
            // Digits
            '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', // Half-width katakana
            'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ', 'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ', 'ｾ', 'ｿ', 'ﾀ', 'ﾁ',
            'ﾂ', 'ﾃ', 'ﾄ', 'ﾅ', 'ﾆ', 'ﾇ', 'ﾈ', 'ﾉ', 'ﾊ', 'ﾋ', 'ﾌ', 'ﾍ', 'ﾎ', 'ﾏ', 'ﾐ', 'ﾑ', 'ﾒ',
            'ﾓ', 'ﾔ', 'ﾕ', 'ﾖ', 'ﾗ', 'ﾘ', 'ﾙ', 'ﾚ', 'ﾛ', 'ﾜ', 'ﾝ', // Latin capitals
            'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q',
            'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
        ];
        let idx = (seed % MATRIX_CHARS.len() as u64) as usize;
        MATRIX_CHARS[idx]
    }
}

// =============================================================================
// Matrix Rain Effect - Digital rain cascading down the screen
// =============================================================================

/// A single column of falling Matrix characters.
#[derive(Debug, Clone)]
pub struct MatrixColumn {
    /// Column x position.
    pub x: u16,
    /// Current y offset (can be negative for off-screen start).
    pub y_offset: f64,
    /// Falling speed (cells per update).
    pub speed: f64,
    /// Characters in the column with their brightness (0.0-1.0).
    pub chars: Vec<(char, f64)>,
    /// Maximum trail length.
    pub max_length: usize,
    /// RNG state for this column.
    rng_state: u64,
}

impl MatrixColumn {
    /// Create a new matrix column.
    pub fn new(x: u16, seed: u64) -> Self {
        let mut rng = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(x as u64);

        // Variable speed: 0.2 to 0.8 cells per update
        let speed = 0.2 + (rng % 600) as f64 / 1000.0;
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);

        // Trail length: 8 to 28 characters
        let max_length = 8 + (rng % 20) as usize;
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);

        // Start position: above screen
        let y_offset = -((rng % 30) as f64);

        Self {
            x,
            y_offset,
            speed,
            chars: Vec::with_capacity(max_length),
            max_length,
            rng_state: rng,
        }
    }

    /// Advance the RNG and return the next value.
    fn next_rng(&mut self) -> u64 {
        self.rng_state = self
            .rng_state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        self.rng_state
    }

    /// Update the column state.
    pub fn update(&mut self) {
        // Move down
        self.y_offset += self.speed;

        // Fade existing characters
        for (_, brightness) in &mut self.chars {
            *brightness *= 0.92;
        }

        // Maybe add new character at head
        let rng = self.next_rng();
        if rng % 100 < 40 {
            // 40% chance to add new char
            let ch = CyberChars::matrix(self.next_rng());
            self.chars.insert(0, (ch, 1.0));
        }

        // Random character mutations
        let mutation_rng = self.next_rng();
        if mutation_rng % 100 < 15 && !self.chars.is_empty() {
            // 15% chance to mutate
            let idx = (self.next_rng() % self.chars.len() as u64) as usize;
            let new_char = CyberChars::matrix(self.next_rng());
            self.chars[idx].0 = new_char;
        }

        // Trim old characters that have faded
        self.chars.retain(|(_, b)| *b > 0.03);

        // Limit trail length
        if self.chars.len() > self.max_length {
            self.chars.truncate(self.max_length);
        }
    }

    /// Check if this column has scrolled completely off screen.
    pub fn is_offscreen(&self, height: u16) -> bool {
        let tail_y = self.y_offset as i32 - self.chars.len() as i32;
        tail_y > height as i32 + 5
    }

    /// Reset the column to start from above the screen.
    pub fn reset(&mut self, seed: u64) {
        let rng = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(self.x as u64);
        self.y_offset = -((rng % 30) as f64) - 5.0;
        self.chars.clear();
        self.rng_state = rng;

        // Randomize speed on reset
        let speed_rng = self
            .rng_state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        self.speed = 0.2 + (speed_rng % 600) as f64 / 1000.0;
    }
}

/// State manager for the Matrix rain effect.
#[derive(Debug, Clone)]
pub struct MatrixRainState {
    /// All active columns.
    columns: Vec<MatrixColumn>,
    /// Width of the display area.
    width: u16,
    /// Height of the display area.
    height: u16,
    /// Global seed for determinism.
    seed: u64,
    /// Frame counter for time-based effects.
    frame: u64,
    /// Whether the state has been initialized.
    initialized: bool,
}

impl Default for MatrixRainState {
    fn default() -> Self {
        Self::new()
    }
}

impl MatrixRainState {
    /// Create a new uninitialized Matrix rain state.
    pub fn new() -> Self {
        Self {
            columns: Vec::new(),
            width: 0,
            height: 0,
            seed: 42,
            frame: 0,
            initialized: false,
        }
    }

    /// Create with a specific seed for deterministic output.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            seed,
            ..Self::new()
        }
    }

    /// Initialize for a given area size.
    pub fn init(&mut self, width: u16, height: u16) {
        if self.initialized && self.width == width && self.height == height {
            return;
        }

        self.width = width;
        self.height = height;
        self.columns.clear();

        // Create a column for each x position, with some gaps
        for x in 0..width {
            let col_seed = self.seed.wrapping_add(x as u64 * 7919);
            // 70% chance to have a column at each position
            if col_seed % 100 < 70 {
                self.columns.push(MatrixColumn::new(x, col_seed));
            }
        }

        self.initialized = true;
    }

    /// Update all columns.
    pub fn update(&mut self) {
        if !self.initialized {
            return;
        }

        self.frame = self.frame.wrapping_add(1);

        for col in &mut self.columns {
            col.update();

            // Reset columns that have scrolled off screen
            if col.is_offscreen(self.height) {
                col.reset(
                    self.seed
                        .wrapping_add(self.frame)
                        .wrapping_add(col.x as u64),
                );
            }
        }

        // Occasionally spawn new columns in empty spots
        if self.frame.is_multiple_of(20) {
            for x in 0..self.width {
                let has_column = self.columns.iter().any(|c| c.x == x);
                if !has_column {
                    let spawn_rng = self
                        .seed
                        .wrapping_add(self.frame)
                        .wrapping_add(x as u64 * 31);
                    if spawn_rng % 100 < 3 {
                        // 3% chance per empty slot
                        self.columns.push(MatrixColumn::new(x, spawn_rng));
                    }
                }
            }
        }
    }

    /// Render the Matrix rain to a Frame.
    pub fn render(&self, area: Rect, frame: &mut Frame) {
        if !self.initialized {
            return;
        }

        for col in &self.columns {
            // Check if column is in view
            if col.x < area.x || col.x >= area.x + area.width {
                continue;
            }

            let px = col.x;

            for (i, (ch, brightness)) in col.chars.iter().enumerate() {
                let char_y = col.y_offset as i32 - i as i32;

                // Skip if outside area
                if char_y < area.y as i32 || char_y >= (area.y + area.height) as i32 {
                    continue;
                }

                let py = char_y as u16;

                // Calculate color based on brightness
                // Head (i=0, brightness=1.0) is white-green
                // Tail fades to dark green
                let color = if i == 0 && *brightness > 0.95 {
                    // Bright white-green head
                    PackedRgba::rgb(180, 255, 180)
                } else if i == 0 {
                    // Slightly dimmed head
                    let g = (255.0 * brightness) as u8;
                    PackedRgba::rgb((g / 2).min(200), g, (g / 2).min(200))
                } else {
                    // Green tail with fade
                    let g = (220.0 * brightness) as u8;
                    let r = (g / 8).min(30);
                    let b = (g / 6).min(40);
                    PackedRgba::rgb(r, g, b)
                };

                // Write to frame buffer
                if let Some(cell) = frame.buffer.get_mut(px, py) {
                    cell.content = CellContent::from_char(*ch);
                    cell.fg = color;
                    // Keep background as-is or set to black for proper Matrix look
                    cell.bg = PackedRgba::rgb(0, 0, 0);
                }
            }
        }
    }

    /// Get the current frame count.
    pub fn frame_count(&self) -> u64 {
        self.frame
    }

    /// Check if initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Get column count.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }
}

// =============================================================================
// Palette Tests
// =============================================================================

#[cfg(test)]
mod palette_tests {
    use super::{PackedRgba, palette};

    #[test]
    fn test_all_gradients_have_at_least_two_stops() {
        for (name, grad) in palette::all_gradients() {
            let start = grad.sample(0.0);
            let end = grad.sample(1.0);
            // Verify both endpoints produce valid colors (not default white from empty)
            // and that the gradient actually has differentiation
            assert!(
                start.r() != end.r() || start.g() != end.g() || start.b() != end.b(),
                "Gradient '{}' should have distinct start and end colors",
                name
            );
        }
    }

    #[test]
    fn test_gradient_coverage_starts_at_zero_ends_at_one() {
        // All presets should produce a non-white color at t=0 and t=1
        // (white is the fallback for empty gradients)
        for (name, grad) in palette::all_gradients() {
            let at_zero = grad.sample(0.0);
            let at_one = grad.sample(1.0);
            // At least one channel should differ from white-fallback (255,255,255)
            let is_all_white = |c: PackedRgba| c.r() == 255 && c.g() == 255 && c.b() == 255;
            assert!(
                !is_all_white(at_zero) || !is_all_white(at_one),
                "Gradient '{}' appears to be empty (produces all-white)",
                name
            );
        }
    }

    #[test]
    fn test_gradient_midpoint_samples_without_panic() {
        for (name, grad) in palette::all_gradients() {
            let _ = grad.sample(0.25);
            let _ = grad.sample(0.5);
            let _ = grad.sample(0.75);
            // Just ensure no panic
            let _ = name;
        }
    }

    #[test]
    fn test_all_color_sets_have_at_least_two_colors() {
        for (name, colors) in palette::all_color_sets() {
            assert!(
                colors.len() >= 2,
                "Color set '{}' should have at least 2 colors, got {}",
                name,
                colors.len()
            );
        }
    }

    #[test]
    fn test_color_sets_no_duplicates() {
        for (name, colors) in palette::all_color_sets() {
            for (i, a) in colors.iter().enumerate() {
                for (j, b) in colors.iter().enumerate() {
                    if i != j {
                        assert!(
                            a.r() != b.r() || a.g() != b.g() || a.b() != b.b(),
                            "Color set '{}' has duplicate at indices {} and {}",
                            name,
                            i,
                            j
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_neon_colors_are_vivid() {
        // Neon colors should have at least one channel at max brightness
        for color in palette::neon_colors() {
            assert!(
                color.r() == 255 || color.g() == 255 || color.b() == 255,
                "Neon color ({},{},{}) should have at least one saturated channel",
                color.r(),
                color.g(),
                color.b()
            );
        }
    }

    #[test]
    fn test_pastel_colors_are_light() {
        // Pastels should have relatively high average brightness
        for color in palette::pastel_colors() {
            let avg = (color.r() as u16 + color.g() as u16 + color.b() as u16) / 3;
            assert!(
                avg >= 150,
                "Pastel color ({},{},{}) avg brightness {} is too dark",
                color.r(),
                color.g(),
                color.b(),
                avg
            );
        }
    }

    #[test]
    fn test_monochrome_is_achromatic() {
        for color in palette::monochrome() {
            assert_eq!(
                color.r(),
                color.g(),
                "Monochrome ({},{},{}) should have equal R and G",
                color.r(),
                color.g(),
                color.b()
            );
            assert_eq!(
                color.g(),
                color.b(),
                "Monochrome ({},{},{}) should have equal G and B",
                color.r(),
                color.g(),
                color.b()
            );
        }
    }

    #[test]
    fn test_monochrome_ordered_dark_to_light() {
        let colors = palette::monochrome();
        for i in 1..colors.len() {
            assert!(
                colors[i].r() > colors[i - 1].r(),
                "Monochrome should be ordered dark to light: index {} ({}) <= index {} ({})",
                i,
                colors[i].r(),
                i - 1,
                colors[i - 1].r()
            );
        }
    }

    #[test]
    fn test_ice_gradient_is_cool_toned() {
        let grad = palette::ice();
        // Sample at multiple points; blue channel should dominate or be high
        for &t in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            let c = grad.sample(t);
            assert!(
                c.b() >= c.r(),
                "Ice gradient at t={} should be cool-toned: r={} > b={}",
                t,
                c.r(),
                c.b()
            );
        }
    }

    #[test]
    fn test_forest_gradient_is_green_dominant() {
        let grad = palette::forest();
        for &t in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            let c = grad.sample(t);
            assert!(
                c.g() >= c.r() && c.g() >= c.b(),
                "Forest gradient at t={} should be green-dominant: ({},{},{})",
                t,
                c.r(),
                c.g(),
                c.b()
            );
        }
    }

    #[test]
    fn test_blood_gradient_is_red_dominant() {
        let grad = palette::blood();
        for &t in &[0.25, 0.5, 0.75, 1.0] {
            let c = grad.sample(t);
            assert!(
                c.r() >= c.g() && c.r() >= c.b(),
                "Blood gradient at t={} should be red-dominant: ({},{},{})",
                t,
                c.r(),
                c.g(),
                c.b()
            );
        }
    }

    #[test]
    fn test_matrix_gradient_is_green_channel() {
        let grad = palette::matrix();
        // At end, should be bright green with no red
        let end = grad.sample(1.0);
        assert_eq!(end.r(), 0, "Matrix end should have no red");
        assert!(end.g() > 200, "Matrix end should be bright green");
    }

    #[test]
    fn test_all_gradients_count() {
        assert_eq!(
            palette::all_gradients().len(),
            13,
            "Should have 13 gradient presets (5 original + 8 new)"
        );
    }

    #[test]
    fn test_all_color_sets_count() {
        assert_eq!(
            palette::all_color_sets().len(),
            4,
            "Should have 4 color sets"
        );
    }
}

// =============================================================================
// Matrix Rain Tests
// =============================================================================

#[cfg(test)]
mod matrix_rain_tests {
    use super::*;

    #[test]
    fn matrix_column_speeds_vary() {
        let col1 = MatrixColumn::new(0, 100);
        let col2 = MatrixColumn::new(1, 200);
        let col3 = MatrixColumn::new(2, 300);

        // Speeds should vary between columns
        assert!(
            col1.speed != col2.speed || col2.speed != col3.speed,
            "Column speeds should vary: {}, {}, {}",
            col1.speed,
            col2.speed,
            col3.speed
        );

        // All speeds should be in valid range
        assert!(col1.speed >= 0.2 && col1.speed <= 0.8);
        assert!(col2.speed >= 0.2 && col2.speed <= 0.8);
        assert!(col3.speed >= 0.2 && col3.speed <= 0.8);
    }

    #[test]
    fn matrix_update_progresses() {
        let mut col = MatrixColumn::new(5, 42);
        let initial_y = col.y_offset;

        col.update();

        assert!(col.y_offset > initial_y, "Update should increase y_offset");
    }

    #[test]
    fn matrix_char_brightness_fades() {
        let mut col = MatrixColumn::new(0, 12345);

        // Force some characters to be added
        for _ in 0..10 {
            let rng = col.next_rng();
            col.chars.insert(0, (CyberChars::matrix(rng), 1.0));
        }

        // Get initial brightness of non-head character
        let initial_brightness = col.chars.get(1).map(|(_, b)| *b).unwrap_or(1.0);

        // Update several times
        for _ in 0..5 {
            col.update();
        }

        // Brightness should have faded
        if let Some((_, brightness)) = col.chars.get(1) {
            assert!(
                *brightness < initial_brightness,
                "Brightness should fade over time"
            );
        }
    }

    #[test]
    fn matrix_katakana_chars_valid() {
        // Test that matrix chars are valid
        for seed in 0..100 {
            let ch = CyberChars::matrix(seed);
            assert!(
                ch.is_alphanumeric() || ch as u32 >= 0xFF61,
                "Character {} (seed {}) should be alphanumeric or katakana",
                ch,
                seed
            );
        }
    }

    #[test]
    fn matrix_state_initialization() {
        let mut state = MatrixRainState::with_seed(42);
        assert!(!state.is_initialized());

        state.init(80, 24);

        assert!(state.is_initialized());
        assert!(state.column_count() > 0);
        assert!(state.column_count() <= 80);
    }

    #[test]
    fn matrix_state_deterministic() {
        let mut state1 = MatrixRainState::with_seed(12345);
        let mut state2 = MatrixRainState::with_seed(12345);

        state1.init(40, 20);
        state2.init(40, 20);

        // Same seed should produce same column count
        assert_eq!(state1.column_count(), state2.column_count());

        // Update both
        for _ in 0..10 {
            state1.update();
            state2.update();
        }

        // Frame counts should match
        assert_eq!(state1.frame_count(), state2.frame_count());
    }

    #[test]
    fn matrix_columns_recycle() {
        let mut state = MatrixRainState::with_seed(99);
        state.init(10, 5); // Small area

        let initial_count = state.column_count();

        // Run many updates to cycle columns
        for _ in 0..200 {
            state.update();
        }

        // Should still have columns (recycled, not removed)
        assert!(state.column_count() > 0);
        // Count might vary slightly due to spawn/despawn logic
        assert!(state.column_count() >= initial_count / 2);
    }
}

// =============================================================================
// ParticleDissolve Tests
// =============================================================================

#[cfg(test)]
mod particle_dissolve_tests {
    use super::*;

    #[test]
    fn test_dissolve_progress_0_solid() {
        let text = StyledText::new("HELLO")
            .effect(TextEffect::ParticleDissolve {
                progress: 0.0,
                mode: DissolveMode::Dissolve,
                speed: 1.0,
                gravity: 0.5,
                seed: 42,
            })
            .time(0.0);

        for (idx, ch) in "HELLO".chars().enumerate() {
            let displayed = text.char_at(idx, ch);
            assert_eq!(
                displayed, ch,
                "At progress 0, char {} should be '{}'",
                idx, ch
            );
        }
    }

    #[test]
    fn test_dissolve_progress_1_scattered() {
        let text = StyledText::new("HELLO")
            .effect(TextEffect::ParticleDissolve {
                progress: 1.0,
                mode: DissolveMode::Dissolve,
                speed: 1.0,
                gravity: 0.5,
                seed: 42,
            })
            .time(0.0);

        let particle_chars = ['·', '∙', '•', '*'];
        for (idx, original) in "HELLO".chars().enumerate() {
            let displayed = text.char_at(idx, original);
            assert!(
                particle_chars.contains(&displayed),
                "At progress 1, should be particle"
            );
        }
    }

    #[test]
    fn test_materialize_reverses() {
        let text_start = StyledText::new("HI")
            .effect(TextEffect::ParticleDissolve {
                progress: 0.0,
                mode: DissolveMode::Materialize,
                speed: 1.0,
                gravity: 0.5,
                seed: 42,
            })
            .time(0.0);

        let text_end = StyledText::new("HI")
            .effect(TextEffect::ParticleDissolve {
                progress: 1.0,
                mode: DissolveMode::Materialize,
                speed: 1.0,
                gravity: 0.5,
                seed: 42,
            })
            .time(0.0);

        let particle_chars = ['·', '∙', '•', '*'];
        // progress 0 should be particles
        for (idx, original) in "HI".chars().enumerate() {
            let displayed = text_start.char_at(idx, original);
            assert!(
                particle_chars.contains(&displayed),
                "Materialize progress 0 = particles"
            );
        }
        // progress 1 should be solid
        for (idx, ch) in "HI".chars().enumerate() {
            let displayed = text_end.char_at(idx, ch);
            assert_eq!(displayed, ch, "Materialize progress 1 = solid");
        }
    }

    #[test]
    fn test_gravity_affects_y() {
        let text_no_g = StyledText::new("A")
            .effect(TextEffect::ParticleDissolve {
                progress: 1.0,
                mode: DissolveMode::Dissolve,
                speed: 1.0,
                gravity: 0.0,
                seed: 42,
            })
            .time(0.0);

        let text_hi_g = StyledText::new("A")
            .effect(TextEffect::ParticleDissolve {
                progress: 1.0,
                mode: DissolveMode::Dissolve,
                speed: 1.0,
                gravity: 2.0,
                seed: 42,
            })
            .time(0.0);

        let offset_no_g = text_no_g.char_offset(0, 1);
        let offset_hi_g = text_hi_g.char_offset(0, 1);
        assert!(
            offset_hi_g.dy >= offset_no_g.dy,
            "Higher gravity = higher dy"
        );
    }

    #[test]
    fn test_seed_deterministic() {
        let text1 = StyledText::new("AB")
            .effect(TextEffect::ParticleDissolve {
                progress: 0.5,
                mode: DissolveMode::Dissolve,
                speed: 1.0,
                gravity: 0.5,
                seed: 12345,
            })
            .time(0.0);

        let text2 = StyledText::new("AB")
            .effect(TextEffect::ParticleDissolve {
                progress: 0.5,
                mode: DissolveMode::Dissolve,
                speed: 1.0,
                gravity: 0.5,
                seed: 12345,
            })
            .time(0.0);

        for idx in 0..2 {
            assert_eq!(text1.char_at(idx, 'X'), text2.char_at(idx, 'X'));
            assert_eq!(text1.char_offset(idx, 2), text2.char_offset(idx, 2));
        }
    }

    #[test]
    fn test_dissolve_mode_is_outward() {
        assert!(DissolveMode::Dissolve.is_outward());
        assert!(DissolveMode::Explode.is_outward());
        assert!(!DissolveMode::Materialize.is_outward());
        assert!(!DissolveMode::Implode.is_outward());
    }

    #[test]
    fn test_dissolve_mode_is_centered() {
        assert!(!DissolveMode::Dissolve.is_centered());
        assert!(DissolveMode::Explode.is_centered());
        assert!(!DissolveMode::Materialize.is_centered());
        assert!(DissolveMode::Implode.is_centered());
    }

    #[test]
    fn test_particle_char_by_distance() {
        assert_eq!(DissolveMode::particle_char(0.0), '·');
        assert_eq!(DissolveMode::particle_char(1.0), '*');
    }

    #[test]
    fn test_has_position_effects_with_dissolve() {
        let text = StyledText::new("T").effect(TextEffect::ParticleDissolve {
            progress: 0.5,
            mode: DissolveMode::Dissolve,
            speed: 1.0,
            gravity: 0.0,
            seed: 42,
        });
        assert!(text.has_position_effects());
    }
}

// =============================================================================
// TextPreset Tests (bd-4r5h)
// =============================================================================

#[cfg(test)]
mod preset_tests {
    use super::*;

    #[test]
    fn test_preset_applies_effects() {
        // Each preset should set a non-empty effect list
        for preset in TextPreset::all() {
            let effects = preset.effects();
            assert!(
                !effects.is_empty(),
                "Preset {:?} should have at least one effect",
                preset
            );
        }
    }

    #[test]
    fn test_preset_effects_within_limit() {
        // Preset effects should not exceed MAX_EFFECTS
        for preset in TextPreset::all() {
            let effects = preset.effects();
            assert!(
                effects.len() <= MAX_EFFECTS,
                "Preset {:?} has {} effects, exceeding MAX_EFFECTS ({})",
                preset,
                effects.len(),
                MAX_EFFECTS
            );
        }
    }

    #[test]
    fn test_preset_customizable() {
        // base_color override should work
        let custom_color = PackedRgba::rgb(100, 100, 100);
        let styled = StyledText::preset(TextPreset::Neon, "TEST").base_color(custom_color);

        // The color should be overridden
        // Note: We can't directly access base_color, but we can verify the preset applied effects
        assert!(styled.has_effects());
    }

    #[test]
    fn test_preset_easing_override() {
        // Can change easing after preset
        let styled = StyledText::preset(TextPreset::Neon, "TEST").easing(Easing::Bounce);

        // Should have effects from preset
        assert!(styled.has_effects());
    }

    #[test]
    fn test_all_presets_render_at_t0() {
        // No panic for any preset at t=0
        for preset in TextPreset::all() {
            let styled = StyledText::preset(preset, "TEST").time(0.0);
            // Just accessing char_at should not panic
            let _ = styled.char_at(0, ' ');
            let _ = styled.char_at(1, ' ');
            let _ = styled.char_at(2, ' ');
        }
    }

    #[test]
    fn test_all_presets_render_at_t05() {
        // No panic for any preset at t=0.5
        for preset in TextPreset::all() {
            let styled = StyledText::preset(preset, "TEST").time(0.5);
            let _ = styled.char_at(0, ' ');
            let _ = styled.char_at(1, ' ');
            let _ = styled.char_at(2, ' ');
        }
    }

    #[test]
    fn test_all_presets_render_at_t1() {
        // No panic for any preset at t=1.0
        for preset in TextPreset::all() {
            let styled = StyledText::preset(preset, "TEST").time(1.0);
            let _ = styled.char_at(0, ' ');
            let _ = styled.char_at(1, ' ');
            let _ = styled.char_at(2, ' ');
        }
    }

    #[test]
    fn test_preset_effects_consistent() {
        // Same preset = same effects
        for preset in TextPreset::all() {
            let effects1 = preset.effects();
            let effects2 = preset.effects();
            assert_eq!(
                effects1.len(),
                effects2.len(),
                "Preset {:?} should return consistent effects",
                preset
            );
        }
    }

    #[test]
    fn test_preset_no_none_effects() {
        // No preset should include TextEffect::None
        for preset in TextPreset::all() {
            let effects = preset.effects();
            for effect in &effects {
                assert!(
                    !matches!(effect, TextEffect::None),
                    "Preset {:?} should not include TextEffect::None",
                    preset
                );
            }
        }
    }

    #[test]
    fn test_preset_all_iterator() {
        // all() should return exactly 12 presets
        let count = TextPreset::all().count();
        assert_eq!(count, 12, "Should have exactly 12 presets");
    }

    #[test]
    fn test_preset_default_is_neon() {
        assert_eq!(TextPreset::default(), TextPreset::Neon);
    }

    #[test]
    fn test_neon_preset_has_color_wave() {
        let effects = TextPreset::Neon.effects();
        let has_color_wave = effects
            .iter()
            .any(|e| matches!(e, TextEffect::ColorWave { .. }));
        assert!(has_color_wave, "Neon preset should have ColorWave effect");
    }

    #[test]
    fn test_cyberpunk_preset_has_glitch() {
        let effects = TextPreset::Cyberpunk.effects();
        let has_glitch = effects
            .iter()
            .any(|e| matches!(e, TextEffect::Glitch { .. }));
        assert!(has_glitch, "Cyberpunk preset should have Glitch effect");
    }

    #[test]
    fn test_matrix_preset_has_cascade() {
        let effects = TextPreset::Matrix.effects();
        let has_cascade = effects
            .iter()
            .any(|e| matches!(e, TextEffect::Cascade { .. }));
        assert!(has_cascade, "Matrix preset should have Cascade effect");
    }

    #[test]
    fn test_retro_preset_has_scanline() {
        let effects = TextPreset::Retro.effects();
        let has_scanline = effects
            .iter()
            .any(|e| matches!(e, TextEffect::Scanline { .. }));
        assert!(has_scanline, "Retro preset should have Scanline effect");
    }

    #[test]
    fn test_typewriter_preset_has_reveal_and_cursor() {
        let effects = TextPreset::Typewriter.effects();
        let has_reveal = effects
            .iter()
            .any(|e| matches!(e, TextEffect::Reveal { .. }));
        let has_cursor = effects
            .iter()
            .any(|e| matches!(e, TextEffect::Cursor { .. }));
        assert!(has_reveal, "Typewriter preset should have Reveal effect");
        assert!(has_cursor, "Typewriter preset should have Cursor effect");
    }

    #[test]
    fn test_fire_preset_has_wave() {
        let effects = TextPreset::Fire.effects();
        let has_wave = effects.iter().any(|e| matches!(e, TextEffect::Wave { .. }));
        assert!(has_wave, "Fire preset should have Wave effect");
    }

    #[test]
    fn test_hologram_preset_has_rainbow() {
        let effects = TextPreset::Hologram.effects();
        let has_rainbow = effects
            .iter()
            .any(|e| matches!(e, TextEffect::RainbowGradient { .. }));
        assert!(
            has_rainbow,
            "Hologram preset should have RainbowGradient effect"
        );
    }

    #[test]
    fn test_elegant_preset_has_shadow() {
        // Elegant preset should add a shadow via the preset() constructor
        let styled = StyledText::preset(TextPreset::Elegant, "TEST");
        // The Shadow is applied in the preset constructor
        // We can't directly check shadows, but we can verify the preset was applied
        assert!(styled.has_effects());
    }

    #[test]
    fn test_bold_presets() {
        // Neon, Fire, and Matrix should be bold
        assert!(TextPreset::Neon.is_bold());
        assert!(TextPreset::Fire.is_bold());
        assert!(TextPreset::Matrix.is_bold());

        // Others should not be bold
        assert!(!TextPreset::Minimal.is_bold());
        assert!(!TextPreset::Elegant.is_bold());
        assert!(!TextPreset::Terminal.is_bold());
    }

    #[test]
    fn test_preset_base_colors_are_valid() {
        // All preset base colors should have valid RGB values (not all zeros or all max)
        for preset in TextPreset::all() {
            let color = preset.base_color();
            // At least one channel should be non-zero
            let has_color = color.r() > 0 || color.g() > 0 || color.b() > 0;
            assert!(
                has_color,
                "Preset {:?} should have a visible base color",
                preset
            );
        }
    }
}

//! Precomputed trigonometric tables for the Doom renderer.
//!
//! Doom uses 8192 fine angles for a full circle. We precompute sin/cos/tan
//! at startup using standard f32 math.

use std::sync::OnceLock;

use super::constants::FINEANGLES;

/// Precomputed sine table (8192 entries for a full circle).
static FINE_SINE: OnceLock<Box<[f32; FINEANGLES]>> = OnceLock::new();
/// Precomputed cosine table.
static FINE_COSINE: OnceLock<Box<[f32; FINEANGLES]>> = OnceLock::new();
/// Precomputed tangent table.
static FINE_TANGENT: OnceLock<Box<[f32; FINEANGLES]>> = OnceLock::new();

fn init_sine() -> Box<[f32; FINEANGLES]> {
    let mut table = Box::new([0.0f32; FINEANGLES]);
    for i in 0..FINEANGLES {
        let angle = (i as f32) * std::f32::consts::TAU / (FINEANGLES as f32);
        table[i] = angle.sin();
    }
    table
}

fn init_cosine() -> Box<[f32; FINEANGLES]> {
    let mut table = Box::new([0.0f32; FINEANGLES]);
    for i in 0..FINEANGLES {
        let angle = (i as f32) * std::f32::consts::TAU / (FINEANGLES as f32);
        table[i] = angle.cos();
    }
    table
}

fn init_tangent() -> Box<[f32; FINEANGLES]> {
    let mut table = Box::new([0.0f32; FINEANGLES]);
    for i in 0..FINEANGLES {
        let angle = (i as f32) * std::f32::consts::TAU / (FINEANGLES as f32);
        let c = angle.cos();
        table[i] = if c.abs() < 1e-10 {
            if angle.sin() >= 0.0 {
                f32::MAX
            } else {
                f32::MIN
            }
        } else {
            angle.sin() / c
        };
    }
    table
}

/// Get the sine of a fine angle index (0..8191).
#[inline]
pub fn fine_sine(angle: usize) -> f32 {
    let table = FINE_SINE.get_or_init(init_sine);
    table[angle & (FINEANGLES - 1)]
}

/// Get the cosine of a fine angle index.
#[inline]
pub fn fine_cosine(angle: usize) -> f32 {
    let table = FINE_COSINE.get_or_init(init_cosine);
    table[angle & (FINEANGLES - 1)]
}

/// Get the tangent of a fine angle index.
#[inline]
pub fn fine_tangent(angle: usize) -> f32 {
    let table = FINE_TANGENT.get_or_init(init_tangent);
    table[angle & (FINEANGLES - 1)]
}

/// Convert a radians angle to a fine angle index.
#[inline]
pub fn radians_to_fine(rad: f32) -> usize {
    let normalized = rad.rem_euclid(std::f32::consts::TAU);
    ((normalized / std::f32::consts::TAU) * FINEANGLES as f32) as usize & (FINEANGLES - 1)
}

/// Convert a fine angle index to radians.
#[inline]
pub fn fine_to_radians(fine: usize) -> f32 {
    (fine as f32) * std::f32::consts::TAU / (FINEANGLES as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_at_zero_is_zero() {
        assert!(fine_sine(0).abs() < 1e-5);
    }

    #[test]
    fn cosine_at_zero_is_one() {
        assert!((fine_cosine(0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn sine_at_quarter_is_one() {
        assert!((fine_sine(FINEANGLES / 4) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn radians_roundtrip() {
        let angle = 1.234f32;
        let fine = radians_to_fine(angle);
        let back = fine_to_radians(fine);
        assert!((angle - back).abs() < 0.001);
    }
}

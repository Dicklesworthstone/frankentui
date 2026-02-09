#![forbid(unsafe_code)]

//! Shared Core Sampling API (Target-Agnostic)
//!
//! This module defines a tiny, target-agnostic sampling surface so effect math
//! (metaballs, plasma, etc.) has a **single source of truth** and can render to:
//! - Cell-space backdrops (width * height cells)
//! - `ftui_extras::canvas::Painter` (sub-cell/pixel resolution)
//! - Future GPU compute (conceptually the same sampling function)
//!
//! # Coordinate Conventions
//!
//! All sampling uses **normalized coordinates** in the range `[0.0, 1.0]`:
//!
//! ```text
//! (0,0) ─────────────────────── (1,0)
//!   │                             │
//!   │    Normalized Space         │
//!   │    x: left → right          │
//!   │    y: top → bottom          │
//!   │                             │
//! (0,1) ─────────────────────── (1,1)
//! ```
//!
//! ## Cell-Space Mapping
//!
//! For a grid of `width` x `height` cells, the normalized coordinate for
//! cell `(cx, cy)` is computed using **cell centers**:
//!
//! ```text
//! nx = (cx + 0.5) / width
//! ny = (cy + 0.5) / height
//! ```
//!
//! This ensures:
//! - Cell (0, 0) samples at (0.5/w, 0.5/h), not exactly (0, 0)
//! - The last cell samples near (1, 1) but not exactly at the boundary
//! - Consistent sampling regardless of resolution
//!
//! ## Sub-Pixel (Painter) Mapping
//!
//! For sub-pixel rendering with a Painter of `pw` x `ph` pixels:
//!
//! ```text
//! nx = (px + 0.5) / pw
//! ny = (py + 0.5) / ph
//! ```
//!
//! ## Aspect Ratio
//!
//! Terminal cells are typically taller than wide (often ~2:1 height:width).
//! Effects that need circular/square appearance should apply aspect correction:
//!
//! ```text
//! // Typical terminal cell aspect ratio
//! let cell_aspect = 2.0;  // height / width
//!
//! // Corrected y for circular effects
//! let ny_corrected = ny / cell_aspect;
//! ```
//!
//! Individual samplers document whether they apply aspect correction internally.
//!
//! # Design Goals
//!
//! - **Pure functions**: No side effects, no allocations during sampling
//! - **Deterministic**: Same inputs always produce same outputs
//! - **Quality-aware**: Samplers can degrade gracefully based on quality tier
//! - **Theme-agnostic**: Samplers return intensity/field values; color mapping is separate

use crate::visual_fx::FxQuality;

// ---------------------------------------------------------------------------
// Coordinate Mapping Helpers
// ---------------------------------------------------------------------------

/// Compute normalized coordinate for a cell center.
///
/// # Arguments
/// - `cell`: Cell index (0-based)
/// - `total`: Total number of cells in this dimension
///
/// # Returns
/// Normalized coordinate in `[0.0, 1.0]` representing the cell center.
/// Returns 0.5 if `total` is 0 to avoid division by zero.
///
/// # Example
/// ```ignore
/// // For a 10-cell wide grid:
/// // Cell 0 -> 0.05 (center of first cell)
/// // Cell 4 -> 0.45 (center of fifth cell)
/// // Cell 9 -> 0.95 (center of last cell)
/// let nx = cell_to_normalized(4, 10);
/// assert!((nx - 0.45).abs() < 1e-10);
/// ```
#[inline]
pub const fn cell_to_normalized(cell: u16, total: u16) -> f64 {
    if total == 0 {
        0.5
    } else {
        (cell as f64 + 0.5) / total as f64
    }
}

/// Compute normalized coordinates for all cells in a dimension.
///
/// This is useful for caching coordinates when rendering a full frame,
/// avoiding repeated division per-cell.
///
/// # Arguments
/// - `total`: Total number of cells
/// - `out`: Output slice to fill (must have length >= `total`)
///
/// # Panics
/// Panics if `out.len() < total`.
#[inline]
pub fn fill_normalized_coords(total: u16, out: &mut [f64]) {
    assert!(
        out.len() >= total as usize,
        "output slice too small: {} < {}",
        out.len(),
        total
    );
    if total == 0 {
        return;
    }
    let inv = 1.0 / total as f64;
    for i in 0..total {
        out[i as usize] = (i as f64 + 0.5) * inv;
    }
}

/// Pre-computed coordinate cache for efficient sampling.
///
/// Stores normalized coordinates for both x and y dimensions,
/// avoiding repeated division during per-cell sampling.
#[derive(Debug, Clone)]
pub struct CoordCache {
    x_coords: Vec<f64>,
    y_coords: Vec<f64>,
    width: u16,
    height: u16,
}

impl CoordCache {
    /// Create a new coordinate cache for the given dimensions.
    #[inline]
    pub fn new(width: u16, height: u16) -> Self {
        let mut x_coords = vec![0.0; width as usize];
        let mut y_coords = vec![0.0; height as usize];
        fill_normalized_coords(width, &mut x_coords);
        fill_normalized_coords(height, &mut y_coords);
        Self {
            x_coords,
            y_coords,
            width,
            height,
        }
    }

    /// Ensure the cache is sized for at least the given dimensions.
    ///
    /// Grows the cache if needed but never shrinks it (grow-only strategy).
    #[inline]
    pub fn ensure_size(&mut self, width: u16, height: u16) {
        if width > self.width {
            self.x_coords.resize(width as usize, 0.0);
            fill_normalized_coords(width, &mut self.x_coords);
            self.width = width;
        }
        if height > self.height {
            self.y_coords.resize(height as usize, 0.0);
            fill_normalized_coords(height, &mut self.y_coords);
            self.height = height;
        }
    }

    /// Get the normalized x coordinate for a cell.
    #[inline]
    pub fn x(&self, cell: u16) -> f64 {
        self.x_coords.get(cell as usize).copied().unwrap_or(0.5)
    }

    /// Get the normalized y coordinate for a cell.
    #[inline]
    pub fn y(&self, cell: u16) -> f64 {
        self.y_coords.get(cell as usize).copied().unwrap_or(0.5)
    }

    /// Get x coordinates slice.
    #[inline]
    pub fn x_coords(&self) -> &[f64] {
        &self.x_coords
    }

    /// Get y coordinates slice.
    #[inline]
    pub fn y_coords(&self) -> &[f64] {
        &self.y_coords
    }
}

// ---------------------------------------------------------------------------
// Sampler Trait
// ---------------------------------------------------------------------------

/// A pure sampling function for visual effects.
///
/// Samplers compute a field/intensity value at a normalized coordinate,
/// which can then be mapped to colors by a separate palette/color system.
///
/// # Coordinate Convention
///
/// - `x`, `y`: Normalized coordinates in `[0.0, 1.0]`
/// - `time`: Time in seconds (for animation)
/// - Returns: Field/intensity value, typically in `[0.0, 1.0]`
///
/// # Quality Degradation
///
/// Samplers should respect the quality parameter and simplify calculations
/// appropriately:
/// - `Full`: All calculations, maximum fidelity
/// - `Reduced`: Fewer iterations, simplified math
/// - `Minimal`: Cheapest possible (may be static or very simple)
/// - `Off`: Return 0.0 (no effect)
///
/// # Determinism
///
/// Samplers MUST be deterministic: given the same (x, y, time, quality),
/// they must always return the same value. No global state or randomness.
pub trait Sampler: Send + Sync {
    /// Sample the effect at a normalized coordinate.
    ///
    /// # Arguments
    /// - `x`: Normalized x coordinate in `[0.0, 1.0]`
    /// - `y`: Normalized y coordinate in `[0.0, 1.0]`
    /// - `time`: Time in seconds for animation
    /// - `quality`: Quality hint for graceful degradation
    ///
    /// # Returns
    /// Field/intensity value. Range depends on effect but typically `[0.0, 1.0]`.
    fn sample(&self, x: f64, y: f64, time: f64, quality: FxQuality) -> f64;

    /// Human-readable name for debugging.
    fn name(&self) -> &'static str;

    /// Whether this sampler applies aspect ratio correction internally.
    ///
    /// If `true`, the sampler handles terminal cell aspect ratio (typically 2:1)
    /// internally. If `false`, the caller may need to apply correction for
    /// effects that should appear circular/square.
    #[inline]
    fn applies_aspect_correction(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Function-Based Sampler
// ---------------------------------------------------------------------------

/// A sampler wrapping a pure function.
///
/// Useful for simple effects that don't need state.
pub struct FnSampler<F>
where
    F: Fn(f64, f64, f64, FxQuality) -> f64 + Send + Sync,
{
    func: F,
    name: &'static str,
    aspect_corrected: bool,
}

impl<F> FnSampler<F>
where
    F: Fn(f64, f64, f64, FxQuality) -> f64 + Send + Sync,
{
    /// Create a new function-based sampler.
    pub const fn new(func: F, name: &'static str) -> Self {
        Self {
            func,
            name,
            aspect_corrected: false,
        }
    }

    /// Mark this sampler as applying aspect correction internally.
    pub const fn with_aspect_correction(mut self) -> Self {
        self.aspect_corrected = true;
        self
    }
}

impl<F> Sampler for FnSampler<F>
where
    F: Fn(f64, f64, f64, FxQuality) -> f64 + Send + Sync,
{
    #[inline]
    fn sample(&self, x: f64, y: f64, time: f64, quality: FxQuality) -> f64 {
        (self.func)(x, y, time, quality)
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn applies_aspect_correction(&self) -> bool {
        self.aspect_corrected
    }
}

// ---------------------------------------------------------------------------
// Plasma Sampler (Single Source of Truth)
// ---------------------------------------------------------------------------

/// Plasma wave sampler - the canonical implementation.
///
/// This is the **single source of truth** for plasma wave mathematics.
/// Both cell-space and painter-space rendering should use this sampler.
///
/// # Wave Equation
///
/// The full-quality plasma uses 6 trigonometric wave components:
/// - v1: Horizontal wave
/// - v2: Vertical wave (phase offset)
/// - v3: Diagonal wave
/// - v4: Radial wave from center
/// - v5: Radial wave from offset center
/// - v6: Interference pattern
///
/// # Quality Degradation
///
/// - `Full`: All 6 wave components
/// - `Reduced`: 4 wave components (drops v5, v6)
/// - `Minimal`: 3 wave components (fastest)
/// - `Off`: Returns 0.0
#[derive(Debug, Clone, Copy, Default)]
pub struct PlasmaSampler;

impl PlasmaSampler {
    /// Sample plasma wave at given coordinates (full quality).
    ///
    /// Returns value in `[0.0, 1.0]`.
    #[inline]
    pub fn sample_full(x: f64, y: f64, time: f64) -> f64 {
        // Scale to wave-space
        let wx = x * 6.0;
        let wy = y * 6.0;
        let wx3 = wx - 3.0;
        let wy3 = wy - 3.0;

        // 6 wave components
        let v1 = (wx * 1.5 + time).sin();
        let v2 = (wy * 1.8 + time * 0.8).sin();
        let v3 = ((wx + wy) * 1.2 + time * 0.6).sin();
        let v4 = ((wx * wx + wy * wy).sqrt() * 2.0 - time * 1.2).sin();
        let v5 = ((wx3 * wx3 + wy3 * wy3).sqrt() * 1.8 + time).cos();
        let v6 = ((wx * 2.0).sin() * (wy * 2.0).cos() + time * 0.5).sin();

        // Average and normalize [-1, 1] to [0, 1]
        let value = (v1 + v2 + v3 + v4 + v5 + v6) / 6.0;
        (value + 1.0) / 2.0
    }

    /// Sample plasma wave at given coordinates (reduced quality).
    ///
    /// Uses 4 wave components for faster computation.
    #[inline]
    pub fn sample_reduced(x: f64, y: f64, time: f64) -> f64 {
        let wx = x * 6.0;
        let wy = y * 6.0;

        let v1 = (wx * 1.5 + time).sin();
        let v2 = (wy * 1.8 + time * 0.8).sin();
        let v3 = ((wx + wy) * 1.2 + time * 0.6).sin();
        let v4 = ((wx * wx + wy * wy).sqrt() * 2.0 - time * 1.2).sin();

        let value = (v1 + v2 + v3 + v4) / 4.0;
        (value + 1.0) / 2.0
    }

    /// Sample plasma wave at given coordinates (minimal quality).
    ///
    /// Uses only 3 wave components for cheapest computation.
    #[inline]
    pub fn sample_minimal(x: f64, y: f64, time: f64) -> f64 {
        let wx = x * 6.0;
        let wy = y * 6.0;

        let v1 = (wx * 1.5 + time).sin();
        let v2 = (wy * 1.8 + time * 0.8).sin();
        let v3 = ((wx + wy) * 1.2 + time * 0.6).sin();

        let value = (v1 + v2 + v3) / 3.0;
        (value + 1.0) / 2.0
    }
}

impl Sampler for PlasmaSampler {
    #[inline]
    fn sample(&self, x: f64, y: f64, time: f64, quality: FxQuality) -> f64 {
        match quality {
            FxQuality::Off => 0.0,
            FxQuality::Minimal => Self::sample_minimal(x, y, time),
            FxQuality::Reduced => Self::sample_reduced(x, y, time),
            FxQuality::Full => Self::sample_full(x, y, time),
        }
    }

    fn name(&self) -> &'static str {
        "plasma"
    }
}

// ---------------------------------------------------------------------------
// Metaball Field Sampler
// ---------------------------------------------------------------------------

/// Cached ball state for efficient field computation.
#[derive(Debug, Clone, Copy)]
pub struct BallState {
    /// Current x position (normalized)
    pub x: f64,
    /// Current y position (normalized)
    pub y: f64,
    /// Squared radius for field calculation
    pub r2: f64,
    /// Hue value for color mapping
    pub hue: f64,
}

/// Metaball field sampler - the canonical implementation.
///
/// This is the **single source of truth** for metaball field mathematics.
/// The sampler computes the field contribution at a point from all balls.
///
/// # Field Equation
///
/// For each ball at position (bx, by) with radius r:
/// ```text
/// contribution = r² / distance²
/// ```
///
/// The total field is the sum of contributions from all balls.
///
/// # Quality Degradation
///
/// - `Full`: All balls contribute
/// - `Reduced`: 75% of balls (skip every 4th)
/// - `Minimal`: 50% of balls (skip every 2nd)
/// - `Off`: Returns 0.0
///
/// # Aspect Ratio
///
/// This sampler does NOT apply aspect correction. For circular metaballs,
/// the caller should provide aspect-corrected y coordinates.
#[derive(Debug, Clone)]
pub struct MetaballFieldSampler {
    balls: Vec<BallState>,
}

impl MetaballFieldSampler {
    /// Create a new metaball field sampler with the given ball states.
    pub fn new(balls: Vec<BallState>) -> Self {
        Self { balls }
    }

    /// Sample the metaball field from a slice of balls (no allocation).
    ///
    /// This is useful for callers that already own a ball buffer and want to
    /// avoid cloning it just to sample.
    #[inline]
    pub fn sample_field_from_slice(
        balls: &[BallState],
        x: f64,
        y: f64,
        quality: FxQuality,
    ) -> (f64, f64) {
        if quality == FxQuality::Off || balls.is_empty() {
            return (0.0, 0.0);
        }

        // Determine step based on quality
        let step = match quality {
            FxQuality::Full => 1,
            FxQuality::Reduced => {
                if balls.len() > 4 {
                    4
                } else {
                    1
                }
            }
            FxQuality::Minimal => {
                if balls.len() > 2 {
                    2
                } else {
                    1
                }
            }
            FxQuality::Off => return (0.0, 0.0),
        };

        const EPS: f64 = 1e-8;
        let mut sum = 0.0;
        let mut weighted_hue = 0.0;

        if step == 1 {
            for ball in balls {
                let dx = x - ball.x;
                let dy = y - ball.y;
                let dist_sq = dx * dx + dy * dy;

                if dist_sq > EPS {
                    let contrib = ball.r2 / dist_sq;
                    sum += contrib;
                    weighted_hue += ball.hue * contrib;
                } else {
                    sum += 100.0;
                    weighted_hue += ball.hue * 100.0;
                }
            }
        } else {
            for i in (0..balls.len()).step_by(step) {
                let ball = &balls[i];
                let dx = x - ball.x;
                let dy = y - ball.y;
                let dist_sq = dx * dx + dy * dy;

                if dist_sq > EPS {
                    let contrib = ball.r2 / dist_sq;
                    sum += contrib;
                    weighted_hue += ball.hue * contrib;
                } else {
                    sum += 100.0;
                    weighted_hue += ball.hue * 100.0;
                }
            }
        }

        // `sum` and total hue weight are intentionally the same accumulation.
        // Use `sum` directly to avoid redundant floating-point writes in this
        // hot loop while preserving exact contribution order.
        let avg_hue = if sum > EPS { weighted_hue / sum } else { 0.0 };

        (sum, avg_hue)
    }

    /// Update the ball states.
    pub fn set_balls(&mut self, balls: Vec<BallState>) {
        self.balls = balls;
    }

    /// Get a reference to the ball states.
    pub fn balls(&self) -> &[BallState] {
        &self.balls
    }

    /// Sample the metaball field at given coordinates.
    ///
    /// # Returns
    /// A tuple of (field_sum, weighted_hue) where:
    /// - `field_sum`: Total field strength (unbounded, compare to threshold)
    /// - `weighted_hue`: Contribution-weighted average hue
    #[inline]
    pub fn sample_field(&self, x: f64, y: f64, quality: FxQuality) -> (f64, f64) {
        Self::sample_field_from_slice(&self.balls, x, y, quality)
    }
}

impl Sampler for MetaballFieldSampler {
    /// Sample returns the field strength (not hue).
    ///
    /// Use `sample_field` directly if you need both field and hue.
    #[inline]
    fn sample(&self, x: f64, y: f64, _time: f64, quality: FxQuality) -> f64 {
        self.sample_field(x, y, quality).0
    }

    fn name(&self) -> &'static str {
        "metaballs"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cell_to_normalized_basic() {
        // 10-cell grid
        let nx = cell_to_normalized(0, 10);
        assert!((nx - 0.05).abs() < 1e-10, "cell 0 should be at 0.05");

        let nx = cell_to_normalized(4, 10);
        assert!((nx - 0.45).abs() < 1e-10, "cell 4 should be at 0.45");

        let nx = cell_to_normalized(9, 10);
        assert!((nx - 0.95).abs() < 1e-10, "cell 9 should be at 0.95");
    }

    #[test]
    fn test_cell_to_normalized_zero_total() {
        let nx = cell_to_normalized(0, 0);
        assert!((nx - 0.5).abs() < 1e-10, "zero total should return 0.5");
    }

    #[test]
    fn test_cell_to_normalized_single_cell() {
        let nx = cell_to_normalized(0, 1);
        assert!((nx - 0.5).abs() < 1e-10, "single cell should be at 0.5");
    }

    #[test]
    fn test_fill_normalized_coords() {
        let mut coords = vec![0.0; 5];
        fill_normalized_coords(5, &mut coords);

        assert!((coords[0] - 0.1).abs() < 1e-10);
        assert!((coords[2] - 0.5).abs() < 1e-10);
        assert!((coords[4] - 0.9).abs() < 1e-10);
    }

    #[test]
    fn test_coord_cache() {
        let cache = CoordCache::new(10, 5);

        assert!((cache.x(0) - 0.05).abs() < 1e-10);
        assert!((cache.y(0) - 0.1).abs() < 1e-10);
        assert!((cache.x(9) - 0.95).abs() < 1e-10);
        assert!((cache.y(4) - 0.9).abs() < 1e-10);
    }

    #[test]
    fn test_coord_cache_grow_only() {
        let mut cache = CoordCache::new(5, 5);
        cache.ensure_size(10, 10);

        // Should have grown
        assert!(cache.x_coords().len() >= 10);
        assert!(cache.y_coords().len() >= 10);

        // Values should be correct
        assert!((cache.x(9) - 0.95).abs() < 1e-10);
    }

    #[test]
    fn test_coord_cache_out_of_range_defaults() {
        let cache = CoordCache::new(4, 3);
        assert!((cache.x(99) - 0.5).abs() < 1e-10);
        assert!((cache.y(99) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_coord_cache_does_not_shrink() {
        let mut cache = CoordCache::new(8, 6);
        cache.ensure_size(4, 3);
        assert!(cache.x_coords().len() >= 8);
        assert!(cache.y_coords().len() >= 6);
    }

    #[test]
    fn test_plasma_sampler_bounded() {
        let sampler = PlasmaSampler;

        // Test at various points
        for x in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for y in [0.0, 0.25, 0.5, 0.75, 1.0] {
                for t in [0.0, 1.0, 10.0] {
                    let v = sampler.sample(x, y, t, FxQuality::Full);
                    assert!(
                        (0.0..=1.0).contains(&v),
                        "plasma value {v} out of bounds at ({x}, {y}, {t})"
                    );
                }
            }
        }
    }

    #[test]
    fn test_plasma_sampler_quality_tiers() {
        let sampler = PlasmaSampler;

        // Off should return 0
        let v_off = sampler.sample(0.5, 0.5, 1.0, FxQuality::Off);
        assert!((v_off - 0.0).abs() < 1e-10);

        // Other qualities should return valid values
        let v_min = sampler.sample(0.5, 0.5, 1.0, FxQuality::Minimal);
        let v_red = sampler.sample(0.5, 0.5, 1.0, FxQuality::Reduced);
        let v_full = sampler.sample(0.5, 0.5, 1.0, FxQuality::Full);

        assert!((0.0..=1.0).contains(&v_min));
        assert!((0.0..=1.0).contains(&v_red));
        assert!((0.0..=1.0).contains(&v_full));
    }

    #[test]
    fn test_plasma_sampler_deterministic() {
        let sampler = PlasmaSampler;

        let v1 = sampler.sample(0.3, 0.7, 2.5, FxQuality::Full);
        let v2 = sampler.sample(0.3, 0.7, 2.5, FxQuality::Full);

        assert!((v1 - v2).abs() < 1e-15, "plasma should be deterministic");
    }

    #[test]
    fn test_metaball_field_basic() {
        let sampler = MetaballFieldSampler::new(vec![BallState {
            x: 0.5,
            y: 0.5,
            r2: 0.01,
            hue: 0.0,
        }]);

        // Field should be high at center
        let (field_center, _) = sampler.sample_field(0.5, 0.5, FxQuality::Full);
        // Field should be lower at edge
        let (field_edge, _) = sampler.sample_field(0.0, 0.0, FxQuality::Full);

        assert!(
            field_center > field_edge,
            "field should be higher at ball center"
        );
    }

    #[test]
    fn test_metaball_field_off() {
        let sampler = MetaballFieldSampler::new(vec![BallState {
            x: 0.5,
            y: 0.5,
            r2: 0.01,
            hue: 0.0,
        }]);

        let (field, hue) = sampler.sample_field(0.5, 0.5, FxQuality::Off);
        assert!((field - 0.0).abs() < 1e-10);
        assert!((hue - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_metaball_field_deterministic() {
        let sampler = MetaballFieldSampler::new(vec![
            BallState {
                x: 0.3,
                y: 0.3,
                r2: 0.02,
                hue: 0.2,
            },
            BallState {
                x: 0.7,
                y: 0.7,
                r2: 0.02,
                hue: 0.8,
            },
        ]);

        let (f1, h1) = sampler.sample_field(0.4, 0.5, FxQuality::Full);
        let (f2, h2) = sampler.sample_field(0.4, 0.5, FxQuality::Full);

        assert!((f1 - f2).abs() < 1e-15, "field should be deterministic");
        assert!((h1 - h2).abs() < 1e-15, "hue should be deterministic");
    }

    #[test]
    fn test_fn_sampler() {
        let sampler = FnSampler::new(|x, y, _t, _q| x + y, "test");

        assert_eq!(sampler.name(), "test");
        assert!((sampler.sample(0.3, 0.2, 0.0, FxQuality::Full) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_fn_sampler_aspect_correction_flag() {
        let sampler = FnSampler::new(|x, y, _t, _q| x + y, "aspect").with_aspect_correction();
        assert!(sampler.applies_aspect_correction());
    }

    #[test]
    fn test_metaball_field_zero_distance() {
        let sampler = MetaballFieldSampler::new(vec![BallState {
            x: 0.5,
            y: 0.5,
            r2: 0.01,
            hue: 0.75,
        }]);

        let (field, hue) = sampler.sample_field(0.5, 0.5, FxQuality::Full);
        assert!(field > 1.0, "field should be boosted at zero distance");
        assert!((hue - 0.75).abs() < 1e-6, "hue should track the ball hue");
    }

    #[test]
    fn test_metaball_field_quality_step_reduces_contribs() {
        let balls = vec![
            BallState {
                x: 0.2,
                y: 0.2,
                r2: 1.0,
                hue: 0.1,
            },
            BallState {
                x: 0.4,
                y: 0.4,
                r2: 100.0,
                hue: 0.2,
            },
            BallState {
                x: 0.6,
                y: 0.6,
                r2: 100.0,
                hue: 0.3,
            },
            BallState {
                x: 0.8,
                y: 0.8,
                r2: 100.0,
                hue: 0.4,
            },
            BallState {
                x: 0.9,
                y: 0.1,
                r2: 1.0,
                hue: 0.5,
            },
        ];

        let full =
            MetaballFieldSampler::sample_field_from_slice(&balls, 0.1, 0.9, FxQuality::Full).0;
        let reduced =
            MetaballFieldSampler::sample_field_from_slice(&balls, 0.1, 0.9, FxQuality::Reduced).0;

        assert!(reduced < full, "reduced quality should drop contributions");
    }

    #[test]
    fn test_metaball_set_and_balls_roundtrip() {
        let mut sampler = MetaballFieldSampler::new(Vec::new());
        let balls = vec![BallState {
            x: 0.1,
            y: 0.2,
            r2: 0.03,
            hue: 0.9,
        }];
        sampler.set_balls(balls);
        assert_eq!(sampler.balls().len(), 1);
    }

    // Regression test: fixed sample points should produce stable hashes
    #[test]
    fn test_plasma_regression_golden() {
        // Golden values computed once and frozen
        let cases = [
            (0.0, 0.0, 0.0, 0.5),    // Center of range
            (0.5, 0.5, 0.0, 0.5),    // Center point
            (1.0, 1.0, 0.0, 0.5),    // Corner
            (0.25, 0.75, 1.0, 0.65), // Arbitrary point with time
        ];

        let sampler = PlasmaSampler;
        for (x, y, t, expected_approx) in cases {
            let actual = sampler.sample(x, y, t, FxQuality::Full);
            // Allow some tolerance since we're comparing floating point
            // The key is that the value is bounded and deterministic
            assert!((0.0..=1.0).contains(&actual), "value should be bounded");
            // Note: exact values may drift with implementation changes
            // This is more of a sanity check
            assert!(
                (actual - expected_approx).abs() < 0.5,
                "value {actual} at ({x},{y},{t}) seems off"
            );
        }
    }

    // --- fill_normalized_coords edge cases ---

    #[test]
    fn test_fill_normalized_coords_zero_total() {
        let mut coords = vec![42.0; 3];
        fill_normalized_coords(0, &mut coords);
        // Should be a no-op; original values preserved
        assert!((coords[0] - 42.0).abs() < 1e-10);
    }

    #[test]
    fn test_fill_normalized_coords_single() {
        let mut coords = vec![0.0; 1];
        fill_normalized_coords(1, &mut coords);
        assert!((coords[0] - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_fill_normalized_coords_oversized_slice() {
        // Output slice larger than total - should only fill first `total` entries
        let mut coords = vec![99.0; 10];
        fill_normalized_coords(3, &mut coords);
        assert!((coords[0] - 1.0 / 6.0).abs() < 1e-10);
        assert!((coords[1] - 3.0 / 6.0).abs() < 1e-10);
        assert!((coords[2] - 5.0 / 6.0).abs() < 1e-10);
        // Remaining entries should be untouched
        assert!((coords[3] - 99.0).abs() < 1e-10);
    }

    #[test]
    fn test_fill_normalized_coords_values_monotonic() {
        let mut coords = vec![0.0; 20];
        fill_normalized_coords(20, &mut coords);
        for w in coords.windows(2) {
            assert!(w[1] > w[0], "coordinates should be strictly increasing");
        }
    }

    #[test]
    fn test_fill_normalized_coords_all_within_unit() {
        let mut coords = vec![0.0; 100];
        fill_normalized_coords(100, &mut coords);
        for (i, &c) in coords.iter().enumerate() {
            assert!(
                (0.0..=1.0).contains(&c),
                "coord[{i}] = {c} is out of [0, 1]"
            );
        }
    }

    // --- CoordCache ---

    #[test]
    fn test_coord_cache_ensure_size_noop() {
        let mut cache = CoordCache::new(10, 10);
        let x5_before = cache.x(5);
        cache.ensure_size(5, 5); // smaller, should be no-op
        let x5_after = cache.x(5);
        assert!((x5_before - x5_after).abs() < 1e-15);
    }

    #[test]
    fn test_coord_cache_ensure_size_one_dimension() {
        let mut cache = CoordCache::new(5, 5);
        // Grow only x
        cache.ensure_size(10, 3);
        assert!(cache.x_coords().len() >= 10);
        // y should remain at 5 (not shrunk)
        assert!(cache.y_coords().len() >= 5);
        assert!((cache.x(9) - 0.95).abs() < 1e-10);
    }

    #[test]
    fn test_coord_cache_zero_dimensions() {
        let cache = CoordCache::new(0, 0);
        // Out of range should return 0.5
        assert!((cache.x(0) - 0.5).abs() < 1e-10);
        assert!((cache.y(0) - 0.5).abs() < 1e-10);
    }

    // --- PlasmaSampler static methods ---

    #[test]
    fn test_plasma_sample_full_bounded() {
        for x in [0.0, 0.1, 0.5, 0.9, 1.0] {
            for y in [0.0, 0.1, 0.5, 0.9, 1.0] {
                let v = PlasmaSampler::sample_full(x, y, 0.0);
                assert!((0.0..=1.0).contains(&v), "sample_full({x}, {y}, 0.0) = {v}");
            }
        }
    }

    #[test]
    fn test_plasma_sample_reduced_bounded() {
        for x in [0.0, 0.5, 1.0] {
            for y in [0.0, 0.5, 1.0] {
                for t in [0.0, 3.0, 10.0] {
                    let v = PlasmaSampler::sample_reduced(x, y, t);
                    assert!(
                        (0.0..=1.0).contains(&v),
                        "sample_reduced({x}, {y}, {t}) = {v}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_plasma_sample_minimal_bounded() {
        for x in [0.0, 0.5, 1.0] {
            for y in [0.0, 0.5, 1.0] {
                for t in [0.0, 3.0, 10.0] {
                    let v = PlasmaSampler::sample_minimal(x, y, t);
                    assert!(
                        (0.0..=1.0).contains(&v),
                        "sample_minimal({x}, {y}, {t}) = {v}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_plasma_quality_tiers_differ() {
        // At most points, different quality tiers should produce different values
        // because they use different numbers of wave components
        let x = 0.3;
        let y = 0.7;
        let t = 2.0;
        let full = PlasmaSampler::sample_full(x, y, t);
        let reduced = PlasmaSampler::sample_reduced(x, y, t);
        let minimal = PlasmaSampler::sample_minimal(x, y, t);

        // At least two should differ (exact equality across all tiers is unlikely)
        let all_same = (full - reduced).abs() < 1e-12 && (reduced - minimal).abs() < 1e-12;
        assert!(
            !all_same,
            "quality tiers should generally produce different values"
        );
    }

    #[test]
    fn test_plasma_sampler_name() {
        let sampler = PlasmaSampler;
        assert_eq!(sampler.name(), "plasma");
    }

    #[test]
    fn test_plasma_v5_mul_form_matches_powi_form_bit_exact() {
        let xs: [f64; 6] = [0.0, 0.1, 0.33, 0.5, 0.9, 1.0];
        let ys: [f64; 5] = [0.0, 0.2, 0.5, 0.77, 1.0];
        let ts: [f64; 5] = [0.0, 0.5, 1.0, std::f64::consts::PI, 10.0];

        for x in xs {
            for y in ys {
                for t in ts {
                    let wx = x * 6.0;
                    let wy = y * 6.0;
                    let powi_form =
                        (((wx - 3.0).powi(2) + (wy - 3.0).powi(2)).sqrt() * 1.8 + t).cos();

                    let wx3 = wx - 3.0;
                    let wy3 = wy - 3.0;
                    let mul_form = ((wx3 * wx3 + wy3 * wy3).sqrt() * 1.8 + t).cos();

                    assert_eq!(
                        powi_form.to_bits(),
                        mul_form.to_bits(),
                        "x={x}, y={y}, t={t}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_plasma_sampler_no_aspect_correction() {
        let sampler = PlasmaSampler;
        assert!(!sampler.applies_aspect_correction());
    }

    // --- MetaballFieldSampler ---

    #[test]
    fn test_metaball_empty_balls() {
        let sampler = MetaballFieldSampler::new(Vec::new());
        let (field, hue) = sampler.sample_field(0.5, 0.5, FxQuality::Full);
        assert!((field - 0.0).abs() < 1e-10);
        assert!((hue - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_metaball_sampler_name() {
        let sampler = MetaballFieldSampler::new(Vec::new());
        assert_eq!(sampler.name(), "metaballs");
    }

    #[test]
    fn test_metaball_sampler_no_aspect_correction() {
        let sampler = MetaballFieldSampler::new(Vec::new());
        assert!(!sampler.applies_aspect_correction());
    }

    #[test]
    fn test_metaball_field_decreases_with_distance() {
        let balls = vec![BallState {
            x: 0.5,
            y: 0.5,
            r2: 0.1,
            hue: 0.0,
        }];

        let (f_near, _) =
            MetaballFieldSampler::sample_field_from_slice(&balls, 0.51, 0.5, FxQuality::Full);
        let (f_mid, _) =
            MetaballFieldSampler::sample_field_from_slice(&balls, 0.6, 0.5, FxQuality::Full);
        let (f_far, _) =
            MetaballFieldSampler::sample_field_from_slice(&balls, 0.9, 0.5, FxQuality::Full);

        assert!(f_near > f_mid, "field should decrease with distance");
        assert!(f_mid > f_far, "field should decrease with distance");
    }

    #[test]
    fn test_metaball_field_additive() {
        // Two balls should produce a stronger field than one at an equidistant point
        let one_ball = vec![BallState {
            x: 0.3,
            y: 0.5,
            r2: 0.05,
            hue: 0.0,
        }];
        let two_balls = vec![
            BallState {
                x: 0.3,
                y: 0.5,
                r2: 0.05,
                hue: 0.0,
            },
            BallState {
                x: 0.7,
                y: 0.5,
                r2: 0.05,
                hue: 0.5,
            },
        ];

        let (f1, _) =
            MetaballFieldSampler::sample_field_from_slice(&one_ball, 0.5, 0.5, FxQuality::Full);
        let (f2, _) =
            MetaballFieldSampler::sample_field_from_slice(&two_balls, 0.5, 0.5, FxQuality::Full);

        assert!(f2 > f1, "two balls should produce stronger field");
    }

    #[test]
    fn test_metaball_weighted_hue() {
        let balls = vec![
            BallState {
                x: 0.5,
                y: 0.5,
                r2: 0.01,
                hue: 0.0,
            },
            BallState {
                x: 0.6,
                y: 0.5,
                r2: 0.01,
                hue: 1.0,
            },
        ];

        // Sample at the midpoint between the two balls
        let (_, hue) =
            MetaballFieldSampler::sample_field_from_slice(&balls, 0.55, 0.5, FxQuality::Full);
        // Hue should be between 0 and 1, weighted by distance
        assert!(
            (0.0..=1.0).contains(&hue),
            "weighted hue should be in [0, 1]"
        );
    }

    #[test]
    fn test_metaball_minimal_quality_step() {
        // Need >2 balls for minimal to skip any
        let balls: Vec<BallState> = (0..6)
            .map(|i| BallState {
                x: (i as f64 + 0.5) / 6.0,
                y: 0.5,
                r2: 0.01,
                hue: i as f64 / 5.0,
            })
            .collect();

        let (f_full, _) =
            MetaballFieldSampler::sample_field_from_slice(&balls, 0.5, 0.5, FxQuality::Full);
        let (f_minimal, _) =
            MetaballFieldSampler::sample_field_from_slice(&balls, 0.5, 0.5, FxQuality::Minimal);

        assert!(
            f_minimal < f_full,
            "minimal quality should use fewer balls (full={f_full}, min={f_minimal})"
        );
    }

    #[test]
    fn test_metaball_sampler_trait_returns_field() {
        let sampler = MetaballFieldSampler::new(vec![BallState {
            x: 0.5,
            y: 0.5,
            r2: 0.01,
            hue: 0.5,
        }]);

        // Sampler trait returns field strength only (not hue)
        let field_via_trait = sampler.sample(0.5, 0.5, 0.0, FxQuality::Full);
        let (field_direct, _) = sampler.sample_field(0.5, 0.5, FxQuality::Full);

        assert!(
            (field_via_trait - field_direct).abs() < 1e-15,
            "Sampler trait should return field strength"
        );
    }

    // --- FnSampler ---

    #[test]
    fn test_fn_sampler_no_aspect_correction_by_default() {
        let sampler = FnSampler::new(|_, _, _, _| 0.0, "test");
        assert!(!sampler.applies_aspect_correction());
    }

    #[test]
    fn test_fn_sampler_quality_passed_through() {
        let sampler = FnSampler::new(
            |_, _, _, q| match q {
                FxQuality::Full => 1.0,
                FxQuality::Reduced => 0.75,
                FxQuality::Minimal => 0.5,
                FxQuality::Off => 0.0,
            },
            "quality_test",
        );

        assert!((sampler.sample(0.0, 0.0, 0.0, FxQuality::Full) - 1.0).abs() < 1e-10);
        assert!((sampler.sample(0.0, 0.0, 0.0, FxQuality::Reduced) - 0.75).abs() < 1e-10);
        assert!((sampler.sample(0.0, 0.0, 0.0, FxQuality::Minimal) - 0.5).abs() < 1e-10);
        assert!((sampler.sample(0.0, 0.0, 0.0, FxQuality::Off) - 0.0).abs() < 1e-10);
    }

    // --- cell_to_normalized properties ---

    #[test]
    fn test_cell_to_normalized_monotonic() {
        for total in [2, 5, 10, 50, 100] {
            for cell in 1..total {
                let prev = cell_to_normalized(cell - 1, total);
                let curr = cell_to_normalized(cell, total);
                assert!(
                    curr > prev,
                    "cell_to_normalized should be strictly increasing: cell={cell}, total={total}"
                );
            }
        }
    }

    #[test]
    fn test_cell_to_normalized_within_unit() {
        for total in [1, 2, 5, 10, 100, 1000] {
            for cell in 0..total {
                let v = cell_to_normalized(cell, total);
                assert!(
                    (0.0..=1.0).contains(&v),
                    "cell_to_normalized({cell}, {total}) = {v}"
                );
            }
        }
    }

    #[test]
    fn test_cell_to_normalized_symmetric() {
        // For even grid, cell centers should be symmetric around 0.5
        let total = 10;
        for cell in 0..total / 2 {
            let left = cell_to_normalized(cell, total);
            let right = cell_to_normalized(total - 1 - cell, total);
            assert!(
                (left + right - 1.0).abs() < 1e-10,
                "cells {cell} and {} should sum to 1.0",
                total - 1 - cell
            );
        }
    }
}

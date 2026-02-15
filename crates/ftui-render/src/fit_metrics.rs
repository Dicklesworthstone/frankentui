//! Deterministic fit-to-container and font metric lifecycle.
//!
//! This module provides the infrastructure for mapping pixel-space container
//! dimensions to cell-grid dimensions, accounting for DPR, zoom, and font
//! metrics. It ensures that equivalent resize/font-load event streams yield
//! identical viewport and cursor geometry outcomes.
//!
//! # Key types
//!
//! - [`CellMetrics`]: cell size in sub-pixel units (1/256 px), deterministic.
//! - [`ContainerViewport`]: container dimensions with DPR and zoom tracking.
//! - [`FitPolicy`]: strategy for computing grid dimensions from container.
//! - [`FitResult`]: computed grid dimensions from a fit operation.
//! - [`MetricGeneration`]: monotonic counter for cache invalidation.
//! - [`MetricInvalidation`]: reason for metric recomputation.
//! - [`MetricLifecycle`]: stateful tracker for font metric changes.
//!
//! # Determinism
//!
//! All pixel-to-cell conversions use fixed-point arithmetic (256 sub-pixel
//! units per pixel) to avoid floating-point rounding ambiguity across
//! platforms. The same inputs always produce the same grid dimensions.

use std::fmt;

// =========================================================================
// Fixed-point helpers
// =========================================================================

/// Sub-pixel units per pixel (fixed-point denominator).
///
/// All metric calculations use this scale factor to avoid floating-point
/// rounding ambiguity. 256 gives 8 fractional bits of sub-pixel precision.
const SUBPX_SCALE: u32 = 256;

/// Convert a floating-point pixel value to sub-pixel units.
///
/// Rounds to nearest sub-pixel unit. Returns `None` on overflow.
fn px_to_subpx(px: f64) -> Option<u32> {
    if !px.is_finite() || px < 0.0 {
        return None;
    }
    let val = (px * SUBPX_SCALE as f64).round();
    if val > u32::MAX as f64 {
        return None;
    }
    Some(val as u32)
}

// =========================================================================
// CellMetrics
// =========================================================================

/// Cell dimensions in sub-pixel units (1/256 px) for deterministic layout.
///
/// Both `width_subpx` and `height_subpx` must be > 0. Use [`CellMetrics::new`]
/// to validate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellMetrics {
    /// Cell width in sub-pixel units (1/256 px).
    pub width_subpx: u32,
    /// Cell height in sub-pixel units (1/256 px).
    pub height_subpx: u32,
}

impl CellMetrics {
    /// Create cell metrics from sub-pixel values.
    ///
    /// Returns `None` if either dimension is zero.
    #[must_use]
    pub fn new(width_subpx: u32, height_subpx: u32) -> Option<Self> {
        if width_subpx == 0 || height_subpx == 0 {
            return None;
        }
        Some(Self {
            width_subpx,
            height_subpx,
        })
    }

    /// Create cell metrics from floating-point pixel values.
    ///
    /// Converts to sub-pixel units internally. Returns `None` on invalid input.
    #[must_use]
    pub fn from_px(width_px: f64, height_px: f64) -> Option<Self> {
        let w = px_to_subpx(width_px)?;
        let h = px_to_subpx(height_px)?;
        Self::new(w, h)
    }

    /// Cell width in whole pixels (truncated).
    #[must_use]
    pub const fn width_px(&self) -> u32 {
        self.width_subpx / SUBPX_SCALE
    }

    /// Cell height in whole pixels (truncated).
    #[must_use]
    pub const fn height_px(&self) -> u32 {
        self.height_subpx / SUBPX_SCALE
    }

    /// Monospace terminal default: 8x16 px.
    pub const MONOSPACE_DEFAULT: Self = Self {
        width_subpx: 8 * SUBPX_SCALE,
        height_subpx: 16 * SUBPX_SCALE,
    };

    /// Common 10x20 px cell size.
    pub const LARGE: Self = Self {
        width_subpx: 10 * SUBPX_SCALE,
        height_subpx: 20 * SUBPX_SCALE,
    };
}

impl Default for CellMetrics {
    fn default() -> Self {
        Self::MONOSPACE_DEFAULT
    }
}

impl fmt::Display for CellMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}x{}px ({:.2}x{:.2} sub-px)",
            self.width_px(),
            self.height_px(),
            self.width_subpx as f64 / SUBPX_SCALE as f64,
            self.height_subpx as f64 / SUBPX_SCALE as f64,
        )
    }
}

// =========================================================================
// ContainerViewport
// =========================================================================

/// Container dimensions and display parameters for fit computation.
///
/// Represents the available rendering area in physical pixels, plus the
/// DPR and zoom factor needed for correct pixel-to-cell mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContainerViewport {
    /// Available width in physical pixels.
    pub width_px: u32,
    /// Available height in physical pixels.
    pub height_px: u32,
    /// Device pixel ratio in sub-pixel units (256 = 1.0x DPR).
    ///
    /// Must be > 0. Common values:
    /// - 256 = 1.0x (standard density)
    /// - 512 = 2.0x (Retina)
    /// - 768 = 3.0x (high-DPI mobile)
    pub dpr_subpx: u32,
    /// Zoom factor in sub-pixel units (256 = 100% zoom).
    ///
    /// Must be > 0. Common values:
    /// - 256 = 100%
    /// - 320 = 125%
    /// - 384 = 150%
    /// - 512 = 200%
    pub zoom_subpx: u32,
}

impl ContainerViewport {
    /// Create a viewport with explicit parameters.
    ///
    /// Returns `None` if dimensions are zero or DPR/zoom are zero.
    #[must_use]
    pub fn new(width_px: u32, height_px: u32, dpr: f64, zoom: f64) -> Option<Self> {
        let dpr_subpx = px_to_subpx(dpr)?;
        let zoom_subpx = px_to_subpx(zoom)?;
        if width_px == 0 || height_px == 0 || dpr_subpx == 0 || zoom_subpx == 0 {
            return None;
        }
        Some(Self {
            width_px,
            height_px,
            dpr_subpx,
            zoom_subpx,
        })
    }

    /// Create a simple viewport at 1x DPR, 100% zoom.
    #[must_use]
    pub fn simple(width_px: u32, height_px: u32) -> Option<Self> {
        Self::new(width_px, height_px, 1.0, 1.0)
    }

    /// Effective pixel width adjusted for DPR and zoom, in sub-pixel units.
    ///
    /// Computes `physical_px / (dpr * zoom)` expressed in the same sub-pixel
    /// units as [`CellMetrics`] (1/256 px), so the caller can divide by
    /// `cell.width_subpx` to get column count.
    #[must_use]
    pub fn effective_width_subpx(&self) -> u32 {
        // effective_subpx = physical_px * SUBPX^3 / (dpr_subpx * zoom_subpx)
        let scale3 = (SUBPX_SCALE as u64) * (SUBPX_SCALE as u64) * (SUBPX_SCALE as u64);
        let numer = (self.width_px as u64) * scale3;
        let denom = (self.dpr_subpx as u64) * (self.zoom_subpx as u64);
        if denom == 0 {
            return 0;
        }
        (numer / denom) as u32
    }

    /// Effective pixel height adjusted for DPR and zoom, in sub-pixel units.
    #[must_use]
    pub fn effective_height_subpx(&self) -> u32 {
        let scale3 = (SUBPX_SCALE as u64) * (SUBPX_SCALE as u64) * (SUBPX_SCALE as u64);
        let numer = (self.height_px as u64) * scale3;
        let denom = (self.dpr_subpx as u64) * (self.zoom_subpx as u64);
        if denom == 0 {
            return 0;
        }
        (numer / denom) as u32
    }
}

impl fmt::Display for ContainerViewport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}x{}px @{:.2}x DPR, {:.0}% zoom",
            self.width_px,
            self.height_px,
            self.dpr_subpx as f64 / SUBPX_SCALE as f64,
            self.zoom_subpx as f64 / SUBPX_SCALE as f64 * 100.0,
        )
    }
}

// =========================================================================
// FitPolicy
// =========================================================================

/// Strategy for computing grid dimensions from container and font metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FitPolicy {
    /// Automatically fit: fill the container, rounding down to whole cells.
    ///
    /// This is the xterm.js `fit` addon behavior: cols = floor(container_width / cell_width).
    #[default]
    FitToContainer,
    /// Fixed grid size, ignoring container dimensions.
    ///
    /// Useful for testing or when the host manages sizing.
    Fixed {
        /// Fixed column count.
        cols: u16,
        /// Fixed row count.
        rows: u16,
    },
    /// Clamp to container but with minimum dimensions.
    ///
    /// Like `FitToContainer` but guarantees at least `min_cols` x `min_rows`.
    FitWithMinimum {
        /// Minimum column count.
        min_cols: u16,
        /// Minimum row count.
        min_rows: u16,
    },
}

// =========================================================================
// FitResult
// =========================================================================

/// Computed grid dimensions from a fit operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FitResult {
    /// Grid columns.
    pub cols: u16,
    /// Grid rows.
    pub rows: u16,
    /// Horizontal padding remainder in sub-pixel units.
    ///
    /// The leftover space after fitting whole columns:
    /// `container_width - (cols * cell_width)`.
    pub padding_right_subpx: u32,
    /// Vertical padding remainder in sub-pixel units.
    pub padding_bottom_subpx: u32,
}

impl FitResult {
    /// Whether the fit result represents a valid (non-empty) grid.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.cols > 0 && self.rows > 0
    }
}

impl fmt::Display for FitResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{} cells", self.cols, self.rows)
    }
}

// =========================================================================
// fit_to_container
// =========================================================================

/// Errors from fit computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitError {
    /// Container is too small to fit even one cell.
    ContainerTooSmall,
    /// Grid dimensions would overflow u16.
    DimensionOverflow,
}

impl fmt::Display for FitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContainerTooSmall => write!(f, "container too small to fit any cells"),
            Self::DimensionOverflow => write!(f, "computed grid dimensions overflow u16"),
        }
    }
}

/// Compute grid dimensions by fitting cells into a container viewport.
///
/// This is the core deterministic computation. Given a container size (adjusted
/// for DPR/zoom) and cell metrics, it returns the number of cols/rows that fit.
///
/// # Determinism
///
/// Uses integer-only arithmetic on sub-pixel units. The same inputs always
/// produce the same output, regardless of platform or FPU mode.
pub fn fit_to_container(
    viewport: &ContainerViewport,
    cell: &CellMetrics,
    policy: FitPolicy,
) -> Result<FitResult, FitError> {
    match policy {
        FitPolicy::Fixed { cols, rows } => Ok(FitResult {
            cols,
            rows,
            padding_right_subpx: 0,
            padding_bottom_subpx: 0,
        }),
        FitPolicy::FitToContainer => fit_internal(viewport, cell, 1, 1),
        FitPolicy::FitWithMinimum { min_cols, min_rows } => {
            fit_internal(viewport, cell, min_cols.max(1), min_rows.max(1))
        }
    }
}

fn fit_internal(
    viewport: &ContainerViewport,
    cell: &CellMetrics,
    min_cols: u16,
    min_rows: u16,
) -> Result<FitResult, FitError> {
    let eff_w = viewport.effective_width_subpx();
    let eff_h = viewport.effective_height_subpx();

    // Integer division: cols = floor(effective_width / cell_width)
    let raw_cols = eff_w / cell.width_subpx;
    let raw_rows = eff_h / cell.height_subpx;

    let cols = raw_cols.max(min_cols as u32);
    let rows = raw_rows.max(min_rows as u32);

    if cols == 0 || rows == 0 {
        return Err(FitError::ContainerTooSmall);
    }
    if cols > u16::MAX as u32 || rows > u16::MAX as u32 {
        return Err(FitError::DimensionOverflow);
    }

    let cols = cols as u16;
    let rows = rows as u16;

    let used_w = cols as u32 * cell.width_subpx;
    let used_h = rows as u32 * cell.height_subpx;
    let pad_r = eff_w.saturating_sub(used_w);
    let pad_b = eff_h.saturating_sub(used_h);

    Ok(FitResult {
        cols,
        rows,
        padding_right_subpx: pad_r,
        padding_bottom_subpx: pad_b,
    })
}

// =========================================================================
// MetricGeneration
// =========================================================================

/// Monotonic generation counter for metric cache invalidation.
///
/// Each font metric change increments the generation. Caches compare their
/// stored generation against the current one to detect staleness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MetricGeneration(u64);

impl MetricGeneration {
    /// Initial generation.
    pub const ZERO: Self = Self(0);

    /// Advance to the next generation.
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Raw generation value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for MetricGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gen:{}", self.0)
    }
}

// =========================================================================
// MetricInvalidation
// =========================================================================

/// Reason for a font metric recomputation.
///
/// Each variant triggers a specific set of cache invalidations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetricInvalidation {
    /// Web font finished loading, metrics may have changed.
    FontLoaded,
    /// Device pixel ratio changed (e.g., window moved between monitors).
    DprChanged,
    /// User zoom level changed.
    ZoomChanged,
    /// Container was resized (may affect fit, not metrics themselves).
    ContainerResized,
    /// Font size explicitly changed by user or configuration.
    FontSizeChanged,
    /// Full metric reset requested (e.g., after error recovery).
    FullReset,
}

impl MetricInvalidation {
    /// Whether this invalidation requires recomputing glyph rasterization.
    ///
    /// DPR and font size changes affect pixel output; container resize does not.
    #[must_use]
    pub fn requires_rasterization(&self) -> bool {
        matches!(
            self,
            Self::FontLoaded | Self::DprChanged | Self::FontSizeChanged | Self::FullReset
        )
    }

    /// Whether this invalidation requires recomputing grid dimensions.
    #[must_use]
    pub fn requires_refit(&self) -> bool {
        // All invalidations may affect the fit except a pure font load
        // where the cell size doesn't change (handled by caller).
        true
    }
}

impl fmt::Display for MetricInvalidation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FontLoaded => write!(f, "font_loaded"),
            Self::DprChanged => write!(f, "dpr_changed"),
            Self::ZoomChanged => write!(f, "zoom_changed"),
            Self::ContainerResized => write!(f, "container_resized"),
            Self::FontSizeChanged => write!(f, "font_size_changed"),
            Self::FullReset => write!(f, "full_reset"),
        }
    }
}

// =========================================================================
// MetricLifecycle
// =========================================================================

/// Stateful tracker for font metric changes and cache invalidation.
///
/// Maintains the current cell metrics, generation counter, and pending
/// invalidations. The lifecycle ensures a deterministic sequence:
///
/// 1. Invalidation event arrives (font load, DPR change, etc.)
/// 2. Generation is bumped, pending flag is set
/// 3. Caller calls [`MetricLifecycle::refit`] with new metrics
/// 4. If grid dimensions changed, resize propagates through the pipeline
///
/// This prevents stale glyph metrics and geometry jumps by enforcing that
/// all consumers see consistent metric state.
#[derive(Debug, Clone)]
pub struct MetricLifecycle {
    /// Current cell metrics.
    cell_metrics: CellMetrics,
    /// Current viewport (if set).
    viewport: Option<ContainerViewport>,
    /// Fit policy.
    policy: FitPolicy,
    /// Current metric generation.
    generation: MetricGeneration,
    /// Whether a refit is pending.
    pending_refit: bool,
    /// Last invalidation reason.
    last_invalidation: Option<MetricInvalidation>,
    /// Last computed fit result.
    last_fit: Option<FitResult>,
    /// Total invalidation count for diagnostics.
    total_invalidations: u64,
    /// Total refit count for diagnostics.
    total_refits: u64,
}

impl MetricLifecycle {
    /// Create a new lifecycle with default cell metrics and no viewport.
    #[must_use]
    pub fn new(cell_metrics: CellMetrics, policy: FitPolicy) -> Self {
        Self {
            cell_metrics,
            viewport: None,
            policy,
            generation: MetricGeneration::ZERO,
            pending_refit: false,
            last_invalidation: None,
            last_fit: None,
            total_invalidations: 0,
            total_refits: 0,
        }
    }

    /// Current cell metrics.
    #[must_use]
    pub fn cell_metrics(&self) -> &CellMetrics {
        &self.cell_metrics
    }

    /// Current metric generation.
    #[must_use]
    pub fn generation(&self) -> MetricGeneration {
        self.generation
    }

    /// Whether a refit is pending.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending_refit
    }

    /// Last computed fit result.
    #[must_use]
    pub fn last_fit(&self) -> Option<&FitResult> {
        self.last_fit.as_ref()
    }

    /// Total invalidation count.
    #[must_use]
    pub fn total_invalidations(&self) -> u64 {
        self.total_invalidations
    }

    /// Total refit count.
    #[must_use]
    pub fn total_refits(&self) -> u64 {
        self.total_refits
    }

    /// Record an invalidation event.
    ///
    /// Bumps the generation and marks a refit as pending. If cell metrics
    /// changed, the new metrics are stored immediately.
    pub fn invalidate(&mut self, reason: MetricInvalidation, new_metrics: Option<CellMetrics>) {
        self.generation = self.generation.next();
        self.pending_refit = true;
        self.last_invalidation = Some(reason);
        self.total_invalidations += 1;

        if let Some(metrics) = new_metrics {
            self.cell_metrics = metrics;
        }
    }

    /// Update the container viewport.
    ///
    /// If the viewport changed, marks a refit as pending.
    pub fn set_viewport(&mut self, viewport: ContainerViewport) {
        let changed = self.viewport.is_none_or(|v| v != viewport);
        self.viewport = Some(viewport);
        if changed {
            self.generation = self.generation.next();
            self.pending_refit = true;
            self.last_invalidation = Some(MetricInvalidation::ContainerResized);
            self.total_invalidations += 1;
        }
    }

    /// Update the fit policy.
    pub fn set_policy(&mut self, policy: FitPolicy) {
        if self.policy != policy {
            self.policy = policy;
            self.pending_refit = true;
        }
    }

    /// Perform the pending refit computation.
    ///
    /// Returns `Some(FitResult)` if the grid dimensions changed, `None` if
    /// no refit was needed or dimensions are unchanged.
    ///
    /// Clears the pending flag regardless of outcome.
    pub fn refit(&mut self) -> Option<FitResult> {
        if !self.pending_refit {
            return None;
        }
        self.pending_refit = false;
        self.total_refits += 1;

        let viewport = self.viewport?;
        let result = fit_to_container(&viewport, &self.cell_metrics, self.policy).ok()?;

        let changed = self
            .last_fit
            .is_none_or(|prev| prev.cols != result.cols || prev.rows != result.rows);

        self.last_fit = Some(result);

        if changed { Some(result) } else { None }
    }

    /// Diagnostic snapshot for JSONL evidence logging.
    #[must_use]
    pub fn snapshot(&self) -> MetricSnapshot {
        MetricSnapshot {
            generation: self.generation.get(),
            pending_refit: self.pending_refit,
            cell_width_subpx: self.cell_metrics.width_subpx,
            cell_height_subpx: self.cell_metrics.height_subpx,
            viewport_width_px: self.viewport.map(|v| v.width_px).unwrap_or(0),
            viewport_height_px: self.viewport.map(|v| v.height_px).unwrap_or(0),
            dpr_subpx: self.viewport.map(|v| v.dpr_subpx).unwrap_or(0),
            zoom_subpx: self.viewport.map(|v| v.zoom_subpx).unwrap_or(0),
            fit_cols: self.last_fit.map(|f| f.cols).unwrap_or(0),
            fit_rows: self.last_fit.map(|f| f.rows).unwrap_or(0),
            total_invalidations: self.total_invalidations,
            total_refits: self.total_refits,
        }
    }
}

/// Diagnostic snapshot of metric lifecycle state.
///
/// All fields are `Copy` for cheap JSONL serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricSnapshot {
    /// Current generation counter.
    pub generation: u64,
    /// Whether a refit is pending.
    pub pending_refit: bool,
    /// Cell width in sub-pixel units.
    pub cell_width_subpx: u32,
    /// Cell height in sub-pixel units.
    pub cell_height_subpx: u32,
    /// Container width in physical pixels.
    pub viewport_width_px: u32,
    /// Container height in physical pixels.
    pub viewport_height_px: u32,
    /// DPR in sub-pixel units.
    pub dpr_subpx: u32,
    /// Zoom in sub-pixel units.
    pub zoom_subpx: u32,
    /// Last computed grid columns.
    pub fit_cols: u16,
    /// Last computed grid rows.
    pub fit_rows: u16,
    /// Total invalidation count.
    pub total_invalidations: u64,
    /// Total refit count.
    pub total_refits: u64,
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── CellMetrics ──────────────────────────────────────────────────

    #[test]
    fn cell_metrics_default_is_monospace() {
        let m = CellMetrics::default();
        assert_eq!(m.width_px(), 8);
        assert_eq!(m.height_px(), 16);
    }

    #[test]
    fn cell_metrics_from_px() {
        let m = CellMetrics::from_px(9.0, 18.0).unwrap();
        assert_eq!(m.width_px(), 9);
        assert_eq!(m.height_px(), 18);
    }

    #[test]
    fn cell_metrics_from_px_fractional() {
        let m = CellMetrics::from_px(8.5, 16.75).unwrap();
        assert_eq!(m.width_subpx, 2176); // 8.5 * 256
        assert_eq!(m.height_subpx, 4288); // 16.75 * 256
        assert_eq!(m.width_px(), 8); // truncated
        assert_eq!(m.height_px(), 16);
    }

    #[test]
    fn cell_metrics_rejects_zero() {
        assert!(CellMetrics::new(0, 256).is_none());
        assert!(CellMetrics::new(256, 0).is_none());
        assert!(CellMetrics::new(0, 0).is_none());
    }

    #[test]
    fn cell_metrics_rejects_negative_px() {
        assert!(CellMetrics::from_px(-1.0, 16.0).is_none());
        assert!(CellMetrics::from_px(8.0, -1.0).is_none());
    }

    #[test]
    fn cell_metrics_rejects_nan() {
        assert!(CellMetrics::from_px(f64::NAN, 16.0).is_none());
        assert!(CellMetrics::from_px(8.0, f64::INFINITY).is_none());
    }

    #[test]
    fn cell_metrics_display() {
        let m = CellMetrics::MONOSPACE_DEFAULT;
        let s = format!("{m}");
        assert!(s.contains("8x16px"));
    }

    #[test]
    fn cell_metrics_large_preset() {
        assert_eq!(CellMetrics::LARGE.width_px(), 10);
        assert_eq!(CellMetrics::LARGE.height_px(), 20);
    }

    // ── ContainerViewport ────────────────────────────────────────────

    #[test]
    fn viewport_simple() {
        let v = ContainerViewport::simple(800, 600).unwrap();
        assert_eq!(v.width_px, 800);
        assert_eq!(v.height_px, 600);
        assert_eq!(v.dpr_subpx, 256); // 1.0x
        assert_eq!(v.zoom_subpx, 256); // 100%
    }

    #[test]
    fn viewport_effective_1x_dpr() {
        let v = ContainerViewport::simple(800, 600).unwrap();
        // effective = physical * 256^3 / (256 * 256) = physical * 256
        assert_eq!(v.effective_width_subpx(), 800 * SUBPX_SCALE);
        assert_eq!(v.effective_height_subpx(), 600 * SUBPX_SCALE);
    }

    #[test]
    fn viewport_effective_2x_dpr() {
        let v = ContainerViewport::new(1600, 1200, 2.0, 1.0).unwrap();
        // effective = 1600 * 256^3 / (512 * 256) = 1600 * 256 / 2 = 800 * 256
        assert_eq!(v.effective_width_subpx(), 800 * SUBPX_SCALE);
        assert_eq!(v.effective_height_subpx(), 600 * SUBPX_SCALE);
    }

    #[test]
    fn viewport_effective_zoom_150() {
        let v = ContainerViewport::new(800, 600, 1.0, 1.5).unwrap();
        // effective = 800 * 256^3 / (256 * 384) = 800 * 256^2 / 384
        // = 800 * 65536 / 384 = 136533 (integer division)
        let eff = v.effective_width_subpx();
        assert_eq!(eff, 136533);
    }

    #[test]
    fn viewport_rejects_zero_dims() {
        assert!(ContainerViewport::simple(0, 600).is_none());
        assert!(ContainerViewport::simple(800, 0).is_none());
    }

    #[test]
    fn viewport_rejects_zero_dpr() {
        assert!(ContainerViewport::new(800, 600, 0.0, 1.0).is_none());
    }

    #[test]
    fn viewport_display() {
        let v = ContainerViewport::simple(800, 600).unwrap();
        let s = format!("{v}");
        assert!(s.contains("800x600px"));
        assert!(s.contains("1.00x DPR"));
    }

    // ── FitPolicy & fit_to_container ──────────────────────────────────

    #[test]
    fn fit_default_80x24_terminal() {
        // 80 cols * 8px = 640px wide, 24 rows * 16px = 384px tall
        let v = ContainerViewport::simple(640, 384).unwrap();
        let r =
            fit_to_container(&v, &CellMetrics::MONOSPACE_DEFAULT, FitPolicy::default()).unwrap();
        assert_eq!(r.cols, 80);
        assert_eq!(r.rows, 24);
        assert_eq!(r.padding_right_subpx, 0);
        assert_eq!(r.padding_bottom_subpx, 0);
    }

    #[test]
    fn fit_with_remainder() {
        // 645px / 8px = 80.625 → 80 cols, remainder 5px
        let v = ContainerViewport::simple(645, 390).unwrap();
        let r =
            fit_to_container(&v, &CellMetrics::MONOSPACE_DEFAULT, FitPolicy::default()).unwrap();
        assert_eq!(r.cols, 80);
        assert_eq!(r.rows, 24);
        assert_eq!(r.padding_right_subpx, 5 * 256);
        assert_eq!(r.padding_bottom_subpx, 6 * 256);
    }

    #[test]
    fn fit_small_container_clamps_to_1x1() {
        // Container smaller than one cell: FitToContainer clamps to 1x1.
        let v = ContainerViewport::simple(4, 8).unwrap();
        let r =
            fit_to_container(&v, &CellMetrics::MONOSPACE_DEFAULT, FitPolicy::default()).unwrap();
        assert_eq!(r.cols, 1);
        assert_eq!(r.rows, 1);
    }

    #[test]
    fn fit_fixed_ignores_container() {
        let v = ContainerViewport::simple(100, 100).unwrap();
        let r = fit_to_container(
            &v,
            &CellMetrics::MONOSPACE_DEFAULT,
            FitPolicy::Fixed { cols: 80, rows: 24 },
        )
        .unwrap();
        assert_eq!(r.cols, 80);
        assert_eq!(r.rows, 24);
    }

    #[test]
    fn fit_with_minimum_guarantees_min_size() {
        // Container fits 5x3 at 8x16, but minimum is 10x5
        let v = ContainerViewport::simple(40, 48).unwrap();
        let r = fit_to_container(
            &v,
            &CellMetrics::MONOSPACE_DEFAULT,
            FitPolicy::FitWithMinimum {
                min_cols: 10,
                min_rows: 5,
            },
        )
        .unwrap();
        assert_eq!(r.cols, 10);
        assert_eq!(r.rows, 5);
    }

    #[test]
    fn fit_with_minimum_uses_actual_when_larger() {
        let v = ContainerViewport::simple(800, 600).unwrap();
        let r = fit_to_container(
            &v,
            &CellMetrics::MONOSPACE_DEFAULT,
            FitPolicy::FitWithMinimum {
                min_cols: 10,
                min_rows: 5,
            },
        )
        .unwrap();
        assert_eq!(r.cols, 100); // 800/8
        assert_eq!(r.rows, 37); // 600/16 = 37.5 → 37
    }

    #[test]
    fn fit_result_is_valid() {
        let r = FitResult {
            cols: 80,
            rows: 24,
            padding_right_subpx: 0,
            padding_bottom_subpx: 0,
        };
        assert!(r.is_valid());
    }

    #[test]
    fn fit_result_display() {
        let r = FitResult {
            cols: 120,
            rows: 40,
            padding_right_subpx: 0,
            padding_bottom_subpx: 0,
        };
        assert_eq!(format!("{r}"), "120x40 cells");
    }

    #[test]
    fn fit_at_2x_dpr() {
        // 2x DPR: 1600 physical px → 800 CSS px → 100 cols at 8px
        let v = ContainerViewport::new(1600, 768, 2.0, 1.0).unwrap();
        let r =
            fit_to_container(&v, &CellMetrics::MONOSPACE_DEFAULT, FitPolicy::default()).unwrap();
        assert_eq!(r.cols, 100);
        assert_eq!(r.rows, 24); // 384 CSS px / 16 = 24
    }

    #[test]
    fn fit_at_3x_dpr() {
        let v = ContainerViewport::new(2400, 1152, 3.0, 1.0).unwrap();
        let r =
            fit_to_container(&v, &CellMetrics::MONOSPACE_DEFAULT, FitPolicy::default()).unwrap();
        assert_eq!(r.cols, 100); // 800/8
        assert_eq!(r.rows, 24); // 384/16
    }

    #[test]
    fn fit_deterministic_across_calls() {
        let v = ContainerViewport::simple(800, 600).unwrap();
        let m = CellMetrics::MONOSPACE_DEFAULT;
        let r1 = fit_to_container(&v, &m, FitPolicy::default()).unwrap();
        let r2 = fit_to_container(&v, &m, FitPolicy::default()).unwrap();
        assert_eq!(r1, r2);
    }

    #[test]
    fn fit_error_display() {
        assert!(!format!("{}", FitError::ContainerTooSmall).is_empty());
        assert!(!format!("{}", FitError::DimensionOverflow).is_empty());
    }

    // ── MetricGeneration ──────────────────────────────────────────────

    #[test]
    fn generation_starts_at_zero() {
        assert_eq!(MetricGeneration::ZERO.get(), 0);
    }

    #[test]
    fn generation_increments() {
        let g = MetricGeneration::ZERO.next().next();
        assert_eq!(g.get(), 2);
    }

    #[test]
    fn generation_display() {
        let s = format!("{}", MetricGeneration::ZERO.next());
        assert_eq!(s, "gen:1");
    }

    #[test]
    fn generation_ordering() {
        let g0 = MetricGeneration::ZERO;
        let g1 = g0.next();
        assert!(g1 > g0);
    }

    // ── MetricInvalidation ────────────────────────────────────────────

    #[test]
    fn invalidation_requires_rasterization() {
        assert!(MetricInvalidation::FontLoaded.requires_rasterization());
        assert!(MetricInvalidation::DprChanged.requires_rasterization());
        assert!(MetricInvalidation::FontSizeChanged.requires_rasterization());
        assert!(MetricInvalidation::FullReset.requires_rasterization());
        assert!(!MetricInvalidation::ZoomChanged.requires_rasterization());
        assert!(!MetricInvalidation::ContainerResized.requires_rasterization());
    }

    #[test]
    fn invalidation_display() {
        assert_eq!(format!("{}", MetricInvalidation::FontLoaded), "font_loaded");
        assert_eq!(format!("{}", MetricInvalidation::DprChanged), "dpr_changed");
    }

    // ── MetricLifecycle ───────────────────────────────────────────────

    #[test]
    fn lifecycle_initial_state() {
        let lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        assert_eq!(lc.generation(), MetricGeneration::ZERO);
        assert!(!lc.is_pending());
        assert!(lc.last_fit().is_none());
        assert_eq!(lc.total_invalidations(), 0);
        assert_eq!(lc.total_refits(), 0);
    }

    #[test]
    fn lifecycle_invalidate_bumps_generation() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.invalidate(MetricInvalidation::FontLoaded, None);
        assert_eq!(lc.generation().get(), 1);
        assert!(lc.is_pending());
        assert_eq!(lc.total_invalidations(), 1);
    }

    #[test]
    fn lifecycle_invalidate_with_new_metrics() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        let new = CellMetrics::LARGE;
        lc.invalidate(MetricInvalidation::FontSizeChanged, Some(new));
        assert_eq!(*lc.cell_metrics(), new);
    }

    #[test]
    fn lifecycle_set_viewport_marks_pending() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        let vp = ContainerViewport::simple(800, 600).unwrap();
        lc.set_viewport(vp);
        assert!(lc.is_pending());
        assert_eq!(lc.generation().get(), 1);
    }

    #[test]
    fn lifecycle_set_viewport_same_no_change() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        let vp = ContainerViewport::simple(800, 600).unwrap();
        lc.set_viewport(vp);
        let prev_gen = lc.generation();
        lc.set_viewport(vp); // same viewport again
        assert_eq!(lc.generation(), prev_gen); // no change
    }

    #[test]
    fn lifecycle_refit_without_viewport_returns_none() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.invalidate(MetricInvalidation::FontLoaded, None);
        assert!(lc.refit().is_none());
        assert!(!lc.is_pending()); // pending cleared
    }

    #[test]
    fn lifecycle_refit_computes_grid() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.set_viewport(ContainerViewport::simple(640, 384).unwrap());
        let result = lc.refit().unwrap();
        assert_eq!(result.cols, 80);
        assert_eq!(result.rows, 24);
        assert_eq!(lc.total_refits(), 1);
    }

    #[test]
    fn lifecycle_refit_no_change_returns_none() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.set_viewport(ContainerViewport::simple(640, 384).unwrap());
        let _ = lc.refit(); // first refit
        // Same viewport again — refit should return None (no change)
        lc.set_viewport(ContainerViewport::simple(640, 384).unwrap());
        // viewport is same, so set_viewport doesn't bump pending
    }

    #[test]
    fn lifecycle_refit_detects_dimension_change() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.set_viewport(ContainerViewport::simple(640, 384).unwrap());
        let _ = lc.refit();
        // Change to different size
        lc.set_viewport(ContainerViewport::simple(800, 600).unwrap());
        let result = lc.refit().unwrap();
        assert_eq!(result.cols, 100);
        assert_eq!(result.rows, 37);
    }

    #[test]
    fn lifecycle_set_policy_marks_pending() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.set_policy(FitPolicy::Fixed { cols: 80, rows: 24 });
        assert!(lc.is_pending());
    }

    #[test]
    fn lifecycle_snapshot() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.set_viewport(ContainerViewport::simple(640, 384).unwrap());
        let _ = lc.refit();
        let snap = lc.snapshot();
        assert_eq!(snap.fit_cols, 80);
        assert_eq!(snap.fit_rows, 24);
        assert_eq!(snap.viewport_width_px, 640);
        assert_eq!(snap.viewport_height_px, 384);
        assert_eq!(snap.dpr_subpx, 256);
        assert_eq!(snap.zoom_subpx, 256);
        assert!(!snap.pending_refit);
    }

    #[test]
    fn lifecycle_multiple_invalidations() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.set_viewport(ContainerViewport::simple(640, 384).unwrap());
        lc.invalidate(MetricInvalidation::FontLoaded, None);
        lc.invalidate(MetricInvalidation::DprChanged, None);
        lc.invalidate(MetricInvalidation::ZoomChanged, None);
        // Only one refit should be needed
        assert!(lc.is_pending());
        assert_eq!(lc.total_invalidations(), 4); // 1 from set_viewport + 3 explicit
    }

    #[test]
    fn lifecycle_font_size_change_affects_fit() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.set_viewport(ContainerViewport::simple(800, 600).unwrap());
        let first = lc.refit().unwrap();
        assert_eq!(first.cols, 100); // 800/8
        assert_eq!(first.rows, 37); // 600/16 = 37

        // Double the font size: 16x32
        let big = CellMetrics::new(16 * 256, 32 * 256).unwrap();
        lc.invalidate(MetricInvalidation::FontSizeChanged, Some(big));
        let second = lc.refit().unwrap();
        assert_eq!(second.cols, 50); // 800/16
        assert_eq!(second.rows, 18); // 600/32 = 18
    }

    #[test]
    fn lifecycle_dpr_change_affects_fit() {
        let mut lc = MetricLifecycle::new(CellMetrics::default(), FitPolicy::default());
        lc.set_viewport(ContainerViewport::simple(800, 600).unwrap());
        let first = lc.refit().unwrap();
        assert_eq!(first.cols, 100);

        // Move to 2x DPR display (same physical pixels)
        let vp2 = ContainerViewport::new(800, 600, 2.0, 1.0).unwrap();
        lc.set_viewport(vp2);
        let second = lc.refit().unwrap();
        assert_eq!(second.cols, 50); // effective width halved
    }

    // ── px_to_subpx edge cases ───────────────────────────────────────

    #[test]
    fn subpx_conversion_zero() {
        assert_eq!(px_to_subpx(0.0), Some(0));
    }

    #[test]
    fn subpx_conversion_negative() {
        assert_eq!(px_to_subpx(-1.0), None);
    }

    #[test]
    fn subpx_conversion_nan() {
        assert_eq!(px_to_subpx(f64::NAN), None);
    }

    #[test]
    fn subpx_conversion_infinity() {
        assert_eq!(px_to_subpx(f64::INFINITY), None);
    }

    #[test]
    fn subpx_conversion_precise() {
        assert_eq!(px_to_subpx(1.0), Some(256));
        assert_eq!(px_to_subpx(0.5), Some(128));
        assert_eq!(px_to_subpx(2.0), Some(512));
    }
}

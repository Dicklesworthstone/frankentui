#![forbid(unsafe_code)]

//! Coordinate mapping guard for pointer/mouse/touch events during rescale.
//!
//! When the grid geometry changes (resize, DPR shift, zoom), coordinates
//! computed by the JS host against the old geometry may arrive in Rust after
//! the grid has already been resized. This module provides validated coordinate
//! types and a guard that detects stale geometry mismatches.
//!
//! # Architecture
//!
//! ```text
//! JS computes (x, y) using GeometrySnapshot v1
//!                  ↓
//!     [ResizeArbiter resolves → grid now v2]
//!                  ↓
//! Rust receives (x, y) → CoordinateGuard validates against v2
//!                  ↓
//!     ACCEPT (bounds ok) or CLAMP (clamped) or REJECT (out of range)
//! ```
//!
//! # Determinism
//!
//! Validation is purely arithmetic on integers — same inputs → same outputs.

use crate::renderer::GridGeometry;

// ---------------------------------------------------------------------------
// Validated coordinate type
// ---------------------------------------------------------------------------

/// A cell coordinate that has been validated against a known grid geometry.
///
/// The coordinate is guaranteed to be within `[0, cols)` × `[0, rows)` of
/// the geometry snapshot it was validated against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValidatedCellCoord {
    /// Column (0-indexed, validated < cols).
    pub col: u16,
    /// Row (0-indexed, validated < rows).
    pub row: u16,
    /// How the coordinate was resolved.
    pub resolution: CoordResolution,
}

/// How a coordinate was resolved during validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoordResolution {
    /// Coordinate was within bounds, used as-is.
    Exact,
    /// Coordinate was out of bounds and clamped to the nearest valid cell.
    Clamped,
}

impl ValidatedCellCoord {
    /// Linear offset into a row-major cell buffer.
    #[must_use]
    pub fn offset(self, cols: u16) -> usize {
        usize::from(self.row) * usize::from(cols) + usize::from(self.col)
    }

    /// Whether this coordinate was clamped from an out-of-bounds value.
    #[must_use]
    pub const fn was_clamped(&self) -> bool {
        matches!(self.resolution, CoordResolution::Clamped)
    }
}

// ---------------------------------------------------------------------------
// Geometry snapshot for version tracking
// ---------------------------------------------------------------------------

/// Monotonic geometry version for stale-detection.
///
/// Incremented each time the arbiter resolves a new geometry. If the version
/// at event creation differs from the version at event consumption, the
/// coordinates may be stale.
pub type GeometryVersion = u64;

/// A frozen geometry snapshot for coordinate validation.
///
/// Captured at a specific point in time (geometry version). All coordinate
/// validation uses this snapshot rather than the live mutable geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeometrySnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: f32,
    pub cell_height_px: f32,
    pub dpr: f32,
    pub zoom: f32,
    pub version: GeometryVersion,
}

impl GeometrySnapshot {
    /// Capture a snapshot from a GridGeometry and version.
    #[must_use]
    pub fn capture(geom: &GridGeometry, version: GeometryVersion) -> Self {
        Self {
            cols: geom.cols,
            rows: geom.rows,
            cell_width_px: geom.cell_width_px,
            cell_height_px: geom.cell_height_px,
            dpr: geom.dpr,
            zoom: geom.zoom,
            version,
        }
    }

    /// Whether two snapshots have the same grid dimensions.
    #[must_use]
    pub fn same_grid(&self, other: &Self) -> bool {
        self.cols == other.cols && self.rows == other.rows
    }

    /// Whether two snapshots have the same scale factors.
    #[must_use]
    pub fn same_scale(&self, other: &Self) -> bool {
        (self.dpr - other.dpr).abs() < f32::EPSILON && (self.zoom - other.zoom).abs() < f32::EPSILON
    }
}

// ---------------------------------------------------------------------------
// Coordinate guard
// ---------------------------------------------------------------------------

/// Guard that validates and transforms pointer coordinates against a known
/// grid geometry, detecting stale-geometry mismatches.
///
/// The guard holds the current geometry version and can validate coordinates
/// that were computed by JS against a potentially older geometry.
#[derive(Debug, Clone)]
pub struct CoordinateGuard {
    /// Current geometry snapshot (the "truth" for validation).
    current: GeometrySnapshot,
}

/// Result of coordinate validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationResult {
    /// Coordinate is valid for the current geometry.
    Valid(ValidatedCellCoord),
    /// Coordinate was computed against a different geometry version but
    /// was successfully remapped/clamped.
    Remapped {
        coord: ValidatedCellCoord,
        /// The geometry version the coordinate was originally computed for.
        source_version: GeometryVersion,
    },
    /// Coordinate is invalid and cannot be meaningfully mapped.
    /// This happens when the grid has zero dimensions (should never occur
    /// in practice due to minimum 1x1 clamping).
    Invalid,
}

impl ValidationResult {
    /// Extract the validated coordinate, regardless of how it was resolved.
    #[must_use]
    pub fn coord(&self) -> Option<ValidatedCellCoord> {
        match self {
            ValidationResult::Valid(c) | ValidationResult::Remapped { coord: c, .. } => Some(*c),
            ValidationResult::Invalid => None,
        }
    }

    /// Whether the coordinate was remapped from a stale geometry version.
    #[must_use]
    pub const fn was_remapped(&self) -> bool {
        matches!(self, ValidationResult::Remapped { .. })
    }
}

impl CoordinateGuard {
    /// Create a guard from a geometry snapshot.
    #[must_use]
    pub fn new(snapshot: GeometrySnapshot) -> Self {
        Self { current: snapshot }
    }

    /// Update the guard's geometry snapshot.
    pub fn update(&mut self, snapshot: GeometrySnapshot) {
        self.current = snapshot;
    }

    /// Current geometry version.
    #[must_use]
    pub fn version(&self) -> GeometryVersion {
        self.current.version
    }

    /// Current grid dimensions.
    #[must_use]
    pub fn grid_dims(&self) -> (u16, u16) {
        (self.current.cols, self.current.rows)
    }

    /// Validate cell coordinates against the current geometry.
    ///
    /// If the coordinates are within bounds, returns `Valid`.
    /// If out of bounds, clamps to the nearest valid cell and returns `Valid`
    /// with `Clamped` resolution.
    #[must_use]
    pub fn validate(&self, x: u16, y: u16) -> ValidationResult {
        let (cols, rows) = (self.current.cols, self.current.rows);
        if cols == 0 || rows == 0 {
            return ValidationResult::Invalid;
        }

        let clamped_x = x.min(cols - 1);
        let clamped_y = y.min(rows - 1);
        let resolution = if clamped_x == x && clamped_y == y {
            CoordResolution::Exact
        } else {
            CoordResolution::Clamped
        };

        ValidationResult::Valid(ValidatedCellCoord {
            col: clamped_x,
            row: clamped_y,
            resolution,
        })
    }

    /// Validate cell coordinates with stale-geometry detection.
    ///
    /// `source_version` is the geometry version the JS host used to compute
    /// the coordinates. If it differs from the current version, the result
    /// is tagged as `Remapped`.
    #[must_use]
    pub fn validate_versioned(
        &self,
        x: u16,
        y: u16,
        source_version: GeometryVersion,
    ) -> ValidationResult {
        let (cols, rows) = (self.current.cols, self.current.rows);
        if cols == 0 || rows == 0 {
            return ValidationResult::Invalid;
        }

        let clamped_x = x.min(cols - 1);
        let clamped_y = y.min(rows - 1);
        let resolution = if clamped_x == x && clamped_y == y {
            CoordResolution::Exact
        } else {
            CoordResolution::Clamped
        };

        let coord = ValidatedCellCoord {
            col: clamped_x,
            row: clamped_y,
            resolution,
        };

        if source_version != self.current.version {
            ValidationResult::Remapped {
                coord,
                source_version,
            }
        } else {
            ValidationResult::Valid(coord)
        }
    }

    /// Map pixel coordinates (device pixels) to cell coordinates.
    ///
    /// This performs the pixel → cell conversion that normally happens on
    /// the JS side, useful for server-side rendering or testing.
    #[must_use]
    pub fn pixel_to_cell(&self, pixel_x: f32, pixel_y: f32) -> ValidationResult {
        let s = &self.current;
        if s.cols == 0 || s.rows == 0 || s.cell_width_px <= 0.0 || s.cell_height_px <= 0.0 {
            return ValidationResult::Invalid;
        }

        let raw_col = (pixel_x / s.cell_width_px).floor();
        let raw_row = (pixel_y / s.cell_height_px).floor();

        // Clamp to valid range.
        let col = raw_col.clamp(0.0, f32::from(s.cols - 1)) as u16;
        let row = raw_row.clamp(0.0, f32::from(s.rows - 1)) as u16;

        let resolution = if raw_col >= 0.0
            && raw_col < f32::from(s.cols)
            && raw_row >= 0.0
            && raw_row < f32::from(s.rows)
        {
            CoordResolution::Exact
        } else {
            CoordResolution::Clamped
        };

        ValidationResult::Valid(ValidatedCellCoord {
            col,
            row,
            resolution,
        })
    }

    /// Map CSS pixel coordinates to cell coordinates, accounting for DPR.
    #[must_use]
    pub fn css_to_cell(&self, css_x: f32, css_y: f32) -> ValidationResult {
        let dpr = self.current.dpr;
        if dpr <= 0.0 || !dpr.is_finite() {
            return ValidationResult::Invalid;
        }
        self.pixel_to_cell(css_x * dpr, css_y * dpr)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::grid_geometry;

    fn make_snapshot(cols: u16, rows: u16, version: GeometryVersion) -> GeometrySnapshot {
        let geom = grid_geometry(cols, rows, 8, 16, 1.0, 1.0);
        GeometrySnapshot::capture(&geom, version)
    }

    fn make_guard() -> CoordinateGuard {
        CoordinateGuard::new(make_snapshot(80, 24, 1))
    }

    // -- ValidatedCellCoord --

    #[test]
    fn offset_computation() {
        let coord = ValidatedCellCoord {
            col: 5,
            row: 3,
            resolution: CoordResolution::Exact,
        };
        assert_eq!(coord.offset(80), 3 * 80 + 5);
    }

    #[test]
    fn was_clamped_false_for_exact() {
        let coord = ValidatedCellCoord {
            col: 0,
            row: 0,
            resolution: CoordResolution::Exact,
        };
        assert!(!coord.was_clamped());
    }

    #[test]
    fn was_clamped_true_for_clamped() {
        let coord = ValidatedCellCoord {
            col: 0,
            row: 0,
            resolution: CoordResolution::Clamped,
        };
        assert!(coord.was_clamped());
    }

    // -- GeometrySnapshot --

    #[test]
    fn snapshot_capture() {
        let geom = grid_geometry(80, 24, 8, 16, 2.0, 1.5);
        let snap = GeometrySnapshot::capture(&geom, 42);
        assert_eq!(snap.cols, 80);
        assert_eq!(snap.rows, 24);
        assert!((snap.dpr - 2.0).abs() < f32::EPSILON);
        assert!((snap.zoom - 1.5).abs() < f32::EPSILON);
        assert_eq!(snap.version, 42);
    }

    #[test]
    fn snapshot_same_grid() {
        let s1 = make_snapshot(80, 24, 1);
        let s2 = make_snapshot(80, 24, 2);
        let s3 = make_snapshot(120, 40, 3);
        assert!(s1.same_grid(&s2));
        assert!(!s1.same_grid(&s3));
    }

    #[test]
    fn snapshot_same_scale() {
        let g1 = grid_geometry(80, 24, 8, 16, 2.0, 1.0);
        let g2 = grid_geometry(120, 40, 8, 16, 2.0, 1.0);
        let g3 = grid_geometry(80, 24, 8, 16, 1.0, 1.5);
        let s1 = GeometrySnapshot::capture(&g1, 1);
        let s2 = GeometrySnapshot::capture(&g2, 2);
        let s3 = GeometrySnapshot::capture(&g3, 3);
        assert!(s1.same_scale(&s2));
        assert!(!s1.same_scale(&s3));
    }

    // -- Basic validation --

    #[test]
    fn validate_in_bounds() {
        let guard = make_guard();
        let result = guard.validate(10, 5);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 10);
        assert_eq!(coord.row, 5);
        assert!(!coord.was_clamped());
    }

    #[test]
    fn validate_at_max_bounds() {
        let guard = make_guard();
        let result = guard.validate(79, 23); // 80 cols, 24 rows → max is 79, 23
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 79);
        assert_eq!(coord.row, 23);
        assert!(!coord.was_clamped());
    }

    #[test]
    fn validate_out_of_bounds_clamps() {
        let guard = make_guard();
        let result = guard.validate(100, 30);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 79);
        assert_eq!(coord.row, 23);
        assert!(coord.was_clamped());
    }

    #[test]
    fn validate_x_out_of_bounds_only() {
        let guard = make_guard();
        let result = guard.validate(200, 10);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 79);
        assert_eq!(coord.row, 10);
        assert!(coord.was_clamped());
    }

    #[test]
    fn validate_y_out_of_bounds_only() {
        let guard = make_guard();
        let result = guard.validate(10, 50);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 10);
        assert_eq!(coord.row, 23);
        assert!(coord.was_clamped());
    }

    #[test]
    fn validate_origin() {
        let guard = make_guard();
        let result = guard.validate(0, 0);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 0);
        assert_eq!(coord.row, 0);
        assert!(!coord.was_clamped());
    }

    // -- Versioned validation --

    #[test]
    fn versioned_same_version_is_valid() {
        let guard = make_guard();
        let result = guard.validate_versioned(10, 5, 1);
        assert!(!result.was_remapped());
        assert!(result.coord().is_some());
    }

    #[test]
    fn versioned_different_version_is_remapped() {
        let guard = make_guard();
        let result = guard.validate_versioned(10, 5, 0); // version 0 ≠ guard version 1
        assert!(result.was_remapped());
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 10);
        assert_eq!(coord.row, 5);
    }

    #[test]
    fn versioned_stale_and_out_of_bounds() {
        let guard = make_guard();
        let result = guard.validate_versioned(100, 30, 0);
        assert!(result.was_remapped());
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 79);
        assert_eq!(coord.row, 23);
        assert!(coord.was_clamped());
    }

    #[test]
    fn versioned_source_version_preserved() {
        let guard = make_guard();
        if let ValidationResult::Remapped { source_version, .. } =
            guard.validate_versioned(10, 5, 42)
        {
            assert_eq!(source_version, 42);
        } else {
            panic!("Expected Remapped");
        }
    }

    // -- Pixel to cell --

    #[test]
    fn pixel_to_cell_basic() {
        let guard = make_guard();
        // Cell is 8px wide, 16px tall at DPR=1.0
        let result = guard.pixel_to_cell(24.0, 48.0);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 3); // 24 / 8 = 3
        assert_eq!(coord.row, 3); // 48 / 16 = 3
        assert!(!coord.was_clamped());
    }

    #[test]
    fn pixel_to_cell_fractional() {
        let guard = make_guard();
        let result = guard.pixel_to_cell(12.5, 20.3);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 1); // floor(12.5 / 8) = 1
        assert_eq!(coord.row, 1); // floor(20.3 / 16) = 1
    }

    #[test]
    fn pixel_to_cell_out_of_bounds_clamps() {
        let guard = make_guard();
        let result = guard.pixel_to_cell(9999.0, 9999.0);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 79);
        assert_eq!(coord.row, 23);
        assert!(coord.was_clamped());
    }

    #[test]
    fn pixel_to_cell_negative_clamps() {
        let guard = make_guard();
        let result = guard.pixel_to_cell(-10.0, -5.0);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 0);
        assert_eq!(coord.row, 0);
        assert!(coord.was_clamped());
    }

    // -- CSS to cell --

    #[test]
    fn css_to_cell_dpr_1() {
        let guard = make_guard(); // DPR=1.0
        let result = guard.css_to_cell(24.0, 48.0);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 3);
        assert_eq!(coord.row, 3);
    }

    #[test]
    fn css_to_cell_dpr_2() {
        let geom = grid_geometry(80, 24, 8, 16, 2.0, 1.0);
        let snap = GeometrySnapshot::capture(&geom, 1);
        let guard = CoordinateGuard::new(snap);
        // At DPR=2, css 24px → 48 device px; cell_width_px = 8*2=16
        // 48 / 16 = 3
        let result = guard.css_to_cell(24.0, 48.0);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 3);
        assert_eq!(coord.row, 3); // 48*2=96; cell_height=16*2=32; 96/32=3
    }

    // -- Guard update --

    #[test]
    fn update_changes_validation_bounds() {
        let mut guard = make_guard();
        assert!(guard.validate(50, 30).coord().unwrap().was_clamped()); // 30 > 23

        guard.update(make_snapshot(80, 40, 2)); // Now 40 rows
        assert!(!guard.validate(50, 30).coord().unwrap().was_clamped()); // 30 < 39
    }

    #[test]
    fn update_changes_version() {
        let mut guard = make_guard();
        assert_eq!(guard.version(), 1);
        guard.update(make_snapshot(80, 24, 5));
        assert_eq!(guard.version(), 5);
    }

    // -- Grid dims --

    #[test]
    fn grid_dims_returns_current() {
        let guard = make_guard();
        assert_eq!(guard.grid_dims(), (80, 24));
    }

    // -- Edge cases --

    #[test]
    fn zero_grid_returns_invalid() {
        let snap = GeometrySnapshot {
            cols: 0,
            rows: 0,
            cell_width_px: 8.0,
            cell_height_px: 16.0,
            dpr: 1.0,
            zoom: 1.0,
            version: 1,
        };
        let guard = CoordinateGuard::new(snap);
        assert!(matches!(guard.validate(0, 0), ValidationResult::Invalid));
    }

    #[test]
    fn one_cell_grid() {
        let guard = CoordinateGuard::new(make_snapshot(1, 1, 1));
        let coord = guard.validate(0, 0).coord().unwrap();
        assert_eq!(coord.col, 0);
        assert_eq!(coord.row, 0);
        assert!(!coord.was_clamped());

        let coord = guard.validate(5, 5).coord().unwrap();
        assert_eq!(coord.col, 0);
        assert_eq!(coord.row, 0);
        assert!(coord.was_clamped());
    }

    #[test]
    fn max_u16_coords() {
        let guard = make_guard();
        let result = guard.validate(u16::MAX, u16::MAX);
        let coord = result.coord().unwrap();
        assert_eq!(coord.col, 79);
        assert_eq!(coord.row, 23);
        assert!(coord.was_clamped());
    }

    #[test]
    fn pixel_to_cell_nan_clamps() {
        let guard = make_guard();
        let result = guard.pixel_to_cell(f32::NAN, f32::NAN);
        // NaN behavior: floor(NaN) = NaN, clamp(NaN, 0, max) = 0 on most platforms
        // The important thing is no panic
        assert!(result.coord().is_some());
    }

    #[test]
    fn pixel_to_cell_infinity_clamps() {
        let guard = make_guard();
        let result = guard.pixel_to_cell(f32::INFINITY, f32::NEG_INFINITY);
        let coord = result.coord().unwrap();
        // +inf → clamped to max col, -inf → clamped to 0 row
        assert_eq!(coord.col, 79);
        assert_eq!(coord.row, 0);
    }

    #[test]
    fn css_to_cell_zero_dpr_returns_invalid() {
        let snap = GeometrySnapshot {
            cols: 80,
            rows: 24,
            cell_width_px: 8.0,
            cell_height_px: 16.0,
            dpr: 0.0,
            zoom: 1.0,
            version: 1,
        };
        let guard = CoordinateGuard::new(snap);
        assert!(matches!(
            guard.css_to_cell(10.0, 10.0),
            ValidationResult::Invalid
        ));
    }

    #[test]
    fn css_to_cell_nan_dpr_returns_invalid() {
        let snap = GeometrySnapshot {
            cols: 80,
            rows: 24,
            cell_width_px: 8.0,
            cell_height_px: 16.0,
            dpr: f32::NAN,
            zoom: 1.0,
            version: 1,
        };
        let guard = CoordinateGuard::new(snap);
        assert!(matches!(
            guard.css_to_cell(10.0, 10.0),
            ValidationResult::Invalid
        ));
    }

    // -- Validation result helpers --

    #[test]
    fn validation_result_coord_extracts() {
        let guard = make_guard();
        assert!(guard.validate(10, 5).coord().is_some());
    }

    #[test]
    fn validation_result_invalid_has_no_coord() {
        assert!(ValidationResult::Invalid.coord().is_none());
    }

    #[test]
    fn validation_result_was_remapped() {
        let guard = make_guard();
        let valid = guard.validate_versioned(10, 5, 1);
        assert!(!valid.was_remapped());
        let remapped = guard.validate_versioned(10, 5, 0);
        assert!(remapped.was_remapped());
    }

    // -- Determinism --

    #[test]
    fn deterministic_same_input_same_output() {
        let guard = make_guard();
        let r1 = guard.validate(50, 20);
        let r2 = guard.validate(50, 20);
        assert_eq!(r1.coord(), r2.coord());
    }

    #[test]
    fn deterministic_pixel_mapping() {
        let guard = make_guard();
        let r1 = guard.pixel_to_cell(123.456, 78.9);
        let r2 = guard.pixel_to_cell(123.456, 78.9);
        assert_eq!(r1.coord(), r2.coord());
    }
}

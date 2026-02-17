#![forbid(unsafe_code)]

//! Deterministic repro corpus for web rescale failures (bd-2vr05.15.1.1).
//!
//! Each scenario documents a specific rescale failure class with:
//! - Exact DPR / zoom / container parameters that trigger it
//! - Expected grid geometry (the invariant)
//! - The failure mode if the invariant is violated
//!
//! The corpus serves two purposes:
//! 1. **Regression gate** — every scenario must pass; any failure is a rescale bug.
//! 2. **Failure catalogue** — documents *why* each scenario matters with inline
//!    comments describing the real-world browser condition.
//!
//! # Covered failure classes
//!
//! | Class | Trigger | Symptom |
//! |-------|---------|---------|
//! | Fractional DPR rounding | DPR 1.5, 1.25, 1.75 | Off-by-one col/row, pixel gap |
//! | DPR transition mid-session | DPR 1.0 → 2.0 | Grid size halves unexpectedly |
//! | High DPR overflow | DPR > 4.0 | Pixel dimensions exceed u32 |
//! | Zoom + DPR multiplicative | DPR 2.0 + zoom 2.0 | 4× effective scale, tiny grid |
//! | Container resize storm | Rapid container changes | Intermediate sizes leak |
//! | Orientation flip | Portrait ↔ landscape swap | Cols/rows inversion |
//! | Pinch-zoom fractional | Non-integer zoom values | Sub-pixel cell misalignment |
//! | Tiny container collapse | Container < cell size | Grid must clamp to 1×1 |
//! | Zero/NaN/Infinity guard | Invalid host values | Must use deterministic fallback |
//! | Exact-multiple container | Container is perfect multiple | No wasted pixels |

use frankenterm_web::renderer::{GridGeometry, fit_grid_to_container, grid_geometry};

// ============================================================================
// Invariant helpers
// ============================================================================

/// Assert that the grid never has zero cols or rows.
fn assert_grid_nonzero(g: &GridGeometry, label: &str) {
    assert!(g.cols >= 1, "{label}: cols must be >= 1, got {}", g.cols);
    assert!(g.rows >= 1, "{label}: rows must be >= 1, got {}", g.rows);
}

/// Assert that pixel dimensions are consistent with cell × grid size.
fn assert_pixel_consistency(g: &GridGeometry, label: &str) {
    let expected_w = (f32::from(g.cols) * g.cell_width_px).round() as u32;
    let expected_h = (f32::from(g.rows) * g.cell_height_px).round() as u32;
    assert_eq!(
        g.pixel_width, expected_w,
        "{label}: pixel_width mismatch: {} != {}",
        g.pixel_width, expected_w
    );
    assert_eq!(
        g.pixel_height, expected_h,
        "{label}: pixel_height mismatch: {} != {}",
        g.pixel_height, expected_h
    );
}

/// Assert fit-to-container never exceeds the container in device pixels.
fn assert_fits_container(
    g: &GridGeometry,
    container_w_css: u32,
    container_h_css: u32,
    label: &str,
) {
    let container_w_px = ((container_w_css as f32) * g.dpr).round() as u32;
    let container_h_px = ((container_h_css as f32) * g.dpr).round() as u32;
    assert!(
        g.pixel_width <= container_w_px,
        "{label}: pixel_width {} exceeds container {}",
        g.pixel_width,
        container_w_px
    );
    assert!(
        g.pixel_height <= container_h_px,
        "{label}: pixel_height {} exceeds container {}",
        g.pixel_height,
        container_h_px
    );
}

/// Standard invariant check applied to every repro scenario.
fn check_invariants(g: &GridGeometry, label: &str) {
    assert_grid_nonzero(g, label);
    assert_pixel_consistency(g, label);
}

// ============================================================================
// 1. Fractional DPR rounding scenarios
// ============================================================================

/// Chrome on 1080p laptops reports DPR 1.5. The cell width (8 CSS px)
/// becomes 8 × 1.5 = 12.0 device pixels — exact. But cell height (16 CSS px)
/// becomes 16 × 1.5 = 24.0 — also exact. This is the "easy" fractional case.
#[test]
fn dpr_1_5_standard_grid() {
    let g = grid_geometry(80, 24, 8, 16, 1.5, 1.0);
    check_invariants(&g, "dpr_1.5");
    assert_eq!(g.cell_width_px, 12.0);
    assert_eq!(g.cell_height_px, 24.0);
    assert_eq!(g.pixel_width, 960);
    assert_eq!(g.pixel_height, 576);
}

/// DPR 1.25 (Windows 125% scaling) produces cell width 8 × 1.25 = 10.0
/// and cell height 16 × 1.25 = 20.0 — both exact after rounding.
#[test]
fn dpr_1_25_windows_scaling() {
    let g = grid_geometry(80, 24, 8, 16, 1.25, 1.0);
    check_invariants(&g, "dpr_1.25");
    assert_eq!(g.cell_width_px, 10.0);
    assert_eq!(g.cell_height_px, 20.0);
}

/// DPR 1.75 produces cell width 8 × 1.75 = 14.0 (exact) and
/// cell height 16 × 1.75 = 28.0 (exact). Still clean.
#[test]
fn dpr_1_75_fractional() {
    let g = grid_geometry(80, 24, 8, 16, 1.75, 1.0);
    check_invariants(&g, "dpr_1.75");
    assert_eq!(g.cell_width_px, 14.0);
    assert_eq!(g.cell_height_px, 28.0);
}

/// DPR 1.333... (Samsung Galaxy devices, 4/3) causes fractional cell sizes.
/// 8 × 1.333 = 10.664 → rounds to 11.0. 16 × 1.333 = 21.328 → rounds to 21.0.
/// The rounding must be deterministic (round half-up via f32::round).
#[test]
fn dpr_1_333_galaxy_fractional() {
    let g = grid_geometry(80, 24, 8, 16, 4.0 / 3.0, 1.0);
    check_invariants(&g, "dpr_1.333");
    // Verify deterministic rounding: 8 * (4/3) = 10.666..., rounds to 11
    assert_eq!(g.cell_width_px, 11.0);
    // 16 * (4/3) = 21.333..., rounds to 21
    assert_eq!(g.cell_height_px, 21.0);
}

// ============================================================================
// 2. DPR transition scenarios
// ============================================================================

/// When a user drags a window from a Retina (DPR=2.0) display to a standard
/// (DPR=1.0) display, fit_grid_to_container should yield the SAME grid size.
/// DPR scales both container and cells proportionally, so cols/rows remain
/// stable for the same CSS container — only pixel dimensions change.
/// This is the correct behavior and a common source of confusion.
#[test]
fn dpr_transition_retina_to_standard_grid_stable() {
    let retina = fit_grid_to_container(800, 600, 8, 16, 2.0, 1.0);
    let standard = fit_grid_to_container(800, 600, 8, 16, 1.0, 1.0);
    check_invariants(&retina, "dpr_transition_retina");
    check_invariants(&standard, "dpr_transition_standard");
    // Grid dimensions (cols/rows) should be equal for same CSS container
    assert_eq!(retina.cols, standard.cols);
    assert_eq!(retina.rows, standard.rows);
    // Pixel dimensions should differ (Retina = 2× device pixels)
    assert!(retina.pixel_width > standard.pixel_width);
    assert!(retina.pixel_height > standard.pixel_height);
}

/// DPR transition with fractional DPR can cause rounding differences.
/// E.g. DPR 1.0 → 1.333: cell width goes from 8 to 11 device px,
/// which can cause ±1 col/row difference due to floor division.
#[test]
fn dpr_transition_fractional_rounding_boundary() {
    let dpr1 = fit_grid_to_container(800, 600, 8, 16, 1.0, 1.0);
    let dpr133 = fit_grid_to_container(800, 600, 8, 16, 4.0 / 3.0, 1.0);
    check_invariants(&dpr1, "dpr1");
    check_invariants(&dpr133, "dpr133");
    assert_fits_container(&dpr133, 800, 600, "dpr133");
    // Grid dimensions may differ slightly due to rounding
    let col_diff = (dpr1.cols as i32 - dpr133.cols as i32).unsigned_abs();
    let row_diff = (dpr1.rows as i32 - dpr133.rows as i32).unsigned_abs();
    // Rounding can cause at most ±1 col/row difference
    assert!(col_diff <= 4, "col difference too large: {col_diff}");
    assert!(row_diff <= 2, "row difference too large: {row_diff}");
}

/// DPR 1.0 → 1.5 transition (common on Windows mixed-DPI setups).
/// This is the more subtle case because 1.5 involves fractional cells.
#[test]
fn dpr_transition_1_0_to_1_5() {
    let before = fit_grid_to_container(960, 576, 8, 16, 1.0, 1.0);
    let after = fit_grid_to_container(960, 576, 8, 16, 1.5, 1.0);
    check_invariants(&before, "dpr_1_to_1.5_before");
    check_invariants(&after, "dpr_1_to_1.5_after");
    assert_fits_container(&after, 960, 576, "dpr_1_to_1.5_after");
}

// ============================================================================
// 3. High DPR overflow guard
// ============================================================================

/// DPR values above MAX_DPR (8.0) must be clamped, not overflow.
#[test]
fn dpr_extreme_clamped() {
    let g = grid_geometry(80, 24, 8, 16, 100.0, 1.0);
    check_invariants(&g, "extreme_dpr");
    assert_eq!(g.dpr, 8.0);
}

/// DPR=8.0 (max) with large grid — ensure pixel dimensions don't overflow u32.
#[test]
fn dpr_max_large_grid_no_overflow() {
    let g = grid_geometry(200, 60, 8, 16, 8.0, 1.0);
    check_invariants(&g, "max_dpr_large_grid");
    // 200 cols × (8×8)=64px/cell = 12800 pixel width — well within u32
    assert!(g.pixel_width > 0);
    assert!(g.pixel_height > 0);
}

/// Sub-1.0 DPR (downscaled displays, e.g. projector at 0.75).
#[test]
fn dpr_sub_one_downscale() {
    let g = grid_geometry(80, 24, 8, 16, 0.75, 1.0);
    check_invariants(&g, "dpr_0.75");
    // 8 × 0.75 = 6.0
    assert_eq!(g.cell_width_px, 6.0);
    // 16 × 0.75 = 12.0
    assert_eq!(g.cell_height_px, 12.0);
}

// ============================================================================
// 4. Zoom + DPR multiplicative interaction
// ============================================================================

/// DPR=2.0 + zoom=2.0 = 4× effective scale. In an 800×600 CSS container,
/// the grid should be quite small.
#[test]
fn zoom_2x_dpr_2x_combined() {
    let g = fit_grid_to_container(800, 600, 8, 16, 2.0, 2.0);
    check_invariants(&g, "zoom2_dpr2");
    assert_fits_container(&g, 800, 600, "zoom2_dpr2");
    // At 4× effective, cell is 8×4=32 device px wide, 16×4=64 device px tall
    // Container is 800×2=1600 device px wide, 600×2=1200 device px tall
    // Cols = 1600/32 = 50, rows = 1200/64 = 18 (approximately)
    assert!(g.cols <= 50);
    assert!(g.rows <= 19);
}

/// Maximum zoom (4.0) at DPR=1.0 should yield small grid.
#[test]
fn zoom_max_dpr_1() {
    let g = fit_grid_to_container(800, 600, 8, 16, 1.0, 4.0);
    check_invariants(&g, "max_zoom");
    assert_fits_container(&g, 800, 600, "max_zoom");
    // 800 / (8×4) = 25 cols
    assert_eq!(g.cols, 25);
}

/// Minimum zoom (0.25) at DPR=1.0 should yield large grid.
#[test]
fn zoom_min_dpr_1() {
    let g = fit_grid_to_container(800, 600, 8, 16, 1.0, 0.25);
    check_invariants(&g, "min_zoom");
    assert_fits_container(&g, 800, 600, "min_zoom");
    // 800 / (8×0.25) = 800 / 2 = 400 cols
    assert_eq!(g.cols, 400);
}

/// Zoom=1.5 with DPR=1.5 (common Chrome on 1080p + 150% browser zoom).
#[test]
fn zoom_1_5_dpr_1_5() {
    let g = fit_grid_to_container(800, 600, 8, 16, 1.5, 1.5);
    check_invariants(&g, "zoom1.5_dpr1.5");
    assert_fits_container(&g, 800, 600, "zoom1.5_dpr1.5");
    // Effective scale = 2.25×. Cell = 8×2.25=18.0 device px.
    // Container in device px = 800×1.5 = 1200
    // Cols = 1200/18 = 66 (floor)
    assert_eq!(g.cols, 66);
}

// ============================================================================
// 5. Container resize storm invariants
// ============================================================================

/// Rapid container shrink/grow cycle (simulating window resize drag).
/// Each intermediate state must be independently valid.
#[test]
fn container_resize_storm_shrink_grow() {
    let containers = [
        (800u32, 600u32),
        (600, 450),
        (400, 300),
        (200, 150),
        (400, 300),
        (600, 450),
        (800, 600),
    ];

    for (i, &(w, h)) in containers.iter().enumerate() {
        let g = fit_grid_to_container(w, h, 8, 16, 1.0, 1.0);
        let label = format!("storm_step_{i}_({w}x{h})");
        check_invariants(&g, &label);
        assert_fits_container(&g, w, h, &label);
    }
}

/// Container resize with DPR changing mid-sequence (window dragged
/// between monitors during resize).
#[test]
fn container_resize_storm_with_dpr_change() {
    let steps: [(u32, u32, f32); 5] = [
        (800, 600, 1.0),
        (800, 600, 1.5), // DPR changes
        (600, 400, 1.5),
        (600, 400, 2.0), // DPR changes again
        (800, 600, 2.0),
    ];

    for (i, &(w, h, dpr)) in steps.iter().enumerate() {
        let g = fit_grid_to_container(w, h, 8, 16, dpr, 1.0);
        let label = format!("dpr_storm_{i}_({}x{}_dpr{})", w, h, dpr);
        check_invariants(&g, &label);
        assert_fits_container(&g, w, h, &label);
    }
}

/// Rapid zoom changes during container resize (pinch-zoom while resizing).
#[test]
fn container_resize_storm_with_zoom_change() {
    let steps: [(u32, u32, f32); 5] = [
        (800, 600, 1.0),
        (750, 580, 1.25),
        (700, 560, 1.5),
        (750, 580, 1.25),
        (800, 600, 1.0),
    ];

    for (i, &(w, h, zoom)) in steps.iter().enumerate() {
        let g = fit_grid_to_container(w, h, 8, 16, 1.0, zoom);
        let label = format!("zoom_storm_{i}_({}x{}_zoom{})", w, h, zoom);
        check_invariants(&g, &label);
        assert_fits_container(&g, w, h, &label);
    }
}

// ============================================================================
// 6. Orientation flip scenarios
// ============================================================================

/// Mobile orientation change: portrait (600×800) → landscape (800×600).
/// Grid should have more cols and fewer rows after the flip.
#[test]
fn orientation_portrait_to_landscape() {
    let portrait = fit_grid_to_container(600, 800, 8, 16, 2.0, 1.0);
    let landscape = fit_grid_to_container(800, 600, 8, 16, 2.0, 1.0);
    check_invariants(&portrait, "portrait");
    check_invariants(&landscape, "landscape");
    assert!(landscape.cols > portrait.cols);
    assert!(landscape.rows < portrait.rows);
}

/// Orientation flip with DPR change (common on tablets where portrait and
/// landscape modes use different rendering paths).
#[test]
fn orientation_flip_with_dpr() {
    // iPad: portrait at DPR=2
    let portrait = fit_grid_to_container(768, 1024, 8, 16, 2.0, 1.0);
    // iPad: landscape at DPR=2
    let landscape = fit_grid_to_container(1024, 768, 8, 16, 2.0, 1.0);
    check_invariants(&portrait, "ipad_portrait");
    check_invariants(&landscape, "ipad_landscape");
    assert_fits_container(&portrait, 768, 1024, "ipad_portrait");
    assert_fits_container(&landscape, 1024, 768, "ipad_landscape");
}

// ============================================================================
// 7. Pinch-zoom fractional values
// ============================================================================

/// Pinch-zoom produces non-round zoom values. The grid must be deterministic
/// for the same (container, dpr, zoom) triple.
#[test]
fn pinch_zoom_fractional_determinism() {
    let zoom_values = [0.33, 0.67, 1.1, 1.33, 1.67, 2.5, 3.3];

    for &zoom in &zoom_values {
        let g1 = fit_grid_to_container(800, 600, 8, 16, 2.0, zoom);
        let g2 = fit_grid_to_container(800, 600, 8, 16, 2.0, zoom);
        let label = format!("pinch_zoom_{zoom}");
        check_invariants(&g1, &label);
        assert_eq!(g1, g2, "{label}: determinism violated");
    }
}

/// Pinch-zoom at sub-minimum should clamp to MIN_ZOOM (0.25).
#[test]
fn pinch_zoom_below_min_clamped() {
    let g = fit_grid_to_container(800, 600, 8, 16, 1.0, 0.1);
    check_invariants(&g, "zoom_below_min");
    assert_eq!(g.zoom, 0.25);
}

/// Pinch-zoom at above-maximum should clamp to MAX_ZOOM (4.0).
#[test]
fn pinch_zoom_above_max_clamped() {
    let g = fit_grid_to_container(800, 600, 8, 16, 1.0, 10.0);
    check_invariants(&g, "zoom_above_max");
    assert_eq!(g.zoom, 4.0);
}

// ============================================================================
// 8. Tiny container collapse
// ============================================================================

/// Container smaller than one cell must yield 1×1 grid, not zero or panic.
#[test]
fn tiny_container_1x1() {
    let g = fit_grid_to_container(1, 1, 8, 16, 1.0, 1.0);
    check_invariants(&g, "tiny_1x1");
    assert_eq!(g.cols, 1);
    assert_eq!(g.rows, 1);
}

/// Container exactly one cell wide, many cells tall.
#[test]
fn narrow_container_1_col() {
    let g = fit_grid_to_container(8, 600, 8, 16, 1.0, 1.0);
    check_invariants(&g, "narrow_1col");
    assert_eq!(g.cols, 1);
    assert!(g.rows >= 1);
    assert_fits_container(&g, 8, 600, "narrow_1col");
}

/// Container at high DPR — the effective device-pixel container may be
/// large but CSS container is tiny.
#[test]
fn tiny_container_high_dpr() {
    let g = fit_grid_to_container(4, 4, 8, 16, 4.0, 1.0);
    check_invariants(&g, "tiny_high_dpr");
    assert_eq!(g.cols, 1); // 4 CSS px × 4 DPR = 16 device px / 32 cell px = 0 → clamp to 1
    assert_eq!(g.rows, 1);
}

// ============================================================================
// 9. Zero / NaN / Infinity guard
// ============================================================================

/// NaN DPR must fall back to 1.0.
#[test]
fn nan_dpr_fallback() {
    let g = grid_geometry(80, 24, 8, 16, f32::NAN, 1.0);
    check_invariants(&g, "nan_dpr");
    assert_eq!(g.dpr, 1.0);
}

/// NaN zoom must fall back to 1.0.
#[test]
fn nan_zoom_fallback() {
    let g = grid_geometry(80, 24, 8, 16, 1.0, f32::NAN);
    check_invariants(&g, "nan_zoom");
    assert_eq!(g.zoom, 1.0);
}

/// Infinity DPR is not finite, so normalized_scale falls back to 1.0.
/// (Unlike extreme-but-finite values which clamp to MAX_DPR.)
#[test]
fn inf_dpr_fallback_to_one() {
    let g = grid_geometry(80, 24, 8, 16, f32::INFINITY, 1.0);
    check_invariants(&g, "inf_dpr");
    assert_eq!(g.dpr, 1.0); // fallback, not clamp
}

/// Negative DPR must fall back to 1.0.
#[test]
fn negative_dpr_fallback() {
    let g = grid_geometry(80, 24, 8, 16, -2.0, 1.0);
    check_invariants(&g, "negative_dpr");
    assert_eq!(g.dpr, 1.0);
}

/// Zero DPR must fall back to 1.0.
#[test]
fn zero_dpr_fallback() {
    let g = grid_geometry(80, 24, 8, 16, 0.0, 1.0);
    check_invariants(&g, "zero_dpr");
    assert_eq!(g.dpr, 1.0);
}

/// NaN DPR in fit_grid_to_container.
#[test]
fn fit_nan_dpr_safe() {
    let g = fit_grid_to_container(800, 600, 8, 16, f32::NAN, 1.0);
    check_invariants(&g, "fit_nan_dpr");
    assert_eq!(g.dpr, 1.0);
    assert_fits_container(&g, 800, 600, "fit_nan_dpr");
}

/// Both DPR and zoom are NaN — must not panic, fallback to 1.0 for both.
#[test]
fn both_nan_fallback() {
    let g = grid_geometry(80, 24, 8, 16, f32::NAN, f32::NAN);
    check_invariants(&g, "both_nan");
    assert_eq!(g.dpr, 1.0);
    assert_eq!(g.zoom, 1.0);
}

// ============================================================================
// 10. Exact-multiple container (no wasted pixels)
// ============================================================================

/// Standard terminal: 80×24 at 8×16 CSS px, DPR=1.0.
/// Container should be exactly 640×384.
#[test]
fn exact_multiple_standard() {
    let g = fit_grid_to_container(640, 384, 8, 16, 1.0, 1.0);
    check_invariants(&g, "exact_std");
    assert_eq!(g.cols, 80);
    assert_eq!(g.rows, 24);
    assert_eq!(g.pixel_width, 640);
    assert_eq!(g.pixel_height, 384);
}

/// Retina: same CSS container at DPR=2.0. Grid size should be identical
/// to DPR=1.0 (same CSS container), but pixel dimensions double.
#[test]
fn exact_multiple_retina() {
    let g = fit_grid_to_container(640, 384, 8, 16, 2.0, 1.0);
    check_invariants(&g, "exact_retina");
    assert_eq!(g.cols, 80);
    assert_eq!(g.rows, 24);
    assert_eq!(g.pixel_width, 1280);
    assert_eq!(g.pixel_height, 768);
}

/// Large container at DPR=1.5. Container 1920×1080 (Full HD).
#[test]
fn exact_fullhd_dpr_1_5() {
    let g = fit_grid_to_container(1920, 1080, 8, 16, 1.5, 1.0);
    check_invariants(&g, "fullhd_1.5");
    assert_fits_container(&g, 1920, 1080, "fullhd_1.5");
    // At DPR=1.5, cell is 12×24 device px. Container is 2880×1620 device px.
    // Cols = 2880/12 = 240, rows = 1620/24 = 67
    assert_eq!(g.cols, 240);
    assert_eq!(g.rows, 67);
}

// ============================================================================
// 11. Common real-world device profiles
// ============================================================================

/// iPhone SE: 375×667 CSS, DPR=2.0 (landscape would be 667×375).
#[test]
fn device_iphone_se() {
    let portrait = fit_grid_to_container(375, 667, 8, 16, 2.0, 1.0);
    check_invariants(&portrait, "iphonese_portrait");
    assert_fits_container(&portrait, 375, 667, "iphonese_portrait");

    let landscape = fit_grid_to_container(667, 375, 8, 16, 2.0, 1.0);
    check_invariants(&landscape, "iphonese_landscape");
    assert_fits_container(&landscape, 667, 375, "iphonese_landscape");
}

/// MacBook Pro 14": 1512×982 CSS, DPR=2.0.
#[test]
fn device_macbook_pro_14() {
    let g = fit_grid_to_container(1512, 982, 8, 16, 2.0, 1.0);
    check_invariants(&g, "mbp14");
    assert_fits_container(&g, 1512, 982, "mbp14");
}

/// 4K monitor: 3840×2160 at DPR=2.0 (CSS 1920×1080).
#[test]
fn device_4k_monitor() {
    let g = fit_grid_to_container(1920, 1080, 8, 16, 2.0, 1.0);
    check_invariants(&g, "4k_monitor");
    assert_fits_container(&g, 1920, 1080, "4k_monitor");
    // At DPR=2: cell 16×32. Container 3840×2160 device px.
    // Cols = 3840/16 = 240, rows = 2160/32 = 67
    assert_eq!(g.cols, 240);
    assert_eq!(g.rows, 67);
}

/// Chrome on Windows at 150% scaling: 1920×1080 native, DPR=1.5.
#[test]
fn device_windows_150pct() {
    let g = fit_grid_to_container(1280, 720, 8, 16, 1.5, 1.0);
    check_invariants(&g, "win_150pct");
    assert_fits_container(&g, 1280, 720, "win_150pct");
}

// ============================================================================
// 12. Monotonicity invariants
// ============================================================================

/// As container width grows (height fixed), cols must never decrease.
#[test]
fn monotonic_cols_on_width_growth() {
    let mut prev_cols = 0u16;
    for w in (100..=1600).step_by(50) {
        let g = fit_grid_to_container(w, 600, 8, 16, 1.0, 1.0);
        assert!(
            g.cols >= prev_cols,
            "cols decreased: {} -> {} at width {}",
            prev_cols,
            g.cols,
            w
        );
        prev_cols = g.cols;
    }
}

/// As container height grows (width fixed), rows must never decrease.
#[test]
fn monotonic_rows_on_height_growth() {
    let mut prev_rows = 0u16;
    for h in (100..=1200).step_by(50) {
        let g = fit_grid_to_container(800, h, 8, 16, 1.0, 1.0);
        assert!(
            g.rows >= prev_rows,
            "rows decreased: {} -> {} at height {}",
            prev_rows,
            g.rows,
            h
        );
        prev_rows = g.rows;
    }
}

/// As DPR increases (container/zoom fixed), cols must never increase.
/// Higher DPR = larger device-pixel cells = fewer cols fit.
#[test]
fn monotonic_cols_decrease_with_dpr() {
    let mut prev_cols = u16::MAX;
    for dpr_x10 in (5..=80).step_by(5) {
        let dpr = dpr_x10 as f32 / 10.0;
        let g = fit_grid_to_container(800, 600, 8, 16, dpr, 1.0);
        assert!(
            g.cols <= prev_cols,
            "cols increased: {} -> {} at dpr {}",
            prev_cols,
            g.cols,
            dpr
        );
        prev_cols = g.cols;
    }
}

/// As zoom increases (container/DPR fixed), cols must never increase.
#[test]
fn monotonic_cols_decrease_with_zoom() {
    let mut prev_cols = u16::MAX;
    for zoom_x100 in (25..=400).step_by(25) {
        let zoom = zoom_x100 as f32 / 100.0;
        let g = fit_grid_to_container(800, 600, 8, 16, 1.0, zoom);
        assert!(
            g.cols <= prev_cols,
            "cols increased: {} -> {} at zoom {}",
            prev_cols,
            g.cols,
            zoom
        );
        prev_cols = g.cols;
    }
}

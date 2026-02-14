#![forbid(unsafe_code)]

//! Unified resize signal arbiter.
//!
//! Serializes and coalesces all resize-affecting signals (ResizeObserver,
//! visualViewport, orientation changes, DPR changes, zoom adjustments, direct
//! grid resizes) into a single deterministic geometry computation per frame.
//!
//! # Architecture
//!
//! ```text
//! [ResizeObserver]     ─┐
//! [visualViewport]     ─┤
//! [DPR change]         ─┼─→ ResizeArbiter ─→ resolve() ─→ ResizeOutcome
//! [Zoom input]         ─┤   (coalesce)       (geometry)    (change summary)
//! [Direct grid resize] ─┤
//! [Orientation change] ─┘
//! ```
//!
//! # Determinism
//!
//! Same signal sequence → same output. The arbiter applies signals in
//! submission order, with last-writer-wins for overlapping fields. The
//! final geometry is computed exactly once per `resolve()` call.

use crate::renderer::{GridGeometry, fit_grid_to_container, grid_geometry};

// ---------------------------------------------------------------------------
// Signal types
// ---------------------------------------------------------------------------

/// A resize-affecting signal from the host environment.
///
/// Each variant captures the minimum data needed to update geometry.
/// Signals are ordered by submission time; the arbiter takes the last
/// value for each field when multiple signals affect the same parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResizeSignal {
    /// Container dimensions changed (e.g. ResizeObserver callback).
    /// Values are CSS pixels.
    ContainerResize { width_css: u32, height_css: u32 },

    /// Device pixel ratio changed (e.g. window moved between displays,
    /// or browser zoom changed).
    DprChange { dpr: f32 },

    /// User-controlled zoom multiplier changed.
    ZoomChange { zoom: f32 },

    /// Combined DPR + zoom change (e.g. `setScale(dpr, zoom)` call).
    ScaleChange { dpr: f32, zoom: f32 },

    /// Direct grid resize to specific dimensions (e.g. explicit `resize(cols, rows)`).
    DirectResize { cols: u16, rows: u16 },

    /// Orientation change (may imply container resize).
    /// Width and height are the new container dimensions in CSS pixels.
    OrientationChange { width_css: u32, height_css: u32 },

    /// Visual viewport change (mobile keyboard, address bar, etc.).
    /// Dimensions are the visible area in CSS pixels.
    VisualViewportChange { width_css: u32, height_css: u32 },
}

/// Source identification for deduplication and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalSource {
    ResizeObserver,
    VisualViewport,
    DevicePixelRatio,
    UserZoom,
    DirectApi,
    OrientationApi,
}

impl ResizeSignal {
    /// Classify the source of this signal for deduplication.
    #[must_use]
    pub const fn source(&self) -> SignalSource {
        match self {
            ResizeSignal::ContainerResize { .. } => SignalSource::ResizeObserver,
            ResizeSignal::DprChange { .. } => SignalSource::DevicePixelRatio,
            ResizeSignal::ZoomChange { .. } => SignalSource::UserZoom,
            ResizeSignal::ScaleChange { .. } => SignalSource::UserZoom,
            ResizeSignal::DirectResize { .. } => SignalSource::DirectApi,
            ResizeSignal::OrientationChange { .. } => SignalSource::OrientationApi,
            ResizeSignal::VisualViewportChange { .. } => SignalSource::VisualViewport,
        }
    }
}

// ---------------------------------------------------------------------------
// Coalesced state
// ---------------------------------------------------------------------------

/// Accumulated resize parameters from coalesced signals.
///
/// Each field is `Option` — only fields explicitly set by signals are
/// updated. Unset fields fall back to the arbiter's baseline state.
#[derive(Debug, Clone, Copy, Default)]
struct CoalescedParams {
    container_width_css: Option<u32>,
    container_height_css: Option<u32>,
    dpr: Option<f32>,
    zoom: Option<f32>,
    direct_cols: Option<u16>,
    direct_rows: Option<u16>,
}

impl CoalescedParams {
    fn apply(&mut self, signal: ResizeSignal) {
        match signal {
            ResizeSignal::ContainerResize {
                width_css,
                height_css,
            } => {
                self.container_width_css = Some(width_css);
                self.container_height_css = Some(height_css);
                // Container resize supersedes direct resize.
                self.direct_cols = None;
                self.direct_rows = None;
            }
            ResizeSignal::DprChange { dpr } => {
                self.dpr = Some(dpr);
            }
            ResizeSignal::ZoomChange { zoom } => {
                self.zoom = Some(zoom);
            }
            ResizeSignal::ScaleChange { dpr, zoom } => {
                self.dpr = Some(dpr);
                self.zoom = Some(zoom);
            }
            ResizeSignal::DirectResize { cols, rows } => {
                self.direct_cols = Some(cols);
                self.direct_rows = Some(rows);
                // Direct resize supersedes container fit.
                self.container_width_css = None;
                self.container_height_css = None;
            }
            ResizeSignal::OrientationChange {
                width_css,
                height_css,
            } => {
                self.container_width_css = Some(width_css);
                self.container_height_css = Some(height_css);
                self.direct_cols = None;
                self.direct_rows = None;
            }
            ResizeSignal::VisualViewportChange {
                width_css,
                height_css,
            } => {
                self.container_width_css = Some(width_css);
                self.container_height_css = Some(height_css);
                self.direct_cols = None;
                self.direct_rows = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// Summary of what changed after resolving coalesced signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeOutcome {
    /// Grid dimensions changed (cols and/or rows differ from baseline).
    pub grid_changed: bool,
    /// Scale factors changed (DPR and/or zoom differ from baseline).
    pub scale_changed: bool,
    /// Number of signals that were coalesced into this resolution.
    pub signals_coalesced: u32,
}

impl ResizeOutcome {
    /// Whether any change occurred.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.grid_changed || self.scale_changed
    }
}

// ---------------------------------------------------------------------------
// Arbiter
// ---------------------------------------------------------------------------

/// Baseline state representing the last resolved geometry.
#[derive(Debug, Clone, Copy)]
struct Baseline {
    geometry: GridGeometry,
    container_width_css: u32,
    container_height_css: u32,
    cell_width_css: u16,
    cell_height_css: u16,
}

/// Unified resize signal arbiter.
///
/// Accepts heterogeneous resize signals, coalesces them per frame, and
/// produces a single deterministic geometry computation on `resolve()`.
///
/// # Usage
///
/// ```rust,ignore
/// let mut arbiter = ResizeArbiter::new(initial_geometry, container_w, container_h, cell_w, cell_h);
///
/// // During frame: push signals as they arrive
/// arbiter.push(ResizeSignal::ContainerResize { width_css: 800, height_css: 600 });
/// arbiter.push(ResizeSignal::DprChange { dpr: 2.0 });
///
/// // At frame boundary: resolve once
/// if arbiter.has_pending() {
///     let (geometry, outcome) = arbiter.resolve();
///     // Apply geometry...
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ResizeArbiter {
    baseline: Baseline,
    pending: CoalescedParams,
    signal_count: u32,
}

impl ResizeArbiter {
    /// Create a new arbiter with the given baseline geometry.
    #[must_use]
    pub fn new(
        geometry: GridGeometry,
        container_width_css: u32,
        container_height_css: u32,
        cell_width_css: u16,
        cell_height_css: u16,
    ) -> Self {
        Self {
            baseline: Baseline {
                geometry,
                container_width_css,
                container_height_css,
                cell_width_css,
                cell_height_css,
            },
            pending: CoalescedParams::default(),
            signal_count: 0,
        }
    }

    /// Push a resize signal. Multiple signals are coalesced until `resolve()`.
    pub fn push(&mut self, signal: ResizeSignal) {
        self.pending.apply(signal);
        self.signal_count = self.signal_count.saturating_add(1);
    }

    /// Whether any signals are pending resolution.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.signal_count > 0
    }

    /// Number of signals accumulated since last resolve.
    #[must_use]
    pub fn pending_count(&self) -> u32 {
        self.signal_count
    }

    /// Resolve all pending signals into a new geometry.
    ///
    /// Returns `(new_geometry, outcome)`. The baseline is updated to the new
    /// geometry, and the pending queue is drained.
    ///
    /// If no signals are pending, returns the current baseline geometry with
    /// a no-change outcome.
    pub fn resolve(&mut self) -> (GridGeometry, ResizeOutcome) {
        if self.signal_count == 0 {
            return (
                self.baseline.geometry,
                ResizeOutcome {
                    grid_changed: false,
                    scale_changed: false,
                    signals_coalesced: 0,
                },
            );
        }

        let p = &self.pending;
        let b = &self.baseline;

        let dpr = p.dpr.unwrap_or(b.geometry.dpr);
        let zoom = p.zoom.unwrap_or(b.geometry.zoom);
        let cell_w = b.cell_width_css;
        let cell_h = b.cell_height_css;

        let new_geometry = if let (Some(cols), Some(rows)) = (p.direct_cols, p.direct_rows) {
            // Direct resize: use exact grid dimensions.
            grid_geometry(cols, rows, cell_w, cell_h, dpr, zoom)
        } else if p.container_width_css.is_some() || p.container_height_css.is_some() {
            // Container-based fit.
            let cw = p.container_width_css.unwrap_or(b.container_width_css);
            let ch = p.container_height_css.unwrap_or(b.container_height_css);
            fit_grid_to_container(cw, ch, cell_w, cell_h, dpr, zoom)
        } else {
            // Only scale changed, keep grid dims.
            grid_geometry(b.geometry.cols, b.geometry.rows, cell_w, cell_h, dpr, zoom)
        };

        let old = &self.baseline.geometry;
        let grid_changed = new_geometry.cols != old.cols || new_geometry.rows != old.rows;
        let scale_changed = (new_geometry.dpr - old.dpr).abs() > f32::EPSILON
            || (new_geometry.zoom - old.zoom).abs() > f32::EPSILON;

        let outcome = ResizeOutcome {
            grid_changed,
            scale_changed,
            signals_coalesced: self.signal_count,
        };

        // Update baseline.
        if let Some(cw) = self.pending.container_width_css {
            self.baseline.container_width_css = cw;
        }
        if let Some(ch) = self.pending.container_height_css {
            self.baseline.container_height_css = ch;
        }
        self.baseline.geometry = new_geometry;

        // Reset pending state.
        self.pending = CoalescedParams::default();
        self.signal_count = 0;

        (new_geometry, outcome)
    }

    /// Current baseline geometry (last resolved or initial).
    #[must_use]
    pub fn current_geometry(&self) -> GridGeometry {
        self.baseline.geometry
    }

    /// Discard all pending signals without resolving.
    pub fn discard(&mut self) {
        self.pending = CoalescedParams::default();
        self.signal_count = 0;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::grid_geometry;

    fn make_arbiter() -> ResizeArbiter {
        let geom = grid_geometry(80, 24, 8, 16, 1.0, 1.0);
        ResizeArbiter::new(geom, 640, 384, 8, 16)
    }

    // -- Signal source classification --

    #[test]
    fn signal_source_container() {
        let s = ResizeSignal::ContainerResize {
            width_css: 800,
            height_css: 600,
        };
        assert_eq!(s.source(), SignalSource::ResizeObserver);
    }

    #[test]
    fn signal_source_dpr() {
        let s = ResizeSignal::DprChange { dpr: 2.0 };
        assert_eq!(s.source(), SignalSource::DevicePixelRatio);
    }

    #[test]
    fn signal_source_zoom() {
        let s = ResizeSignal::ZoomChange { zoom: 1.5 };
        assert_eq!(s.source(), SignalSource::UserZoom);
    }

    #[test]
    fn signal_source_scale() {
        let s = ResizeSignal::ScaleChange {
            dpr: 2.0,
            zoom: 1.5,
        };
        assert_eq!(s.source(), SignalSource::UserZoom);
    }

    #[test]
    fn signal_source_direct() {
        let s = ResizeSignal::DirectResize {
            cols: 120,
            rows: 40,
        };
        assert_eq!(s.source(), SignalSource::DirectApi);
    }

    #[test]
    fn signal_source_orientation() {
        let s = ResizeSignal::OrientationChange {
            width_css: 600,
            height_css: 800,
        };
        assert_eq!(s.source(), SignalSource::OrientationApi);
    }

    #[test]
    fn signal_source_visual_viewport() {
        let s = ResizeSignal::VisualViewportChange {
            width_css: 400,
            height_css: 700,
        };
        assert_eq!(s.source(), SignalSource::VisualViewport);
    }

    // -- Arbiter basic lifecycle --

    #[test]
    fn no_pending_resolve_returns_baseline() {
        let mut arb = make_arbiter();
        let (geom, outcome) = arb.resolve();
        assert_eq!(geom.cols, 80);
        assert_eq!(geom.rows, 24);
        assert!(!outcome.changed());
        assert_eq!(outcome.signals_coalesced, 0);
    }

    #[test]
    fn has_pending_after_push() {
        let mut arb = make_arbiter();
        assert!(!arb.has_pending());
        arb.push(ResizeSignal::DprChange { dpr: 2.0 });
        assert!(arb.has_pending());
        assert_eq!(arb.pending_count(), 1);
    }

    #[test]
    fn resolve_clears_pending() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DprChange { dpr: 2.0 });
        arb.resolve();
        assert!(!arb.has_pending());
        assert_eq!(arb.pending_count(), 0);
    }

    #[test]
    fn discard_clears_pending() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DprChange { dpr: 2.0 });
        arb.push(ResizeSignal::ZoomChange { zoom: 1.5 });
        arb.discard();
        assert!(!arb.has_pending());
        let (geom, outcome) = arb.resolve();
        assert_eq!(geom.cols, 80);
        assert!(!outcome.changed());
    }

    // -- Container resize --

    #[test]
    fn container_resize_changes_grid() {
        let mut arb = make_arbiter();
        // Bigger container → more cols/rows
        arb.push(ResizeSignal::ContainerResize {
            width_css: 960,
            height_css: 640,
        });
        let (geom, outcome) = arb.resolve();
        assert!(outcome.grid_changed);
        assert!(geom.cols > 80);
        assert!(geom.rows > 24);
    }

    #[test]
    fn container_resize_preserves_scale() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::ContainerResize {
            width_css: 960,
            height_css: 640,
        });
        let (geom, outcome) = arb.resolve();
        assert!(!outcome.scale_changed);
        assert!((geom.dpr - 1.0).abs() < f32::EPSILON);
        assert!((geom.zoom - 1.0).abs() < f32::EPSILON);
    }

    // -- DPR change --

    #[test]
    fn dpr_change_reports_scale_changed() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DprChange { dpr: 2.0 });
        let (geom, outcome) = arb.resolve();
        assert!(outcome.scale_changed);
        assert!((geom.dpr - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn dpr_change_preserves_grid_dims() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DprChange { dpr: 2.0 });
        let (geom, _) = arb.resolve();
        // DPR-only change with no container → keep grid dims
        assert_eq!(geom.cols, 80);
        assert_eq!(geom.rows, 24);
    }

    // -- Zoom change --

    #[test]
    fn zoom_change_reports_scale_changed() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::ZoomChange { zoom: 1.5 });
        let (geom, outcome) = arb.resolve();
        assert!(outcome.scale_changed);
        assert!((geom.zoom - 1.5).abs() < f32::EPSILON);
    }

    // -- Combined scale change --

    #[test]
    fn scale_change_sets_both() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::ScaleChange {
            dpr: 2.0,
            zoom: 1.5,
        });
        let (geom, outcome) = arb.resolve();
        assert!(outcome.scale_changed);
        assert!((geom.dpr - 2.0).abs() < f32::EPSILON);
        assert!((geom.zoom - 1.5).abs() < f32::EPSILON);
    }

    // -- Direct resize --

    #[test]
    fn direct_resize_sets_exact_dims() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DirectResize {
            cols: 120,
            rows: 40,
        });
        let (geom, outcome) = arb.resolve();
        assert!(outcome.grid_changed);
        assert_eq!(geom.cols, 120);
        assert_eq!(geom.rows, 40);
    }

    #[test]
    fn direct_resize_supersedes_container() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::ContainerResize {
            width_css: 960,
            height_css: 640,
        });
        arb.push(ResizeSignal::DirectResize { cols: 50, rows: 20 });
        let (geom, _) = arb.resolve();
        assert_eq!(geom.cols, 50);
        assert_eq!(geom.rows, 20);
    }

    #[test]
    fn container_supersedes_direct() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DirectResize { cols: 50, rows: 20 });
        arb.push(ResizeSignal::ContainerResize {
            width_css: 960,
            height_css: 640,
        });
        let (geom, _) = arb.resolve();
        // Container came last, so it wins
        assert!(geom.cols > 50);
    }

    // -- Coalescing --

    #[test]
    fn multiple_container_resizes_last_wins() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::ContainerResize {
            width_css: 400,
            height_css: 300,
        });
        arb.push(ResizeSignal::ContainerResize {
            width_css: 960,
            height_css: 640,
        });
        let (geom, outcome) = arb.resolve();
        assert_eq!(outcome.signals_coalesced, 2);
        assert!(geom.cols > 80); // Second (larger) wins
    }

    #[test]
    fn mixed_signals_coalesce() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DprChange { dpr: 2.0 });
        arb.push(ResizeSignal::ContainerResize {
            width_css: 1280,
            height_css: 960,
        });
        arb.push(ResizeSignal::ZoomChange { zoom: 1.25 });
        let (geom, outcome) = arb.resolve();
        assert_eq!(outcome.signals_coalesced, 3);
        assert!(outcome.grid_changed);
        assert!(outcome.scale_changed);
        assert!((geom.dpr - 2.0).abs() < f32::EPSILON);
        assert!((geom.zoom - 1.25).abs() < f32::EPSILON);
    }

    // -- Orientation and visual viewport --

    #[test]
    fn orientation_change_acts_as_container_resize() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::OrientationChange {
            width_css: 384,
            height_css: 640,
        });
        let (geom, outcome) = arb.resolve();
        assert!(outcome.grid_changed);
        // Portrait orientation: narrower width → fewer cols
        assert!(geom.cols < 80);
    }

    #[test]
    fn visual_viewport_change_acts_as_container_resize() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::VisualViewportChange {
            width_css: 640,
            height_css: 300,
        });
        let (_geom, outcome) = arb.resolve();
        assert!(outcome.grid_changed);
    }

    // -- Baseline update --

    #[test]
    fn resolve_updates_baseline() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DprChange { dpr: 2.0 });
        arb.resolve();

        // Second resolve with no signals returns new baseline
        let (geom, outcome) = arb.resolve();
        assert!(!outcome.changed());
        assert!((geom.dpr - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn successive_resolves_track_changes() {
        let mut arb = make_arbiter();

        // First: change DPR
        arb.push(ResizeSignal::DprChange { dpr: 2.0 });
        let (_, o1) = arb.resolve();
        assert!(o1.scale_changed);

        // Second: same DPR → no change
        arb.push(ResizeSignal::DprChange { dpr: 2.0 });
        let (_, o2) = arb.resolve();
        assert!(!o2.scale_changed);

        // Third: different DPR → change again
        arb.push(ResizeSignal::DprChange { dpr: 3.0 });
        let (_, o3) = arb.resolve();
        assert!(o3.scale_changed);
    }

    // -- current_geometry --

    #[test]
    fn current_geometry_returns_latest() {
        let mut arb = make_arbiter();
        assert_eq!(arb.current_geometry().cols, 80);

        arb.push(ResizeSignal::DirectResize {
            cols: 120,
            rows: 40,
        });
        // Before resolve: still old baseline
        assert_eq!(arb.current_geometry().cols, 80);

        arb.resolve();
        assert_eq!(arb.current_geometry().cols, 120);
    }

    // -- Determinism --

    #[test]
    fn deterministic_same_sequence_same_result() {
        let signals = vec![
            ResizeSignal::DprChange { dpr: 2.0 },
            ResizeSignal::ContainerResize {
                width_css: 1024,
                height_css: 768,
            },
            ResizeSignal::ZoomChange { zoom: 1.25 },
        ];

        let mut arb1 = make_arbiter();
        let mut arb2 = make_arbiter();

        for &s in &signals {
            arb1.push(s);
            arb2.push(s);
        }

        let (g1, o1) = arb1.resolve();
        let (g2, o2) = arb2.resolve();
        assert_eq!(g1, g2);
        assert_eq!(o1, o2);
    }

    // -- Edge cases --

    #[test]
    fn zero_container_size_clamps_to_minimum() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::ContainerResize {
            width_css: 0,
            height_css: 0,
        });
        let (geom, _) = arb.resolve();
        // fit_grid_to_container clamps to at least 1 col, 1 row
        assert!(geom.cols >= 1);
        assert!(geom.rows >= 1);
    }

    #[test]
    fn extreme_dpr_is_clamped() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DprChange { dpr: 100.0 });
        let (geom, _) = arb.resolve();
        // DPR is clamped to MAX_DPR (8.0)
        assert!(geom.dpr <= 8.0);
    }

    #[test]
    fn nan_dpr_falls_back() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DprChange { dpr: f32::NAN });
        let (geom, _) = arb.resolve();
        // NaN falls back to 1.0 in normalized_scale
        assert!((geom.dpr - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn negative_dpr_falls_back() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::DprChange { dpr: -1.0 });
        let (geom, _) = arb.resolve();
        assert!((geom.dpr - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn extreme_zoom_is_clamped() {
        let mut arb = make_arbiter();
        arb.push(ResizeSignal::ZoomChange { zoom: 50.0 });
        let (geom, _) = arb.resolve();
        assert!(geom.zoom <= 4.0);
    }
}

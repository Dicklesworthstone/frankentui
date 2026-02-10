#![forbid(unsafe_code)]

//! Constraint visualization overlay for layout debugging.
//!
//! Provides a visual overlay that shows layout constraint violations,
//! requested vs received sizes, and constraint bounds at widget positions.
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::{ConstraintOverlay, LayoutDebugger, Widget};
//!
//! let mut debugger = LayoutDebugger::new();
//! debugger.set_enabled(true);
//!
//! // Record constraint data during layout...
//!
//! // Later, render the overlay
//! let overlay = ConstraintOverlay::new(&debugger);
//! overlay.render(area, &mut frame);
//! ```

use crate::Widget;
use crate::layout_debugger::{LayoutDebugger, LayoutRecord};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::{BorderChars, Draw};
use ftui_render::frame::Frame;

/// Visualization style for constraint overlay.
#[derive(Debug, Clone)]
pub struct ConstraintOverlayStyle {
    /// Border color for widgets without constraint violations.
    pub normal_color: PackedRgba,
    /// Border color for widgets exceeding max constraints (overflow).
    pub overflow_color: PackedRgba,
    /// Border color for widgets below min constraints (underflow).
    pub underflow_color: PackedRgba,
    /// Color for the "requested" size outline.
    pub requested_color: PackedRgba,
    /// Label foreground color.
    pub label_fg: PackedRgba,
    /// Label background color.
    pub label_bg: PackedRgba,
    /// Whether to show requested vs received size difference.
    pub show_size_diff: bool,
    /// Whether to show constraint bounds in labels.
    pub show_constraint_bounds: bool,
    /// Whether to show border outlines.
    pub show_borders: bool,
    /// Whether to show labels.
    pub show_labels: bool,
    /// Border characters to use.
    pub border_chars: BorderChars,
}

impl Default for ConstraintOverlayStyle {
    fn default() -> Self {
        Self {
            normal_color: PackedRgba::rgb(100, 200, 100),
            overflow_color: PackedRgba::rgb(240, 80, 80),
            underflow_color: PackedRgba::rgb(240, 200, 80),
            requested_color: PackedRgba::rgb(80, 150, 240),
            label_fg: PackedRgba::rgb(255, 255, 255),
            label_bg: PackedRgba::rgb(0, 0, 0),
            show_size_diff: true,
            show_constraint_bounds: true,
            show_borders: true,
            show_labels: true,
            border_chars: BorderChars::ASCII,
        }
    }
}

/// Constraint visualization overlay widget.
///
/// Renders layout constraint information as a visual overlay:
/// - Red borders for overflow violations (received > max)
/// - Yellow borders for underflow violations (received < min)
/// - Green borders for widgets within constraints
/// - Blue dashed outline showing requested size vs received size
/// - Labels showing widget name, sizes, and constraint bounds
pub struct ConstraintOverlay<'a> {
    debugger: &'a LayoutDebugger,
    style: ConstraintOverlayStyle,
}

impl<'a> ConstraintOverlay<'a> {
    /// Create a new constraint overlay for the given debugger.
    pub fn new(debugger: &'a LayoutDebugger) -> Self {
        Self {
            debugger,
            style: ConstraintOverlayStyle::default(),
        }
    }

    /// Set custom styling.
    #[must_use]
    pub fn style(mut self, style: ConstraintOverlayStyle) -> Self {
        self.style = style;
        self
    }

    fn render_record(&self, record: &LayoutRecord, area: Rect, buf: &mut Buffer) {
        // Only render if the received area intersects with our render area
        let Some(clipped) = record.area_received.intersection_opt(&area) else {
            return;
        };
        if clipped.is_empty() {
            return;
        }

        // Determine constraint status
        let constraints = &record.constraints;
        let received = &record.area_received;

        let is_overflow = (constraints.max_width != 0 && received.width > constraints.max_width)
            || (constraints.max_height != 0 && received.height > constraints.max_height);
        let is_underflow =
            received.width < constraints.min_width || received.height < constraints.min_height;

        let border_color = if is_overflow {
            self.style.overflow_color
        } else if is_underflow {
            self.style.underflow_color
        } else {
            self.style.normal_color
        };

        // Draw received area border
        if self.style.show_borders {
            let border_cell = Cell::from_char('+').with_fg(border_color);
            buf.draw_border(clipped, self.style.border_chars, border_cell);
        }

        // Draw requested area outline if different from received
        if self.style.show_size_diff {
            let requested = &record.area_requested;
            if requested != received
                && let Some(req_clipped) = requested.intersection_opt(&area)
                && !req_clipped.is_empty()
            {
                // Draw dashed corners to indicate requested size
                let req_cell = Cell::from_char('.').with_fg(self.style.requested_color);
                self.draw_requested_outline(req_clipped, buf, req_cell);
            }
        }

        // Draw label
        if self.style.show_labels {
            let label = self.format_label(record, is_overflow, is_underflow);
            let label_x = clipped.x.saturating_add(1);
            let label_y = clipped.y;
            let max_x = clipped.right();

            if label_x < max_x {
                let label_cell = Cell::from_char(' ')
                    .with_fg(self.style.label_fg)
                    .with_bg(self.style.label_bg);
                let _ = buf.print_text_clipped(label_x, label_y, &label, label_cell, max_x);
            }
        }

        // Render children
        for child in &record.children {
            self.render_record(child, area, buf);
        }
    }

    fn draw_requested_outline(&self, area: Rect, buf: &mut Buffer, cell: Cell) {
        // Draw corner dots to indicate requested size boundary
        if area.width >= 1 && area.height >= 1 {
            buf.set_fast(area.x, area.y, cell);
        }
        if area.width >= 2 && area.height >= 1 {
            buf.set_fast(area.right().saturating_sub(1), area.y, cell);
        }
        if area.width >= 1 && area.height >= 2 {
            buf.set_fast(area.x, area.bottom().saturating_sub(1), cell);
        }
        if area.width >= 2 && area.height >= 2 {
            buf.set_fast(
                area.right().saturating_sub(1),
                area.bottom().saturating_sub(1),
                cell,
            );
        }
    }

    fn format_label(&self, record: &LayoutRecord, is_overflow: bool, is_underflow: bool) -> String {
        let status = if is_overflow {
            "!"
        } else if is_underflow {
            "?"
        } else {
            ""
        };

        let mut label = format!("{}{}", record.widget_name, status);

        // Add size info
        let req = &record.area_requested;
        let got = &record.area_received;
        if req.width != got.width || req.height != got.height {
            label.push_str(&format!(
                " {}x{}\u{2192}{}x{}",
                req.width, req.height, got.width, got.height
            ));
        } else {
            label.push_str(&format!(" {}x{}", got.width, got.height));
        }

        // Add constraint bounds if requested
        if self.style.show_constraint_bounds {
            let c = &record.constraints;
            if c.min_width != 0 || c.min_height != 0 || c.max_width != 0 || c.max_height != 0 {
                label.push_str(&format!(
                    " [{}..{} x {}..{}]",
                    c.min_width,
                    if c.max_width == 0 {
                        "\u{221E}".to_string()
                    } else {
                        c.max_width.to_string()
                    },
                    c.min_height,
                    if c.max_height == 0 {
                        "\u{221E}".to_string()
                    } else {
                        c.max_height.to_string()
                    }
                ));
            }
        }

        label
    }
}

impl Widget for ConstraintOverlay<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if !self.debugger.enabled() {
            return;
        }

        for record in self.debugger.records() {
            self.render_record(record, area, &mut frame.buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_debugger::LayoutConstraints;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn overlay_renders_nothing_when_disabled() {
        let mut debugger = LayoutDebugger::new();
        // Not enabled, so record is ignored
        debugger.record(LayoutRecord::new(
            "Root",
            Rect::new(0, 0, 10, 4),
            Rect::new(0, 0, 10, 4),
            LayoutConstraints::unconstrained(),
        ));

        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        // Buffer should be unchanged (all default cells)
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn overlay_renders_border_for_valid_constraint() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new(
            "Root",
            Rect::new(1, 1, 6, 4),
            Rect::new(1, 1, 6, 4),
            LayoutConstraints::new(4, 10, 2, 6),
        ));

        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        // Should have border drawn
        let cell = frame.buffer.get(1, 1).unwrap();
        assert_eq!(cell.content.as_char(), Some('+'));
    }

    #[test]
    fn overlay_uses_overflow_color_when_exceeds_max() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        // Received 10x4 but max is 8x3 (overflow)
        debugger.record(LayoutRecord::new(
            "Overflow",
            Rect::new(0, 0, 10, 4),
            Rect::new(0, 0, 10, 4),
            LayoutConstraints::new(0, 8, 0, 3),
        ));

        let style = ConstraintOverlayStyle {
            overflow_color: PackedRgba::rgb(255, 0, 0),
            ..Default::default()
        };

        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.fg, PackedRgba::rgb(255, 0, 0));
    }

    #[test]
    fn overlay_uses_underflow_color_when_below_min() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        // Received 4x2 but min is 6x3 (underflow)
        debugger.record(LayoutRecord::new(
            "Underflow",
            Rect::new(0, 0, 4, 2),
            Rect::new(0, 0, 4, 2),
            LayoutConstraints::new(6, 0, 3, 0),
        ));

        let style = ConstraintOverlayStyle {
            underflow_color: PackedRgba::rgb(255, 255, 0),
            ..Default::default()
        };

        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.fg, PackedRgba::rgb(255, 255, 0));
    }

    #[test]
    fn overlay_shows_requested_vs_received_diff() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        // Requested 10x5 but got 8x4
        debugger.record(LayoutRecord::new(
            "Diff",
            Rect::new(0, 0, 10, 5),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::unconstrained(),
        ));

        let style = ConstraintOverlayStyle {
            requested_color: PackedRgba::rgb(0, 0, 255),
            ..Default::default()
        };

        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        // Corner of requested area (10x5) should have dot marker
        let cell = frame.buffer.get(9, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('.'));
        assert_eq!(cell.fg, PackedRgba::rgb(0, 0, 255));
    }

    #[test]
    fn overlay_renders_children() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);

        let child = LayoutRecord::new(
            "Child",
            Rect::new(2, 2, 4, 2),
            Rect::new(2, 2, 4, 2),
            LayoutConstraints::unconstrained(),
        );
        let parent = LayoutRecord::new(
            "Parent",
            Rect::new(0, 0, 10, 6),
            Rect::new(0, 0, 10, 6),
            LayoutConstraints::unconstrained(),
        )
        .with_child(child);
        debugger.record(parent);

        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        // Both parent and child should have borders
        let parent_cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(parent_cell.content.as_char(), Some('+'));

        let child_cell = frame.buffer.get(2, 2).unwrap();
        assert_eq!(child_cell.content.as_char(), Some('+'));
    }

    #[test]
    fn overlay_clips_to_render_area() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new(
            "PartiallyVisible",
            Rect::new(5, 5, 10, 10),
            Rect::new(5, 5, 10, 10),
            LayoutConstraints::unconstrained(),
        ));

        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        // Render area is 0,0,10,10 but widget is at 5,5,10,10
        overlay.render(Rect::new(0, 0, 10, 10), &mut frame);

        // Should render the visible portion
        let cell = frame.buffer.get(5, 5).unwrap();
        assert_eq!(cell.content.as_char(), Some('+'));

        // Outside render area should be empty
        let outside = frame.buffer.get(0, 0).unwrap();
        assert!(outside.is_empty());
    }

    #[test]
    fn format_label_includes_status_marker() {
        let debugger = LayoutDebugger::new();
        let overlay = ConstraintOverlay::new(&debugger);

        // Overflow case
        let record = LayoutRecord::new(
            "Widget",
            Rect::new(0, 0, 10, 4),
            Rect::new(0, 0, 10, 4),
            LayoutConstraints::new(0, 8, 0, 0),
        );
        let label = overlay.format_label(&record, true, false);
        assert!(label.starts_with("Widget!"));

        // Underflow case
        let label = overlay.format_label(&record, false, true);
        assert!(label.starts_with("Widget?"));

        // Normal case
        let label = overlay.format_label(&record, false, false);
        assert!(label.starts_with("Widget "));
    }

    #[test]
    fn style_can_be_customized() {
        let debugger = LayoutDebugger::new();
        let style = ConstraintOverlayStyle {
            show_borders: false,
            show_labels: false,
            show_size_diff: false,
            ..Default::default()
        };

        let overlay = ConstraintOverlay::new(&debugger).style(style);
        assert!(!overlay.style.show_borders);
        assert!(!overlay.style.show_labels);
    }

    #[test]
    fn default_style_values() {
        let s = ConstraintOverlayStyle::default();
        assert_eq!(s.normal_color, PackedRgba::rgb(100, 200, 100));
        assert_eq!(s.overflow_color, PackedRgba::rgb(240, 80, 80));
        assert_eq!(s.underflow_color, PackedRgba::rgb(240, 200, 80));
        assert_eq!(s.requested_color, PackedRgba::rgb(80, 150, 240));
        assert!(s.show_size_diff);
        assert!(s.show_constraint_bounds);
        assert!(s.show_borders);
        assert!(s.show_labels);
    }

    #[test]
    fn format_label_same_requested_and_received() {
        let debugger = LayoutDebugger::new();
        let overlay = ConstraintOverlay::new(&debugger);
        let record = LayoutRecord::new(
            "Box",
            Rect::new(0, 0, 8, 4),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::unconstrained(),
        );
        let label = overlay.format_label(&record, false, false);
        assert!(label.contains("8x4"));
        // Should NOT contain arrow since sizes are equal.
        assert!(!label.contains('\u{2192}'));
    }

    #[test]
    fn format_label_different_sizes_shows_arrow() {
        let debugger = LayoutDebugger::new();
        let overlay = ConstraintOverlay::new(&debugger);
        let record = LayoutRecord::new(
            "Box",
            Rect::new(0, 0, 10, 5),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::unconstrained(),
        );
        let label = overlay.format_label(&record, false, false);
        // Should contain "10x5→8x4"
        assert!(label.contains("10x5"));
        assert!(label.contains('\u{2192}'));
        assert!(label.contains("8x4"));
    }

    #[test]
    fn format_label_constraint_bounds_infinity() {
        let debugger = LayoutDebugger::new();
        let overlay = ConstraintOverlay::new(&debugger);
        // min_width=5, max_width=0 (infinity), min_height=0, max_height=10
        let record = LayoutRecord::new(
            "W",
            Rect::new(0, 0, 8, 4),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::new(5, 0, 0, 10),
        );
        let label = overlay.format_label(&record, false, false);
        // max_width=0 should render as ∞
        assert!(label.contains('\u{221E}'));
        assert!(label.contains("5.."));
    }

    #[test]
    fn format_label_no_bounds_when_all_zero() {
        let debugger = LayoutDebugger::new();
        let overlay = ConstraintOverlay::new(&debugger);
        let record = LayoutRecord::new(
            "W",
            Rect::new(0, 0, 8, 4),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::new(0, 0, 0, 0),
        );
        let label = overlay.format_label(&record, false, false);
        // All-zero constraints → no bounds shown.
        assert!(!label.contains('['));
    }

    #[test]
    fn format_label_no_bounds_when_disabled() {
        let debugger = LayoutDebugger::new();
        let style = ConstraintOverlayStyle {
            show_constraint_bounds: false,
            ..Default::default()
        };
        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let record = LayoutRecord::new(
            "W",
            Rect::new(0, 0, 8, 4),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::new(5, 10, 3, 8),
        );
        let label = overlay.format_label(&record, false, false);
        assert!(!label.contains('['));
    }

    #[test]
    fn enabled_debugger_with_no_records_renders_nothing() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        // No records added.
        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn record_fully_outside_render_area_is_skipped() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new(
            "Offscreen",
            Rect::new(50, 50, 10, 10),
            Rect::new(50, 50, 10, 10),
            LayoutConstraints::unconstrained(),
        ));

        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        // Nothing should be drawn since record is fully outside render area.
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    // ─── Edge-case tests (bd-3szd1) ────────────────────────────────────

    #[test]
    fn zero_size_record_is_skipped() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new(
            "Empty",
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 0, 0),
            LayoutConstraints::unconstrained(),
        ));

        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn one_by_one_record_renders_border() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new(
            "Tiny",
            Rect::new(2, 2, 1, 1),
            Rect::new(2, 2, 1, 1),
            LayoutConstraints::unconstrained(),
        ));

        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);
        // 1x1 area: border drawn at the single cell
        let cell = frame.buffer.get(2, 2).unwrap();
        assert!(!cell.is_empty());
    }

    #[test]
    fn overflow_only_height() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        // width OK (5 <= 10), height overflow (8 > 6)
        debugger.record(LayoutRecord::new(
            "HOverflow",
            Rect::new(0, 0, 5, 8),
            Rect::new(0, 0, 5, 8),
            LayoutConstraints::new(0, 10, 0, 6),
        ));

        let style = ConstraintOverlayStyle {
            overflow_color: PackedRgba::rgb(255, 0, 0),
            ..Default::default()
        };
        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.fg, PackedRgba::rgb(255, 0, 0), "height overflow color");
    }

    #[test]
    fn underflow_only_height() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        // width OK (6 >= 4), height underflow (2 < 3)
        debugger.record(LayoutRecord::new(
            "HUnderflow",
            Rect::new(0, 0, 6, 2),
            Rect::new(0, 0, 6, 2),
            LayoutConstraints::new(4, 0, 3, 0),
        ));

        let style = ConstraintOverlayStyle {
            underflow_color: PackedRgba::rgb(255, 255, 0),
            ..Default::default()
        };
        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(
            cell.fg,
            PackedRgba::rgb(255, 255, 0),
            "height underflow color"
        );
    }

    #[test]
    fn overflow_takes_priority_over_underflow() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        // width overflow (10 > 8), height underflow (2 < 3)
        debugger.record(LayoutRecord::new(
            "Both",
            Rect::new(0, 0, 10, 2),
            Rect::new(0, 0, 10, 2),
            LayoutConstraints::new(0, 8, 3, 0),
        ));

        let style = ConstraintOverlayStyle {
            overflow_color: PackedRgba::rgb(255, 0, 0),
            underflow_color: PackedRgba::rgb(255, 255, 0),
            ..Default::default()
        };
        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(
            cell.fg,
            PackedRgba::rgb(255, 0, 0),
            "overflow wins over underflow"
        );
    }

    #[test]
    fn multiple_records_all_render() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new(
            "A",
            Rect::new(0, 0, 5, 3),
            Rect::new(0, 0, 5, 3),
            LayoutConstraints::unconstrained(),
        ));
        debugger.record(LayoutRecord::new(
            "B",
            Rect::new(6, 0, 5, 3),
            Rect::new(6, 0, 5, 3),
            LayoutConstraints::unconstrained(),
        ));

        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('+'));
        assert_eq!(frame.buffer.get(6, 0).unwrap().content.as_char(), Some('+'));
    }

    #[test]
    fn deeply_nested_children_render() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);

        let grandchild = LayoutRecord::new(
            "GC",
            Rect::new(4, 4, 3, 2),
            Rect::new(4, 4, 3, 2),
            LayoutConstraints::unconstrained(),
        );
        let child = LayoutRecord::new(
            "Child",
            Rect::new(2, 2, 8, 6),
            Rect::new(2, 2, 8, 6),
            LayoutConstraints::unconstrained(),
        )
        .with_child(grandchild);
        let parent = LayoutRecord::new(
            "Parent",
            Rect::new(0, 0, 12, 10),
            Rect::new(0, 0, 12, 10),
            LayoutConstraints::unconstrained(),
        )
        .with_child(child);
        debugger.record(parent);

        let overlay = ConstraintOverlay::new(&debugger);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 12, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 12), &mut frame);

        // All three levels should render borders
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('+'));
        assert_eq!(frame.buffer.get(2, 2).unwrap().content.as_char(), Some('+'));
        assert_eq!(frame.buffer.get(4, 4).unwrap().content.as_char(), Some('+'));
    }

    #[test]
    fn format_label_empty_widget_name() {
        let debugger = LayoutDebugger::new();
        let overlay = ConstraintOverlay::new(&debugger);
        let record = LayoutRecord::new(
            "",
            Rect::new(0, 0, 5, 3),
            Rect::new(0, 0, 5, 3),
            LayoutConstraints::unconstrained(),
        );
        let label = overlay.format_label(&record, false, false);
        assert!(label.contains("5x3"), "size should still appear: {label}");
    }

    #[test]
    fn format_label_both_bounds_finite() {
        let debugger = LayoutDebugger::new();
        let overlay = ConstraintOverlay::new(&debugger);
        let record = LayoutRecord::new(
            "W",
            Rect::new(0, 0, 8, 4),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::new(4, 12, 2, 8),
        );
        let label = overlay.format_label(&record, false, false);
        // Should show [4..12 x 2..8]
        assert!(label.contains("[4..12 x 2..8]"), "label={label}");
    }

    #[test]
    fn requested_outline_not_drawn_when_same_as_received() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new(
            "Same",
            Rect::new(0, 0, 6, 4),
            Rect::new(0, 0, 6, 4),
            LayoutConstraints::unconstrained(),
        ));

        let style = ConstraintOverlayStyle {
            requested_color: PackedRgba::rgb(0, 0, 255),
            ..Default::default()
        };
        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        // The '.' marker should not appear since areas are identical
        // Check a corner that would have '+' from border but not '.'
        let cell = frame.buffer.get(5, 3).unwrap(); // bottom-right corner
        assert_ne!(
            cell.content.as_char(),
            Some('.'),
            "dot should not appear when same size"
        );
    }

    #[test]
    fn style_clone_and_debug() {
        let style = ConstraintOverlayStyle::default();
        let cloned = style.clone();
        let _ = format!("{cloned:?}");
        assert_eq!(cloned.normal_color, style.normal_color);
    }

    #[test]
    fn max_width_zero_means_unconstrained_no_overflow() {
        let debugger = LayoutDebugger::new();
        let overlay = ConstraintOverlay::new(&debugger);
        // max_width=0 means no max constraint
        let record = LayoutRecord::new(
            "W",
            Rect::new(0, 0, 100, 4),
            Rect::new(0, 0, 100, 4),
            LayoutConstraints::new(0, 0, 0, 0),
        );
        // is_overflow check: max_width!=0 && received.width>max_width
        // With max_width=0, first condition is false, so not overflow
        let label = overlay.format_label(&record, false, false);
        assert!(!label.contains('!'), "should not be overflow: {label}");
    }

    // ─── End edge-case tests (bd-3szd1) ──────────────────────────────

    #[test]
    fn no_borders_when_show_borders_disabled() {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new(
            "NoBorder",
            Rect::new(0, 0, 6, 4),
            Rect::new(0, 0, 6, 4),
            LayoutConstraints::unconstrained(),
        ));

        let style = ConstraintOverlayStyle {
            show_borders: false,
            show_labels: false,
            show_size_diff: false,
            ..Default::default()
        };
        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        overlay.render(Rect::new(0, 0, 20, 10), &mut frame);

        // No border should be drawn.
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }
}

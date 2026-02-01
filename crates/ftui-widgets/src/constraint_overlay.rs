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
//! let debugger = LayoutDebugger::new();
//! debugger.set_enabled(true);
//!
//! // Record constraint data during layout...
//!
//! // Later, render the overlay
//! let overlay = ConstraintOverlay::new(&debugger);
//! overlay.render(area, &mut buf);
//! ```

use crate::layout_debugger::{LayoutDebugger, LayoutRecord};
use crate::Widget;
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::{BorderChars, Draw};

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

    fn render_record(&self, record: &LayoutRecord, area: Rect, buf: &mut Buffer, depth: usize) {
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
            if requested != received {
                if let Some(req_clipped) = requested.intersection_opt(&area) {
                    if !req_clipped.is_empty() {
                        // Draw dashed corners to indicate requested size
                        let req_cell = Cell::from_char('.').with_fg(self.style.requested_color);
                        self.draw_requested_outline(req_clipped, buf, req_cell);
                    }
                }
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
            self.render_record(child, area, buf, depth + 1);
        }
    }

    fn draw_requested_outline(&self, area: Rect, buf: &mut Buffer, cell: Cell) {
        // Draw corner dots to indicate requested size boundary
        if area.width >= 1 && area.height >= 1 {
            buf.set(area.x, area.y, cell.clone());
        }
        if area.width >= 2 && area.height >= 1 {
            buf.set(area.right().saturating_sub(1), area.y, cell.clone());
        }
        if area.width >= 1 && area.height >= 2 {
            buf.set(area.x, area.bottom().saturating_sub(1), cell.clone());
        }
        if area.width >= 2 && area.height >= 2 {
            buf.set(
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
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.debugger.enabled() {
            return;
        }

        for record in self.debugger.records() {
            self.render_record(record, area, buf, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_debugger::LayoutConstraints;

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
        let mut buf = Buffer::new(20, 10);
        overlay.render(Rect::new(0, 0, 20, 10), &mut buf);

        // Buffer should be unchanged (all default cells)
        assert!(buf.get(0, 0).unwrap().is_empty());
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
        let mut buf = Buffer::new(20, 10);
        overlay.render(Rect::new(0, 0, 20, 10), &mut buf);

        // Should have border drawn
        let cell = buf.get(1, 1).unwrap();
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

        let mut style = ConstraintOverlayStyle::default();
        style.overflow_color = PackedRgba::rgb(255, 0, 0);

        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut buf = Buffer::new(20, 10);
        overlay.render(Rect::new(0, 0, 20, 10), &mut buf);

        let cell = buf.get(0, 0).unwrap();
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

        let mut style = ConstraintOverlayStyle::default();
        style.underflow_color = PackedRgba::rgb(255, 255, 0);

        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut buf = Buffer::new(20, 10);
        overlay.render(Rect::new(0, 0, 20, 10), &mut buf);

        let cell = buf.get(0, 0).unwrap();
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

        let mut style = ConstraintOverlayStyle::default();
        style.show_size_diff = true;
        style.requested_color = PackedRgba::rgb(0, 0, 255);

        let overlay = ConstraintOverlay::new(&debugger).style(style);
        let mut buf = Buffer::new(20, 10);
        overlay.render(Rect::new(0, 0, 20, 10), &mut buf);

        // Corner of requested area (10x5) should have dot marker
        let cell = buf.get(9, 0).unwrap();
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
        let mut buf = Buffer::new(20, 10);
        overlay.render(Rect::new(0, 0, 20, 10), &mut buf);

        // Both parent and child should have borders
        let parent_cell = buf.get(0, 0).unwrap();
        assert_eq!(parent_cell.content.as_char(), Some('+'));

        let child_cell = buf.get(2, 2).unwrap();
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
        let mut buf = Buffer::new(10, 10);
        // Render area is 0,0,10,10 but widget is at 5,5,10,10
        overlay.render(Rect::new(0, 0, 10, 10), &mut buf);

        // Should render the visible portion
        let cell = buf.get(5, 5).unwrap();
        assert_eq!(cell.content.as_char(), Some('+'));

        // Outside render area should be empty
        let outside = buf.get(0, 0).unwrap();
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
        let mut style = ConstraintOverlayStyle::default();
        style.show_borders = false;
        style.show_labels = false;
        style.show_size_diff = false;

        let overlay = ConstraintOverlay::new(&debugger).style(style);
        assert!(!overlay.style.show_borders);
        assert!(!overlay.style.show_labels);
    }
}

#![forbid(unsafe_code)]

//! Minimap overlay for large Mermaid diagrams.
//!
//! Renders a scaled-down Braille preview of the full diagram layout in a
//! corner of the viewport, with a rectangle indicating the currently visible
//! region. Useful for orientation when panning large graphs.

use crate::canvas::{Mode, Painter};
use crate::mermaid_layout::{DiagramLayout, LayoutPoint, LayoutRect};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};

/// Which corner of the diagram area to place the minimap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MinimapCorner {
    #[default]
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

/// Configuration for the minimap overlay.
#[derive(Debug, Clone, Copy)]
pub struct MinimapConfig {
    /// Maximum width of the minimap in terminal cells.
    pub max_width: u16,
    /// Maximum height of the minimap in terminal cells.
    pub max_height: u16,
    /// Corner placement.
    pub corner: MinimapCorner,
    /// Margin from the edge (in terminal cells).
    pub margin: u16,
    /// Color for node dots.
    pub node_color: PackedRgba,
    /// Color for edge lines.
    pub edge_color: PackedRgba,
    /// Color for the viewport indicator rectangle.
    pub viewport_color: PackedRgba,
    /// Color for the selected/highlighted node.
    pub highlight_color: PackedRgba,
    /// Background color (semi-transparent effect via solid fill).
    pub bg_color: PackedRgba,
    /// Border color for the minimap frame.
    pub border_color: PackedRgba,
}

impl Default for MinimapConfig {
    fn default() -> Self {
        Self {
            max_width: 30,
            max_height: 15,
            corner: MinimapCorner::BottomRight,
            margin: 1,
            node_color: PackedRgba::rgb(80, 180, 255),
            edge_color: PackedRgba::rgb(100, 100, 100),
            viewport_color: PackedRgba::rgb(255, 220, 60),
            highlight_color: PackedRgba::rgb(255, 100, 80),
            bg_color: PackedRgba::rgb(20, 20, 30),
            border_color: PackedRgba::rgb(60, 60, 80),
        }
    }
}

/// Precomputed minimap state from a diagram layout.
///
/// Create once per layout change, then call [`Minimap::render`] each frame
/// with the current viewport to draw the overlay.
#[derive(Debug)]
pub struct Minimap {
    /// Painter with nodes and edges already drawn.
    painter: Painter,
    /// The diagram bounding box used for coordinate mapping.
    bounding_box: LayoutRect,
    /// Terminal cell dimensions of the minimap content area (inside border).
    content_cells: (u16, u16),
    /// Configuration snapshot.
    config: MinimapConfig,
}

impl Minimap {
    /// Build a minimap from a completed diagram layout.
    ///
    /// This pre-renders all nodes and edges into a Braille painter. The
    /// painter is reused across frames — only the viewport indicator changes.
    #[must_use]
    pub fn new(layout: &DiagramLayout, config: MinimapConfig) -> Self {
        let bb = &layout.bounding_box;

        // Determine content area in cells, preserving aspect ratio.
        let (content_w, content_h) = fit_aspect_ratio(
            bb.width,
            bb.height,
            config.max_width.saturating_sub(2), // account for border
            config.max_height.saturating_sub(2),
        );

        // Sub-pixel dimensions for Braille (2 cols x 4 rows per cell).
        let px_w = content_w * Mode::Braille.cols_per_cell();
        let px_h = content_h * Mode::Braille.rows_per_cell();

        let mut painter = Painter::new(px_w, px_h, Mode::Braille);

        // Draw edges first (underneath nodes).
        for edge in &layout.edges {
            if edge.waypoints.len() >= 2 {
                for pair in edge.waypoints.windows(2) {
                    let (x0, y0) = layout_to_px(pair[0], bb, px_w, px_h);
                    let (x1, y1) = layout_to_px(pair[1], bb, px_w, px_h);
                    painter.line_colored(x0, y0, x1, y1, Some(config.edge_color));
                }
            }
        }

        // Draw nodes as small filled rectangles.
        for node in &layout.nodes {
            let (nx, ny) = layout_to_px(
                LayoutPoint {
                    x: node.rect.x,
                    y: node.rect.y,
                },
                bb,
                px_w,
                px_h,
            );
            let (nx2, ny2) = layout_to_px(
                LayoutPoint {
                    x: node.rect.x + node.rect.width,
                    y: node.rect.y + node.rect.height,
                },
                bb,
                px_w,
                px_h,
            );
            let nw = (nx2 - nx).max(1);
            let nh = (ny2 - ny).max(1);
            // For very small nodes, at least draw a point.
            if nw <= 2 && nh <= 2 {
                painter.point_colored(nx, ny, config.node_color);
            } else {
                painter.rect_filled(nx, ny, nw, nh);
                // Color the border pixels.
                for x in nx..nx + nw {
                    painter.point_colored(x, ny, config.node_color);
                    painter.point_colored(x, ny + nh - 1, config.node_color);
                }
                for y in ny..ny + nh {
                    painter.point_colored(nx, y, config.node_color);
                    painter.point_colored(nx + nw - 1, y, config.node_color);
                }
            }
        }

        Self {
            painter,
            bounding_box: *bb,
            content_cells: (content_w, content_h),
            config,
        }
    }

    /// Compute where the minimap should be placed within the given area.
    #[must_use]
    pub fn placement(&self, area: Rect) -> Rect {
        let (cw, ch) = self.content_cells;
        // +2 for border on each side.
        let total_w = cw + 2;
        let total_h = ch + 2;

        if total_w > area.width || total_h > area.height {
            return Rect::new(0, 0, 0, 0);
        }

        let m = self.config.margin;
        let x = match self.config.corner {
            MinimapCorner::TopLeft | MinimapCorner::BottomLeft => area.x + m,
            MinimapCorner::TopRight | MinimapCorner::BottomRight => {
                area.x + area.width - total_w - m
            }
        };
        let y = match self.config.corner {
            MinimapCorner::TopLeft | MinimapCorner::TopRight => area.y + m,
            MinimapCorner::BottomLeft | MinimapCorner::BottomRight => {
                area.y + area.height - total_h - m
            }
        };

        Rect::new(x, y, total_w, total_h)
    }

    /// Render the minimap overlay into a buffer.
    ///
    /// `area` is the full diagram rendering area (used for placement).
    /// `viewport` describes the currently visible region in diagram coordinates
    /// (same coordinate system as `DiagramLayout`).
    /// `selected_node` optionally highlights a node.
    pub fn render(
        &self,
        area: Rect,
        buf: &mut Buffer,
        viewport: Option<&LayoutRect>,
        selected_node: Option<usize>,
    ) {
        let minimap_rect = self.placement(area);
        if minimap_rect.is_empty() {
            return;
        }

        // Fill background.
        let bg_cell = Cell::from_char(' ').with_bg(self.config.bg_color);
        for y in minimap_rect.y..minimap_rect.y + minimap_rect.height {
            for x in minimap_rect.x..minimap_rect.x + minimap_rect.width {
                buf.set_fast(x, y, bg_cell);
            }
        }

        // Draw border.
        self.draw_border(minimap_rect, buf);

        // Render Braille content inside the border.
        let content_area = Rect::new(
            minimap_rect.x + 1,
            minimap_rect.y + 1,
            self.content_cells.0,
            self.content_cells.1,
        );

        let style = ftui_style::Style::new().fg(self.config.node_color);
        self.painter.render_to_buffer(content_area, buf, style);

        // Draw viewport indicator.
        if let Some(vp) = viewport {
            self.draw_viewport_indicator(content_area, buf, vp);
        }

        // Highlight selected node.
        if let Some(_node_idx) = selected_node {
            // The node is already drawn in the painter; this hook is
            // reserved for future enhancement (e.g. brighter dot overlay).
        }
    }

    /// Draw a simple box-drawing border around the minimap.
    fn draw_border(&self, rect: Rect, buf: &mut Buffer) {
        let x1 = rect.x;
        let y1 = rect.y;
        let x2 = rect.x + rect.width.saturating_sub(1);
        let y2 = rect.y + rect.height.saturating_sub(1);

        let bc = self.config.border_color;

        // Corners.
        buf.set_fast(x1, y1, Cell::from_char('\u{250C}').with_fg(bc));
        buf.set_fast(x2, y1, Cell::from_char('\u{2510}').with_fg(bc));
        buf.set_fast(x1, y2, Cell::from_char('\u{2514}').with_fg(bc));
        buf.set_fast(x2, y2, Cell::from_char('\u{2518}').with_fg(bc));

        // Horizontal edges.
        for x in (x1 + 1)..x2 {
            buf.set_fast(x, y1, Cell::from_char('\u{2500}').with_fg(bc));
            buf.set_fast(x, y2, Cell::from_char('\u{2500}').with_fg(bc));
        }

        // Vertical edges.
        for y in (y1 + 1)..y2 {
            buf.set_fast(x1, y, Cell::from_char('\u{2502}').with_fg(bc));
            buf.set_fast(x2, y, Cell::from_char('\u{2502}').with_fg(bc));
        }
    }

    /// Draw a rectangle in the minimap content area indicating the viewport.
    fn draw_viewport_indicator(&self, content_area: Rect, buf: &mut Buffer, viewport: &LayoutRect) {
        let bb = &self.bounding_box;
        let (cw, ch) = self.content_cells;

        if bb.width <= 0.0 || bb.height <= 0.0 || cw == 0 || ch == 0 {
            return;
        }

        // Map viewport corners to terminal cell coordinates within content_area.
        let scale_x = f64::from(cw) / bb.width;
        let scale_y = f64::from(ch) / bb.height;

        let vx1 = ((viewport.x - bb.x) * scale_x).round() as i32;
        let vy1 = ((viewport.y - bb.y) * scale_y).round() as i32;
        let vx2 = ((viewport.x + viewport.width - bb.x) * scale_x).round() as i32;
        let vy2 = ((viewport.y + viewport.height - bb.y) * scale_y).round() as i32;

        // Clamp to content area.
        let cx = content_area.x as i32;
        let cy = content_area.y as i32;
        let cw_i = cw as i32;
        let ch_i = ch as i32;

        let left = vx1.max(0).min(cw_i - 1) + cx;
        let top = vy1.max(0).min(ch_i - 1) + cy;
        let right = vx2.max(0).min(cw_i) + cx;
        let bottom = vy2.max(0).min(ch_i) + cy;

        let vc = self.config.viewport_color;

        // Draw viewport rectangle outline using colored cells.
        for x in left..right {
            recolor_cell(buf, x as u16, top as u16, vc);
            recolor_cell(buf, x as u16, (bottom - 1).max(top) as u16, vc);
        }
        for y in top..bottom {
            recolor_cell(buf, left as u16, y as u16, vc);
            recolor_cell(buf, (right - 1).max(left) as u16, y as u16, vc);
        }
    }

    /// Total terminal cell size of the minimap (including border).
    #[must_use]
    pub fn total_size(&self) -> (u16, u16) {
        (self.content_cells.0 + 2, self.content_cells.1 + 2)
    }

    /// Whether the minimap would be too small to be useful.
    #[must_use]
    pub fn is_trivial(&self) -> bool {
        self.content_cells.0 < 3 || self.content_cells.1 < 2
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Fit a diagram aspect ratio into a maximum cell budget.
fn fit_aspect_ratio(diagram_w: f64, diagram_h: f64, max_w: u16, max_h: u16) -> (u16, u16) {
    if diagram_w <= 0.0 || diagram_h <= 0.0 || max_w == 0 || max_h == 0 {
        return (max_w.max(1), max_h.max(1));
    }

    let aspect = diagram_w / diagram_h;
    // Braille has 2:4 sub-pixel ratio, so each cell is roughly 1:2 in pixel space.
    // Adjust for the visual aspect ratio.
    let cell_aspect = aspect * 0.5; // cells are ~twice as tall as wide

    let w_from_h = (f64::from(max_h) * cell_aspect).round() as u16;
    let h_from_w = (f64::from(max_w) / cell_aspect).round() as u16;

    if w_from_h <= max_w {
        (w_from_h.max(3), max_h)
    } else {
        (max_w, h_from_w.min(max_h).max(2))
    }
}

/// Map a layout-space point to sub-pixel coordinates.
fn layout_to_px(p: LayoutPoint, bb: &LayoutRect, px_w: u16, px_h: u16) -> (i32, i32) {
    if bb.width <= 0.0 || bb.height <= 0.0 {
        return (0, 0);
    }
    let x = ((p.x - bb.x) / bb.width * f64::from(px_w.saturating_sub(1))).round() as i32;
    let y = ((p.y - bb.y) / bb.height * f64::from(px_h.saturating_sub(1))).round() as i32;
    (x.max(0), y.max(0))
}

/// Recolor an existing cell's foreground without changing its content.
fn recolor_cell(buf: &mut Buffer, x: u16, y: u16, color: PackedRgba) {
    if let Some(existing) = buf.get(x, y) {
        let mut cell = *existing;
        cell.fg = color;
        buf.set_fast(x, y, cell);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mermaid_layout::{
        DiagramLayout, LayoutEdgePath, LayoutNodeBox, LayoutPoint, LayoutRect, LayoutStats,
    };

    fn simple_layout() -> DiagramLayout {
        DiagramLayout {
            nodes: vec![
                LayoutNodeBox {
                    node_idx: 0,
                    rect: LayoutRect {
                        x: 0.0,
                        y: 0.0,
                        width: 10.0,
                        height: 5.0,
                    },
                    label_rect: None,
                    rank: 0,
                    order: 0,
                },
                LayoutNodeBox {
                    node_idx: 1,
                    rect: LayoutRect {
                        x: 30.0,
                        y: 20.0,
                        width: 10.0,
                        height: 5.0,
                    },
                    label_rect: None,
                    rank: 1,
                    order: 0,
                },
            ],
            clusters: vec![],
            edges: vec![LayoutEdgePath {
                edge_idx: 0,
                waypoints: vec![
                    LayoutPoint { x: 5.0, y: 5.0 },
                    LayoutPoint { x: 35.0, y: 20.0 },
                ],
                bundle_count: 1,
                bundle_members: Vec::new(),
            }],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 25.0,
            },
            stats: LayoutStats {
                iterations_used: 1,
                max_iterations: 10,
                budget_exceeded: false,
                crossings: 0,
                ranks: 2,
                max_rank_width: 1,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        }
    }

    #[test]
    fn minimap_creation() {
        let layout = simple_layout();
        let config = MinimapConfig::default();
        let minimap = Minimap::new(&layout, config);

        assert!(!minimap.is_trivial());
        let (w, h) = minimap.total_size();
        assert!(w > 2);
        assert!(h > 2);
        assert!(w <= config.max_width + 2);
        assert!(h <= config.max_height + 2);
    }

    #[test]
    fn placement_bottom_right() {
        let layout = simple_layout();
        let config = MinimapConfig {
            corner: MinimapCorner::BottomRight,
            margin: 1,
            ..MinimapConfig::default()
        };
        let minimap = Minimap::new(&layout, config);
        let area = Rect::new(0, 0, 120, 40);
        let rect = minimap.placement(area);

        // Should be in bottom-right with margin.
        assert!(rect.x + rect.width <= area.x + area.width);
        assert!(rect.y + rect.height <= area.y + area.height);
        assert!(rect.x > area.x + area.width / 2); // right half
        assert!(rect.y > area.y + area.height / 2); // bottom half
    }

    #[test]
    fn placement_top_left() {
        let layout = simple_layout();
        let config = MinimapConfig {
            corner: MinimapCorner::TopLeft,
            margin: 1,
            ..MinimapConfig::default()
        };
        let minimap = Minimap::new(&layout, config);
        let area = Rect::new(0, 0, 120, 40);
        let rect = minimap.placement(area);

        assert_eq!(rect.x, area.x + 1); // margin
        assert_eq!(rect.y, area.y + 1);
    }

    #[test]
    fn placement_too_small_returns_zero() {
        let layout = simple_layout();
        let config = MinimapConfig::default();
        let minimap = Minimap::new(&layout, config);

        // Area too small to fit minimap.
        let area = Rect::new(0, 0, 3, 3);
        let rect = minimap.placement(area);
        assert!(rect.is_empty());
    }

    #[test]
    fn render_does_not_panic() {
        let layout = simple_layout();
        let config = MinimapConfig::default();
        let minimap = Minimap::new(&layout, config);

        let mut buf = Buffer::new(120, 40);
        let area = Rect::new(0, 0, 120, 40);

        let viewport = LayoutRect {
            x: 5.0,
            y: 5.0,
            width: 20.0,
            height: 15.0,
        };

        minimap.render(area, &mut buf, Some(&viewport), None);
        minimap.render(area, &mut buf, None, None);
        minimap.render(area, &mut buf, Some(&viewport), Some(0));
    }

    #[test]
    fn fit_aspect_ratio_wide() {
        let (w, h) = fit_aspect_ratio(100.0, 20.0, 30, 15);
        assert!(w <= 30);
        assert!(h <= 15);
        assert!(w >= 3);
        assert!(h >= 2);
    }

    #[test]
    fn fit_aspect_ratio_tall() {
        let (w, h) = fit_aspect_ratio(20.0, 100.0, 30, 15);
        assert!(w <= 30);
        assert!(h <= 15);
    }

    #[test]
    fn fit_aspect_ratio_zero() {
        let (w, h) = fit_aspect_ratio(0.0, 0.0, 30, 15);
        assert!(w >= 1);
        assert!(h >= 1);
    }

    #[test]
    fn layout_to_px_maps_corners() {
        let bb = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        };

        let (x, y) = layout_to_px(LayoutPoint { x: 0.0, y: 0.0 }, &bb, 60, 40);
        assert_eq!((x, y), (0, 0));

        let (x, y) = layout_to_px(LayoutPoint { x: 100.0, y: 50.0 }, &bb, 60, 40);
        assert_eq!((x, y), (59, 39));
    }

    #[test]
    fn render_with_empty_layout() {
        let layout = DiagramLayout {
            nodes: vec![],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: 0,
                budget_exceeded: false,
                crossings: 0,
                ranks: 0,
                max_rank_width: 0,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };

        let minimap = Minimap::new(&layout, MinimapConfig::default());
        let mut buf = Buffer::new(80, 24);
        minimap.render(Rect::new(0, 0, 80, 24), &mut buf, None, None);
    }

    #[test]
    fn border_draws_box_chars() {
        let layout = simple_layout();
        let minimap = Minimap::new(&layout, MinimapConfig::default());
        let mut buf = Buffer::new(120, 40);
        let area = Rect::new(0, 0, 120, 40);

        minimap.render(area, &mut buf, None, None);

        let rect = minimap.placement(area);
        if !rect.is_empty() {
            // Top-left corner should be box-drawing char.
            if let Some(cell) = buf.get(rect.x, rect.y) {
                assert_eq!(cell.content.as_char(), Some('\u{250C}'));
            }
            // Top-right corner should be box-drawing char.
            if let Some(cell) = buf.get(rect.x + rect.width - 1, rect.y) {
                assert_eq!(cell.content.as_char(), Some('\u{2510}'));
            }
        }
    }

    #[test]
    fn minimap_corner_default_is_bottom_right() {
        let corner = MinimapCorner::default();
        assert_eq!(corner, MinimapCorner::BottomRight);
    }

    #[test]
    fn config_default_fields() {
        let config = MinimapConfig::default();
        assert_eq!(config.max_width, 30);
        assert_eq!(config.max_height, 15);
        assert_eq!(config.margin, 1);
        assert_eq!(config.corner, MinimapCorner::BottomRight);
    }

    #[test]
    fn total_size_includes_border() {
        let layout = simple_layout();
        let minimap = Minimap::new(&layout, MinimapConfig::default());
        let (w, h) = minimap.total_size();
        let (cw, ch) = minimap.content_cells;
        assert_eq!(w, cw + 2);
        assert_eq!(h, ch + 2);
    }

    #[test]
    fn placement_bottom_left() {
        let layout = simple_layout();
        let config = MinimapConfig {
            corner: MinimapCorner::BottomLeft,
            margin: 1,
            ..MinimapConfig::default()
        };
        let minimap = Minimap::new(&layout, config);
        let area = Rect::new(0, 0, 120, 40);
        let rect = minimap.placement(area);
        // Should be in bottom-left with margin
        assert_eq!(rect.x, area.x + 1);
        assert!(rect.y > area.y + area.height / 2);
    }

    #[test]
    fn placement_top_right() {
        let layout = simple_layout();
        let config = MinimapConfig {
            corner: MinimapCorner::TopRight,
            margin: 1,
            ..MinimapConfig::default()
        };
        let minimap = Minimap::new(&layout, config);
        let area = Rect::new(0, 0, 120, 40);
        let rect = minimap.placement(area);
        assert!(rect.x > area.x + area.width / 2);
        assert_eq!(rect.y, area.y + 1);
    }

    #[test]
    fn layout_to_px_zero_bounding_box() {
        let bb = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        let (x, y) = layout_to_px(LayoutPoint { x: 5.0, y: 5.0 }, &bb, 60, 40);
        assert_eq!((x, y), (0, 0));
    }

    #[test]
    fn is_trivial_for_tiny_config() {
        let layout = simple_layout();
        let config = MinimapConfig {
            max_width: 4,
            max_height: 3,
            ..MinimapConfig::default()
        };
        let minimap = Minimap::new(&layout, config);
        // With max 4x3 minus 2 for border = 2x1 content, should be trivial
        assert!(minimap.is_trivial());
    }

    #[test]
    fn render_with_viewport_and_selected_node() {
        let layout = simple_layout();
        let minimap = Minimap::new(&layout, MinimapConfig::default());
        let mut buf = Buffer::new(120, 40);
        let area = Rect::new(0, 0, 120, 40);
        let viewport = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 25.0,
        };
        // Should not panic with both viewport and selected node
        minimap.render(area, &mut buf, Some(&viewport), Some(0));
        minimap.render(area, &mut buf, Some(&viewport), Some(1));
    }
}

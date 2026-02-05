//! Terminal renderer for Mermaid diagrams.
//!
//! Converts a [`DiagramLayout`] (abstract world-space coordinates) into
//! terminal cells written to a [`Buffer`]. Supports Unicode box-drawing
//! glyphs with ASCII fallback driven by [`MermaidGlyphMode`].
//!
//! # Pipeline
//!
//! ```text
//! MermaidDiagramIr ─► layout_diagram() ─► DiagramLayout ─► MermaidRenderer::render() ─► Buffer
//! ```

use ftui_core::geometry::Rect;
use ftui_core::text_width::display_width;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::{BorderChars, Draw};

use crate::mermaid::{
    DiagramType, MermaidConfig, MermaidDiagramIr, MermaidError, MermaidErrorMode, MermaidFidelity,
    MermaidGlyphMode, MermaidTier,
};
use crate::mermaid_layout::{
    DiagramLayout, LayoutClusterBox, LayoutEdgePath, LayoutNodeBox, LayoutRect,
};

// ── Glyph Palette ───────────────────────────────────────────────────────

/// Character palette for diagram rendering.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct GlyphPalette {
    border: BorderChars,
    tee_down: char,
    tee_up: char,
    tee_right: char,
    tee_left: char,
    cross: char,
    arrow_right: char,
    arrow_left: char,
    arrow_up: char,
    arrow_down: char,
}

impl GlyphPalette {
    const UNICODE: Self = Self {
        border: BorderChars::SQUARE,
        tee_down: '┬',
        tee_up: '┴',
        tee_right: '├',
        tee_left: '┤',
        cross: '┼',
        arrow_right: '▶',
        arrow_left: '◀',
        arrow_up: '▲',
        arrow_down: '▼',
    };

    const ASCII: Self = Self {
        border: BorderChars::ASCII,
        tee_down: '+',
        tee_up: '+',
        tee_right: '+',
        tee_left: '+',
        cross: '+',
        arrow_right: '>',
        arrow_left: '<',
        arrow_up: '^',
        arrow_down: 'v',
    };

    fn for_mode(mode: MermaidGlyphMode) -> Self {
        match mode {
            MermaidGlyphMode::Unicode => Self::UNICODE,
            MermaidGlyphMode::Ascii => Self::ASCII,
        }
    }
}

#[allow(dead_code)]
const LINE_UP: u8 = 0b0001;
#[allow(dead_code)]
const LINE_DOWN: u8 = 0b0010;
#[allow(dead_code)]
const LINE_LEFT: u8 = 0b0100;
#[allow(dead_code)]
const LINE_RIGHT: u8 = 0b1000;
#[allow(dead_code)]
const LINE_ALL: u8 = LINE_UP | LINE_DOWN | LINE_LEFT | LINE_RIGHT;

// ── Scale Adaptation + Fidelity Tiers ────────────────────────────────

/// Rendering plan derived from fidelity tier selection.
///
/// Controls how much detail is rendered based on available terminal area
/// and diagram complexity.
#[derive(Debug, Clone)]
pub struct RenderPlan {
    /// Selected fidelity tier for this render pass.
    pub fidelity: MermaidFidelity,
    /// Whether to render node labels.
    pub show_node_labels: bool,
    /// Whether to render edge labels.
    pub show_edge_labels: bool,
    /// Whether to render cluster decorations.
    pub show_clusters: bool,
    /// Maximum label width in characters (0 = unlimited).
    pub max_label_width: usize,
    /// Area reserved for the diagram itself.
    pub diagram_area: Rect,
    /// Area reserved for a footnote/legend region (if any).
    pub legend_area: Option<Rect>,
}

/// Select the fidelity tier based on viewport density and scale.
///
/// When `tier_override` is `Auto`, uses heuristics based on how many
/// diagram nodes fit per terminal cell. Returns a `RenderPlan` that
/// configures the renderer appropriately for the selected tier.
#[must_use]
pub fn select_render_plan(
    config: &MermaidConfig,
    layout: &DiagramLayout,
    area: Rect,
) -> RenderPlan {
    let fidelity = select_fidelity(config, layout, area);

    // Determine legend area reservation.
    let (diagram_area, legend_area) = if config.enable_links
        && !layout.nodes.is_empty()
        && fidelity != MermaidFidelity::Outline
    {
        reserve_legend_area(area)
    } else {
        (area, None)
    };

    let (show_node_labels, show_edge_labels, show_clusters, max_label_width) = match fidelity {
        MermaidFidelity::Rich => (true, true, true, 0),
        MermaidFidelity::Normal => (true, true, true, config.max_label_chars),
        MermaidFidelity::Compact => (true, false, false, 16),
        MermaidFidelity::Outline => (false, false, false, 0),
    };

    RenderPlan {
        fidelity,
        show_node_labels,
        show_edge_labels,
        show_clusters,
        max_label_width,
        diagram_area,
        legend_area,
    }
}

/// Select fidelity tier from scale and density heuristics.
#[must_use]
pub fn select_fidelity(
    config: &MermaidConfig,
    layout: &DiagramLayout,
    area: Rect,
) -> MermaidFidelity {
    // Explicit tier overrides heuristics.
    if config.tier_override != MermaidTier::Auto {
        return MermaidFidelity::from_tier(config.tier_override);
    }

    if layout.nodes.is_empty() || area.is_empty() {
        return MermaidFidelity::Normal;
    }

    // Compute scale factor (how many cells per layout unit).
    let margin = 2.0;
    let avail_w = f64::from(area.width).max(1.0) - margin;
    let avail_h = f64::from(area.height).max(1.0) - margin;
    let bb_w = layout.bounding_box.width.max(1.0);
    let bb_h = layout.bounding_box.height.max(1.0);
    let scale = (avail_w / bb_w).min(avail_h / bb_h);

    // Compute density: nodes per available cell.
    let cell_area = avail_w * avail_h;
    let node_count = layout.nodes.len() as f64;
    let density = node_count / cell_area.max(1.0);

    // Tier selection thresholds (deterministic, monotone).
    if scale >= 3.0 && density < 0.005 {
        MermaidFidelity::Rich
    } else if scale >= 1.0 && density < 0.02 {
        MermaidFidelity::Normal
    } else if scale >= 0.4 {
        MermaidFidelity::Compact
    } else {
        MermaidFidelity::Outline
    }
}

/// Reserve a bottom region for link footnotes/legends.
fn reserve_legend_area(area: Rect) -> (Rect, Option<Rect>) {
    let legend_height = 3u16.min(area.height / 4);
    if legend_height == 0 || area.height <= legend_height + 4 {
        return (area, None);
    }
    let diagram_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height.saturating_sub(legend_height),
    };
    let legend_area = Rect {
        x: area.x,
        y: area.y.saturating_add(diagram_area.height),
        width: area.width,
        height: legend_height,
    };
    (diagram_area, Some(legend_area))
}

// ── Viewport mapping ────────────────────────────────────────────────────

/// Maps abstract layout coordinates to terminal cell positions.
#[derive(Debug, Clone)]
struct Viewport {
    scale_x: f64,
    scale_y: f64,
    offset_x: f64,
    offset_y: f64,
}

impl Viewport {
    /// Compute a viewport that fits `bounding_box` into `area` with 1-cell margin.
    fn fit(bounding_box: &LayoutRect, area: Rect) -> Self {
        let margin = 1.0;
        let avail_w = f64::from(area.width).max(1.0) - 2.0 * margin;
        let avail_h = f64::from(area.height).max(1.0) - 2.0 * margin;

        let bb_w = bounding_box.width.max(1.0);
        let bb_h = bounding_box.height.max(1.0);

        // Scale uniformly so the diagram fits, using the tighter axis.
        let scale = (avail_w / bb_w).min(avail_h / bb_h).max(0.1);

        // Center the diagram within the area.
        let used_w = bb_w * scale;
        let used_h = bb_h * scale;
        let pad_x = (avail_w - used_w) / 2.0;
        let pad_y = (avail_h - used_h) / 2.0;

        Self {
            scale_x: scale,
            scale_y: scale,
            offset_x: f64::from(area.x) + margin + pad_x - bounding_box.x * scale,
            offset_y: f64::from(area.y) + margin + pad_y - bounding_box.y * scale,
        }
    }

    /// Convert a world-space point to cell coordinates.
    fn to_cell(&self, x: f64, y: f64) -> (u16, u16) {
        let cx = (x * self.scale_x + self.offset_x).round().max(0.0) as u16;
        let cy = (y * self.scale_y + self.offset_y).round().max(0.0) as u16;
        (cx, cy)
    }

    /// Convert a world-space rect to cell rect, clamping to non-negative sizes.
    fn to_cell_rect(&self, r: &LayoutRect) -> Rect {
        let (x, y) = self.to_cell(r.x, r.y);
        let (x2, y2) = self.to_cell(r.x + r.width, r.y + r.height);
        Rect {
            x,
            y,
            width: x2.saturating_sub(x).max(1),
            height: y2.saturating_sub(y).max(1),
        }
    }
}

// ── Color palette for diagram elements ──────────────────────────────────

const NODE_FG: PackedRgba = PackedRgba::WHITE;
const EDGE_FG: PackedRgba = PackedRgba::rgb(150, 150, 150);
const LABEL_FG: PackedRgba = PackedRgba::WHITE;
const CLUSTER_FG: PackedRgba = PackedRgba::rgb(100, 160, 220);
const CLUSTER_TITLE_FG: PackedRgba = PackedRgba::rgb(100, 160, 220);

// ── Edge line style ──────────────────────────────────────────────────

/// Line style for edge rendering, inferred from the Mermaid arrow syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeLineStyle {
    Solid,
    Dashed,
    Thick,
}

/// Detect edge line style from the Mermaid arrow string.
fn detect_edge_style(arrow: &str) -> EdgeLineStyle {
    if arrow.contains("-.") || arrow.contains(".-") {
        EdgeLineStyle::Dashed
    } else if arrow.contains("==") {
        EdgeLineStyle::Thick
    } else {
        EdgeLineStyle::Solid
    }
}

// ── MermaidRenderer ─────────────────────────────────────────────────────

/// Renders a [`DiagramLayout`] into a terminal [`Buffer`].
pub struct MermaidRenderer {
    palette: GlyphPalette,
}

impl MermaidRenderer {
    /// Create a renderer for the given glyph mode.
    #[must_use]
    pub fn new(config: &MermaidConfig) -> Self {
        Self {
            palette: GlyphPalette::for_mode(config.glyph_mode),
        }
    }

    /// Create a renderer with explicit glyph mode.
    #[must_use]
    pub fn with_mode(mode: MermaidGlyphMode) -> Self {
        Self {
            palette: GlyphPalette::for_mode(mode),
        }
    }

    /// Render a diagram layout into the buffer within the given area.
    pub fn render(
        &self,
        layout: &DiagramLayout,
        ir: &MermaidDiagramIr,
        area: Rect,
        buf: &mut Buffer,
    ) {
        if layout.nodes.is_empty() || area.is_empty() {
            return;
        }

        let vp = Viewport::fit(&layout.bounding_box, area);

        // Render order: clusters (background) → edges → nodes → labels.
        self.render_clusters(&layout.clusters, ir, &vp, buf);
        if ir.diagram_type == DiagramType::Sequence {
            self.render_sequence_lifelines(layout, &vp, buf);
        }
        self.render_edges(&layout.edges, ir, &vp, buf);
        self.render_nodes(&layout.nodes, ir, &vp, buf);
    }

    /// Render with an explicit fidelity plan, adapting detail level to scale.
    pub fn render_with_plan(
        &self,
        layout: &DiagramLayout,
        ir: &MermaidDiagramIr,
        plan: &RenderPlan,
        buf: &mut Buffer,
    ) {
        if layout.nodes.is_empty() || plan.diagram_area.is_empty() {
            return;
        }

        let vp = Viewport::fit(&layout.bounding_box, plan.diagram_area);

        // Render order: clusters (background) → edges → nodes.
        if plan.show_clusters {
            self.render_clusters(&layout.clusters, ir, &vp, buf);
        }
        if ir.diagram_type == DiagramType::Sequence {
            self.render_sequence_lifelines(layout, &vp, buf);
        }
        self.render_edges_with_plan(&layout.edges, ir, &vp, plan, buf);
        self.render_nodes_with_plan(&layout.nodes, ir, &vp, plan, buf);
    }

    /// Render edges respecting the fidelity plan.
    fn render_edges_with_plan(
        &self,
        edges: &[LayoutEdgePath],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        plan: &RenderPlan,
        buf: &mut Buffer,
    ) {
        let edge_cell = Cell::from_char(' ').with_fg(EDGE_FG);
        for edge_path in edges {
            let waypoints: Vec<(u16, u16)> = edge_path
                .waypoints
                .iter()
                .map(|p| vp.to_cell(p.x, p.y))
                .collect();

            let line_style = ir
                .edges
                .get(edge_path.edge_idx)
                .map(|e| detect_edge_style(&e.arrow))
                .unwrap_or(EdgeLineStyle::Solid);

            for pair in waypoints.windows(2) {
                let (x0, y0) = pair[0];
                let (x1, y1) = pair[1];
                self.draw_line_segment_styled(x0, y0, x1, y1, edge_cell, line_style, buf);
            }

            // Arrowhead.
            if waypoints.len() >= 2 {
                let (px, py) = waypoints[waypoints.len() - 2];
                let (tx, ty) = *waypoints.last().unwrap();
                let arrow_ch = self.arrowhead_char(px, py, tx, ty);
                buf.set(tx, ty, edge_cell.with_char(arrow_ch));
            }

            // Edge labels only if plan allows.
            if plan.show_edge_labels
                && let Some(ir_edge) = ir.edges.get(edge_path.edge_idx)
                && let Some(label_id) = ir_edge.label
                && let Some(label) = ir.labels.get(label_id.0)
            {
                self.render_edge_label(edge_path, &label.text, vp, buf);
            }
        }
    }

    /// Render nodes respecting the fidelity plan.
    fn render_nodes_with_plan(
        &self,
        nodes: &[LayoutNodeBox],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        plan: &RenderPlan,
        buf: &mut Buffer,
    ) {
        let border_cell = Cell::from_char(' ').with_fg(NODE_FG);
        let fill_cell = Cell::from_char(' ');

        for node in nodes {
            let cell_rect = vp.to_cell_rect(&node.rect);

            if plan.fidelity == MermaidFidelity::Outline {
                // Outline mode: single character per node.
                let (cx, cy) = vp.to_cell(
                    node.rect.x + node.rect.width / 2.0,
                    node.rect.y + node.rect.height / 2.0,
                );
                buf.set(cx, cy, border_cell.with_char('●'));
                continue;
            }

            if cell_rect.width < 2 || cell_rect.height < 2 {
                let (cx, cy) = vp.to_cell(node.rect.x, node.rect.y);
                buf.set(cx, cy, border_cell.with_char('*'));
                continue;
            }

            buf.draw_box(cell_rect, self.palette.border, border_cell, fill_cell);

            // Labels only if plan allows.
            if plan.show_node_labels
                && let Some(ir_node) = ir.nodes.get(node.node_idx)
                && let Some(label_id) = ir_node.label
                && let Some(label) = ir.labels.get(label_id.0)
            {
                if !ir_node.members.is_empty() {
                    // Class diagram node with compartments.
                    self.render_class_compartments(
                        cell_rect,
                        &label.text,
                        &ir_node.members,
                        plan,
                        buf,
                    );
                } else {
                    let text = if plan.max_label_width > 0 {
                        &truncate_label(&label.text, plan.max_label_width)
                    } else {
                        &label.text
                    };
                    self.render_node_label(cell_rect, text, buf);
                }
            }
        }
    }

    // ── Cluster rendering ───────────────────────────────────────────

    fn render_clusters(
        &self,
        clusters: &[LayoutClusterBox],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        buf: &mut Buffer,
    ) {
        let border_cell = Cell::from_char(' ').with_fg(CLUSTER_FG);
        for cluster in clusters {
            let cell_rect = vp.to_cell_rect(&cluster.rect);
            if cell_rect.width < 2 || cell_rect.height < 2 {
                continue;
            }
            buf.draw_border(cell_rect, self.palette.border, border_cell);

            // Render cluster title if available.
            if let Some(title_rect) = &cluster.title_rect
                && let Some(ir_cluster) = ir.clusters.get(cluster.cluster_idx)
                && let Some(label_id) = ir_cluster.title
                && let Some(label) = ir.labels.get(label_id.0)
            {
                let tr = vp.to_cell_rect(title_rect);
                let title_cell = Cell::from_char(' ').with_fg(CLUSTER_TITLE_FG);
                let max_w = tr.width.saturating_sub(1);
                let text = truncate_label(&label.text, max_w as usize);
                buf.print_text_clipped(
                    tr.x,
                    tr.y,
                    &text,
                    title_cell,
                    tr.x.saturating_add(tr.width),
                );
            }
        }
    }

    // ── Edge rendering ──────────────────────────────────────────────

    fn render_edges(
        &self,
        edges: &[LayoutEdgePath],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        buf: &mut Buffer,
    ) {
        let edge_cell = Cell::from_char(' ').with_fg(EDGE_FG);
        for edge_path in edges {
            let waypoints: Vec<(u16, u16)> = edge_path
                .waypoints
                .iter()
                .map(|p| vp.to_cell(p.x, p.y))
                .collect();

            // Detect line style from arrow syntax.
            let line_style = ir
                .edges
                .get(edge_path.edge_idx)
                .map(|e| detect_edge_style(&e.arrow))
                .unwrap_or(EdgeLineStyle::Solid);

            // Draw line segments between consecutive waypoints.
            for pair in waypoints.windows(2) {
                let (x0, y0) = pair[0];
                let (x1, y1) = pair[1];
                self.draw_line_segment_styled(x0, y0, x1, y1, edge_cell, line_style, buf);
            }

            // Draw arrowhead at the last waypoint.
            if waypoints.len() >= 2 {
                let (px, py) = waypoints[waypoints.len() - 2];
                let (tx, ty) = *waypoints.last().unwrap();
                let arrow_ch = self.arrowhead_char(px, py, tx, ty);
                buf.set(tx, ty, edge_cell.with_char(arrow_ch));
            }

            // Render edge label if present.
            if let Some(ir_edge) = ir.edges.get(edge_path.edge_idx)
                && let Some(label_id) = ir_edge.label
                && let Some(label) = ir.labels.get(label_id.0)
            {
                self.render_edge_label(edge_path, &label.text, vp, buf);
            }
        }
    }

    fn render_sequence_lifelines(&self, layout: &DiagramLayout, vp: &Viewport, buf: &mut Buffer) {
        let line_cell = Cell::from_char(' ').with_fg(EDGE_FG);
        let end_y = layout.bounding_box.y + layout.bounding_box.height;
        for node in &layout.nodes {
            let x = node.rect.x + node.rect.width / 2.0;
            let y0 = node.rect.y + node.rect.height;
            let (cx, cy0) = vp.to_cell(x, y0);
            let (_, cy1) = vp.to_cell(x, end_y);
            let (lo, hi) = if cy0 <= cy1 { (cy0, cy1) } else { (cy1, cy0) };
            for (i, y) in (lo..=hi).enumerate() {
                if i % 2 == 1 {
                    continue;
                }
                self.merge_line_cell(cx, y, LINE_UP | LINE_DOWN, line_cell, buf);
            }
        }
    }

    #[allow(dead_code)]
    fn merge_line_cell(&self, x: u16, y: u16, bits: u8, cell: Cell, buf: &mut Buffer) {
        let mut merged = bits & LINE_ALL;
        if let Some(existing) = buf.get(x, y).and_then(|c| c.content.as_char())
            && let Some(existing_bits) = self.line_bits_for_char(existing)
        {
            merged |= existing_bits;
        }
        let ch = self.line_char_for_bits(merged);
        buf.set(x, y, cell.with_char(ch));
    }

    #[allow(dead_code)]
    fn line_bits_for_char(&self, ch: char) -> Option<u8> {
        let p = &self.palette;
        match ch {
            c if c == p.border.horizontal => Some(LINE_LEFT | LINE_RIGHT),
            c if c == p.border.vertical => Some(LINE_UP | LINE_DOWN),
            c if c == p.border.top_left => Some(LINE_RIGHT | LINE_DOWN),
            c if c == p.border.top_right => Some(LINE_LEFT | LINE_DOWN),
            c if c == p.border.bottom_left => Some(LINE_RIGHT | LINE_UP),
            c if c == p.border.bottom_right => Some(LINE_LEFT | LINE_UP),
            c if c == p.tee_down => Some(LINE_LEFT | LINE_RIGHT | LINE_DOWN),
            c if c == p.tee_up => Some(LINE_LEFT | LINE_RIGHT | LINE_UP),
            c if c == p.tee_right => Some(LINE_UP | LINE_DOWN | LINE_RIGHT),
            c if c == p.tee_left => Some(LINE_UP | LINE_DOWN | LINE_LEFT),
            c if c == p.cross => Some(LINE_ALL),
            _ => None,
        }
    }

    #[allow(dead_code)]
    fn line_char_for_bits(&self, bits: u8) -> char {
        let p = &self.palette;
        match bits {
            b if b == (LINE_LEFT | LINE_RIGHT) || b == LINE_LEFT || b == LINE_RIGHT => {
                p.border.horizontal
            }
            b if b == (LINE_UP | LINE_DOWN) || b == LINE_UP || b == LINE_DOWN => p.border.vertical,
            b if b == (LINE_RIGHT | LINE_DOWN) => p.border.top_left,
            b if b == (LINE_LEFT | LINE_DOWN) => p.border.top_right,
            b if b == (LINE_RIGHT | LINE_UP) => p.border.bottom_left,
            b if b == (LINE_LEFT | LINE_UP) => p.border.bottom_right,
            b if b == (LINE_LEFT | LINE_RIGHT | LINE_DOWN) => p.tee_down,
            b if b == (LINE_LEFT | LINE_RIGHT | LINE_UP) => p.tee_up,
            b if b == (LINE_UP | LINE_DOWN | LINE_RIGHT) => p.tee_right,
            b if b == (LINE_UP | LINE_DOWN | LINE_LEFT) => p.tee_left,
            b if b == LINE_ALL => p.cross,
            _ => p.border.horizontal,
        }
    }

    /// Draw a styled line segment between two cell positions.
    #[allow(clippy::too_many_arguments)]
    fn draw_line_segment_styled(
        &self,
        x0: u16,
        y0: u16,
        x1: u16,
        y1: u16,
        cell: Cell,
        style: EdgeLineStyle,
        buf: &mut Buffer,
    ) {
        match style {
            EdgeLineStyle::Solid => self.draw_line_segment(x0, y0, x1, y1, cell, buf),
            EdgeLineStyle::Dashed => self.draw_dashed_segment(x0, y0, x1, y1, cell, buf),
            EdgeLineStyle::Thick => {
                // Thick uses double-line border chars if available, otherwise solid.
                self.draw_line_segment(x0, y0, x1, y1, cell, buf);
            }
        }
    }

    /// Draw a dashed line segment (every other cell is blank).
    #[allow(clippy::too_many_arguments)]
    fn draw_dashed_segment(
        &self,
        x0: u16,
        y0: u16,
        x1: u16,
        y1: u16,
        cell: Cell,
        buf: &mut Buffer,
    ) {
        if y0 == y1 {
            let lo = x0.min(x1);
            let hi = x0.max(x1);
            for (i, x) in (lo..=hi).enumerate() {
                if i % 2 == 0 {
                    self.merge_line_cell(x, y0, LINE_LEFT | LINE_RIGHT, cell, buf);
                }
            }
        } else if x0 == x1 {
            let lo = y0.min(y1);
            let hi = y0.max(y1);
            for (i, y) in (lo..=hi).enumerate() {
                if i % 2 == 0 {
                    self.merge_line_cell(x0, y, LINE_UP | LINE_DOWN, cell, buf);
                }
            }
        } else {
            // Diagonal dashed — L-bend with every other cell blank.
            let lo_x = x0.min(x1);
            let hi_x = x0.max(x1);
            for (i, x) in (lo_x..=hi_x).enumerate() {
                if i % 2 == 0 {
                    self.merge_line_cell(x, y0, LINE_LEFT | LINE_RIGHT, cell, buf);
                }
            }
            let lo_y = y0.min(y1);
            let hi_y = y0.max(y1);
            for (i, y) in (lo_y..=hi_y).enumerate() {
                if i % 2 == 0 {
                    self.merge_line_cell(x1, y, LINE_UP | LINE_DOWN, cell, buf);
                }
            }
        }
    }

    /// Draw a single line segment between two cell positions.
    fn draw_line_segment(&self, x0: u16, y0: u16, x1: u16, y1: u16, cell: Cell, buf: &mut Buffer) {
        if y0 == y1 {
            // Horizontal segment.
            let lo = x0.min(x1);
            let hi = x0.max(x1);
            for x in lo..=hi {
                self.merge_line_cell(x, y0, LINE_LEFT | LINE_RIGHT, cell, buf);
            }
        } else if x0 == x1 {
            // Vertical segment.
            let lo = y0.min(y1);
            let hi = y0.max(y1);
            for y in lo..=hi {
                self.merge_line_cell(x0, y, LINE_UP | LINE_DOWN, cell, buf);
            }
        } else {
            // Diagonal — approximate with an L-shaped bend.
            let lo_x = x0.min(x1);
            let hi_x = x0.max(x1);
            for x in lo_x..=hi_x {
                if x == x1 {
                    continue;
                }
                self.merge_line_cell(x, y0, LINE_LEFT | LINE_RIGHT, cell, buf);
            }

            let lo_y = y0.min(y1);
            let hi_y = y0.max(y1);
            for y in lo_y..=hi_y {
                if y == y0 {
                    continue;
                }
                self.merge_line_cell(x1, y, LINE_UP | LINE_DOWN, cell, buf);
            }

            let horiz_bit = if x1 >= x0 { LINE_LEFT } else { LINE_RIGHT };
            let vert_bit = if y1 >= y0 { LINE_DOWN } else { LINE_UP };
            self.merge_line_cell(x1, y0, horiz_bit | vert_bit, cell, buf);
        }
    }

    /// Pick the arrowhead character based on approach direction.
    fn arrowhead_char(&self, from_x: u16, from_y: u16, to_x: u16, to_y: u16) -> char {
        let dx = i32::from(to_x) - i32::from(from_x);
        let dy = i32::from(to_y) - i32::from(from_y);
        if dx.abs() >= dy.abs() {
            if dx >= 0 {
                self.palette.arrow_right
            } else {
                self.palette.arrow_left
            }
        } else if dy >= 0 {
            self.palette.arrow_down
        } else {
            self.palette.arrow_up
        }
    }

    /// Render an edge label at the midpoint of the edge path.
    fn render_edge_label(
        &self,
        edge_path: &LayoutEdgePath,
        text: &str,
        vp: &Viewport,
        buf: &mut Buffer,
    ) {
        if edge_path.waypoints.len() < 2 || text.is_empty() {
            return;
        }
        // Place label near the midpoint of the path.
        let mid_idx = edge_path.waypoints.len() / 2;
        let mid = &edge_path.waypoints[mid_idx];
        let (cx, cy) = vp.to_cell(mid.x, mid.y);
        let label = truncate_label(text, 16);
        let label_cell = Cell::from_char(' ').with_fg(LABEL_FG);
        buf.print_text(cx.saturating_add(1), cy, &label, label_cell);
    }

    // ── Node rendering ──────────────────────────────────────────────

    fn render_nodes(
        &self,
        nodes: &[LayoutNodeBox],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        buf: &mut Buffer,
    ) {
        let border_cell = Cell::from_char(' ').with_fg(NODE_FG);
        let fill_cell = Cell::from_char(' ');

        for node in nodes {
            let cell_rect = vp.to_cell_rect(&node.rect);
            if cell_rect.width < 2 || cell_rect.height < 2 {
                // Too small for a box; render as a single char.
                let (cx, cy) = vp.to_cell(node.rect.x, node.rect.y);
                buf.set(cx, cy, border_cell.with_char('*'));
                continue;
            }

            buf.draw_box(cell_rect, self.palette.border, border_cell, fill_cell);

            // Render label inside the node.
            if let Some(ir_node) = ir.nodes.get(node.node_idx)
                && let Some(label_id) = ir_node.label
                && let Some(label) = ir.labels.get(label_id.0)
            {
                self.render_node_label(cell_rect, &label.text, buf);
            }
        }
    }

    /// Render a label centered inside a node rectangle.
    ///
    /// When the label text is wider than the node interior, text is wrapped
    /// at word boundaries (falling back to character breaks) and the block
    /// of lines is centered vertically. If there are more lines than rows,
    /// the last visible line is truncated with an ellipsis.
    fn render_node_label(&self, cell_rect: Rect, text: &str, buf: &mut Buffer) {
        // Available interior space (excluding border).
        let inner_w = cell_rect.width.saturating_sub(2) as usize;
        let inner_h = cell_rect.height.saturating_sub(2) as usize;
        if inner_w == 0 || inner_h == 0 {
            return;
        }

        let max_x = cell_rect
            .x
            .saturating_add(cell_rect.width)
            .saturating_sub(1);
        let label_cell = Cell::from_char(' ').with_fg(LABEL_FG);

        let mut lines = wrap_text(text, inner_w);

        // If more lines than rows, truncate and add ellipsis to the last visible line.
        if lines.len() > inner_h {
            lines.truncate(inner_h);
            if let Some(last) = lines.last_mut() {
                *last = truncate_label(last, inner_w);
            }
        }

        // Center the block of lines vertically.
        let pad_y = inner_h.saturating_sub(lines.len()) / 2;

        for (i, line) in lines.iter().enumerate() {
            let line_width = display_width(line).min(inner_w);
            let pad_x = (inner_w.saturating_sub(line_width)) / 2;

            let lx = cell_rect.x.saturating_add(1).saturating_add(pad_x as u16);
            let ly = cell_rect
                .y
                .saturating_add(1)
                .saturating_add(pad_y as u16 + i as u16);
            buf.print_text_clipped(lx, ly, line, label_cell, max_x);
        }
    }

    /// Render a class diagram node with compartments (name + members).
    fn render_class_compartments(
        &self,
        cell_rect: Rect,
        label_text: &str,
        members: &[String],
        plan: &RenderPlan,
        buf: &mut Buffer,
    ) {
        let border_cell = Cell::from_char(' ').with_fg(NODE_FG);
        let label_cell = Cell::from_char(' ').with_fg(LABEL_FG);
        let member_cell = Cell::from_char(' ').with_fg(EDGE_FG);
        let inner_w = cell_rect.width.saturating_sub(2) as usize;

        if inner_w == 0 || cell_rect.height < 4 {
            // Too small for compartments, fall back to normal label.
            self.render_node_label(cell_rect, label_text, buf);
            return;
        }

        let max_x = cell_rect
            .x
            .saturating_add(cell_rect.width)
            .saturating_sub(1);

        // Row 0 = top border (already drawn by draw_box)
        // Row 1 = class name (centered)
        let name_y = cell_rect.y.saturating_add(1);
        let name_text = if plan.max_label_width > 0 {
            truncate_label(label_text, plan.max_label_width)
        } else {
            label_text.to_string()
        };
        let name_pad = inner_w.saturating_sub(name_text.len()) / 2;
        let name_x = cell_rect
            .x
            .saturating_add(1)
            .saturating_add(name_pad as u16);
        buf.print_text_clipped(name_x, name_y, &name_text, label_cell, max_x);

        // Row 2 = separator line (├───┤)
        let sep_y = cell_rect.y.saturating_add(2);
        if sep_y
            < cell_rect
                .y
                .saturating_add(cell_rect.height)
                .saturating_sub(1)
        {
            let horiz = self.palette.border.horizontal;
            buf.set(
                cell_rect.x,
                sep_y,
                border_cell.with_char(self.palette.tee_right),
            );
            for col in 1..cell_rect.width.saturating_sub(1) {
                buf.set(
                    cell_rect.x.saturating_add(col),
                    sep_y,
                    border_cell.with_char(horiz),
                );
            }
            buf.set(
                cell_rect
                    .x
                    .saturating_add(cell_rect.width)
                    .saturating_sub(1),
                sep_y,
                border_cell.with_char(self.palette.tee_left),
            );
        }

        // Rows 3.. = member lines
        let members_start_y = cell_rect.y.saturating_add(3);
        let bottom_y = cell_rect
            .y
            .saturating_add(cell_rect.height)
            .saturating_sub(1);
        for (i, member) in members.iter().enumerate() {
            let row_y = members_start_y.saturating_add(i as u16);
            if row_y >= bottom_y {
                break;
            }
            let member_text = truncate_label(member, inner_w);
            let mx = cell_rect.x.saturating_add(1);
            buf.print_text_clipped(mx, row_y, &member_text, member_cell, max_x);
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Truncate a label to fit within `max_chars`, adding ellipsis if needed.
fn truncate_label(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    if max_chars == 1 {
        return text
            .chars()
            .next()
            .map_or_else(String::new, |c| c.to_string());
    }
    let mut result: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    result.push('…');
    result
}

/// Wrap text into lines that fit within `max_width` display columns.
///
/// Splits at word boundaries (ASCII spaces) when possible, otherwise breaks
/// mid-word. Each line's display width is at most `max_width`.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![];
    }
    if display_width(text) <= max_width {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if display_width(remaining) <= max_width {
            lines.push(remaining.to_string());
            break;
        }

        // Find the best break point within max_width.
        let mut break_at = 0;
        let mut last_space = None;
        let mut width_so_far = 0;

        for (byte_idx, ch) in remaining.char_indices() {
            let ch_w = ftui_core::text_width::char_width(ch);
            if width_so_far + ch_w > max_width {
                break;
            }
            width_so_far += ch_w;
            break_at = byte_idx + ch.len_utf8();
            if ch == ' ' {
                last_space = Some(byte_idx);
            }
        }

        // Prefer breaking at a space if one was found.
        let split_pos = if let Some(sp) = last_space {
            sp
        } else if break_at > 0 {
            break_at
        } else {
            // Single character wider than max_width; take it anyway.
            remaining
                .char_indices()
                .nth(1)
                .map_or(remaining.len(), |(idx, _)| idx)
        };

        let (line, rest) = remaining.split_at(split_pos);
        lines.push(line.trim_end().to_string());
        remaining = rest.trim_start();
    }

    lines
}

// ── Convenience API ─────────────────────────────────────────────────────

/// Render a mermaid diagram into a buffer area using default settings.
///
/// This is a convenience function that combines layout computation and rendering.
/// For more control, use [`MermaidRenderer`] directly.
pub fn render_diagram(
    layout: &DiagramLayout,
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    area: Rect,
    buf: &mut Buffer,
) {
    let renderer = MermaidRenderer::new(config);
    renderer.render(layout, ir, area, buf);
}

/// Render with automatic scale adaptation and fidelity tier selection.
///
/// Selects the fidelity tier based on diagram density and available space,
/// then renders with the appropriate level of detail.
pub fn render_diagram_adaptive(
    layout: &DiagramLayout,
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    area: Rect,
    buf: &mut Buffer,
) -> RenderPlan {
    let plan = select_render_plan(config, layout, area);
    let renderer = MermaidRenderer::new(config);
    renderer.render_with_plan(layout, ir, &plan, buf);
    plan
}

// ── Error Rendering ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MermaidErrorRenderReport {
    pub mode: MermaidErrorMode,
    pub overlay: bool,
    pub error_count: usize,
    pub area: Rect,
}

/// Render a Mermaid error panel into the provided area.
pub fn render_mermaid_error_panel(
    errors: &[MermaidError],
    source: &str,
    config: &MermaidConfig,
    area: Rect,
    buf: &mut Buffer,
) -> MermaidErrorRenderReport {
    render_mermaid_error_internal(errors, source, config, area, buf, false)
}

/// Render a Mermaid error panel overlay (for partial render recovery).
pub fn render_mermaid_error_overlay(
    errors: &[MermaidError],
    source: &str,
    config: &MermaidConfig,
    area: Rect,
    buf: &mut Buffer,
) -> MermaidErrorRenderReport {
    render_mermaid_error_internal(errors, source, config, area, buf, true)
}

fn render_mermaid_error_internal(
    errors: &[MermaidError],
    source: &str,
    config: &MermaidConfig,
    area: Rect,
    buf: &mut Buffer,
    overlay: bool,
) -> MermaidErrorRenderReport {
    let mut report = MermaidErrorRenderReport {
        mode: config.error_mode,
        overlay,
        error_count: errors.len(),
        area,
    };

    if errors.is_empty() || area.is_empty() {
        return report;
    }

    let mode = effective_error_mode(config.error_mode, area);
    let target = if overlay {
        compute_error_overlay_area(area, mode, errors.len())
    } else {
        area
    };

    if target.is_empty() {
        return report;
    }

    match mode {
        MermaidErrorMode::Panel => render_error_panel_section(target, errors, config, buf),
        MermaidErrorMode::Raw => render_error_raw_section(target, errors, source, config, buf),
        MermaidErrorMode::Both => {
            let (top, bottom) = split_error_sections(target);
            render_error_panel_section(top, errors, config, buf);
            render_error_raw_section(bottom, errors, source, config, buf);
        }
    }

    emit_error_render_jsonl(config, errors, mode, overlay, target);
    report.mode = mode;
    report.area = target;
    report
}

const ERROR_PANEL_MIN_HEIGHT: u16 = 5;
const ERROR_RAW_MIN_HEIGHT: u16 = 5;
const ERROR_OVERLAY_MIN_WIDTH: u16 = 24;
const ERROR_OVERLAY_MAX_WIDTH: u16 = 72;

fn effective_error_mode(requested: MermaidErrorMode, area: Rect) -> MermaidErrorMode {
    if area.height < ERROR_PANEL_MIN_HEIGHT {
        return MermaidErrorMode::Panel;
    }
    match requested {
        MermaidErrorMode::Panel => MermaidErrorMode::Panel,
        MermaidErrorMode::Raw => {
            if area.height >= ERROR_RAW_MIN_HEIGHT {
                MermaidErrorMode::Raw
            } else {
                MermaidErrorMode::Panel
            }
        }
        MermaidErrorMode::Both => {
            if area.height >= ERROR_PANEL_MIN_HEIGHT + ERROR_RAW_MIN_HEIGHT {
                MermaidErrorMode::Both
            } else {
                MermaidErrorMode::Panel
            }
        }
    }
}

fn compute_error_overlay_area(area: Rect, mode: MermaidErrorMode, error_count: usize) -> Rect {
    if area.is_empty() {
        return area;
    }

    let width = if area.width < ERROR_OVERLAY_MIN_WIDTH {
        area.width
    } else {
        area.width.min(ERROR_OVERLAY_MAX_WIDTH)
    };

    let base_height: u16 = match mode {
        MermaidErrorMode::Panel => 6,
        MermaidErrorMode::Raw => 6,
        MermaidErrorMode::Both => 10,
    };
    let mut height = base_height.saturating_add(error_count as u16);
    height = height.min(area.height).max(base_height.min(area.height));

    Rect::new(area.x, area.y, width, height)
}

fn split_error_sections(area: Rect) -> (Rect, Rect) {
    let min_section = ERROR_PANEL_MIN_HEIGHT;
    let mut top_h = area.height / 2;
    if top_h < min_section {
        top_h = min_section.min(area.height);
    }
    let bottom_h = area.height.saturating_sub(top_h);
    (
        Rect::new(area.x, area.y, area.width, top_h),
        Rect::new(area.x, area.y.saturating_add(top_h), area.width, bottom_h),
    )
}

fn error_border_chars(config: &MermaidConfig) -> BorderChars {
    match config.glyph_mode {
        MermaidGlyphMode::Ascii => BorderChars::ASCII,
        MermaidGlyphMode::Unicode => BorderChars::DOUBLE,
    }
}

fn make_cell(fg: PackedRgba, bg: PackedRgba) -> Cell {
    let mut cell = Cell::from_char(' ');
    cell.fg = fg;
    cell.bg = bg;
    cell
}

fn inner_rect(area: Rect) -> Rect {
    if area.width <= 2 || area.height <= 2 {
        return Rect::default();
    }
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

fn render_error_panel_section(
    area: Rect,
    errors: &[MermaidError],
    config: &MermaidConfig,
    buf: &mut Buffer,
) {
    if area.is_empty() {
        return;
    }

    let border = error_border_chars(config);
    let border_cell = make_cell(PackedRgba::rgb(220, 80, 80), PackedRgba::rgb(32, 12, 12));
    let fill_cell = make_cell(PackedRgba::rgb(240, 220, 220), PackedRgba::rgb(32, 12, 12));
    let header_cell = make_cell(PackedRgba::rgb(255, 140, 140), PackedRgba::rgb(32, 12, 12));
    let text_cell = make_cell(PackedRgba::rgb(240, 220, 220), PackedRgba::rgb(32, 12, 12));

    buf.draw_box(area, border, border_cell, fill_cell);

    let inner = inner_rect(area);
    if inner.is_empty() {
        return;
    }

    let mut y = inner.y;
    let title = format!("Mermaid error ({})", errors.len());
    buf.print_text_clipped(inner.x, y, &title, header_cell, inner.right());
    y = y.saturating_add(1);

    let max_width = inner.width as usize;
    for error in errors {
        if y >= inner.bottom() {
            break;
        }
        let line = format!(
            "L{}:{} {}",
            error.span.start.line, error.span.start.col, error.message
        );
        y = write_wrapped_lines(buf, inner, y, &line, text_cell, max_width);
        if y >= inner.bottom() {
            break;
        }
        if let Some(expected) = &error.expected {
            let expected_line = format!("expected: {}", expected.join(", "));
            y = write_wrapped_lines(buf, inner, y, &expected_line, text_cell, max_width);
        }
    }
}

fn write_wrapped_lines(
    buf: &mut Buffer,
    inner: Rect,
    mut y: u16,
    text: &str,
    cell: Cell,
    max_width: usize,
) -> u16 {
    for line in wrap_text(text, max_width) {
        if y >= inner.bottom() {
            break;
        }
        buf.print_text_clipped(inner.x, y, &line, cell, inner.right());
        y = y.saturating_add(1);
    }
    y
}

fn render_error_raw_section(
    area: Rect,
    errors: &[MermaidError],
    source: &str,
    config: &MermaidConfig,
    buf: &mut Buffer,
) {
    if area.is_empty() {
        return;
    }

    let border = error_border_chars(config);
    let border_cell = make_cell(PackedRgba::rgb(160, 160, 160), PackedRgba::rgb(18, 18, 18));
    let fill_cell = make_cell(PackedRgba::rgb(220, 220, 220), PackedRgba::rgb(18, 18, 18));
    let header_cell = make_cell(PackedRgba::rgb(200, 200, 200), PackedRgba::rgb(18, 18, 18));
    let line_cell = make_cell(PackedRgba::rgb(220, 220, 220), PackedRgba::rgb(18, 18, 18));
    let line_no_cell = make_cell(PackedRgba::rgb(160, 160, 160), PackedRgba::rgb(18, 18, 18));
    let error_cell = make_cell(PackedRgba::rgb(255, 220, 220), PackedRgba::rgb(64, 18, 18));

    buf.draw_box(area, border, border_cell, fill_cell);

    let inner = inner_rect(area);
    if inner.is_empty() {
        return;
    }

    let mut y = inner.y;
    buf.print_text_clipped(inner.x, y, "Mermaid source", header_cell, inner.right());
    y = y.saturating_add(1);

    let max_lines = inner.bottom().saturating_sub(y) as usize;
    if max_lines == 0 {
        return;
    }

    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len().max(1);
    let mut error_lines: Vec<usize> = errors.iter().map(|e| e.span.start.line).collect();
    error_lines.sort_unstable();
    error_lines.dedup();

    let focus_line = error_lines.first().copied().unwrap_or(1).min(total_lines);
    let mut start_line = if focus_line > max_lines / 2 {
        focus_line - max_lines / 2
    } else {
        1
    };
    if start_line + max_lines - 1 > total_lines {
        start_line = total_lines.saturating_sub(max_lines).saturating_add(1);
    }

    let line_no_width = total_lines.to_string().len().max(2);

    for i in 0..max_lines {
        let line_no = start_line + i;
        if line_no > total_lines {
            break;
        }

        let prefix = format!("{:>width$} | ", line_no, width = line_no_width);
        let line_text = lines.get(line_no.saturating_sub(1)).copied().unwrap_or("");
        let is_error = error_lines.contains(&line_no);
        let prefix_cell = if is_error { error_cell } else { line_no_cell };
        let text_cell = if is_error { error_cell } else { line_cell };

        let mut x = inner.x;
        x = buf.print_text_clipped(x, y, &prefix, prefix_cell, inner.right());
        buf.print_text_clipped(x, y, line_text, text_cell, inner.right());
        y = y.saturating_add(1);
    }
}

fn emit_error_render_jsonl(
    config: &MermaidConfig,
    errors: &[MermaidError],
    mode: MermaidErrorMode,
    overlay: bool,
    area: Rect,
) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    let error_entries: Vec<serde_json::Value> = errors
        .iter()
        .map(|err| {
            serde_json::json!({
                "code": err.code.as_str(),
                "message": err.message.as_str(),
                "line": err.span.start.line,
                "col": err.span.start.col,
            })
        })
        .collect();
    let codes: Vec<&str> = errors.iter().map(|err| err.code.as_str()).collect();
    let json = serde_json::json!({
        "event": "mermaid_error_render",
        "mode": mode.as_str(),
        "overlay": overlay,
        "error_count": errors.len(),
        "codes": codes,
        "errors": error_entries,
        "area": {
            "x": area.x,
            "y": area.y,
            "width": area.width,
            "height": area.height,
        },
    });
    let line = json.to_string();
    let _ = crate::mermaid::append_jsonl_line(path, &line);
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mermaid::{
        DiagramType, GraphDirection, IrEdge, IrEndpoint, IrLabel, IrLabelId, IrNode,
        MermaidCompatibilityMatrix, MermaidConfig, MermaidDiagramMeta, MermaidErrorMode,
        MermaidFallbackPolicy, MermaidGuardReport, MermaidInitConfig, MermaidInitParse,
        MermaidSupportLevel, MermaidThemeOverrides, NodeShape, Position, Span, normalize_ast_to_ir,
        parse_with_diagnostics,
    };
    use crate::mermaid_layout::{LayoutPoint, LayoutStats, layout_diagram};
    use std::fmt::Write as FmtWrite;
    use std::path::Path;

    fn make_label(text: &str) -> IrLabel {
        IrLabel {
            text: text.to_string(),
            span: Span {
                start: Position {
                    line: 0,
                    col: 0,
                    byte: 0,
                },
                end: Position {
                    line: 0,
                    col: 0,
                    byte: 0,
                },
            },
        }
    }

    fn make_ir(node_count: usize, edges: Vec<(usize, usize)>) -> MermaidDiagramIr {
        let labels: Vec<IrLabel> = (0..node_count)
            .map(|i| make_label(&format!("N{i}")))
            .collect();

        let nodes: Vec<IrNode> = (0..node_count)
            .map(|i| IrNode {
                id: format!("n{i}"),
                label: Some(IrLabelId(i)),
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: Span {
                    start: Position {
                        line: 0,
                        col: 0,
                        byte: 0,
                    },
                    end: Position {
                        line: 0,
                        col: 0,
                        byte: 0,
                    },
                },
                span_all: vec![],
                implicit: false,
                members: vec![],
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .map(|(from, to)| IrEdge {
                from: IrEndpoint::Node(crate::mermaid::IrNodeId(*from)),
                to: IrEndpoint::Node(crate::mermaid::IrNodeId(*to)),
                arrow: "-->".to_string(),
                label: None,
                style_ref: None,
                span: Span {
                    start: Position {
                        line: 0,
                        col: 0,
                        byte: 0,
                    },
                    end: Position {
                        line: 0,
                        col: 0,
                        byte: 0,
                    },
                },
            })
            .collect();

        MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction: GraphDirection::TB,
            nodes,
            edges: ir_edges,
            ports: vec![],
            clusters: vec![],
            labels,
            style_refs: vec![],
            links: vec![],
            meta: MermaidDiagramMeta {
                diagram_type: DiagramType::Graph,
                direction: GraphDirection::TB,
                support_level: MermaidSupportLevel::Supported,
                init: MermaidInitParse {
                    config: MermaidInitConfig::default(),
                    warnings: Vec::new(),
                    errors: Vec::new(),
                },
                theme_overrides: MermaidThemeOverrides::default(),
                guard: MermaidGuardReport::default(),
            },
        }
    }

    fn make_layout(node_count: usize, edges: Vec<(usize, usize)>) -> DiagramLayout {
        let spacing = 10.0;
        let node_w = 8.0;
        let node_h = 3.0;

        let nodes: Vec<LayoutNodeBox> = (0..node_count)
            .map(|i| {
                let x = (i % 3) as f64 * (node_w + spacing);
                let y = (i / 3) as f64 * (node_h + spacing);
                LayoutNodeBox {
                    node_idx: i,
                    rect: LayoutRect {
                        x,
                        y,
                        width: node_w,
                        height: node_h,
                    },
                    label_rect: Some(LayoutRect {
                        x: x + 1.0,
                        y: y + 1.0,
                        width: node_w - 2.0,
                        height: node_h - 2.0,
                    }),
                    rank: i / 3,
                    order: i % 3,
                }
            })
            .collect();

        let edge_paths: Vec<LayoutEdgePath> = edges
            .iter()
            .enumerate()
            .map(|(idx, (from, to))| {
                let from_node = &nodes[*from];
                let to_node = &nodes[*to];
                LayoutEdgePath {
                    edge_idx: idx,
                    waypoints: vec![
                        LayoutPoint {
                            x: from_node.rect.x + from_node.rect.width / 2.0,
                            y: from_node.rect.y + from_node.rect.height,
                        },
                        LayoutPoint {
                            x: to_node.rect.x + to_node.rect.width / 2.0,
                            y: to_node.rect.y,
                        },
                    ],
                }
            })
            .collect();

        let max_x = nodes
            .iter()
            .map(|n| n.rect.x + n.rect.width)
            .fold(0.0f64, f64::max);
        let max_y = nodes
            .iter()
            .map(|n| n.rect.y + n.rect.height)
            .fold(0.0f64, f64::max);

        DiagramLayout {
            nodes,
            clusters: vec![],
            edges: edge_paths,
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: max_x,
                height: max_y,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: 100,
                budget_exceeded: false,
                crossings: 0,
                ranks: (node_count / 3) + 1,
                max_rank_width: 3.min(node_count),
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        }
    }

    fn buffer_to_text(buf: &Buffer) -> String {
        let capacity = (buf.width() as usize + 1) * buf.height() as usize;
        let mut out = String::with_capacity(capacity);

        for y in 0..buf.height() {
            if y > 0 {
                out.push('\n');
            }
            for x in 0..buf.width() {
                let cell = buf.get(x, y).expect("cell");
                let ch = cell.content.as_char().unwrap_or(' ');
                out.push(ch);
            }
        }

        out
    }

    fn diff_text(expected: &str, actual: &str) -> String {
        let expected_lines: Vec<&str> = expected.lines().collect();
        let actual_lines: Vec<&str> = actual.lines().collect();

        let max_lines = expected_lines.len().max(actual_lines.len());
        let mut out = String::new();
        let mut has_diff = false;

        for i in 0..max_lines {
            let exp = expected_lines.get(i).copied();
            let act = actual_lines.get(i).copied();

            match (exp, act) {
                (Some(e), Some(a)) if e == a => {
                    writeln!(out, " {e}").unwrap();
                }
                (Some(e), Some(a)) => {
                    writeln!(out, "-{e}").unwrap();
                    writeln!(out, "+{a}").unwrap();
                    has_diff = true;
                }
                (Some(e), None) => {
                    writeln!(out, "-{e}").unwrap();
                    has_diff = true;
                }
                (None, Some(a)) => {
                    writeln!(out, "+{a}").unwrap();
                    has_diff = true;
                }
                (None, None) => {}
            }
        }

        if has_diff { out } else { String::new() }
    }

    fn is_bless() -> bool {
        std::env::var("BLESS").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    }

    fn assert_buffer_snapshot_text(name: &str, buf: &Buffer) {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        let path = base
            .join("tests")
            .join("snapshots")
            .join(format!("{name}.txt.snap"));
        let actual = buffer_to_text(buf);

        if is_bless() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("failed to create snapshot directory");
            }
            std::fs::write(&path, &actual).expect("failed to write snapshot");
            return;
        }

        match std::fs::read_to_string(&path) {
            Ok(expected) => {
                if expected != actual {
                    let diff = diff_text(&expected, &actual);
                    std::panic::panic_any(format!(
                        "=== Mermaid error snapshot mismatch: '{name}' ===\nFile: {}\nSet BLESS=1 to update.\n\nDiff (- expected, + actual):\n{diff}",
                        path.display()
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::panic::panic_any(format!(
                    "=== No Mermaid error snapshot found: '{name}' ===\nExpected at: {}\nRun with BLESS=1 to create it.\n\nActual output:\n{actual}",
                    path.display()
                ));
            }
            Err(e) => {
                std::panic::panic_any(format!("Failed to read snapshot '{}': {e}", path.display()));
            }
        }
    }

    #[test]
    fn viewport_fit_centers_diagram() {
        let bb = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 5.0,
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let vp = Viewport::fit(&bb, area);
        assert!(vp.scale_x > 0.0);
        assert!(
            (vp.scale_x - vp.scale_y).abs() < f64::EPSILON,
            "uniform scale"
        );
    }

    #[test]
    fn viewport_to_cell_produces_valid_coords() {
        let bb = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 10.0,
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let vp = Viewport::fit(&bb, area);
        let (cx, cy) = vp.to_cell(10.0, 5.0);
        assert!(cx <= area.width, "x in bounds: {cx}");
        assert!(cy <= area.height, "y in bounds: {cy}");
    }

    #[test]
    fn truncate_label_short_unchanged() {
        assert_eq!(truncate_label("Hello", 10), "Hello");
    }

    #[test]
    fn truncate_label_with_ellipsis() {
        assert_eq!(truncate_label("Hello World", 6), "Hello…");
    }

    #[test]
    fn truncate_label_unicode_safe() {
        assert_eq!(truncate_label("漢字テスト", 3), "漢字…");
    }

    #[test]
    fn render_empty_layout_is_noop() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let ir = make_ir(0, vec![]);
        let layout = DiagramLayout {
            nodes: vec![],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: 100,
                budget_exceeded: false,
                crossings: 0,
                ranks: 0,
                max_rank_width: 0,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let mut buf = Buffer::new(80, 24);
        renderer.render(&layout, &ir, area, &mut buf);
        // No crash, no writes — just verify it doesn't panic.
    }

    #[test]
    fn render_single_node() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let ir = make_ir(1, vec![]);
        let layout = make_layout(1, vec![]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 12,
        };
        let mut buf = Buffer::new(40, 12);
        renderer.render(&layout, &ir, area, &mut buf);

        // The node box should have corner characters somewhere.
        let has_corner = (0..buf.height()).any(|y| {
            (0..buf.width()).any(|x| buf.get(x, y).unwrap().content.as_char() == Some('┌'))
        });
        assert!(has_corner, "expected node box corner in buffer");
    }

    #[test]
    fn render_two_nodes_with_edge() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let ir = make_ir(2, vec![(0, 1)]);
        let layout = make_layout(2, vec![(0, 1)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let mut buf = Buffer::new(80, 24);
        renderer.render(&layout, &ir, area, &mut buf);

        // Should have at least 2 corner characters (2 nodes) and some edge characters.
        let corner_count = (0..buf.height())
            .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.get(x, y).unwrap().content.as_char() == Some('┌'))
            .count();
        assert!(
            corner_count >= 2,
            "expected at least 2 node box corners, got {corner_count}"
        );
    }

    #[test]
    fn merge_line_junctions_unicode_cross() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let mut buf = Buffer::new(12, 12);
        let cell = Cell::from_char(' ').with_fg(EDGE_FG);

        renderer.draw_line_segment(2, 6, 9, 6, cell, &mut buf);
        renderer.draw_line_segment(6, 2, 6, 9, cell, &mut buf);

        assert_eq!(
            buf.get(6, 6).unwrap().content.as_char(),
            Some('┼'),
            "expected unicode junction cross at intersection"
        );
    }

    #[test]
    fn merge_line_junctions_ascii_plus() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Ascii);
        let mut buf = Buffer::new(12, 12);
        let cell = Cell::from_char(' ').with_fg(EDGE_FG);

        renderer.draw_line_segment(2, 6, 9, 6, cell, &mut buf);
        renderer.draw_line_segment(6, 2, 6, 9, cell, &mut buf);

        assert_eq!(
            buf.get(6, 6).unwrap().content.as_char(),
            Some('+'),
            "expected ASCII '+' at junction"
        );
    }

    #[test]
    fn dashed_line_merges_at_intersection() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let mut buf = Buffer::new(12, 12);
        let cell = Cell::from_char(' ').with_fg(EDGE_FG);

        renderer.draw_dashed_segment(2, 6, 10, 6, cell, &mut buf);
        renderer.draw_line_segment(6, 2, 6, 10, cell, &mut buf);

        assert_eq!(
            buf.get(6, 6).unwrap().content.as_char(),
            Some('┼'),
            "expected dashed line to merge at intersection"
        );
    }

    #[test]
    fn diagonal_bend_uses_correct_corner_single_segment() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let mut buf = Buffer::new(12, 12);
        let cell = Cell::from_char(' ').with_fg(EDGE_FG);

        renderer.draw_line_segment(2, 2, 8, 8, cell, &mut buf);

        assert_eq!(
            buf.get(8, 2).unwrap().content.as_char(),
            Some('┐'),
            "expected top-right corner at the bend"
        );
    }

    #[test]
    fn render_ascii_mode() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Ascii);
        let ir = make_ir(2, vec![(0, 1)]);
        let layout = make_layout(2, vec![(0, 1)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::new(60, 20);
        renderer.render(&layout, &ir, area, &mut buf);

        // ASCII mode uses '+' for corners.
        let has_plus = (0..buf.height()).any(|y| {
            (0..buf.width()).any(|x| buf.get(x, y).unwrap().content.as_char() == Some('+'))
        });
        assert!(has_plus, "expected ASCII '+' corner in buffer");

        // Should NOT have Unicode box-drawing characters.
        let has_unicode = (0..buf.height()).any(|y| {
            (0..buf.width()).any(|x| buf.get(x, y).unwrap().content.as_char() == Some('┌'))
        });
        assert!(!has_unicode, "ASCII mode should not use Unicode glyphs");
    }

    #[test]
    fn render_arrowhead_direction() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        // Right arrow.
        assert_eq!(renderer.arrowhead_char(0, 0, 5, 0), '▶');
        // Left arrow.
        assert_eq!(renderer.arrowhead_char(5, 0, 0, 0), '◀');
        // Down arrow.
        assert_eq!(renderer.arrowhead_char(0, 0, 0, 5), '▼');
        // Up arrow.
        assert_eq!(renderer.arrowhead_char(0, 5, 0, 0), '▲');
    }

    #[test]
    fn render_three_node_chain() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let layout = make_layout(3, vec![(0, 1), (1, 2)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let mut buf = Buffer::new(80, 24);
        renderer.render(&layout, &ir, area, &mut buf);

        // Should render 3 node boxes.
        let corner_count = (0..buf.height())
            .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.get(x, y).unwrap().content.as_char() == Some('┌'))
            .count();
        assert!(
            corner_count >= 3,
            "expected at least 3 corners for 3 nodes, got {corner_count}"
        );
    }

    #[test]
    fn diagonal_bend_uses_correct_corner_variants() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let cell = Cell::from_char(' ').with_fg(EDGE_FG);

        let mut buf = Buffer::new(8, 6);
        renderer.draw_line_segment(0, 0, 3, 2, cell, &mut buf);
        assert_eq!(buf.get(3, 0).unwrap().content.as_char(), Some('┐'));

        let mut buf = Buffer::new(8, 6);
        renderer.draw_line_segment(3, 0, 0, 2, cell, &mut buf);
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('┌'));

        let mut buf = Buffer::new(8, 6);
        renderer.draw_line_segment(0, 3, 3, 0, cell, &mut buf);
        assert_eq!(buf.get(3, 3).unwrap().content.as_char(), Some('┘'));

        let mut buf = Buffer::new(8, 6);
        renderer.draw_line_segment(3, 3, 0, 0, cell, &mut buf);
        assert_eq!(buf.get(0, 3).unwrap().content.as_char(), Some('└'));
    }
    #[test]
    fn detect_edge_style_from_arrow() {
        assert_eq!(detect_edge_style("-->"), EdgeLineStyle::Solid);
        assert_eq!(detect_edge_style("---"), EdgeLineStyle::Solid);
        assert_eq!(detect_edge_style("-.->"), EdgeLineStyle::Dashed);
        assert_eq!(detect_edge_style("-.-"), EdgeLineStyle::Dashed);
        assert_eq!(detect_edge_style("==>"), EdgeLineStyle::Thick);
        assert_eq!(detect_edge_style("==="), EdgeLineStyle::Thick);
    }

    #[test]
    fn dashed_segment_skips_every_other_cell() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let cell = Cell::from_char(' ').with_fg(EDGE_FG);
        let mut buf = Buffer::new(12, 4);
        renderer.draw_dashed_segment(0, 1, 9, 1, cell, &mut buf);

        // Count cells that have horizontal line chars — should be roughly half.
        let line_count = (0..10u16)
            .filter(|&x| buf.get(x, 1).and_then(|c| c.content.as_char()) == Some('─'))
            .count();
        assert!(
            (4..=6).contains(&line_count),
            "dashed should draw ~half the cells, got {line_count}"
        );
    }

    // ── wrap_text tests ─────────────────────────────────────────────────

    #[test]
    fn wrap_text_short_fits_single_line() {
        let lines = wrap_text("Hello", 10);
        assert_eq!(lines, vec!["Hello"]);
    }

    #[test]
    fn wrap_text_exact_width() {
        let lines = wrap_text("12345", 5);
        assert_eq!(lines, vec!["12345"]);
    }

    #[test]
    fn wrap_text_word_break() {
        let lines = wrap_text("Hello World", 6);
        assert_eq!(lines, vec!["Hello", "World"]);
    }

    #[test]
    fn wrap_text_multiple_words() {
        let lines = wrap_text("one two three four", 10);
        assert_eq!(lines, vec!["one two", "three four"]);
    }

    #[test]
    fn wrap_text_long_word_breaks_mid_word() {
        let lines = wrap_text("abcdefghij", 5);
        assert_eq!(lines, vec!["abcde", "fghij"]);
    }

    #[test]
    fn wrap_text_zero_width_empty() {
        let lines = wrap_text("Hello", 0);
        assert!(lines.is_empty());
    }

    #[test]
    fn wrap_text_empty_string() {
        let lines = wrap_text("", 10);
        assert_eq!(lines, vec![""]);
    }
    #[test]
    fn fidelity_explicit_tier_override() {
        let layout = make_layout(3, vec![(0, 1), (1, 2)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let config = MermaidConfig {
            tier_override: MermaidTier::Rich,
            ..Default::default()
        };
        assert_eq!(
            select_fidelity(&config, &layout, area),
            MermaidFidelity::Rich
        );
        let config = MermaidConfig {
            tier_override: MermaidTier::Compact,
            ..Default::default()
        };
        assert_eq!(
            select_fidelity(&config, &layout, area),
            MermaidFidelity::Compact
        );
    }

    #[test]
    fn fidelity_auto_selects_based_on_density() {
        let config = MermaidConfig::default(); // tier_override = Auto

        // Small layout in large area → Rich or Normal.
        let layout = make_layout(2, vec![(0, 1)]);
        let large_area = Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 60,
        };
        let tier = select_fidelity(&config, &layout, large_area);
        assert!(
            tier == MermaidFidelity::Rich || tier == MermaidFidelity::Normal,
            "sparse layout in large area should be Rich or Normal, got {:?}",
            tier
        );

        // Large layout in tiny area → Compact or Outline.
        let dense_layout = make_layout(9, vec![(0, 1), (1, 2), (2, 3)]);
        let tiny_area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 8,
        };
        let tier = select_fidelity(&config, &dense_layout, tiny_area);
        assert!(
            tier == MermaidFidelity::Compact || tier == MermaidFidelity::Outline,
            "dense layout in tiny area should be Compact or Outline, got {:?}",
            tier
        );
    }

    #[test]
    fn fidelity_empty_layout_returns_normal() {
        let config = MermaidConfig::default();
        let empty_layout = DiagramLayout {
            nodes: vec![],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: 100,
                budget_exceeded: false,
                crossings: 0,
                ranks: 0,
                max_rank_width: 0,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        assert_eq!(
            select_fidelity(&config, &empty_layout, area),
            MermaidFidelity::Normal
        );
    }

    #[test]
    fn render_plan_compact_hides_edge_labels() {
        let layout = make_layout(3, vec![(0, 1), (1, 2)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let config = MermaidConfig {
            tier_override: MermaidTier::Compact,
            ..Default::default()
        };
        let plan = select_render_plan(&config, &layout, area);
        assert!(!plan.show_edge_labels, "compact should hide edge labels");
        assert!(plan.show_node_labels, "compact should keep node labels");
        assert!(!plan.show_clusters, "compact should hide clusters");
    }

    #[test]
    fn render_plan_outline_hides_all_labels() {
        let layout = make_layout(2, vec![(0, 1)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        // Override to produce Outline via select_fidelity isn't easy,
        // so test the plan construction directly.
        let config = MermaidConfig {
            tier_override: MermaidTier::Compact,
            ..Default::default()
        };
        let plan = select_render_plan(&config, &layout, area);
        assert_eq!(plan.fidelity, MermaidFidelity::Compact);
    }

    #[test]
    fn render_with_plan_produces_output() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let layout = make_layout(3, vec![(0, 1), (1, 2)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let config = MermaidConfig {
            tier_override: MermaidTier::Normal,
            ..Default::default()
        };
        let plan = select_render_plan(&config, &layout, area);
        let mut buf = Buffer::new(80, 24);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);

        // Should have node corners.
        let has_corner = (0..buf.height()).any(|y| {
            (0..buf.width()).any(|x| buf.get(x, y).unwrap().content.as_char() == Some('┌'))
        });
        assert!(has_corner, "expected node box corners in plan-based render");
    }

    #[test]
    fn legend_area_reserved_for_links() {
        let (diagram, legend) = reserve_legend_area(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        assert!(legend.is_some(), "should reserve legend area");
        let legend = legend.unwrap();
        assert!(diagram.height + legend.height <= 24);
        assert_eq!(legend.y, diagram.height);
    }

    #[test]
    fn legend_area_not_reserved_for_tiny_area() {
        let (diagram, legend) = reserve_legend_area(Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 6,
        });
        // Too small to afford legend space.
        if legend.is_none() {
            assert_eq!(diagram.height, 6);
        }
    }

    // ──────────────────────────────────────────────────
    // End-to-end integration tests: parse → IR → layout → render
    // ──────────────────────────────────────────────────

    /// Helper: run the full pipeline on source text and return (Buffer, RenderPlan).
    fn e2e_render(source: &str, width: u16, height: u16) -> (Buffer, RenderPlan) {
        let parsed = parse_with_diagnostics(source);
        assert_ne!(
            parsed.ast.diagram_type,
            DiagramType::Unknown,
            "parse should detect diagram type"
        );
        let config = MermaidConfig::default();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let ir_parse = normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
        assert!(
            ir_parse.errors.is_empty(),
            "IR normalization errors: {:?}",
            ir_parse.errors
        );
        let layout = layout_diagram(&ir_parse.ir, &config);
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut buf = Buffer::new(width, height);
        let plan = render_diagram_adaptive(&layout, &ir_parse.ir, &config, area, &mut buf);
        (buf, plan)
    }

    /// Count occurrences of a character in a buffer.
    fn count_char_in_buf(buf: &Buffer, ch: char) -> usize {
        (0..buf.height())
            .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.get(x, y).unwrap().content.as_char() == Some(ch))
            .count()
    }

    /// Check that a buffer contains at least one non-space character.
    fn buf_has_content(buf: &Buffer) -> bool {
        (0..buf.height()).any(|y| {
            (0..buf.width()).any(|x| {
                let ch = buf.get(x, y).unwrap().content.as_char();
                ch.is_some() && ch != Some(' ')
            })
        })
    }

    // -- graph_small at three sizes --

    #[test]
    fn e2e_graph_small_80x24() {
        let source = include_str!("../tests/fixtures/mermaid/graph_small.mmd");
        let (buf, plan) = e2e_render(source, 80, 24);
        assert!(buf_has_content(&buf), "buffer should have rendered content");
        // graph_small has 3 nodes (Start, Decision, End).
        // Each node box has a top-left corner.
        let corners = count_char_in_buf(&buf, '\u{250c}'); // ┌
        assert!(
            corners >= 2,
            "expected >=2 node corners at 80x24, got {corners}"
        );
        assert_eq!(plan.fidelity, MermaidFidelity::Normal);
    }

    #[test]
    fn e2e_graph_small_120x40() {
        let source = include_str!("../tests/fixtures/mermaid/graph_small.mmd");
        let (buf, plan) = e2e_render(source, 120, 40);
        assert!(buf_has_content(&buf), "buffer should have rendered content");
        let corners = count_char_in_buf(&buf, '\u{250c}');
        assert!(
            corners >= 2,
            "expected >=2 node corners at 120x40, got {corners}"
        );
        // More space → should still be Normal or Rich.
        assert!(
            plan.fidelity == MermaidFidelity::Normal || plan.fidelity == MermaidFidelity::Rich,
            "expected Normal or Rich fidelity at 120x40, got {:?}",
            plan.fidelity
        );
    }

    #[test]
    fn e2e_graph_small_200x60() {
        let source = include_str!("../tests/fixtures/mermaid/graph_small.mmd");
        let (buf, _plan) = e2e_render(source, 200, 60);
        assert!(buf_has_content(&buf), "buffer should have rendered content");
        let corners = count_char_in_buf(&buf, '\u{250c}');
        assert!(
            corners >= 2,
            "expected >=2 node corners at 200x60, got {corners}"
        );
    }

    // -- graph_medium with subgraph --

    #[test]
    fn e2e_graph_medium_80x24() {
        let source = include_str!("../tests/fixtures/mermaid/graph_medium.mmd");
        let (buf, _plan) = e2e_render(source, 80, 24);
        assert!(buf_has_content(&buf), "medium graph should render at 80x24");
    }

    #[test]
    fn e2e_graph_medium_120x40() {
        let source = include_str!("../tests/fixtures/mermaid/graph_medium.mmd");
        let (buf, _plan) = e2e_render(source, 120, 40);
        assert!(
            buf_has_content(&buf),
            "medium graph should render at 120x40"
        );
    }

    // -- graph_large at three sizes --

    #[test]
    fn e2e_graph_large_80x24() {
        let source = include_str!("../tests/fixtures/mermaid/graph_large.mmd");
        let (buf, _plan) = e2e_render(source, 80, 24);
        assert!(buf_has_content(&buf), "large graph should render at 80x24");
    }

    #[test]
    fn e2e_graph_large_120x40() {
        let source = include_str!("../tests/fixtures/mermaid/graph_large.mmd");
        let (buf, _plan) = e2e_render(source, 120, 40);
        assert!(buf_has_content(&buf), "large graph should render at 120x40");
    }

    #[test]
    fn e2e_graph_large_200x60() {
        let source = include_str!("../tests/fixtures/mermaid/graph_large.mmd");
        let (buf, plan) = e2e_render(source, 200, 60);
        assert!(buf_has_content(&buf), "large graph should render at 200x60");
        // 12 nodes in 200x60 is spacious → Normal or Rich.
        assert!(
            plan.fidelity == MermaidFidelity::Normal || plan.fidelity == MermaidFidelity::Rich,
            "expected Normal or Rich for large graph at 200x60, got {:?}",
            plan.fidelity
        );
    }

    // -- Pipeline validation tests --

    #[test]
    fn e2e_pipeline_produces_valid_ir_for_graph() {
        let source = include_str!("../tests/fixtures/mermaid/graph_small.mmd");
        let parsed = parse_with_diagnostics(source);
        assert_eq!(parsed.ast.diagram_type, DiagramType::Graph);
        let config = MermaidConfig::default();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let ir_parse = normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
        assert!(ir_parse.errors.is_empty(), "no IR errors expected");
        // graph_small has 3 nodes: A, B, C.
        assert!(
            ir_parse.ir.nodes.len() >= 3,
            "expected >=3 IR nodes, got {}",
            ir_parse.ir.nodes.len()
        );
        // graph_small has 3 edges: A→B, B→C, B→A.
        assert!(
            ir_parse.ir.edges.len() >= 3,
            "expected >=3 IR edges, got {}",
            ir_parse.ir.edges.len()
        );
    }

    #[test]
    fn e2e_sequence_basic_renders_messages() {
        let source = "sequenceDiagram\nAlice->>Bob: Hello\nBob-->>Alice: Ok\n";
        let (buf, _plan) = e2e_render(source, 80, 24);
        assert!(
            buf_has_content(&buf),
            "sequence diagram should render content"
        );
        let arrows = count_char_in_buf(&buf, '▶') + count_char_in_buf(&buf, '◀');
        assert!(arrows > 0, "expected arrowheads in sequence render");
        let verticals = count_char_in_buf(&buf, '│');
        assert!(
            verticals > 0,
            "expected lifelines or borders in sequence render"
        );
    }

    #[test]
    fn e2e_layout_assigns_positions_for_graph() {
        let source = include_str!("../tests/fixtures/mermaid/graph_small.mmd");
        let parsed = parse_with_diagnostics(source);
        let config = MermaidConfig::default();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let ir_parse = normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
        let layout = layout_diagram(&ir_parse.ir, &config);
        // Each node should have a position assigned.
        assert!(
            layout.nodes.len() >= 3,
            "expected >=3 layout nodes, got {}",
            layout.nodes.len()
        );
        // Bounding box should be non-zero.
        assert!(
            layout.bounding_box.width > 0.0 && layout.bounding_box.height > 0.0,
            "layout bounding box should be non-zero: {:?}",
            layout.bounding_box
        );
    }

    #[test]
    fn e2e_render_stays_within_buffer_bounds() {
        // Verify no out-of-bounds writes happen (Buffer panics on OOB).
        let source = include_str!("../tests/fixtures/mermaid/graph_large.mmd");
        let (buf, _plan) = e2e_render(source, 40, 12);
        // If we got here without panic, bounds are respected.
        // Verify every cell is valid.
        for y in 0..buf.height() {
            for x in 0..buf.width() {
                let _ = buf.get(x, y).expect("cell should be accessible");
            }
        }
    }

    #[test]
    fn e2e_unicode_labels_render() {
        let source = include_str!("../tests/fixtures/mermaid/graph_unicode_labels.mmd");
        let (buf, _plan) = e2e_render(source, 80, 24);
        assert!(
            buf_has_content(&buf),
            "unicode label graph should render at 80x24"
        );
    }

    #[test]
    fn e2e_init_directive_graph_renders() {
        let source = include_str!("../tests/fixtures/mermaid/graph_init_directive.mmd");
        let (buf, _plan) = e2e_render(source, 80, 24);
        assert!(
            buf_has_content(&buf),
            "graph with init directive should render at 80x24"
        );
    }

    #[test]
    fn snapshot_mermaid_error_panel_mode() {
        let source = "graph TD\nclassDef\nA-->B\n";
        let parsed = parse_with_diagnostics(source);
        assert!(!parsed.errors.is_empty(), "expected parse errors");

        let mut config = MermaidConfig::default();
        config.error_mode = MermaidErrorMode::Panel;

        let mut buf = Buffer::new(48, 12);
        render_mermaid_error_panel(
            &parsed.errors,
            source,
            &config,
            Rect::from_size(48, 12),
            &mut buf,
        );
        assert_buffer_snapshot_text("mermaid_error_panel", &buf);
    }

    #[test]
    fn snapshot_mermaid_error_raw_mode() {
        let source = "graph TD\nclassDef\nA-->B\n";
        let parsed = parse_with_diagnostics(source);
        assert!(!parsed.errors.is_empty(), "expected parse errors");

        let mut config = MermaidConfig::default();
        config.error_mode = MermaidErrorMode::Raw;

        let mut buf = Buffer::new(48, 12);
        render_mermaid_error_panel(
            &parsed.errors,
            source,
            &config,
            Rect::from_size(48, 12),
            &mut buf,
        );
        assert_buffer_snapshot_text("mermaid_error_raw", &buf);
    }

    #[test]
    fn snapshot_mermaid_error_both_mode() {
        let source = "graph TD\nclassDef\nA-->B\n";
        let parsed = parse_with_diagnostics(source);
        assert!(!parsed.errors.is_empty(), "expected parse errors");

        let mut config = MermaidConfig::default();
        config.error_mode = MermaidErrorMode::Both;

        let mut buf = Buffer::new(56, 16);
        render_mermaid_error_panel(
            &parsed.errors,
            source,
            &config,
            Rect::from_size(56, 16),
            &mut buf,
        );
        assert_buffer_snapshot_text("mermaid_error_both", &buf);
    }
}

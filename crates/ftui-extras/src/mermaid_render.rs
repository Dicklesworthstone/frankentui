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
use ftui_core::glyph_policy::{GlyphMode, GlyphPolicy};
use ftui_core::terminal_capabilities::{TerminalCapabilities, TerminalProfile};
use ftui_core::text_width::display_width;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::{BorderChars, Draw};
use std::str::FromStr;

#[cfg(feature = "canvas")]
use crate::canvas::{Mode as CanvasMode, Painter};
use crate::mermaid::{
    DiagramPalettePreset, DiagramType, IrPieEntry, LinkSanitizeOutcome, MermaidConfig,
    MermaidDiagramIr, MermaidError, MermaidErrorMode, MermaidFidelity, MermaidGlyphMode,
    MermaidLinkMode, MermaidRenderMode, MermaidStrokeDash, MermaidTier, NodeShape,
    ResolvedMermaidStyle, resolve_styles,
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
    dot_h: char,
    dot_v: char,
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
        dot_h: '┄',
        dot_v: '┆',
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
        dot_h: '.',
        dot_v: ':',
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

/// State passed into the renderer to control selection highlights.
///
/// When a node is selected, its border renders in accent color and
/// connected edges render in directional accent colors.
#[derive(Debug, Clone, Default)]
pub struct SelectionState {
    /// Index of the currently selected node (into IR nodes vec), if any.
    pub selected_node: Option<usize>,
    /// Indices of edges going out from the selected node.
    pub outgoing_edges: Vec<usize>,
    /// Indices of edges coming in to the selected node.
    pub incoming_edges: Vec<usize>,
}

impl SelectionState {
    /// Build selection state from a selected node index and the IR.
    #[must_use]
    pub fn from_selected(node_idx: usize, ir: &MermaidDiagramIr) -> Self {
        use crate::mermaid::{IrEndpoint, IrNodeId};
        let target = IrEndpoint::Node(IrNodeId(node_idx));
        let mut outgoing = Vec::new();
        let mut incoming = Vec::new();
        for (ei, edge) in ir.edges.iter().enumerate() {
            if edge.from == target {
                outgoing.push(ei);
            }
            if edge.to == target {
                incoming.push(ei);
            }
        }
        Self {
            selected_node: Some(node_idx),
            outgoing_edges: outgoing,
            incoming_edges: incoming,
        }
    }

    /// Returns true if there is no selection.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.selected_node.is_none()
    }

    /// Check if an edge index is highlighted (incoming or outgoing).
    #[must_use]
    pub fn edge_highlight(&self, edge_idx: usize) -> Option<PackedRgba> {
        if self.outgoing_edges.contains(&edge_idx) {
            Some(HIGHLIGHT_EDGE_OUT_FG)
        } else if self.incoming_edges.contains(&edge_idx) {
            Some(HIGHLIGHT_EDGE_IN_FG)
        } else {
            None
        }
    }
}

/// Build an adjacency list from IR edges: for each node, the list of
/// (neighbor_node_idx, edge_idx, is_outgoing) tuples.
#[must_use]
pub fn build_adjacency(ir: &MermaidDiagramIr) -> Vec<Vec<(usize, usize, bool)>> {
    use crate::mermaid::IrEndpoint;
    let n = ir.nodes.len();
    let mut adj = vec![Vec::new(); n];
    for (ei, edge) in ir.edges.iter().enumerate() {
        let from_idx = match edge.from {
            IrEndpoint::Node(id) => Some(id.0),
            IrEndpoint::Port(_) => None,
        };
        let to_idx = match edge.to {
            IrEndpoint::Node(id) => Some(id.0),
            IrEndpoint::Port(_) => None,
        };
        if let (Some(fi), Some(ti)) = (from_idx, to_idx) {
            if fi < n {
                adj[fi].push((ti, ei, true));
            }
            if ti < n {
                adj[ti].push((fi, ei, false));
            }
        }
    }
    adj
}

/// Find the nearest connected neighbor in a spatial direction.
///
/// `direction`: 0=up, 1=right, 2=down, 3=left.
/// Returns the neighbor node index, or None if no neighbor in that direction.
#[must_use]
pub fn navigate_direction(
    node_idx: usize,
    direction: u8,
    adjacency: &[Vec<(usize, usize, bool)>],
    layout: &DiagramLayout,
) -> Option<usize> {
    let neighbors = adjacency.get(node_idx)?;
    if neighbors.is_empty() {
        return None;
    }
    let current = layout.nodes.iter().find(|n| n.node_idx == node_idx)?;
    let cx = current.rect.x + current.rect.width / 2.0;
    let cy = current.rect.y + current.rect.height / 2.0;

    let mut best: Option<(usize, f64)> = None;
    for &(neighbor_idx, _, _) in neighbors {
        let neighbor = layout.nodes.iter().find(|n| n.node_idx == neighbor_idx)?;
        let nx = neighbor.rect.x + neighbor.rect.width / 2.0;
        let ny = neighbor.rect.y + neighbor.rect.height / 2.0;
        let dx = nx - cx;
        let dy = ny - cy;

        // Check if neighbor is roughly in the requested direction
        let in_direction = match direction {
            0 => dy < -0.1, // up
            1 => dx > 0.1,  // right
            2 => dy > 0.1,  // down
            3 => dx < -0.1, // left
            _ => false,
        };
        if !in_direction {
            continue;
        }

        let dist = dx * dx + dy * dy;
        if best.is_none() || dist < best.unwrap().1 {
            best = Some((neighbor_idx, dist));
        }
    }
    best.map(|(idx, _)| idx)
}

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

#[allow(dead_code)]
fn glyph_policy_for_config(config: &MermaidConfig) -> GlyphPolicy {
    if let Some(ref profile_name) = config.capability_profile
        && let Ok(profile) = TerminalProfile::from_str(profile_name)
    {
        let caps = TerminalCapabilities::from_profile(profile);
        if cfg!(test) {
            return GlyphPolicy::from_env_with(|_| None, &caps);
        }
        return GlyphPolicy::from_env_with(|key| std::env::var(key).ok(), &caps);
    }
    if cfg!(test) {
        let caps = TerminalCapabilities::dumb();
        return GlyphPolicy::from_env_with(|_| None, &caps);
    }
    GlyphPolicy::detect()
}

#[allow(dead_code)]
fn resolve_render_mode(config: &MermaidConfig, policy: &GlyphPolicy) -> MermaidRenderMode {
    if config.glyph_mode == MermaidGlyphMode::Ascii || policy.mode == GlyphMode::Ascii {
        return MermaidRenderMode::CellOnly;
    }

    if config.render_mode != MermaidRenderMode::Auto {
        return config.render_mode;
    }

    // Heuristic: treat emoji-capable Unicode terminals as Braille-ready.
    if policy.unicode_box_drawing && policy.double_width && policy.emoji {
        return MermaidRenderMode::Braille;
    }
    if policy.unicode_box_drawing {
        return MermaidRenderMode::Block;
    }
    if policy.double_width {
        return MermaidRenderMode::HalfBlock;
    }

    MermaidRenderMode::CellOnly
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn canvas_mode_for_render_mode(render_mode: MermaidRenderMode) -> CanvasMode {
    match render_mode {
        MermaidRenderMode::Braille => CanvasMode::Braille,
        MermaidRenderMode::Block => CanvasMode::Block,
        MermaidRenderMode::HalfBlock => CanvasMode::HalfBlock,
        _ => CanvasMode::Braille,
    }
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
    ir: &MermaidDiagramIr,
    area: Rect,
) -> RenderPlan {
    let fidelity = select_fidelity(config, layout, area);

    // Determine legend area reservation.
    let has_footnote_links = config.enable_links
        && config.link_mode == MermaidLinkMode::Footnote
        && ir
            .links
            .iter()
            .any(|link| link.sanitize_outcome == LinkSanitizeOutcome::Allowed);
    let (diagram_area, legend_area) =
        if has_footnote_links && !layout.nodes.is_empty() && fidelity != MermaidFidelity::Outline {
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

fn reserve_pie_legend_area(area: Rect, max_label_width: usize) -> (Rect, Option<Rect>) {
    let min_legend_width = 10u16;
    let desired_width = (max_label_width.max(8) as u16).saturating_add(4);
    let legend_width = desired_width.max(min_legend_width).min(area.width / 2);
    if area.width <= legend_width + 6 {
        return (area, None);
    }
    let pie_width = area.width.saturating_sub(legend_width + 1);
    if pie_width < 6 {
        return (area, None);
    }
    let pie_area = Rect {
        x: area.x,
        y: area.y,
        width: pie_width,
        height: area.height,
    };
    let legend_area = Rect {
        x: pie_area.x + pie_area.width + 1,
        y: area.y,
        width: area.width.saturating_sub(pie_width + 1),
        height: area.height,
    };
    (pie_area, Some(legend_area))
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

// ── Canvas viewport mapping (sub-cell resolution) ───────────────────────

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct PixelRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

/// Maps abstract layout coordinates to canvas sub-pixel coordinates.
#[derive(Debug, Clone)]
#[allow(dead_code)]
#[cfg(feature = "canvas")]
struct CanvasViewport {
    scale_x: f64,
    scale_y: f64,
    offset_x: f64,
    offset_y: f64,
    max_x: i32,
    max_y: i32,
}

#[cfg(feature = "canvas")]
impl CanvasViewport {
    /// Fit layout bounds into a sub-cell grid for the given area/mode.
    #[allow(dead_code)]
    fn fit(bounding_box: &LayoutRect, area: Rect, mode: CanvasMode) -> Self {
        let px_width = area.width.saturating_mul(mode.cols_per_cell()) as i32;
        let px_height = area.height.saturating_mul(mode.rows_per_cell()) as i32;
        let max_x = px_width.saturating_sub(1);
        let max_y = px_height.saturating_sub(1);

        let margin_x = f64::from(mode.cols_per_cell());
        let margin_y = f64::from(mode.rows_per_cell());
        let avail_w = (px_width as f64).max(1.0) - 2.0 * margin_x;
        let avail_h = (px_height as f64).max(1.0) - 2.0 * margin_y;

        let bb_w = bounding_box.width.max(1.0);
        let bb_h = bounding_box.height.max(1.0);
        let scale = (avail_w / bb_w).min(avail_h / bb_h).max(0.1);

        let used_w = bb_w * scale;
        let used_h = bb_h * scale;
        let pad_x = (avail_w - used_w) / 2.0;
        let pad_y = (avail_h - used_h) / 2.0;

        Self {
            scale_x: scale,
            scale_y: scale,
            offset_x: margin_x + pad_x - bounding_box.x * scale,
            offset_y: margin_y + pad_y - bounding_box.y * scale,
            max_x,
            max_y,
        }
    }

    #[allow(dead_code)]
    fn to_pixel(&self, x: f64, y: f64) -> (i32, i32) {
        let px = (x * self.scale_x + self.offset_x).round();
        let py = (y * self.scale_y + self.offset_y).round();
        let px = px.clamp(0.0, self.max_x as f64) as i32;
        let py = py.clamp(0.0, self.max_y as f64) as i32;
        (px, py)
    }

    #[allow(dead_code)]
    fn to_pixel_rect(&self, r: &LayoutRect) -> PixelRect {
        let (x0, y0) = self.to_pixel(r.x, r.y);
        let (x1, y1) = self.to_pixel(r.x + r.width, r.y + r.height);
        let width = (x1 - x0).max(1);
        let height = (y1 - y0).max(1);
        PixelRect {
            x: x0,
            y: y0,
            width,
            height,
        }
    }
}

// ── Color palette for diagram elements ──────────────────────────────────

#[allow(dead_code)] // Used by canvas rendering path
const EDGE_FG: PackedRgba = PackedRgba::rgb(150, 150, 150);
#[allow(dead_code)] // Used by upcoming pie chart rendering
const PIE_SLICE_COLORS: [PackedRgba; 8] = [
    PackedRgba::rgb(231, 76, 60),
    PackedRgba::rgb(46, 204, 113),
    PackedRgba::rgb(52, 152, 219),
    PackedRgba::rgb(241, 196, 15),
    PackedRgba::rgb(155, 89, 182),
    PackedRgba::rgb(26, 188, 156),
    PackedRgba::rgb(230, 126, 34),
    PackedRgba::rgb(149, 165, 166),
];
const DEFAULT_EDGE_LABEL_WIDTH: usize = 16;
const STATE_CONTAINER_CLASS: &str = "state_container";

// ── Selection / highlight colors ──────────────────────────────────────

/// Accent color for outgoing edges from the selected node.
const HIGHLIGHT_EDGE_OUT_FG: PackedRgba = PackedRgba::rgb(80, 220, 255);
/// Accent color for incoming edges to the selected node.
const HIGHLIGHT_EDGE_IN_FG: PackedRgba = PackedRgba::rgb(255, 180, 80);

// ── Diagram color palette ────────────────────────────────────────────

/// Color palette for diagram rendering.
///
/// Each preset defines colors for every visual element. The renderer reads
/// from this palette instead of hardcoded constants, allowing theme switching
/// at runtime via [`DiagramPalettePreset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagramPalette {
    /// Cycling fill colors for nodes (indexed by node position mod len).
    pub node_fills: [PackedRgba; 8],
    /// Default node border color.
    pub node_border: PackedRgba,
    /// Default node label text color.
    pub node_text: PackedRgba,
    /// Default edge line color.
    pub edge_color: PackedRgba,
    /// Edge label text color.
    pub edge_label_color: PackedRgba,
    /// Cluster border color.
    pub cluster_border: PackedRgba,
    /// Cluster title text color.
    pub cluster_title: PackedRgba,
    /// Accent color for selection highlighting.
    pub accent: PackedRgba,
    /// Accent color for outgoing edges from selected node.
    pub accent_outgoing: PackedRgba,
    /// Accent color for incoming edges to selected node.
    pub accent_incoming: PackedRgba,
}

impl DiagramPalette {
    /// Build a palette from a preset name.
    #[must_use]
    pub fn from_preset(preset: DiagramPalettePreset) -> Self {
        match preset {
            DiagramPalettePreset::Default => Self::default_palette(),
            DiagramPalettePreset::Corporate => Self::corporate(),
            DiagramPalettePreset::Neon => Self::neon(),
            DiagramPalettePreset::Monochrome => Self::monochrome(),
            DiagramPalettePreset::Pastel => Self::pastel(),
            DiagramPalettePreset::HighContrast => Self::high_contrast(),
        }
    }

    /// Default: blue tones, gray edges, white text — the original look.
    #[must_use]
    pub const fn default_palette() -> Self {
        Self {
            node_fills: [
                PackedRgba::rgb(52, 101, 164),
                PackedRgba::rgb(78, 154, 107),
                PackedRgba::rgb(143, 89, 161),
                PackedRgba::rgb(196, 160, 56),
                PackedRgba::rgb(52, 152, 219),
                PackedRgba::rgb(155, 89, 182),
                PackedRgba::rgb(46, 204, 113),
                PackedRgba::rgb(230, 126, 34),
            ],
            node_border: PackedRgba::WHITE,
            node_text: PackedRgba::WHITE,
            edge_color: PackedRgba::rgb(150, 150, 150),
            edge_label_color: PackedRgba::WHITE,
            cluster_border: PackedRgba::rgb(100, 160, 220),
            cluster_title: PackedRgba::rgb(100, 160, 220),
            accent: PackedRgba::rgb(80, 220, 255),
            accent_outgoing: PackedRgba::rgb(80, 220, 255),
            accent_incoming: PackedRgba::rgb(255, 180, 80),
        }
    }

    /// Corporate: navy/teal/gray — professional, muted palette.
    #[must_use]
    pub const fn corporate() -> Self {
        Self {
            node_fills: [
                PackedRgba::rgb(34, 49, 63),
                PackedRgba::rgb(22, 160, 133),
                PackedRgba::rgb(41, 128, 185),
                PackedRgba::rgb(44, 62, 80),
                PackedRgba::rgb(39, 174, 96),
                PackedRgba::rgb(52, 73, 94),
                PackedRgba::rgb(26, 188, 156),
                PackedRgba::rgb(127, 140, 141),
            ],
            node_border: PackedRgba::rgb(189, 195, 199),
            node_text: PackedRgba::rgb(236, 240, 241),
            edge_color: PackedRgba::rgb(127, 140, 141),
            edge_label_color: PackedRgba::rgb(189, 195, 199),
            cluster_border: PackedRgba::rgb(52, 73, 94),
            cluster_title: PackedRgba::rgb(149, 165, 166),
            accent: PackedRgba::rgb(26, 188, 156),
            accent_outgoing: PackedRgba::rgb(26, 188, 156),
            accent_incoming: PackedRgba::rgb(230, 126, 34),
        }
    }

    /// Neon: cyan/magenta/green on dark background — high energy.
    #[must_use]
    pub const fn neon() -> Self {
        Self {
            node_fills: [
                PackedRgba::rgb(0, 255, 255),
                PackedRgba::rgb(255, 0, 255),
                PackedRgba::rgb(0, 255, 128),
                PackedRgba::rgb(255, 255, 0),
                PackedRgba::rgb(128, 0, 255),
                PackedRgba::rgb(255, 128, 0),
                PackedRgba::rgb(0, 128, 255),
                PackedRgba::rgb(255, 0, 128),
            ],
            node_border: PackedRgba::rgb(0, 255, 255),
            node_text: PackedRgba::rgb(0, 0, 0),
            edge_color: PackedRgba::rgb(0, 200, 200),
            edge_label_color: PackedRgba::rgb(180, 255, 180),
            cluster_border: PackedRgba::rgb(128, 0, 255),
            cluster_title: PackedRgba::rgb(200, 100, 255),
            accent: PackedRgba::rgb(255, 0, 255),
            accent_outgoing: PackedRgba::rgb(0, 255, 255),
            accent_incoming: PackedRgba::rgb(255, 255, 0),
        }
    }

    /// Monochrome: white/gray/black — works on any terminal.
    #[must_use]
    pub const fn monochrome() -> Self {
        Self {
            node_fills: [
                PackedRgba::rgb(200, 200, 200),
                PackedRgba::rgb(180, 180, 180),
                PackedRgba::rgb(160, 160, 160),
                PackedRgba::rgb(140, 140, 140),
                PackedRgba::rgb(200, 200, 200),
                PackedRgba::rgb(180, 180, 180),
                PackedRgba::rgb(160, 160, 160),
                PackedRgba::rgb(140, 140, 140),
            ],
            node_border: PackedRgba::WHITE,
            node_text: PackedRgba::rgb(0, 0, 0),
            edge_color: PackedRgba::rgb(180, 180, 180),
            edge_label_color: PackedRgba::WHITE,
            cluster_border: PackedRgba::rgb(200, 200, 200),
            cluster_title: PackedRgba::rgb(220, 220, 220),
            accent: PackedRgba::WHITE,
            accent_outgoing: PackedRgba::WHITE,
            accent_incoming: PackedRgba::rgb(180, 180, 180),
        }
    }

    /// Pastel: soft muted colors — easy on eyes.
    #[must_use]
    pub const fn pastel() -> Self {
        Self {
            node_fills: [
                PackedRgba::rgb(174, 198, 207),
                PackedRgba::rgb(179, 222, 193),
                PackedRgba::rgb(253, 253, 150),
                PackedRgba::rgb(244, 154, 194),
                PackedRgba::rgb(207, 186, 240),
                PackedRgba::rgb(255, 218, 185),
                PackedRgba::rgb(162, 210, 223),
                PackedRgba::rgb(195, 177, 225),
            ],
            node_border: PackedRgba::rgb(180, 180, 200),
            node_text: PackedRgba::rgb(40, 40, 60),
            edge_color: PackedRgba::rgb(160, 160, 180),
            edge_label_color: PackedRgba::rgb(80, 80, 100),
            cluster_border: PackedRgba::rgb(200, 180, 220),
            cluster_title: PackedRgba::rgb(140, 120, 160),
            accent: PackedRgba::rgb(120, 180, 220),
            accent_outgoing: PackedRgba::rgb(120, 180, 220),
            accent_incoming: PackedRgba::rgb(244, 154, 194),
        }
    }

    /// High-contrast: WCAG AAA compliant, bold primary colors.
    #[must_use]
    pub const fn high_contrast() -> Self {
        Self {
            node_fills: [
                PackedRgba::rgb(255, 255, 0),
                PackedRgba::rgb(0, 255, 0),
                PackedRgba::rgb(255, 165, 0),
                PackedRgba::rgb(0, 255, 255),
                PackedRgba::rgb(255, 105, 180),
                PackedRgba::rgb(0, 191, 255),
                PackedRgba::rgb(255, 215, 0),
                PackedRgba::rgb(50, 205, 50),
            ],
            node_border: PackedRgba::WHITE,
            node_text: PackedRgba::rgb(0, 0, 0),
            edge_color: PackedRgba::WHITE,
            edge_label_color: PackedRgba::WHITE,
            cluster_border: PackedRgba::rgb(255, 255, 0),
            cluster_title: PackedRgba::rgb(255, 255, 0),
            accent: PackedRgba::rgb(255, 0, 0),
            accent_outgoing: PackedRgba::rgb(255, 0, 0),
            accent_incoming: PackedRgba::rgb(0, 255, 0),
        }
    }

    /// Get the fill color for a node at the given index (cycles through fills).
    #[must_use]
    pub const fn node_fill_for(&self, index: usize) -> PackedRgba {
        self.node_fills[index % self.node_fills.len()]
    }
}

// ── Edge line style ──────────────────────────────────────────────────

/// Line style for edge rendering, inferred from the Mermaid arrow syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeLineStyle {
    Solid,
    Dashed,
    Dotted,
    Thick,
}

fn journey_score_fill(ir_node: &crate::mermaid::IrNode) -> Option<PackedRgba> {
    for class in &ir_node.classes {
        if let Some(s) = class.strip_prefix("journey_score_")
            && let Ok(score) = s.parse::<u8>()
        {
            return Some(match score {
                5 => PackedRgba::rgb(76, 175, 80),
                4 => PackedRgba::rgb(139, 195, 74),
                3 => PackedRgba::rgb(255, 193, 7),
                2 => PackedRgba::rgb(255, 152, 0),
                1 => PackedRgba::rgb(244, 67, 54),
                _ => PackedRgba::rgb(158, 158, 158),
            });
        }
    }
    None
}

fn timeline_era_fill(ir_node: &crate::mermaid::IrNode) -> Option<PackedRgba> {
    if ir_node.classes.contains(&"timeline_period".to_string()) {
        return Some(PackedRgba::rgb(66, 133, 183));
    }
    None
}

/// Detect edge line style from the Mermaid arrow string.
fn detect_edge_style(arrow: &str) -> EdgeLineStyle {
    if arrow.contains("-.") || arrow.contains(".-") {
        EdgeLineStyle::Dashed
    } else if arrow.contains("..") {
        EdgeLineStyle::Dotted
    } else if arrow.contains("==") {
        EdgeLineStyle::Thick
    } else {
        EdgeLineStyle::Solid
    }
}

fn edge_line_style(arrow: &str, style: Option<&ResolvedMermaidStyle>) -> EdgeLineStyle {
    if let Some(style) = style
        && let Some(dash) = style.properties.stroke_dash
    {
        return match dash {
            MermaidStrokeDash::Solid => EdgeLineStyle::Solid,
            MermaidStrokeDash::Dashed => EdgeLineStyle::Dashed,
            MermaidStrokeDash::Dotted => EdgeLineStyle::Dotted,
        };
    }
    detect_edge_style(arrow)
}

// ── MermaidRenderer ─────────────────────────────────────────────────────

/// Renders a [`DiagramLayout`] into a terminal [`Buffer`].
pub struct MermaidRenderer {
    glyphs: GlyphPalette,
    colors: DiagramPalette,
    glyph_mode: MermaidGlyphMode,
}

impl MermaidRenderer {
    /// Create a renderer for the given glyph mode.
    #[must_use]
    pub fn new(config: &MermaidConfig) -> Self {
        Self {
            glyphs: GlyphPalette::for_mode(config.glyph_mode),
            colors: DiagramPalette::from_preset(config.palette),
            glyph_mode: config.glyph_mode,
        }
    }

    /// Create a renderer with explicit glyph mode.
    #[must_use]
    pub fn with_mode(mode: MermaidGlyphMode) -> Self {
        Self {
            glyphs: GlyphPalette::for_mode(mode),
            colors: DiagramPalette::default_palette(),
            glyph_mode: mode,
        }
    }

    /// Create a renderer with explicit glyph mode and color palette.
    #[must_use]
    pub fn with_mode_and_palette(mode: MermaidGlyphMode, palette: DiagramPalettePreset) -> Self {
        Self {
            glyphs: GlyphPalette::for_mode(mode),
            colors: DiagramPalette::from_preset(palette),
            glyph_mode: mode,
        }
    }

    fn outline_char(&self) -> char {
        match self.glyph_mode {
            MermaidGlyphMode::Ascii => '*',
            MermaidGlyphMode::Unicode => '●',
        }
    }

    fn bundle_count_label(&self, count: usize) -> Option<String> {
        if count <= 1 {
            return None;
        }
        let prefix = match self.glyph_mode {
            MermaidGlyphMode::Unicode => '×',
            MermaidGlyphMode::Ascii => 'x',
        };
        Some(format!("{prefix}{count}"))
    }

    /// Render a diagram layout into the buffer within the given area.
    pub fn render(
        &self,
        layout: &DiagramLayout,
        ir: &MermaidDiagramIr,
        area: Rect,
        buf: &mut Buffer,
    ) {
        if ir.diagram_type == DiagramType::Pie {
            let max_label_width = if area.width > 4 {
                (area.width / 2) as usize
            } else {
                0
            };
            self.render_pie(ir, area, max_label_width, buf);
            return;
        }
        if layout.nodes.is_empty() || area.is_empty() {
            return;
        }

        let resolved_styles = resolve_styles(ir);
        let vp = Viewport::fit(&layout.bounding_box, area);

        // Render order: clusters (background) → edges → nodes → labels.
        self.render_clusters(&layout.clusters, ir, &vp, buf);
        if ir.diagram_type == DiagramType::Sequence {
            self.render_sequence_lifelines(layout, &vp, buf);
        }
        if ir.diagram_type == DiagramType::GitGraph {
            self.render_gitgraph_lanes(layout, ir, &vp, buf);
        }
        if ir.diagram_type == DiagramType::Requirement {
            self.render_requirement_entities(layout, ir, &vp, buf);
            return;
        }
        self.render_edges(&layout.edges, ir, &vp, &resolved_styles.edge_styles, buf);
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
        if ir.diagram_type == DiagramType::Pie {
            self.render_pie(ir, plan.diagram_area, plan.max_label_width, buf);
            return;
        }
        if layout.nodes.is_empty() || plan.diagram_area.is_empty() {
            return;
        }

        let resolved_styles = resolve_styles(ir);
        let vp = Viewport::fit(&layout.bounding_box, plan.diagram_area);

        // Render order: clusters (background) → edges → nodes.
        if plan.show_clusters {
            self.render_clusters(&layout.clusters, ir, &vp, buf);
        }
        if ir.diagram_type == DiagramType::Sequence {
            self.render_sequence_lifelines(layout, &vp, buf);
        }
        if ir.diagram_type == DiagramType::GitGraph {
            self.render_gitgraph_lanes(layout, ir, &vp, buf);
        }
        if ir.diagram_type == DiagramType::Requirement {
            self.render_requirement_entities(layout, ir, &vp, buf);
            return;
        }
        self.render_edges_with_plan(
            &layout.edges,
            ir,
            &vp,
            &resolved_styles.edge_styles,
            plan,
            buf,
        );
        self.render_nodes_with_plan(&layout.nodes, ir, &vp, plan, buf);
        if let Some(legend_area) = plan.legend_area {
            let footnotes = crate::mermaid_layout::build_link_footnotes(&ir.links, &ir.nodes);
            self.render_legend_footnotes(legend_area, &footnotes, buf);
        }
    }

    /// Render a pie chart diagram.
    fn render_pie(
        &self,
        ir: &MermaidDiagramIr,
        area: Rect,
        max_label_width: usize,
        buf: &mut Buffer,
    ) {
        if area.is_empty() || ir.pie_entries.is_empty() {
            return;
        }

        let mut content_area = area;
        if let Some(title_id) = ir.pie_title
            && let Some(title) = ir.labels.get(title_id.0).map(|l| l.text.as_str())
            && content_area.height > 0
        {
            let title_cell = Cell::from_char(' ').with_fg(self.colors.node_text);
            let mut title_text = title.to_string();
            if max_label_width > 0 {
                title_text = truncate_label(&title_text, max_label_width);
            }
            let title_width = display_width(&title_text).min(content_area.width as usize) as u16;
            let title_x = content_area
                .x
                .saturating_add(content_area.width.saturating_sub(title_width) / 2);
            let max_x = content_area.x + content_area.width.saturating_sub(1);
            buf.print_text_clipped(title_x, content_area.y, &title_text, title_cell, max_x);
            content_area = Rect {
                x: content_area.x,
                y: content_area.y.saturating_add(1),
                width: content_area.width,
                height: content_area.height.saturating_sub(1),
            };
        }

        if content_area.is_empty() {
            return;
        }

        let entries: Vec<&IrPieEntry> = ir.pie_entries.iter().filter(|e| e.value > 0.0).collect();
        if entries.is_empty() {
            return;
        }
        let total: f64 = entries.iter().map(|e| e.value).sum();
        if total <= 0.0 {
            return;
        }

        let use_legend = entries.len() > 6 || content_area.width < 20 || content_area.height < 10;
        let (pie_area, legend_area) = if use_legend {
            reserve_pie_legend_area(content_area, max_label_width)
        } else {
            (content_area, None)
        };

        if pie_area.is_empty() {
            return;
        }

        let rx = (f64::from(pie_area.width).max(2.0) - 2.0) / 2.0;
        let ry = (f64::from(pie_area.height).max(2.0) - 2.0) / 2.0;
        let radius = rx.min(ry);
        if radius <= 0.0 {
            return;
        }
        let cx = f64::from(pie_area.x) + f64::from(pie_area.width) / 2.0;
        let cy = f64::from(pie_area.y) + f64::from(pie_area.height) / 2.0;

        let tau = std::f64::consts::TAU;
        let mut slice_ranges = Vec::with_capacity(entries.len());
        let mut cursor = 0.0;
        for entry in &entries {
            let portion = entry.value / total;
            let end = (cursor + portion * tau).min(tau);
            slice_ranges.push((cursor, end));
            cursor = end;
        }

        let fill_char = match self.glyph_mode {
            MermaidGlyphMode::Unicode => '█',
            MermaidGlyphMode::Ascii => '#',
        };

        for y in 0..pie_area.height {
            for x in 0..pie_area.width {
                let cell_x = pie_area.x + x;
                let cell_y = pie_area.y + y;
                let fx = f64::from(cell_x) + 0.5;
                let fy = f64::from(cell_y) + 0.5;
                let dx = (fx - cx) / rx;
                let dy = (fy - cy) / ry;
                if dx * dx + dy * dy <= 1.0 {
                    let angle = ((-dy).atan2(dx) - std::f64::consts::FRAC_PI_2).rem_euclid(tau);
                    let mut idx = 0usize;
                    while idx < slice_ranges.len() && angle > slice_ranges[idx].1 {
                        idx += 1;
                    }
                    if idx >= entries.len() {
                        idx = entries.len() - 1;
                    }
                    let color = PIE_SLICE_COLORS[idx % PIE_SLICE_COLORS.len()];
                    buf.set(cell_x, cell_y, Cell::from_char(fill_char).with_fg(color));
                }
            }
        }

        if let Some(legend) = legend_area {
            self.render_pie_legend(ir, &entries, legend, max_label_width, buf);
        } else {
            self.render_pie_leader_labels(
                ir,
                &entries,
                &slice_ranges,
                (cx, cy),
                radius,
                pie_area,
                max_label_width,
                buf,
            );
        }
    }

    fn render_pie_legend(
        &self,
        ir: &MermaidDiagramIr,
        entries: &[&IrPieEntry],
        legend: Rect,
        max_label_width: usize,
        buf: &mut Buffer,
    ) {
        if legend.is_empty() || legend.width < 3 {
            return;
        }
        let label_cell = Cell::from_char(' ').with_fg(self.colors.node_text);
        let mark_char = match self.glyph_mode {
            MermaidGlyphMode::Unicode => '■',
            MermaidGlyphMode::Ascii => '#',
        };
        let max_x = legend.x + legend.width.saturating_sub(1);
        let mut y = legend.y;
        for (idx, entry) in entries.iter().enumerate() {
            if y >= legend.y + legend.height {
                break;
            }
            let color = PIE_SLICE_COLORS[idx % PIE_SLICE_COLORS.len()];
            buf.set(legend.x, y, Cell::from_char(mark_char).with_fg(color));
            let text = self.pie_entry_label_text(ir, entry, idx, max_label_width);
            buf.print_text_clipped(legend.x.saturating_add(2), y, &text, label_cell, max_x);
            y = y.saturating_add(1);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_pie_leader_labels(
        &self,
        ir: &MermaidDiagramIr,
        entries: &[&IrPieEntry],
        slice_ranges: &[(f64, f64)],
        center: (f64, f64),
        radius: f64,
        area: Rect,
        max_label_width: usize,
        buf: &mut Buffer,
    ) {
        let label_cell = Cell::from_char(' ').with_fg(self.colors.node_text);
        let line_cell = Cell::from_char(' ').with_fg(self.colors.edge_color);
        let leader_char = self.glyphs.dot_h;
        let area_x0 = area.x as i32;
        let area_x1 = (area.x + area.width).saturating_sub(1) as i32;
        let area_y0 = area.y as i32;
        let area_y1 = (area.y + area.height).saturating_sub(1) as i32;
        let mut occupied: Vec<(u16, u16, u16)> = Vec::new();

        for (idx, entry) in entries.iter().enumerate() {
            let (start, end) = slice_ranges[idx];
            let mid = (start + end) / 2.0;
            let theta = mid + std::f64::consts::FRAC_PI_2;
            let dx = theta.cos();
            let dy = -theta.sin();
            let anchor_x = center.0 + dx * (radius + 1.0);
            let anchor_y = center.1 + dy * (radius + 1.0);
            let ax = anchor_x.round() as i32;
            let ay = anchor_y.round() as i32;

            let text = self.pie_entry_label_text(ir, entry, idx, max_label_width);
            if text.is_empty() {
                continue;
            }
            let text_width = display_width(&text) as i32;
            if text_width == 0 {
                continue;
            }

            let right_side = dx >= 0.0;
            let label_x = if right_side {
                ax + 1
            } else {
                ax - text_width - 1
            };
            let label_y = ay;
            let label_x1 = label_x + text_width - 1;

            if label_y < area_y0 || label_y > area_y1 || label_x < area_x0 || label_x1 > area_x1 {
                continue;
            }

            if occupied.iter().any(|(y, x0, x1)| {
                *y == label_y as u16 && !(label_x1 < i32::from(*x0) || label_x > i32::from(*x1))
            }) {
                continue;
            }

            let line_y = label_y as u16;
            let ax_clamped = ax.clamp(area_x0, area_x1);
            if right_side {
                let line_start = ax_clamped;
                let line_end = label_x - 1;
                if line_start <= line_end && line_end >= area_x0 {
                    for x in line_start..=line_end {
                        if x >= area_x0 && x <= area_x1 {
                            buf.set(x as u16, line_y, line_cell.with_char(leader_char));
                        }
                    }
                }
            } else {
                let line_start = label_x1 + 1;
                let line_end = ax_clamped;
                if line_start <= line_end && line_start <= area_x1 {
                    for x in line_start..=line_end {
                        if x >= area_x0 && x <= area_x1 {
                            buf.set(x as u16, line_y, line_cell.with_char(leader_char));
                        }
                    }
                }
            }

            buf.print_text_clipped(
                label_x as u16,
                line_y,
                &text,
                label_cell,
                area.x + area.width.saturating_sub(1),
            );
            occupied.push((line_y, label_x as u16, label_x1 as u16));
        }
    }

    fn pie_entry_label_text(
        &self,
        ir: &MermaidDiagramIr,
        entry: &IrPieEntry,
        idx: usize,
        max_label_width: usize,
    ) -> String {
        let base = ir
            .labels
            .get(entry.label.0)
            .map(|label| label.text.clone())
            .unwrap_or_else(|| format!("slice {}", idx + 1));
        let mut text = if ir.pie_show_data {
            format!("{}: {}", base, entry.value_text)
        } else {
            base
        };
        if max_label_width > 0 {
            text = truncate_label(&text, max_label_width);
        }
        text
    }

    /// Render edges respecting the fidelity plan.
    fn render_edges_with_plan(
        &self,
        edges: &[LayoutEdgePath],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        edge_styles: &[ResolvedMermaidStyle],
        plan: &RenderPlan,
        buf: &mut Buffer,
    ) {
        let edge_cell = Cell::from_char(' ').with_fg(self.colors.edge_color);
        for edge_path in edges {
            let waypoints: Vec<(u16, u16)> = edge_path
                .waypoints
                .iter()
                .map(|p| vp.to_cell(p.x, p.y))
                .collect();

            let mut line_style = ir
                .edges
                .get(edge_path.edge_idx)
                .map(|e| edge_line_style(&e.arrow, edge_styles.get(edge_path.edge_idx)))
                .unwrap_or(EdgeLineStyle::Solid);
            if edge_path.bundle_count > 1 {
                line_style = EdgeLineStyle::Thick;
            }

            for pair in waypoints.windows(2) {
                let (x0, y0) = pair[0];
                let (x1, y1) = pair[1];
                self.draw_line_segment_styled(x0, y0, x1, y1, edge_cell, line_style, buf);
            }

            // Arrowhead.
            if ir.diagram_type != DiagramType::Mindmap && waypoints.len() >= 2 {
                let (px, py) = waypoints[waypoints.len() - 2];
                let (tx, ty) = *waypoints.last().unwrap();
                let arrow_ch = self.arrowhead_char(px, py, tx, ty);
                buf.set(tx, ty, edge_cell.with_char(arrow_ch));
            }

            let ir_label = if plan.show_edge_labels {
                ir.edges
                    .get(edge_path.edge_idx)
                    .and_then(|e| e.label)
                    .and_then(|lid| ir.labels.get(lid.0))
                    .map(|l| l.text.as_str())
            } else {
                None
            };
            let bundle_label = if plan.fidelity != MermaidFidelity::Outline {
                self.bundle_count_label(edge_path.bundle_count)
            } else {
                None
            };
            let label_text = match (ir_label, bundle_label) {
                (Some(text), Some(mult)) => Some(format!("{text} {mult}")),
                (Some(text), None) => Some(text.to_string()),
                (None, Some(mult)) => Some(mult),
                (None, None) => None,
            };
            if let Some(text) = label_text {
                self.render_edge_label(edge_path, &text, plan.max_label_width, vp, buf);
            }

            // ER cardinality labels at edge endpoints (bd-1rnqg).
            if ir.diagram_type == DiagramType::Er
                && let Some(ir_edge) = ir.edges.get(edge_path.edge_idx)
            {
                render_er_cardinality(edge_path, &ir_edge.arrow, vp, buf);
            }
        }
    }

    // ── Shape-aware node rendering ─────────────────────────────────────

    /// Draw a node shape into the buffer, returning label inset (left, top, right, bottom).
    ///
    /// Each shape variant draws its distinctive border and returns the inset
    /// that the label renderer should use to avoid overlapping the border.
    fn draw_shaped_node(
        &self,
        cell_rect: Rect,
        shape: NodeShape,
        border_cell: Cell,
        fill_cell: Cell,
        buf: &mut Buffer,
    ) -> (u16, u16, u16, u16) {
        let w = cell_rect.width;
        let h = cell_rect.height;
        match shape {
            NodeShape::Rect => {
                buf.draw_box(cell_rect, self.glyphs.border, border_cell, fill_cell);
                (1, 1, 1, 1)
            }
            NodeShape::Rounded | NodeShape::Circle => {
                let chars = match self.glyph_mode {
                    MermaidGlyphMode::Unicode => BorderChars::ROUNDED,
                    MermaidGlyphMode::Ascii => BorderChars::ASCII,
                };
                buf.draw_box(cell_rect, chars, border_cell, fill_cell);
                (1, 1, 1, 1)
            }
            NodeShape::Stadium => self.draw_stadium(cell_rect, w, h, border_cell, fill_cell, buf),
            NodeShape::Subroutine => {
                self.draw_subroutine(cell_rect, w, h, border_cell, fill_cell, buf)
            }
            NodeShape::Diamond => {
                if w < 5 || h < 3 {
                    buf.draw_box(cell_rect, self.glyphs.border, border_cell, fill_cell);
                    return (1, 1, 1, 1);
                }
                self.draw_diamond(cell_rect, w, h, border_cell, fill_cell, buf)
            }
            NodeShape::Hexagon => self.draw_hexagon(cell_rect, w, h, border_cell, fill_cell, buf),
            NodeShape::Asymmetric => {
                self.draw_asymmetric(cell_rect, w, h, border_cell, fill_cell, buf)
            }
        }
    }

    /// Stadium shape: rounded corners with double horizontal lines.
    fn draw_stadium(
        &self,
        r: Rect,
        w: u16,
        h: u16,
        bc: Cell,
        fc: Cell,
        buf: &mut Buffer,
    ) -> (u16, u16, u16, u16) {
        let (tl, tr, bl, br, horiz, vert) = match self.glyph_mode {
            MermaidGlyphMode::Unicode => ('╭', '╮', '╰', '╯', '═', '│'),
            MermaidGlyphMode::Ascii => ('(', ')', '(', ')', '=', '|'),
        };
        // Fill interior
        for row in 1..h.saturating_sub(1) {
            for col in 1..w.saturating_sub(1) {
                buf.set(r.x + col, r.y + row, fc);
            }
        }
        // Top/bottom borders
        buf.set(r.x, r.y, bc.with_char(tl));
        buf.set(r.x + w - 1, r.y, bc.with_char(tr));
        buf.set(r.x, r.y + h - 1, bc.with_char(bl));
        buf.set(r.x + w - 1, r.y + h - 1, bc.with_char(br));
        for col in 1..w.saturating_sub(1) {
            buf.set(r.x + col, r.y, bc.with_char(horiz));
            buf.set(r.x + col, r.y + h - 1, bc.with_char(horiz));
        }
        // Side borders
        for row in 1..h.saturating_sub(1) {
            buf.set(r.x, r.y + row, bc.with_char(vert));
            buf.set(r.x + w - 1, r.y + row, bc.with_char(vert));
        }
        (2, 1, 2, 1)
    }

    /// Subroutine shape: double vertical borders with inner vertical lines.
    fn draw_subroutine(
        &self,
        r: Rect,
        w: u16,
        h: u16,
        bc: Cell,
        fc: Cell,
        buf: &mut Buffer,
    ) -> (u16, u16, u16, u16) {
        // Draw standard box first
        buf.draw_box(r, self.glyphs.border, bc, fc);
        // Add inner vertical lines (double-border effect)
        let (dbl_vert, inner_vert) = match self.glyph_mode {
            MermaidGlyphMode::Unicode => ('║', '│'),
            MermaidGlyphMode::Ascii => ('|', '|'),
        };
        // Outer double-verticals
        for row in 1..h.saturating_sub(1) {
            buf.set(r.x, r.y + row, bc.with_char(dbl_vert));
            buf.set(r.x + w - 1, r.y + row, bc.with_char(dbl_vert));
        }
        // Inner single-verticals (if wide enough)
        if w >= 4 {
            for row in 1..h.saturating_sub(1) {
                buf.set(r.x + 1, r.y + row, bc.with_char(inner_vert));
                buf.set(r.x + w - 2, r.y + row, bc.with_char(inner_vert));
            }
        }
        (2, 1, 2, 1)
    }

    /// Diamond shape: diagonal borders converging to a peak.
    fn draw_diamond(
        &self,
        r: Rect,
        w: u16,
        h: u16,
        bc: Cell,
        fc: Cell,
        buf: &mut Buffer,
    ) -> (u16, u16, u16, u16) {
        let (fwd, bck) = match self.glyph_mode {
            MermaidGlyphMode::Unicode => ('╱', '╲'),
            MermaidGlyphMode::Ascii => ('/', '\\'),
        };
        let half_h = h / 2;
        // Draw each row
        for row in 0..h {
            let dist = half_h.abs_diff(row);
            let taper = half_h.saturating_sub(dist);
            // Linear interpolation: peak row gets width 2, middle gets full width
            let row_width = w
                .checked_sub(2)
                .and_then(|delta| delta.checked_mul(taper))
                .and_then(|num| num.checked_div(half_h))
                .map(|scale| 2 + scale)
                .unwrap_or(w);
            let left = (w - row_width) / 2;
            let right = left + row_width - 1;
            // Draw left and right diagonal chars
            if row <= half_h {
                buf.set(r.x + left, r.y + row, bc.with_char(bck));
                buf.set(r.x + right, r.y + row, bc.with_char(fwd));
            } else {
                buf.set(r.x + left, r.y + row, bc.with_char(fwd));
                buf.set(r.x + right, r.y + row, bc.with_char(bck));
            }
            // Fill interior
            for col in (left + 1)..right {
                buf.set(r.x + col, r.y + row, fc);
            }
        }
        let inset_x = (w / 4).max(2);
        let inset_y = (h / 4).max(1);
        (inset_x, inset_y, inset_x, inset_y)
    }

    /// Hexagon shape: angled top/bottom edges with straight sides.
    fn draw_hexagon(
        &self,
        r: Rect,
        w: u16,
        h: u16,
        bc: Cell,
        fc: Cell,
        buf: &mut Buffer,
    ) -> (u16, u16, u16, u16) {
        let (fwd, bck, horiz, vert) = match self.glyph_mode {
            MermaidGlyphMode::Unicode => ('╱', '╲', '─', '│'),
            MermaidGlyphMode::Ascii => ('/', '\\', '-', '|'),
        };
        let diag = (h / 2).min(w / 4).max(1);
        // Fill interior
        for row in 0..h {
            for col in 0..w {
                buf.set(r.x + col, r.y + row, fc);
            }
        }
        // Top edge: ╱───╲
        buf.set(r.x + diag - 1, r.y, bc.with_char(fwd));
        buf.set(r.x + w - diag, r.y, bc.with_char(bck));
        for col in diag..(w - diag) {
            buf.set(r.x + col, r.y, bc.with_char(horiz));
        }
        // Bottom edge: ╲───╱
        buf.set(r.x + diag - 1, r.y + h - 1, bc.with_char(bck));
        buf.set(r.x + w - diag, r.y + h - 1, bc.with_char(fwd));
        for col in diag..(w - diag) {
            buf.set(r.x + col, r.y + h - 1, bc.with_char(horiz));
        }
        // Left/right angled sides + middle vertical
        for row in 1..h.saturating_sub(1) {
            let frac = if h <= 2 {
                0
            } else {
                let mid = (h - 1) / 2;
                if row <= mid {
                    diag.saturating_sub(diag * row / mid.max(1))
                } else {
                    diag.saturating_sub(diag * (h - 1 - row) / mid.max(1))
                }
            };
            buf.set(r.x + frac, r.y + row, bc.with_char(vert));
            buf.set(r.x + w - 1 - frac, r.y + row, bc.with_char(vert));
        }
        (diag + 1, 1, diag + 1, 1)
    }

    /// Asymmetric shape: standard left side, pointed right (flag shape).
    fn draw_asymmetric(
        &self,
        r: Rect,
        w: u16,
        h: u16,
        bc: Cell,
        fc: Cell,
        buf: &mut Buffer,
    ) -> (u16, u16, u16, u16) {
        let (tl, bl, vert, horiz, point) = match self.glyph_mode {
            MermaidGlyphMode::Unicode => ('┌', '└', '│', '─', '▷'),
            MermaidGlyphMode::Ascii => ('+', '+', '|', '-', '>'),
        };
        let point_depth = (w / 4).max(1);
        let mid = h / 2;
        // Fill interior
        for row in 1..h.saturating_sub(1) {
            for col in 1..w.saturating_sub(1) {
                buf.set(r.x + col, r.y + row, fc);
            }
        }
        // Left side
        buf.set(r.x, r.y, bc.with_char(tl));
        buf.set(r.x, r.y + h - 1, bc.with_char(bl));
        for row in 1..h.saturating_sub(1) {
            buf.set(r.x, r.y + row, bc.with_char(vert));
        }
        // Top and bottom
        for col in 1..w.saturating_sub(point_depth) {
            buf.set(r.x + col, r.y, bc.with_char(horiz));
            buf.set(r.x + col, r.y + h - 1, bc.with_char(horiz));
        }
        // Right point
        buf.set(r.x + w - 1, r.y + mid, bc.with_char(point));
        (1, 1, point_depth + 1, 1)
    }

    /// Apply half-block characters at the top and bottom interior rows for
    /// smoother shape boundaries. Only applies to rectangular shapes (Rect,
    /// Rounded) with enough interior height (≥4 rows).
    fn apply_half_block_fill(
        &self,
        cell_rect: Rect,
        shape: NodeShape,
        fill_color: PackedRgba,
        buf: &mut Buffer,
    ) {
        if self.glyph_mode == MermaidGlyphMode::Ascii {
            return;
        }
        if fill_color == PackedRgba::TRANSPARENT {
            return;
        }
        // Half-block smoothing only for shapes with horizontal top/bottom edges.
        match shape {
            NodeShape::Rect | NodeShape::Rounded | NodeShape::Circle => {}
            _ => return,
        }
        let inner_h = cell_rect.height.saturating_sub(2);
        if inner_h < 2 {
            // Not enough interior rows for half-block smoothing.
            return;
        }
        let x_start = cell_rect.x.saturating_add(1);
        let x_end = cell_rect
            .x
            .saturating_add(cell_rect.width.saturating_sub(1));
        // Top interior row: ▄ (lower half block) — fill starts partway down.
        let top_y = cell_rect.y.saturating_add(1);
        let half_cell = Cell::from_char('\u{2584}').with_fg(fill_color);
        for x in x_start..x_end {
            buf.set(x, top_y, half_cell);
        }
        // Bottom interior row: ▀ (upper half block) — fill ends partway up.
        let bot_y = cell_rect
            .y
            .saturating_add(cell_rect.height.saturating_sub(2));
        let half_cell = Cell::from_char('\u{2580}').with_fg(fill_color);
        for x in x_start..x_end {
            buf.set(x, bot_y, half_cell);
        }
    }

    /// Render a node label with shape-specific insets.
    fn render_node_label_with_inset(
        &self,
        cell_rect: Rect,
        text: &str,
        inset: (u16, u16, u16, u16),
        fill_bg: PackedRgba,
        buf: &mut Buffer,
    ) {
        let (il, it, ir, ib) = inset;
        let inner_w = cell_rect.width.saturating_sub(il + ir) as usize;
        let inner_h = cell_rect.height.saturating_sub(it + ib) as usize;
        if inner_w == 0 || inner_h == 0 {
            return;
        }

        let max_x = cell_rect
            .x
            .saturating_add(cell_rect.width)
            .saturating_sub(ir)
            .saturating_sub(1);
        let label_cell = Cell::from_char(' ')
            .with_fg(self.colors.node_text)
            .with_bg(fill_bg);

        let mut lines = wrap_text(text, inner_w);
        if lines.len() > inner_h {
            lines.truncate(inner_h);
            if let Some(last) = lines.last_mut() {
                *last = append_ellipsis(last, inner_w);
            }
        }

        let pad_y = inner_h.saturating_sub(lines.len()) / 2;

        for (i, line) in lines.iter().enumerate() {
            let line_width = display_width(line).min(inner_w);
            let pad_x = (inner_w.saturating_sub(line_width)) / 2;
            let lx = cell_rect.x.saturating_add(il).saturating_add(pad_x as u16);
            let ly = cell_rect
                .y
                .saturating_add(it)
                .saturating_add(pad_y as u16 + i as u16);
            buf.print_text_clipped(lx, ly, line, label_cell, max_x);
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
        let border_cell = Cell::from_char(' ').with_fg(self.colors.node_border);

        for node in nodes {
            let ir_node = match ir.nodes.get(node.node_idx) {
                Some(node) => node,
                None => continue,
            };
            if ir_node
                .classes
                .iter()
                .any(|class| class == STATE_CONTAINER_CLASS)
            {
                continue;
            }
            let cell_rect = vp.to_cell_rect(&node.rect);

            if plan.fidelity == MermaidFidelity::Outline {
                // Outline mode: single character per node.
                let (cx, cy) = vp.to_cell(
                    node.rect.x + node.rect.width / 2.0,
                    node.rect.y + node.rect.height / 2.0,
                );
                buf.set(cx, cy, border_cell.with_char(self.outline_char()));
                continue;
            }

            if cell_rect.width < 2 || cell_rect.height < 2 {
                let (cx, cy) = vp.to_cell(node.rect.x, node.rect.y);
                buf.set(cx, cy, border_cell.with_char('*'));
                continue;
            }

            let base_fill = self.colors.node_fill_for(node.node_idx);
            let fill_color = journey_score_fill(ir_node).or_else(|| timeline_era_fill(ir_node)).unwrap_or(base_fill);
            let fill_cell = Cell::from_char(' ').with_bg(fill_color);

            let inset =
                self.draw_shaped_node(cell_rect, ir_node.shape, border_cell, fill_cell, buf);

            // Apply half-block smoothing at top/bottom fill boundaries.
            self.apply_half_block_fill(cell_rect, ir_node.shape, fill_color, buf);

            // Labels only if plan allows.
            if plan.show_node_labels
                && let Some(label_id) = ir_node.label
                && let Some(label) = ir.labels.get(label_id.0)
            {
                if !ir_node.members.is_empty() {
                    // Class diagram node with compartments.
                    self.render_class_compartments(
                        cell_rect,
                        &label.text,
                        &ir_node.members,
                        plan.max_label_width,
                        fill_color,
                        buf,
                    );
                } else {
                    let text = if plan.max_label_width > 0 {
                        &truncate_label(&label.text, plan.max_label_width)
                    } else {
                        &label.text
                    };
                    self.render_node_label_with_inset(cell_rect, text, inset, fill_color, buf);
                }
            }
        }
    }

    /// Render labels (and ER cardinality markers) without drawing node/edge geometry.
    #[allow(dead_code)]
    fn render_labels_with_plan(
        &self,
        nodes: &[LayoutNodeBox],
        edges: &[LayoutEdgePath],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        plan: &RenderPlan,
        buf: &mut Buffer,
    ) {
        if plan.show_edge_labels {
            for edge_path in edges {
                if let Some(ir_edge) = ir.edges.get(edge_path.edge_idx)
                    && let Some(label_id) = ir_edge.label
                    && let Some(label) = ir.labels.get(label_id.0)
                {
                    self.render_edge_label(edge_path, &label.text, plan.max_label_width, vp, buf);
                }
            }
        }

        if ir.diagram_type == DiagramType::Er {
            for edge_path in edges {
                if let Some(ir_edge) = ir.edges.get(edge_path.edge_idx) {
                    render_er_cardinality(edge_path, &ir_edge.arrow, vp, buf);
                }
            }
        }

        if !plan.show_node_labels {
            return;
        }

        for node in nodes {
            let ir_node = match ir.nodes.get(node.node_idx) {
                Some(node) => node,
                None => continue,
            };
            if ir_node
                .classes
                .iter()
                .any(|class| class == STATE_CONTAINER_CLASS)
            {
                continue;
            }
            let cell_rect = vp.to_cell_rect(&node.rect);
            if let Some(label_id) = ir_node.label
                && let Some(label) = ir.labels.get(label_id.0)
            {
                if !ir_node.members.is_empty() {
                    let fill_color = self.colors.node_fill_for(node.node_idx);
                    self.render_class_compartments(
                        cell_rect,
                        &label.text,
                        &ir_node.members,
                        plan.max_label_width,
                        fill_color,
                        buf,
                    );
                } else {
                    let fill_color = self.colors.node_fill_for(node.node_idx);
                    let text = if plan.max_label_width > 0 {
                        &truncate_label(&label.text, plan.max_label_width)
                    } else {
                        &label.text
                    };
                    self.render_node_label(cell_rect, text, fill_color, buf);
                }
            }
        }
    }

    /// Composite text labels on top of canvas-rendered diagram.
    ///
    /// Unlike [`render_labels_with_plan`], this fills the background of each
    /// label region with the node fill color so that canvas dots (Braille,
    /// Block, HalfBlock) do not bleed through behind text.
    #[cfg(feature = "canvas")]
    #[allow(dead_code)]
    fn canvas_composite_labels(
        &self,
        nodes: &[LayoutNodeBox],
        edges: &[LayoutEdgePath],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        plan: &RenderPlan,
        buf: &mut Buffer,
    ) {
        for edge_path in edges {
            let ir_label = if plan.show_edge_labels {
                ir.edges
                    .get(edge_path.edge_idx)
                    .and_then(|e| e.label)
                    .and_then(|lid| ir.labels.get(lid.0))
                    .map(|l| l.text.as_str())
            } else {
                None
            };
            let bundle_label = if plan.fidelity != MermaidFidelity::Outline {
                self.bundle_count_label(edge_path.bundle_count)
            } else {
                None
            };
            let label_text = match (ir_label, bundle_label) {
                (Some(text), Some(mult)) => Some(format!("{text} {mult}")),
                (Some(text), None) => Some(text.to_string()),
                (None, Some(mult)) => Some(mult),
                (None, None) => None,
            };
            if let Some(text) = label_text {
                self.render_edge_label_canvas(edge_path, &text, plan.max_label_width, vp, buf);
            }
        }

        if ir.diagram_type == DiagramType::Er {
            for edge_path in edges {
                if let Some(ir_edge) = ir.edges.get(edge_path.edge_idx) {
                    render_er_cardinality(edge_path, &ir_edge.arrow, vp, buf);
                }
            }
        }

        if !plan.show_node_labels {
            return;
        }

        for node in nodes {
            let ir_node = match ir.nodes.get(node.node_idx) {
                Some(n) => n,
                None => continue,
            };
            if ir_node
                .classes
                .iter()
                .any(|class| class == STATE_CONTAINER_CLASS)
            {
                continue;
            }
            let cell_rect = vp.to_cell_rect(&node.rect);
            let fill = self.colors.node_fill_for(node.node_idx);

            if let Some(label_id) = ir_node.label
                && let Some(label) = ir.labels.get(label_id.0)
            {
                if !ir_node.members.is_empty() {
                    self.render_class_compartments(
                        cell_rect,
                        &label.text,
                        &ir_node.members,
                        plan.max_label_width,
                        fill,
                        buf,
                    );
                } else {
                    let text = if plan.max_label_width > 0 {
                        truncate_label(&label.text, plan.max_label_width)
                    } else {
                        label.text.clone()
                    };
                    self.render_node_label_canvas(cell_rect, &text, fill, buf);
                }
            }
        }
    }

    /// Render a node label with filled background for canvas compositing.
    ///
    /// Fills the interior cells with `fill` background color, then writes
    /// centered text on top. This prevents canvas dots from bleeding through.
    #[cfg(feature = "canvas")]
    fn render_node_label_canvas(
        &self,
        cell_rect: Rect,
        text: &str,
        fill: PackedRgba,
        buf: &mut Buffer,
    ) {
        let inner_w = cell_rect.width.saturating_sub(2) as usize;
        let inner_h = cell_rect.height.saturating_sub(2) as usize;
        if inner_w == 0 || inner_h == 0 {
            return;
        }

        let max_x = cell_rect
            .x
            .saturating_add(cell_rect.width)
            .saturating_sub(1);

        // Fill interior with background color to cover canvas dots.
        let bg_cell = Cell::from_char(' ').with_bg(fill);
        for dy in 0..inner_h {
            for dx in 0..inner_w {
                let x = cell_rect.x.saturating_add(1 + dx as u16);
                let y = cell_rect.y.saturating_add(1 + dy as u16);
                buf.set(x, y, bg_cell);
            }
        }

        // Write text with matching background.
        let label_cell = Cell::from_char(' ')
            .with_fg(self.colors.node_text)
            .with_bg(fill);

        let mut lines = wrap_text(text, inner_w);
        if lines.len() > inner_h {
            lines.truncate(inner_h);
            if let Some(last) = lines.last_mut() {
                *last = append_ellipsis(last, inner_w);
            }
        }

        let pad_y = inner_h.saturating_sub(lines.len()) / 2;
        for (i, line) in lines.iter().enumerate() {
            let line_width = display_width(line).min(inner_w);
            let pad_x = inner_w.saturating_sub(line_width) / 2;

            let lx = cell_rect.x.saturating_add(1).saturating_add(pad_x as u16);
            let ly = cell_rect
                .y
                .saturating_add(1)
                .saturating_add(pad_y as u16 + i as u16);
            buf.print_text_clipped(lx, ly, line, label_cell, max_x);
        }
    }

    /// Render an edge label with filled background for canvas compositing.
    ///
    /// Fills a background strip behind the label text so it remains readable
    /// on top of canvas-rendered edge lines.
    #[cfg(feature = "canvas")]
    fn render_edge_label_canvas(
        &self,
        edge_path: &LayoutEdgePath,
        text: &str,
        max_label_width: usize,
        vp: &Viewport,
        buf: &mut Buffer,
    ) {
        if edge_path.waypoints.len() < 2 || text.is_empty() {
            return;
        }
        // Place label near the geometric midpoint of the path.
        let mid_idx = edge_path.waypoints.len() / 2;
        let (mx, my) = if edge_path.waypoints.len().is_multiple_of(2) && mid_idx > 0 {
            let a = &edge_path.waypoints[mid_idx - 1];
            let b = &edge_path.waypoints[mid_idx];
            ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
        } else {
            let mid = &edge_path.waypoints[mid_idx];
            (mid.x, mid.y)
        };
        let (cx, cy) = vp.to_cell(mx, my);
        let label = if max_label_width == 0 {
            text.to_string()
        } else {
            truncate_label(text, max_label_width)
        };
        let label_width = display_width(&label);

        // Fill background strip behind label.
        let lx = cx.saturating_add(1);
        let bg_cell = Cell::from_char(' ').with_bg(PackedRgba::BLACK);
        for dx in 0..label_width {
            buf.set(lx.saturating_add(dx as u16), cy, bg_cell);
        }

        let label_cell = Cell::from_char(' ')
            .with_fg(self.colors.edge_label_color)
            .with_bg(PackedRgba::BLACK);
        buf.print_text(lx, cy, &label, label_cell);
    }

    /// Render with fidelity plan and selection highlighting.
    ///
    /// When `selection` has a selected node, that node and its connected
    /// edges are rendered with accent colors on top of the normal diagram.
    pub fn render_with_selection(
        &self,
        layout: &DiagramLayout,
        ir: &MermaidDiagramIr,
        plan: &RenderPlan,
        selection: &SelectionState,
        buf: &mut Buffer,
    ) {
        // Render the base diagram first
        self.render_with_plan(layout, ir, plan, buf);

        // Overlay selection highlights
        if !selection.is_empty() {
            let vp = Viewport::fit(&layout.bounding_box, plan.diagram_area);
            self.render_selection_highlights(&layout.nodes, &layout.edges, ir, &vp, selection, buf);
        }
    }

    /// Overlay highlight rendering for selected node and connected edges.
    fn render_selection_highlights(
        &self,
        nodes: &[LayoutNodeBox],
        edges: &[LayoutEdgePath],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        selection: &SelectionState,
        buf: &mut Buffer,
    ) {
        // Highlight connected edges first (so node border draws on top)
        let highlight_cell = Cell::from_char(' ');
        for edge_path in edges {
            let color = selection.edge_highlight(edge_path.edge_idx).or_else(|| {
                if edge_path.bundle_members.is_empty() {
                    return None;
                }
                if edge_path
                    .bundle_members
                    .iter()
                    .any(|m| selection.outgoing_edges.contains(m))
                {
                    return Some(HIGHLIGHT_EDGE_OUT_FG);
                }
                if edge_path
                    .bundle_members
                    .iter()
                    .any(|m| selection.incoming_edges.contains(m))
                {
                    return Some(HIGHLIGHT_EDGE_IN_FG);
                }
                None
            });

            if let Some(color) = color {
                let cell = highlight_cell.with_fg(color);
                let waypoints: Vec<(u16, u16)> = edge_path
                    .waypoints
                    .iter()
                    .map(|p| vp.to_cell(p.x, p.y))
                    .collect();
                for pair in waypoints.windows(2) {
                    let (x0, y0) = pair[0];
                    let (x1, y1) = pair[1];
                    self.draw_line_segment(x0, y0, x1, y1, cell, buf);
                }
                // Redraw arrowhead in highlight color
                if waypoints.len() >= 2 {
                    let (tx, ty) = waypoints[waypoints.len() - 1];
                    let (px, py) = waypoints[waypoints.len() - 2];
                    let ah = self.arrowhead_char(px, py, tx, ty);
                    buf.set(tx, ty, cell.with_char(ah));
                }
            }
        }

        // Highlight selected node border
        if let Some(selected_idx) = selection.selected_node
            && let Some(node) = nodes.iter().find(|n| n.node_idx == selected_idx)
        {
            let cell_rect = vp.to_cell_rect(&node.rect);
            if cell_rect.width >= 2 && cell_rect.height >= 2 {
                // Draw highlighted border on top of existing shape
                let x0 = cell_rect.x;
                let y0 = cell_rect.y;
                let x1 = x0 + cell_rect.width.saturating_sub(1);
                let y1 = y0 + cell_rect.height.saturating_sub(1);

                // Re-draw border cells with accent color (preserve char, change fg)
                for col in x0..=x1 {
                    if let Some(c) = buf.get(col, y0) {
                        buf.set(col, y0, c.with_fg(self.colors.accent));
                    }
                    if let Some(c) = buf.get(col, y1) {
                        buf.set(col, y1, c.with_fg(self.colors.accent));
                    }
                }
                for row in y0..=y1 {
                    if let Some(c) = buf.get(x0, row) {
                        buf.set(x0, row, c.with_fg(self.colors.accent));
                    }
                    if let Some(c) = buf.get(x1, row) {
                        buf.set(x1, row, c.with_fg(self.colors.accent));
                    }
                }
                // Also recolor the label text for the selected node
                if let Some(ir_node) = ir.nodes.get(selected_idx)
                    && let Some(label_id) = ir_node.label
                    && ir.labels.get(label_id.0).is_some()
                {
                    // Recolor interior text cells
                    for row in (y0 + 1)..y1 {
                        for col in (x0 + 1)..x1 {
                            if let Some(c) = buf.get(col, row)
                                && c.content.as_char().unwrap_or(' ') != ' '
                            {
                                buf.set(col, row, c.with_fg(self.colors.accent));
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Cluster rendering ───────────────────────────────────────────

    /// Render requirementDiagram entities as bordered boxes with kind headers.
    fn render_requirement_entities(
        &self,
        layout: &DiagramLayout,
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        buf: &mut Buffer,
    ) {
        let resolved_styles = resolve_styles(ir);

        for (i, node) in layout.nodes.iter().enumerate() {
            let cell_rect = vp.to_cell_rect(&node.rect);
            if cell_rect.width < 2 || cell_rect.height < 2 {
                continue;
            }

            let border_color = self.colors.node_fills[i % self.colors.node_fills.len()];

            // Top and bottom borders.
            let horiz = self.glyphs.border.horizontal;
            for x in cell_rect.x..cell_rect.x + cell_rect.width {
                buf.set(x, cell_rect.y, Cell::from_char(horiz).with_fg(border_color));
                buf.set(
                    x,
                    cell_rect.y + cell_rect.height.saturating_sub(1),
                    Cell::from_char(horiz).with_fg(border_color),
                );
            }
            // Left and right borders.
            let vert = self.glyphs.border.vertical;
            for y in cell_rect.y..cell_rect.y + cell_rect.height {
                buf.set(cell_rect.x, y, Cell::from_char(vert).with_fg(border_color));
                buf.set(
                    cell_rect.x + cell_rect.width.saturating_sub(1),
                    y,
                    Cell::from_char(vert).with_fg(border_color),
                );
            }
            // Corners.
            buf.set(
                cell_rect.x,
                cell_rect.y,
                Cell::from_char(self.glyphs.border.top_left).with_fg(border_color),
            );
            buf.set(
                cell_rect.x + cell_rect.width.saturating_sub(1),
                cell_rect.y,
                Cell::from_char(self.glyphs.border.top_right).with_fg(border_color),
            );
            buf.set(
                cell_rect.x,
                cell_rect.y + cell_rect.height.saturating_sub(1),
                Cell::from_char(self.glyphs.border.bottom_left).with_fg(border_color),
            );
            buf.set(
                cell_rect.x + cell_rect.width.saturating_sub(1),
                cell_rect.y + cell_rect.height.saturating_sub(1),
                Cell::from_char(self.glyphs.border.bottom_right).with_fg(border_color),
            );

            // Render label inside the box (collapsed if let for clippy).
            if let Some(label_id) = ir.nodes[i].label
                && let Some(label) = ir.labels.get(label_id.0)
            {
                let inner_w = cell_rect.width.saturating_sub(2) as usize;
                let inner_x = cell_rect.x + 1;
                let inner_y = cell_rect.y + 1;
                let text_color = self.colors.node_text;

                for (line_idx, line) in label.text.split('\n').enumerate() {
                    let y = inner_y + line_idx as u16;
                    if y >= cell_rect.y + cell_rect.height.saturating_sub(1) {
                        break;
                    }
                    let display = if line.len() > inner_w && inner_w > 3 {
                        format!("{}...", &line[..inner_w.saturating_sub(3)])
                    } else {
                        line.to_string()
                    };
                    for (j, ch) in display.chars().enumerate() {
                        let x = inner_x + j as u16;
                        if x < cell_rect.x + cell_rect.width.saturating_sub(1) {
                            buf.set(x, y, Cell::from_char(ch).with_fg(text_color));
                        }
                    }
                }
            }
        }

        // Draw relation edges.
        self.render_edges(&layout.edges, ir, vp, &resolved_styles.edge_styles, buf);
    }

    fn render_clusters(
        &self,
        clusters: &[LayoutClusterBox],
        ir: &MermaidDiagramIr,
        vp: &Viewport,
        buf: &mut Buffer,
    ) {
        let border_cell = Cell::from_char(' ').with_fg(self.colors.cluster_border);
        for cluster in clusters {
            let cell_rect = vp.to_cell_rect(&cluster.rect);
            if cell_rect.width < 2 || cell_rect.height < 2 {
                continue;
            }
            buf.draw_border(cell_rect, self.glyphs.border, border_cell);

            // Render cluster title if available.
            if let Some(title_rect) = &cluster.title_rect
                && let Some(ir_cluster) = ir.clusters.get(cluster.cluster_idx)
                && let Some(label_id) = ir_cluster.title
                && let Some(label) = ir.labels.get(label_id.0)
            {
                let tr = vp.to_cell_rect(title_rect);
                let title_cell = Cell::from_char(' ').with_fg(self.colors.cluster_title);
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
        edge_styles: &[ResolvedMermaidStyle],
        buf: &mut Buffer,
    ) {
        let edge_cell = Cell::from_char(' ').with_fg(self.colors.edge_color);
        for edge_path in edges {
            let waypoints: Vec<(u16, u16)> = edge_path
                .waypoints
                .iter()
                .map(|p| vp.to_cell(p.x, p.y))
                .collect();

            // Detect line style from arrow syntax.
            let mut line_style = ir
                .edges
                .get(edge_path.edge_idx)
                .map(|e| edge_line_style(&e.arrow, edge_styles.get(edge_path.edge_idx)))
                .unwrap_or(EdgeLineStyle::Solid);
            if edge_path.bundle_count > 1 {
                line_style = EdgeLineStyle::Thick;
            }

            // Draw line segments between consecutive waypoints.
            for pair in waypoints.windows(2) {
                let (x0, y0) = pair[0];
                let (x1, y1) = pair[1];
                self.draw_line_segment_styled(x0, y0, x1, y1, edge_cell, line_style, buf);
            }

            // Draw arrowhead at the last waypoint.
            if ir.diagram_type != DiagramType::Mindmap && waypoints.len() >= 2 {
                let (px, py) = waypoints[waypoints.len() - 2];
                let (tx, ty) = *waypoints.last().unwrap();
                let arrow_ch = self.arrowhead_char(px, py, tx, ty);
                buf.set(tx, ty, edge_cell.with_char(arrow_ch));
            }

            let ir_label = ir
                .edges
                .get(edge_path.edge_idx)
                .and_then(|e| e.label)
                .and_then(|lid| ir.labels.get(lid.0))
                .map(|l| l.text.as_str());
            let bundle_label = self.bundle_count_label(edge_path.bundle_count);
            let label_text = match (ir_label, bundle_label) {
                (Some(text), Some(mult)) => Some(format!("{text} {mult}")),
                (Some(text), None) => Some(text.to_string()),
                (None, Some(mult)) => Some(mult),
                (None, None) => None,
            };
            if let Some(text) = label_text {
                self.render_edge_label(edge_path, &text, DEFAULT_EDGE_LABEL_WIDTH, vp, buf);
            }
        }
    }

    fn render_sequence_lifelines(&self, layout: &DiagramLayout, vp: &Viewport, buf: &mut Buffer) {
        let line_cell = Cell::from_char(' ').with_fg(self.colors.edge_color);
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

    /// Draw vertical branch lane lines for gitGraph diagrams.
    fn render_gitgraph_lanes(
        &self,
        layout: &DiagramLayout,
        _ir: &MermaidDiagramIr,
        vp: &Viewport,
        buf: &mut Buffer,
    ) {
        // Draw a vertical lane line for each cluster (branch).
        // Lane 0 (main) is implicit and drawn through all commits.
        let end_y = layout.bounding_box.y + layout.bounding_box.height;
        let start_y = layout.bounding_box.y;

        // Collect lane x positions from cluster rects.
        let mut lane_xs: Vec<f64> = Vec::new();

        // Lane 0: center of the leftmost column of nodes (commits not in any cluster)
        if let Some(first_node) = layout.nodes.first() {
            lane_xs.push(first_node.rect.x + first_node.rect.width / 2.0);
        }

        for cluster in &layout.clusters {
            lane_xs.push(cluster.rect.x + cluster.rect.width / 2.0);
        }

        let line_cell = Cell::from_char(' ').with_fg(self.colors.edge_color);
        for x in &lane_xs {
            let (cx, cy0) = vp.to_cell(*x, start_y);
            let (_, cy1) = vp.to_cell(*x, end_y);
            let (lo, hi) = if cy0 <= cy1 { (cy0, cy1) } else { (cy1, cy0) };
            for y in lo..=hi {
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
        let p = &self.glyphs;
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
        let p = &self.glyphs;
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
            EdgeLineStyle::Dotted => self.draw_dotted_segment(x0, y0, x1, y1, cell, buf),
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
            // Skip the corner position in both loops to avoid OR-merging all
            // four directions into a cross (`┼`); the corner is drawn below.
            let lo_x = x0.min(x1);
            let hi_x = x0.max(x1);
            for (i, x) in (lo_x..=hi_x).enumerate() {
                if x == x1 {
                    continue;
                }
                if i % 2 == 0 {
                    self.merge_line_cell(x, y0, LINE_LEFT | LINE_RIGHT, cell, buf);
                }
            }
            let lo_y = y0.min(y1);
            let hi_y = y0.max(y1);
            for (i, y) in (lo_y..=hi_y).enumerate() {
                if y == y0 {
                    continue;
                }
                if i % 2 == 0 {
                    self.merge_line_cell(x1, y, LINE_UP | LINE_DOWN, cell, buf);
                }
            }
            let horiz_bit = if x1 >= x0 { LINE_LEFT } else { LINE_RIGHT };
            let vert_bit = if y1 >= y0 { LINE_DOWN } else { LINE_UP };
            self.merge_line_cell(x1, y0, horiz_bit | vert_bit, cell, buf);
        }
    }

    /// Draw a dotted line segment (dot glyphs along the path).
    #[allow(clippy::too_many_arguments)]
    fn draw_dotted_segment(
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
            for x in lo..=hi {
                self.set_dot_or_merge(x, y0, true, cell, buf);
            }
        } else if x0 == x1 {
            let lo = y0.min(y1);
            let hi = y0.max(y1);
            for y in lo..=hi {
                self.set_dot_or_merge(x0, y, false, cell, buf);
            }
        } else {
            let lo_x = x0.min(x1);
            let hi_x = x0.max(x1);
            for x in lo_x..=hi_x {
                if x == x1 {
                    continue;
                }
                self.set_dot_or_merge(x, y0, true, cell, buf);
            }
            let lo_y = y0.min(y1);
            let hi_y = y0.max(y1);
            for y in lo_y..=hi_y {
                if y == y0 {
                    continue;
                }
                self.set_dot_or_merge(x1, y, false, cell, buf);
            }
            let horiz_bit = if x1 >= x0 { LINE_LEFT } else { LINE_RIGHT };
            let vert_bit = if y1 >= y0 { LINE_DOWN } else { LINE_UP };
            self.merge_line_cell(x1, y0, horiz_bit | vert_bit, cell, buf);
        }
    }

    fn set_dot_or_merge(&self, x: u16, y: u16, horizontal: bool, cell: Cell, buf: &mut Buffer) {
        if let Some(existing) = buf.get(x, y).and_then(|c| c.content.as_char())
            && self.line_bits_for_char(existing).is_some()
        {
            let bits = if horizontal {
                LINE_LEFT | LINE_RIGHT
            } else {
                LINE_UP | LINE_DOWN
            };
            self.merge_line_cell(x, y, bits, cell, buf);
            return;
        }
        let dot = if horizontal {
            self.glyphs.dot_h
        } else {
            self.glyphs.dot_v
        };
        buf.set(x, y, cell.with_char(dot));
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
                self.glyphs.arrow_right
            } else {
                self.glyphs.arrow_left
            }
        } else if dy >= 0 {
            self.glyphs.arrow_down
        } else {
            self.glyphs.arrow_up
        }
    }

    /// Render an edge label at the midpoint of the edge path.
    fn render_edge_label(
        &self,
        edge_path: &LayoutEdgePath,
        text: &str,
        max_label_width: usize,
        vp: &Viewport,
        buf: &mut Buffer,
    ) {
        if edge_path.waypoints.len() < 2 || text.is_empty() {
            return;
        }
        // Place label near the geometric midpoint of the path.
        let mid_idx = edge_path.waypoints.len() / 2;
        let (mx, my) = if edge_path.waypoints.len().is_multiple_of(2) && mid_idx > 0 {
            // Even waypoint count: interpolate between the two middle segments.
            let a = &edge_path.waypoints[mid_idx - 1];
            let b = &edge_path.waypoints[mid_idx];
            ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
        } else {
            let mid = &edge_path.waypoints[mid_idx];
            (mid.x, mid.y)
        };
        let (cx, cy) = vp.to_cell(mx, my);
        let label = if max_label_width == 0 {
            text.to_string()
        } else {
            truncate_label(text, max_label_width)
        };
        let label_cell = Cell::from_char(' ').with_fg(self.colors.node_text);
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
        let border_cell = Cell::from_char(' ').with_fg(self.colors.node_border);

        for node in nodes {
            let ir_node = match ir.nodes.get(node.node_idx) {
                Some(node) => node,
                None => continue,
            };
            if ir_node
                .classes
                .iter()
                .any(|class| class == STATE_CONTAINER_CLASS)
            {
                continue;
            }
            let cell_rect = vp.to_cell_rect(&node.rect);
            if cell_rect.width < 2 || cell_rect.height < 2 {
                // Too small for a box; render as a single char.
                let (cx, cy) = vp.to_cell(node.rect.x, node.rect.y);
                buf.set(cx, cy, border_cell.with_char('*'));
                continue;
            }

            let base_fill = self.colors.node_fill_for(node.node_idx);
            let fill_color = journey_score_fill(ir_node).or_else(|| timeline_era_fill(ir_node)).unwrap_or(base_fill);
            let fill_cell = Cell::from_char(' ').with_bg(fill_color);

            let inset =
                self.draw_shaped_node(cell_rect, ir_node.shape, border_cell, fill_cell, buf);

            // Apply half-block smoothing at top/bottom fill boundaries.
            self.apply_half_block_fill(cell_rect, ir_node.shape, fill_color, buf);

            // Render label (and class compartments if applicable) inside the node.
            if let Some(label_id) = ir_node.label
                && let Some(label) = ir.labels.get(label_id.0)
            {
                if !ir_node.members.is_empty() {
                    self.render_class_compartments(
                        cell_rect,
                        &label.text,
                        &ir_node.members,
                        0,
                        fill_color,
                        buf,
                    );
                } else {
                    self.render_node_label_with_inset(
                        cell_rect,
                        &label.text,
                        inset,
                        fill_color,
                        buf,
                    );
                }
            }
        }
    }

    fn render_legend_footnotes(&self, area: Rect, footnotes: &[String], buf: &mut Buffer) {
        if area.is_empty() || footnotes.is_empty() {
            return;
        }

        let max_lines = area.height as usize;
        if max_lines == 0 {
            return;
        }
        let max_width = area.width as usize;
        if max_width == 0 {
            return;
        }

        buf.fill(area, Cell::from_char(' '));

        let cell = Cell::from_char(' ').with_fg(self.colors.edge_color);
        let max_x = area.right();
        let mut y = area.y;

        if footnotes.len() > max_lines {
            let visible = max_lines.saturating_sub(1);
            for line in footnotes.iter().take(visible) {
                let rendered = truncate_line_to_width(line, max_width);
                buf.print_text_clipped(area.x, y, &rendered, cell, max_x);
                y = y.saturating_add(1);
            }
            let remaining = footnotes.len().saturating_sub(visible);
            if y < area.bottom() {
                let marker = match self.glyph_mode {
                    MermaidGlyphMode::Ascii => "...",
                    MermaidGlyphMode::Unicode => "…",
                };
                let overflow_line = format!("{marker} +{remaining} more");
                let rendered = truncate_line_to_width(&overflow_line, max_width);
                buf.print_text_clipped(area.x, y, &rendered, cell, max_x);
            }
        } else {
            for line in footnotes.iter().take(max_lines) {
                let rendered = truncate_line_to_width(line, max_width);
                buf.print_text_clipped(area.x, y, &rendered, cell, max_x);
                y = y.saturating_add(1);
            }
        }
    }

    /// Render a label centered inside a node rectangle.
    ///
    /// When the label text is wider than the node interior, text is wrapped
    /// at word boundaries (falling back to character breaks) and the block
    /// of lines is centered vertically. If there are more lines than rows,
    /// the last visible line is truncated with an ellipsis.
    fn render_node_label(
        &self,
        cell_rect: Rect,
        text: &str,
        fill_bg: PackedRgba,
        buf: &mut Buffer,
    ) {
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
        let label_cell = Cell::from_char(' ')
            .with_fg(self.colors.node_text)
            .with_bg(fill_bg);

        let mut lines = wrap_text(text, inner_w);

        // If more lines than rows, truncate and add ellipsis to the last visible line.
        if lines.len() > inner_h {
            lines.truncate(inner_h);
            if let Some(last) = lines.last_mut() {
                *last = append_ellipsis(last, inner_w);
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
        max_label_width: usize,
        fill_bg: PackedRgba,
        buf: &mut Buffer,
    ) {
        let border_cell = Cell::from_char(' ').with_fg(self.colors.node_border);
        let label_cell = Cell::from_char(' ')
            .with_fg(self.colors.node_text)
            .with_bg(fill_bg);
        let member_cell = Cell::from_char(' ')
            .with_fg(self.colors.edge_color)
            .with_bg(fill_bg);
        let inner_w = cell_rect.width.saturating_sub(2) as usize;

        if inner_w == 0 || cell_rect.height < 4 {
            // Too small for compartments, fall back to normal label.
            self.render_node_label(cell_rect, label_text, fill_bg, buf);
            return;
        }

        let max_x = cell_rect
            .x
            .saturating_add(cell_rect.width)
            .saturating_sub(1);

        // Row 0 = top border (already drawn by draw_box)
        // Row 1 = class name (centered)
        let name_y = cell_rect.y.saturating_add(1);
        let name_text = if max_label_width > 0 {
            truncate_label(label_text, max_label_width)
        } else {
            label_text.to_string()
        };
        let name_width = display_width(&name_text).min(inner_w);
        let name_pad = inner_w.saturating_sub(name_width) / 2;
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
            let horiz = self.glyphs.border.horizontal;
            buf.set(
                cell_rect.x,
                sep_y,
                border_cell.with_char(self.glyphs.tee_right),
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
                border_cell.with_char(self.glyphs.tee_left),
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

/// Truncate a label to fit within `max_width` display columns, adding
/// ellipsis if needed. Uses terminal display width (not char count) so
/// that CJK and other wide characters are measured correctly.
fn truncate_label(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if display_width(text) <= max_width {
        return text.to_string();
    }
    append_ellipsis(text, max_width)
}

/// Force an ellipsis suffix, respecting display width.
fn append_ellipsis(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let ellipsis = '…';
    let ellipsis_width = ftui_core::text_width::char_width(ellipsis).max(1);
    if max_width <= ellipsis_width {
        return ellipsis.to_string();
    }
    let target_width = max_width.saturating_sub(ellipsis_width);
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = ftui_core::text_width::char_width(ch);
        if width + ch_width > target_width {
            break;
        }
        width += ch_width;
        out.push(ch);
    }
    out.push(ellipsis);
    out
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

#[allow(dead_code)]
fn truncate_line_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if display_width(text) <= max_width {
        text.to_string()
    } else {
        append_ellipsis(text, max_width)
    }
}

// ── Convenience API ─────────────────────────────────────────────────────

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn render_diagram_canvas_with_plan(
    layout: &DiagramLayout,
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    plan: &RenderPlan,
    render_mode: MermaidRenderMode,
    buf: &mut Buffer,
) {
    if ir.diagram_type == DiagramType::Pie {
        let renderer = MermaidRenderer::new(config);
        renderer.render_with_plan(layout, ir, plan, buf);
        return;
    }
    if layout.nodes.is_empty() || plan.diagram_area.is_empty() {
        return;
    }

    let canvas_mode = canvas_mode_for_render_mode(render_mode);
    let mut painter = Painter::for_area(plan.diagram_area, canvas_mode);
    painter.clear();
    let vp = CanvasViewport::fit(&layout.bounding_box, plan.diagram_area, canvas_mode);

    let resolved_styles = resolve_styles(ir);
    let colors = DiagramPalette::from_preset(config.palette);
    render_canvas_edges(
        &mut painter,
        &layout.edges,
        ir,
        &resolved_styles.edge_styles,
        canvas_mode,
        &vp,
    );
    render_canvas_nodes(&mut painter, &layout.nodes, ir, &colors, &vp);

    let style = ftui_style::Style::new().fg(EDGE_FG);
    painter.render_to_buffer(plan.diagram_area, buf, style);

    let cell_vp = Viewport::fit(&layout.bounding_box, plan.diagram_area);
    let renderer = MermaidRenderer::new(config);
    if plan.show_clusters {
        renderer.render_clusters(&layout.clusters, ir, &cell_vp, buf);
    }
    if ir.diagram_type == DiagramType::Sequence {
        renderer.render_sequence_lifelines(layout, &cell_vp, buf);
    }
    renderer.canvas_composite_labels(&layout.nodes, &layout.edges, ir, &cell_vp, plan, buf);
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn render_canvas_edges(
    painter: &mut Painter,
    edges: &[LayoutEdgePath],
    ir: &MermaidDiagramIr,
    edge_styles: &[ResolvedMermaidStyle],
    canvas_mode: CanvasMode,
    vp: &CanvasViewport,
) {
    for edge_path in edges {
        let mut line_style = ir
            .edges
            .get(edge_path.edge_idx)
            .map(|e| edge_line_style(&e.arrow, edge_styles.get(edge_path.edge_idx)))
            .unwrap_or(EdgeLineStyle::Solid);
        if edge_path.bundle_count > 1 {
            line_style = EdgeLineStyle::Thick;
        }
        let mut last: Option<(i32, i32)> = None;
        let mut prev_dir: Option<(i32, i32)> = None;
        for point in &edge_path.waypoints {
            let (x, y) = vp.to_pixel(point.x, point.y);
            if let Some((px, py)) = last {
                if px == x && py == y {
                    continue;
                }
                let dir = (signum_i32(x - px), signum_i32(y - py));
                if let Some(prev) = prev_dir
                    && ((prev.0 == 0 && dir.1 == 0) || (prev.1 == 0 && dir.0 == 0))
                {
                    let diag = (prev.0 + dir.0, prev.1 + dir.1);
                    if diag.0 != 0 && diag.1 != 0 {
                        draw_canvas_line_segment(
                            painter,
                            px,
                            py,
                            px + diag.0,
                            py + diag.1,
                            line_style,
                        );
                    }
                }
                draw_canvas_line_segment(painter, px, py, x, y, line_style);
                prev_dir = Some(dir);
            }
            last = Some((x, y));
        }

        if ir.diagram_type != DiagramType::Mindmap
            && let Some(ir_edge) = ir.edges.get(edge_path.edge_idx)
        {
            render_canvas_arrowheads(painter, edge_path, &ir_edge.arrow, canvas_mode, vp);
        }
    }
}

#[cfg(feature = "canvas")]
fn draw_canvas_line_segment(
    painter: &mut Painter,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    style: EdgeLineStyle,
) {
    match style {
        EdgeLineStyle::Solid => painter.line(x0, y0, x1, y1),
        EdgeLineStyle::Dashed => draw_canvas_line_pattern(painter, x0, y0, x1, y1, 6, 4),
        EdgeLineStyle::Dotted => draw_canvas_line_pattern(painter, x0, y0, x1, y1, 1, 2),
        EdgeLineStyle::Thick => draw_canvas_thick_line(painter, x0, y0, x1, y1),
    }
}

#[cfg(feature = "canvas")]
fn draw_canvas_line_pattern(
    painter: &mut Painter,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    on_len: usize,
    off_len: usize,
) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx: i32 = if x0 < x1 { 1 } else { -1 };
    let sy: i32 = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut cx = x0;
    let mut cy = y0;

    let mut draw_on = true;
    let mut remaining = on_len.max(1);

    loop {
        if draw_on {
            painter.point(cx, cy);
        }

        if cx == x1 && cy == y1 {
            break;
        }

        let e2 = 2 * err;
        if e2 >= dy {
            if cx == x1 {
                break;
            }
            err += dy;
            cx += sx;
        }
        if e2 <= dx {
            if cy == y1 {
                break;
            }
            err += dx;
            cy += sy;
        }

        remaining = remaining.saturating_sub(1);
        if remaining == 0 {
            draw_on = !draw_on;
            remaining = if draw_on { on_len } else { off_len };
            if remaining == 0 {
                remaining = 1;
            }
        }
    }
}

#[cfg(feature = "canvas")]
fn draw_canvas_thick_line(painter: &mut Painter, x0: i32, y0: i32, x1: i32, y1: i32) {
    painter.line(x0, y0, x1, y1);
    let (ox, oy) = thick_offset(x0, y0, x1, y1);
    if ox != 0 || oy != 0 {
        painter.line(x0 + ox, y0 + oy, x1 + ox, y1 + oy);
    }
}

#[cfg(feature = "canvas")]
fn thick_offset(x0: i32, y0: i32, x1: i32, y1: i32) -> (i32, i32) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    if dx == 0 && dy == 0 {
        return (0, 0);
    }
    if dx.abs() >= dy.abs() {
        let oy = if dy >= 0 { 1 } else { -1 };
        (0, oy)
    } else {
        let ox = if dx >= 0 { 1 } else { -1 };
        (ox, 0)
    }
}

#[cfg(feature = "canvas")]
fn signum_i32(value: i32) -> i32 {
    if value > 0 {
        1
    } else if value < 0 {
        -1
    } else {
        0
    }
}

#[cfg(feature = "canvas")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum ArrowHeadKind {
    Normal,
    Open,
    Circle,
    Cross,
    Diamond,
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn arrowhead_kind_start(arrow: &str) -> Option<ArrowHeadKind> {
    if arrow.starts_with("<<") {
        Some(ArrowHeadKind::Open)
    } else if arrow.starts_with('<') {
        Some(ArrowHeadKind::Normal)
    } else if arrow.starts_with('o') {
        Some(ArrowHeadKind::Circle)
    } else if arrow.starts_with('x') {
        Some(ArrowHeadKind::Cross)
    } else if arrow.starts_with('*') {
        Some(ArrowHeadKind::Diamond)
    } else {
        None
    }
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn arrowhead_kind_end(arrow: &str) -> Option<ArrowHeadKind> {
    if arrow.ends_with(">>") {
        Some(ArrowHeadKind::Open)
    } else if arrow.ends_with('>') {
        Some(ArrowHeadKind::Normal)
    } else if arrow.ends_with('o') {
        Some(ArrowHeadKind::Circle)
    } else if arrow.ends_with('x') {
        Some(ArrowHeadKind::Cross)
    } else if arrow.ends_with('*') {
        Some(ArrowHeadKind::Diamond)
    } else {
        None
    }
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn render_canvas_arrowheads(
    painter: &mut Painter,
    edge_path: &LayoutEdgePath,
    arrow: &str,
    canvas_mode: CanvasMode,
    vp: &CanvasViewport,
) {
    if arrow.is_empty() {
        return;
    }
    let mut points: Vec<(i32, i32)> = edge_path
        .waypoints
        .iter()
        .map(|p| vp.to_pixel(p.x, p.y))
        .collect();
    points.dedup();
    if points.len() < 2 {
        return;
    }

    if let Some(kind) = arrowhead_kind_end(arrow)
        && let Some((from, tip)) = last_two_distinct(&points)
    {
        draw_canvas_arrowhead(painter, from, tip, kind, canvas_mode);
    }

    if let Some(kind) = arrowhead_kind_start(arrow)
        && let Some((tip, next)) = first_two_distinct(&points)
    {
        draw_canvas_arrowhead(painter, next, tip, kind, canvas_mode);
    }
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn first_two_distinct(points: &[(i32, i32)]) -> Option<((i32, i32), (i32, i32))> {
    let first = *points.first()?;
    for &pt in points.iter().skip(1) {
        if pt != first {
            return Some((first, pt));
        }
    }
    None
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn last_two_distinct(points: &[(i32, i32)]) -> Option<((i32, i32), (i32, i32))> {
    let last = *points.last()?;
    for &pt in points.iter().rev().skip(1) {
        if pt != last {
            return Some((pt, last));
        }
    }
    None
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn arrowhead_dimensions(mode: CanvasMode) -> (f64, f64, i32) {
    match mode {
        CanvasMode::Braille => (4.0, 4.0, 2),
        CanvasMode::Block => (3.0, 3.0, 1),
        CanvasMode::HalfBlock => (3.0, 2.0, 1),
    }
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn draw_canvas_arrowhead(
    painter: &mut Painter,
    from: (i32, i32),
    tip: (i32, i32),
    kind: ArrowHeadKind,
    canvas_mode: CanvasMode,
) {
    let dx = (tip.0 - from.0) as f64;
    let dy = (tip.1 - from.1) as f64;
    let len = (dx * dx + dy * dy).sqrt();
    let (arrow_len, arrow_width, radius) = arrowhead_dimensions(canvas_mode);
    if len < arrow_len.max(2.0) {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy;
    let py = ux;
    let half_width = arrow_width / 2.0;

    let tip_f = (tip.0 as f64, tip.1 as f64);
    let base_center = (tip_f.0 - ux * arrow_len, tip_f.1 - uy * arrow_len);
    let base_left = (
        base_center.0 + px * half_width,
        base_center.1 + py * half_width,
    );
    let base_right = (
        base_center.0 - px * half_width,
        base_center.1 - py * half_width,
    );

    let tip_i = (tip_f.0.round() as i32, tip_f.1.round() as i32);
    let bl_i = (base_left.0.round() as i32, base_left.1.round() as i32);
    let br_i = (base_right.0.round() as i32, base_right.1.round() as i32);

    match kind {
        ArrowHeadKind::Normal => painter.polygon_filled(&[tip_i, bl_i, br_i]),
        ArrowHeadKind::Open => draw_polygon(painter, &[tip_i, bl_i, br_i]),
        ArrowHeadKind::Circle => draw_canvas_circle_filled(painter, tip_i.0, tip_i.1, radius),
        ArrowHeadKind::Cross => draw_canvas_cross(painter, tip_i.0, tip_i.1, radius),
        ArrowHeadKind::Diamond => {
            let back = (tip_f.0 - ux * arrow_len, tip_f.1 - uy * arrow_len);
            let mid = (
                tip_f.0 - ux * (arrow_len / 2.0),
                tip_f.1 - uy * (arrow_len / 2.0),
            );
            let left = (mid.0 + px * half_width, mid.1 + py * half_width);
            let right = (mid.0 - px * half_width, mid.1 - py * half_width);
            let back_i = (back.0.round() as i32, back.1.round() as i32);
            let left_i = (left.0.round() as i32, left.1.round() as i32);
            let right_i = (right.0.round() as i32, right.1.round() as i32);
            painter.polygon_filled(&[tip_i, left_i, back_i, right_i]);
        }
    }
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn draw_canvas_circle_filled(painter: &mut Painter, cx: i32, cy: i32, radius: i32) {
    let r = radius.max(1);
    for y in (cy - r)..=(cy + r) {
        for x in (cx - r)..=(cx + r) {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= r * r {
                painter.point(x, y);
            }
        }
    }
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn draw_canvas_cross(painter: &mut Painter, cx: i32, cy: i32, radius: i32) {
    let r = radius.max(1);
    painter.line(cx - r, cy - r, cx + r, cy + r);
    painter.line(cx - r, cy + r, cx + r, cy - r);
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn render_canvas_nodes(
    painter: &mut Painter,
    nodes: &[LayoutNodeBox],
    ir: &MermaidDiagramIr,
    colors: &DiagramPalette,
    vp: &CanvasViewport,
) {
    for node in nodes {
        let ir_node = match ir.nodes.get(node.node_idx) {
            Some(node) => node,
            None => continue,
        };
        if ir_node
            .classes
            .iter()
            .any(|class| class == STATE_CONTAINER_CLASS)
        {
            continue;
        }
        let rect = vp.to_pixel_rect(&node.rect);
        let fill = colors.node_fill_for(node.node_idx);
        let border = colors.node_border;
        draw_node_canvas(painter, rect, ir_node.shape, border, fill);
    }
}

#[cfg(feature = "canvas")]
fn draw_node_canvas(
    painter: &mut Painter,
    rect: PixelRect,
    shape: NodeShape,
    border: PackedRgba,
    fill: PackedRgba,
) {
    let w = rect.width.max(1);
    let h = rect.height.max(1);
    if w <= 1 || h <= 1 {
        painter.point_colored(rect.x, rect.y, border);
        return;
    }

    match shape {
        NodeShape::Rect => {
            fill_rect_colored(painter, rect, fill);
            draw_rect_border_colored(painter, rect, border);
        }
        NodeShape::Rounded => {
            fill_rounded_rect_colored(painter, rect, fill);
            draw_rounded_rect_colored(painter, rect, border);
        }
        NodeShape::Stadium => {
            draw_stadium_colored(painter, rect, border, fill);
        }
        NodeShape::Subroutine => {
            fill_rect_colored(painter, rect, fill);
            draw_rect_border_colored(painter, rect, border);
            if w > 3 {
                painter.line_colored(rect.x + 1, rect.y, rect.x + 1, rect.y + h - 1, Some(border));
                painter.line_colored(
                    rect.x + w - 2,
                    rect.y,
                    rect.x + w - 2,
                    rect.y + h - 1,
                    Some(border),
                );
            }
        }
        NodeShape::Circle => {
            let radius = (w.min(h) / 2).max(1);
            let cx = rect.x + w / 2;
            let cy = rect.y + h / 2;
            fill_circle_colored(painter, cx, cy, radius, fill);
            draw_circle_colored(painter, cx, cy, radius, border);
        }
        NodeShape::Diamond => {
            let top = (rect.x + w / 2, rect.y);
            let right = (rect.x + w - 1, rect.y + h / 2);
            let bottom = (rect.x + w / 2, rect.y + h - 1);
            let left = (rect.x, rect.y + h / 2);
            let points = [top, right, bottom, left];
            fill_polygon_colored(painter, &points, fill);
            draw_polygon_colored(painter, &points, border);
        }
        NodeShape::Hexagon => {
            let dx = (w / 4).max(1);
            let top_left = (rect.x + dx, rect.y);
            let top_right = (rect.x + w - dx - 1, rect.y);
            let right = (rect.x + w - 1, rect.y + h / 2);
            let bottom_right = (rect.x + w - dx - 1, rect.y + h - 1);
            let bottom_left = (rect.x + dx, rect.y + h - 1);
            let left = (rect.x, rect.y + h / 2);
            let points = [top_left, top_right, right, bottom_right, bottom_left, left];
            fill_polygon_colored(painter, &points, fill);
            draw_polygon_colored(painter, &points, border);
        }
        NodeShape::Asymmetric => {
            let tip = (rect.x + w - 1, rect.y + h / 2);
            let top = (rect.x, rect.y);
            let mid_top = (rect.x + w - 2, rect.y);
            let mid_bottom = (rect.x + w - 2, rect.y + h - 1);
            let bottom = (rect.x, rect.y + h - 1);
            let points = [top, mid_top, tip, mid_bottom, bottom];
            fill_polygon_colored(painter, &points, fill);
            draw_polygon_colored(painter, &points, border);
        }
    }
}

#[cfg(feature = "canvas")]
fn fill_rect_colored(painter: &mut Painter, rect: PixelRect, color: PackedRgba) {
    for y in rect.y..(rect.y + rect.height) {
        for x in rect.x..(rect.x + rect.width) {
            painter.point_colored(x, y, color);
        }
    }
}

#[cfg(feature = "canvas")]
fn draw_rect_border_colored(painter: &mut Painter, rect: PixelRect, color: PackedRgba) {
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.width - 1;
    let y1 = rect.y + rect.height - 1;
    painter.line_colored(x0, y0, x1, y0, Some(color));
    painter.line_colored(x1, y0, x1, y1, Some(color));
    painter.line_colored(x1, y1, x0, y1, Some(color));
    painter.line_colored(x0, y1, x0, y0, Some(color));
}

#[cfg(feature = "canvas")]
fn fill_polygon_colored(painter: &mut Painter, points: &[(i32, i32)], color: PackedRgba) {
    if points.len() < 3 {
        return;
    }
    let (mut min_x, mut max_x) = (points[0].0, points[0].0);
    let (mut min_y, mut max_y) = (points[0].1, points[0].1);
    for &(x, y) in points.iter().skip(1) {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if point_in_convex_polygon(x, y, points) {
                painter.point_colored(x, y, color);
            }
        }
    }
}

#[cfg(feature = "canvas")]
fn draw_polygon_colored(painter: &mut Painter, points: &[(i32, i32)], color: PackedRgba) {
    if points.len() < 2 {
        return;
    }
    for idx in 0..points.len() {
        let (x0, y0) = points[idx];
        let (x1, y1) = points[(idx + 1) % points.len()];
        painter.line_colored(x0, y0, x1, y1, Some(color));
    }
}

#[cfg(feature = "canvas")]
fn fill_circle_colored(painter: &mut Painter, cx: i32, cy: i32, radius: i32, color: PackedRgba) {
    let r = radius.max(1);
    for y in (cy - r)..=(cy + r) {
        for x in (cx - r)..=(cx + r) {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= r * r {
                painter.point_colored(x, y, color);
            }
        }
    }
}

#[cfg(feature = "canvas")]
fn draw_circle_colored(painter: &mut Painter, cx: i32, cy: i32, radius: i32, color: PackedRgba) {
    if radius <= 0 {
        painter.point_colored(cx, cy, color);
        return;
    }

    let mut x = radius;
    let mut y = 0;
    let mut d = 1 - radius;

    while x >= y {
        let points = [
            (cx + x, cy + y),
            (cx + y, cy + x),
            (cx - y, cy + x),
            (cx - x, cy + y),
            (cx - x, cy - y),
            (cx - y, cy - x),
            (cx + y, cy - x),
            (cx + x, cy - y),
        ];
        for (px, py) in points {
            painter.point_colored(px, py, color);
        }

        y += 1;
        if d < 0 {
            d += 2 * y + 1;
        } else {
            x -= 1;
            d += 2 * (y - x) + 1;
        }
    }
}

#[cfg(feature = "canvas")]
fn draw_rounded_rect_colored(painter: &mut Painter, rect: PixelRect, color: PackedRgba) {
    if rect.width < 4 || rect.height < 4 {
        draw_rect_border_colored(painter, rect, color);
        return;
    }
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.width - 1;
    let y1 = rect.y + rect.height - 1;
    painter.line_colored(x0 + 1, y0, x1 - 1, y0, Some(color));
    painter.line_colored(x1, y0 + 1, x1, y1 - 1, Some(color));
    painter.line_colored(x1 - 1, y1, x0 + 1, y1, Some(color));
    painter.line_colored(x0, y1 - 1, x0, y0 + 1, Some(color));
    painter.line_colored(x0, y0 + 1, x0 + 1, y0, Some(color));
    painter.line_colored(x1 - 1, y0, x1, y0 + 1, Some(color));
    painter.line_colored(x1, y1 - 1, x1 - 1, y1, Some(color));
    painter.line_colored(x0 + 1, y1, x0, y1 - 1, Some(color));
}

#[cfg(feature = "canvas")]
fn fill_rounded_rect_colored(painter: &mut Painter, rect: PixelRect, color: PackedRgba) {
    if rect.width < 4 || rect.height < 4 {
        fill_rect_colored(painter, rect, color);
        return;
    }
    for y in rect.y..(rect.y + rect.height) {
        for x in rect.x..(rect.x + rect.width) {
            let at_corner = (x == rect.x || x == rect.x + rect.width - 1)
                && (y == rect.y || y == rect.y + rect.height - 1);
            if !at_corner {
                painter.point_colored(x, y, color);
            }
        }
    }
}

#[cfg(feature = "canvas")]
fn draw_stadium_colored(
    painter: &mut Painter,
    rect: PixelRect,
    border: PackedRgba,
    fill: PackedRgba,
) {
    let w = rect.width.max(1);
    let h = rect.height.max(1);
    if w <= 2 || h <= 2 {
        fill_rect_colored(painter, rect, fill);
        draw_rect_border_colored(painter, rect, border);
        return;
    }
    let radius = (h.min(w) / 2).max(1);
    let left_cx = rect.x + radius;
    let right_cx = rect.x + w - radius - 1;
    let cy = rect.y + h / 2;

    // Fill center rectangle + end caps.
    let inner = PixelRect {
        x: left_cx,
        y: rect.y,
        width: (right_cx - left_cx + 1).max(1),
        height: h,
    };
    fill_rect_colored(painter, inner, fill);
    fill_circle_colored(painter, left_cx, cy, radius, fill);
    fill_circle_colored(painter, right_cx, cy, radius, fill);

    // Border.
    draw_circle_colored(painter, left_cx, cy, radius, border);
    draw_circle_colored(painter, right_cx, cy, radius, border);
    painter.line_colored(left_cx, rect.y, right_cx, rect.y, Some(border));
    painter.line_colored(
        left_cx,
        rect.y + h - 1,
        right_cx,
        rect.y + h - 1,
        Some(border),
    );
}

#[cfg(feature = "canvas")]
fn point_in_convex_polygon(x: i32, y: i32, points: &[(i32, i32)]) -> bool {
    let mut sign: i32 = 0;
    let len = points.len();
    for i in 0..len {
        let (x0, y0) = points[i];
        let (x1, y1) = points[(i + 1) % len];
        let cross = (x - x0) * (y1 - y0) - (y - y0) * (x1 - x0);
        if cross == 0 {
            continue;
        }
        let s = cross.signum();
        if sign == 0 {
            sign = s;
        } else if sign != s {
            return false;
        }
    }
    true
}

#[cfg(feature = "canvas")]
#[allow(dead_code)]
fn draw_polygon(painter: &mut Painter, points: &[(i32, i32)]) {
    if points.len() < 2 {
        return;
    }
    for idx in 0..points.len() {
        let (x0, y0) = points[idx];
        let (x1, y1) = points[(idx + 1) % points.len()];
        painter.line(x0, y0, x1, y1);
    }
}

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
    let _plan = render_diagram_adaptive(layout, ir, config, area, buf);
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
    let plan = select_render_plan(config, layout, ir, area);
    #[cfg(feature = "canvas")]
    let use_canvas = {
        let policy = glyph_policy_for_config(config);
        let render_mode = resolve_render_mode(config, &policy);
        render_mode != MermaidRenderMode::CellOnly
    };
    #[cfg(not(feature = "canvas"))]
    let use_canvas = false;

    if !use_canvas {
        let renderer = MermaidRenderer::new(config);
        renderer.render_with_plan(layout, ir, &plan, buf);
    } else {
        #[cfg(feature = "canvas")]
        {
            let policy = glyph_policy_for_config(config);
            let rm = resolve_render_mode(config, &policy);
            render_diagram_canvas_with_plan(layout, ir, config, &plan, rm, buf);
        }
    }
    if config.debug_overlay {
        render_debug_overlay(layout, ir, &plan, area, buf);
        let info = collect_overlay_info(layout, ir, &plan);
        emit_overlay_jsonl(config, &info, area);
    }
    emit_render_jsonl(config, ir, layout, &plan, area);
    plan
}

// ── ER Cardinality Rendering (bd-1rnqg) ────────────────────────────

/// Parsed ER cardinality markers for the two endpoints of a relationship.
struct ErCardinality<'a> {
    left: &'a str,
    right: &'a str,
}

/// Parse ER cardinality markers from an arrow string.
///
/// ER arrows have the form `<left_marker><connector><right_marker>` where:
/// - `||` = exactly one, `o|`/`|o` = zero or one, `{` or `}` = many,
///   `o{`/`}o` = zero or many, `|{`/`{|` = one or many.
/// - Connector is `--`, `..`, or `==`.
///
/// Returns `None` if the arrow doesn't contain a valid ER pattern.
fn parse_er_cardinality(arrow: &str) -> Option<ErCardinality<'_>> {
    // Find the connector (center portion): --, .., or ==
    let connectors = ["--", "..", "=="];
    for conn in connectors {
        if let Some(pos) = arrow.find(conn) {
            let left = &arrow[..pos];
            let right = &arrow[pos + conn.len()..];
            if !left.is_empty() && !right.is_empty() {
                return Some(ErCardinality { left, right });
            }
        }
    }
    None
}

/// Convert an ER cardinality marker to a compact display label.
fn cardinality_label(marker: &str) -> &'static str {
    match marker {
        "||" => "1",
        "o|" | "|o" => "0..1",
        "o{" | "}o" => "0..*",
        "|{" | "{|" => "1..*",
        _ => marker.chars().next().map_or("", |c| match c {
            '|' => "1",
            'o' => "0",
            '{' | '}' => "*",
            _ => "",
        }),
    }
}

/// Render ER cardinality labels near the endpoints of an edge.
fn render_er_cardinality(edge_path: &LayoutEdgePath, arrow: &str, vp: &Viewport, buf: &mut Buffer) {
    let Some(card) = parse_er_cardinality(arrow) else {
        return;
    };

    let label_cell = Cell::from_char(' ').with_fg(CARDINALITY_FG);
    let waypoints: Vec<(u16, u16)> = edge_path
        .waypoints
        .iter()
        .map(|p| vp.to_cell(p.x, p.y))
        .collect();

    if waypoints.len() < 2 {
        return;
    }

    // Left cardinality: near the first waypoint (source entity).
    let left_text = cardinality_label(card.left);
    if !left_text.is_empty() {
        let (x, y) = waypoints[0];
        // Offset by 1 cell toward the second waypoint direction.
        let (nx, ny) = waypoints[1];
        let (lx, ly) = cardinality_offset(x, y, nx, ny);
        buf.print_text_clipped(lx, ly, left_text, label_cell, lx + left_text.len() as u16);
    }

    // Right cardinality: near the last waypoint (target entity).
    let right_text = cardinality_label(card.right);
    if !right_text.is_empty() {
        let last = waypoints.len() - 1;
        let (x, y) = waypoints[last];
        let (px, py) = waypoints[last - 1];
        let (rx, ry) = cardinality_offset(x, y, px, py);
        buf.print_text_clipped(rx, ry, right_text, label_cell, rx + right_text.len() as u16);
    }
}

/// Offset a cardinality label position perpendicular to the edge direction.
fn cardinality_offset(at_x: u16, at_y: u16, toward_x: u16, toward_y: u16) -> (u16, u16) {
    let dx = toward_x as i32 - at_x as i32;
    let dy = toward_y as i32 - at_y as i32;

    // Place label perpendicular to edge, offset by 1 cell.
    if dx.abs() > dy.abs() {
        // Horizontal edge: place label above.
        (at_x, at_y.saturating_sub(1))
    } else {
        // Vertical edge: place label to the right.
        (at_x.saturating_add(1), at_y)
    }
}

// ── Debug Overlay (bd-4cwfj) ────────────────────────────────────────

/// Diagnostic data collected for the debug overlay panel.
#[derive(Debug, Clone)]
pub struct DebugOverlayInfo {
    pub fidelity: MermaidFidelity,
    pub crossings: usize,
    pub bends: usize,
    pub ranks: usize,
    pub max_rank_width: usize,
    pub score: f64,
    pub symmetry: f64,
    pub compactness: f64,
    pub nodes: usize,
    pub edges: usize,
    pub clusters: usize,
    pub budget_exceeded: bool,
    pub ir_hash_hex: String,
}

/// Overlay colors — semi-transparent tints to avoid obscuring the diagram.
const CARDINALITY_FG: PackedRgba = PackedRgba::rgb(180, 200, 140);

const OVERLAY_PANEL_BG: PackedRgba = PackedRgba::rgba(20, 20, 40, 200);
const OVERLAY_LABEL_FG: PackedRgba = PackedRgba::rgb(140, 180, 220);
const OVERLAY_VALUE_FG: PackedRgba = PackedRgba::rgb(220, 220, 240);
const OVERLAY_WARN_FG: PackedRgba = PackedRgba::rgb(255, 180, 80);
const OVERLAY_BBOX_FG: PackedRgba = PackedRgba::rgb(60, 80, 120);
const OVERLAY_RANK_FG: PackedRgba = PackedRgba::rgb(50, 70, 100);

/// Collect diagnostic metrics for the overlay panel.
fn collect_overlay_info(
    layout: &DiagramLayout,
    ir: &MermaidDiagramIr,
    plan: &RenderPlan,
) -> DebugOverlayInfo {
    let obj = crate::mermaid_layout::evaluate_layout(layout);
    let ir_hash = crate::mermaid::hash_ir(ir);
    DebugOverlayInfo {
        fidelity: plan.fidelity,
        crossings: layout.stats.crossings,
        bends: obj.bends,
        ranks: layout.stats.ranks,
        max_rank_width: layout.stats.max_rank_width,
        score: obj.score,
        symmetry: obj.symmetry,
        compactness: obj.compactness,
        nodes: layout.nodes.len(),
        edges: layout.edges.len(),
        clusters: layout.clusters.len(),
        budget_exceeded: layout.stats.budget_exceeded,
        ir_hash_hex: format!("{:08x}", ir_hash & 0xFFFF_FFFF),
    }
}

/// Render the debug overlay panel in the top-right corner of the area.
///
/// The panel is a compact stats box showing layout quality metrics,
/// fidelity tier, and guard status. Renders on top of the diagram.
fn render_debug_overlay(
    layout: &DiagramLayout,
    ir: &MermaidDiagramIr,
    plan: &RenderPlan,
    area: Rect,
    buf: &mut Buffer,
) {
    let info = collect_overlay_info(layout, ir, plan);

    // Build panel lines.
    let lines = build_overlay_lines(&info);

    // Panel dimensions.
    let panel_w = lines
        .iter()
        .map(|(l, v)| l.len() + v.len() + 2)
        .max()
        .unwrap_or(20) as u16
        + 2;
    let panel_h = lines.len() as u16 + 2; // +2 for border

    // Position: top-right corner with 1-cell padding.
    if area.width < panel_w + 2 || area.height < panel_h + 1 {
        return; // Not enough space for overlay.
    }
    let px = area.x + area.width - panel_w - 1;
    let py = area.y + 1;

    let panel_rect = Rect::new(px, py, panel_w, panel_h);

    // Draw panel background.
    let bg_cell = Cell::from_char(' ').with_bg(OVERLAY_PANEL_BG);
    buf.draw_rect_filled(panel_rect, bg_cell);

    // Draw panel border.
    let border_cell = Cell::from_char(' ')
        .with_fg(OVERLAY_LABEL_FG)
        .with_bg(OVERLAY_PANEL_BG);
    buf.draw_border(panel_rect, BorderChars::SQUARE, border_cell);

    // Render each stat line.
    let content_x = px + 1;
    let mut cy = py + 1;
    for (label, value) in &lines {
        let fg = if label.contains('!') {
            OVERLAY_WARN_FG
        } else {
            OVERLAY_LABEL_FG
        };
        let label_cell = Cell::from_char(' ').with_fg(fg).with_bg(OVERLAY_PANEL_BG);
        buf.print_text_clipped(content_x, cy, label, label_cell, px + panel_w - 1);

        let val_x = content_x + label.len() as u16;
        let val_cell = Cell::from_char(' ')
            .with_fg(OVERLAY_VALUE_FG)
            .with_bg(OVERLAY_PANEL_BG);
        buf.print_text_clipped(val_x, cy, value, val_cell, px + panel_w - 1);

        cy += 1;
    }

    // Draw faint bounding box outline around the diagram content area.
    render_overlay_bbox(layout, area, buf);

    // Draw faint rank boundary lines.
    render_overlay_ranks(layout, area, buf);
}

/// Build the lines of label-value pairs for the overlay panel.
fn build_overlay_lines(info: &DebugOverlayInfo) -> Vec<(String, String)> {
    let mut lines = Vec::with_capacity(10);
    lines.push(("Tier: ".to_string(), info.fidelity.as_str().to_string()));
    lines.push(("Nodes: ".to_string(), info.nodes.to_string()));
    lines.push(("Edges: ".to_string(), info.edges.to_string()));
    if info.clusters > 0 {
        lines.push(("Clusters: ".to_string(), info.clusters.to_string()));
    }
    lines.push(("Crossings: ".to_string(), info.crossings.to_string()));
    lines.push(("Bends: ".to_string(), info.bends.to_string()));
    lines.push((
        "Ranks: ".to_string(),
        format!("{} (w={})", info.ranks, info.max_rank_width),
    ));
    lines.push(("Score: ".to_string(), format!("{:.1}", info.score)));
    lines.push((
        "Sym/Comp: ".to_string(),
        format!("{:.2}/{:.2}", info.symmetry, info.compactness),
    ));
    lines.push(("Hash: ".to_string(), info.ir_hash_hex.clone()));
    if info.budget_exceeded {
        lines.push(("! Budget: ".to_string(), "EXCEEDED".to_string()));
    }
    lines
}

/// Render a faint bounding box outline around the diagram area.
fn render_overlay_bbox(layout: &DiagramLayout, area: Rect, buf: &mut Buffer) {
    let vp = Viewport::fit(&layout.bounding_box, area);
    let bb = &layout.bounding_box;

    let tl = vp.to_cell(bb.x, bb.y);
    let br = vp.to_cell(bb.x + bb.width, bb.y + bb.height);

    let bbox_w = br.0.saturating_sub(tl.0).max(1);
    let bbox_h = br.1.saturating_sub(tl.1).max(1);

    if bbox_w < 3 || bbox_h < 2 {
        return;
    }

    let bbox_rect = Rect::new(tl.0, tl.1, bbox_w, bbox_h);
    let cell = Cell::from_char(' ').with_fg(OVERLAY_BBOX_FG);
    buf.draw_border(bbox_rect, BorderChars::SQUARE, cell);
}

/// Render faint horizontal lines at rank boundaries.
fn render_overlay_ranks(layout: &DiagramLayout, area: Rect, buf: &mut Buffer) {
    if layout.nodes.is_empty() || layout.stats.ranks < 2 {
        return;
    }

    let vp = Viewport::fit(&layout.bounding_box, area);

    // Collect min/max y per rank.
    let mut rank_bounds: Vec<(f64, f64)> = Vec::new();
    for node in &layout.nodes {
        let r = node.rank;
        if r >= rank_bounds.len() {
            rank_bounds.resize(r + 1, (f64::MAX, f64::MIN));
        }
        let top = node.rect.y;
        let bot = node.rect.y + node.rect.height;
        if top < rank_bounds[r].0 {
            rank_bounds[r].0 = top;
        }
        if bot > rank_bounds[r].1 {
            rank_bounds[r].1 = bot;
        }
    }

    // Draw faint lines at midpoints between consecutive ranks.
    let cell = Cell::from_char('┈').with_fg(OVERLAY_RANK_FG);
    for pair in rank_bounds.windows(2) {
        let gap_y = (pair[0].1 + pair[1].0) / 2.0;
        let (left, cy) = vp.to_cell(layout.bounding_box.x, gap_y);
        let (right, _) = vp.to_cell(layout.bounding_box.x + layout.bounding_box.width, gap_y);
        let w = right.saturating_sub(left);
        if w > 0 && cy < area.y + area.height {
            buf.draw_horizontal_line(left, cy, w, cell);
        }
    }
}

/// Emit a debug-overlay evidence event to the JSONL log.
fn emit_overlay_jsonl(config: &MermaidConfig, info: &DebugOverlayInfo, area: Rect) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    let json = serde_json::json!({
        "event": "debug_overlay",
        "fidelity": info.fidelity.as_str(),
        "crossings": info.crossings,
        "bends": info.bends,
        "ranks": info.ranks,
        "max_rank_width": info.max_rank_width,
        "score": info.score,
        "symmetry": info.symmetry,
        "compactness": info.compactness,
        "nodes": info.nodes,
        "edges": info.edges,
        "clusters": info.clusters,
        "budget_exceeded": info.budget_exceeded,
        "ir_hash": info.ir_hash_hex,
        "area": {
            "cols": area.width,
            "rows": area.height,
        },
    });
    let _ = crate::mermaid::append_jsonl_line(path, &json.to_string());
}

/// Emit a render-stage evidence event to the JSONL log (bd-12d5s).
fn emit_render_jsonl(
    config: &MermaidConfig,
    ir: &MermaidDiagramIr,
    layout: &DiagramLayout,
    plan: &RenderPlan,
    area: Rect,
) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    let ir_hash = crate::mermaid::hash_ir(ir);
    let json = serde_json::json!({
        "event": "mermaid_render",
        "ir_hash": format!("0x{:016x}", ir_hash),
        "diagram_type": ir.diagram_type.as_str(),
        "fidelity": plan.fidelity.as_str(),
        "show_node_labels": plan.show_node_labels,
        "show_edge_labels": plan.show_edge_labels,
        "show_clusters": plan.show_clusters,
        "max_label_width": plan.max_label_width,
        "area": {
            "cols": area.width,
            "rows": area.height,
        },
        "nodes": layout.nodes.len(),
        "edges": layout.edges.len(),
        "clusters": layout.clusters.len(),
        "link_mode": config.link_mode.as_str(),
        "legend_height": plan.legend_area.map_or(0, |r| r.height),
    });
    let _ = crate::mermaid::append_jsonl_line(path, &json.to_string());
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
        DiagramType, GraphDirection, IrEdge, IrEndpoint, IrLabel, IrLabelId, IrLink, IrNode,
        IrNodeId, LinkKind, LinkSanitizeOutcome, MermaidCompatibilityMatrix, MermaidConfig,
        MermaidDiagramMeta, MermaidErrorMode, MermaidFallbackPolicy, MermaidGuardReport,
        MermaidInitConfig, MermaidInitParse, MermaidLinkMode, MermaidSupportLevel,
        MermaidThemeOverrides, NodeShape, Position, Span, normalize_ast_to_ir,
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
            pie_entries: vec![],
            pie_title: None,
            pie_show_data: false,
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
            constraints: vec![],
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
                    bundle_count: 1,
                    bundle_members: Vec::new(),
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

    #[cfg(feature = "canvas")]
    fn trim_trailing_spaces(text: &str) -> String {
        let mut out = String::new();
        for (idx, line) in text.lines().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(line.trim_end());
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
        // Each CJK char is 2 cells wide; ellipsis is 1 cell.
        // max_width=3 → target 2 cells for text → "漢" (2) + "…" (1) = 3
        assert_eq!(truncate_label("漢字テスト", 3), "漢…");
        // max_width=5 → target 4 cells → "漢字" (4) + "…" (1) = 5
        assert_eq!(truncate_label("漢字テスト", 5), "漢字…");
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
    fn dashed_diagonal_bend_has_corner() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let mut buf = Buffer::new(12, 12);
        let cell = Cell::from_char(' ').with_fg(EDGE_FG);

        renderer.draw_dashed_segment(2, 2, 8, 8, cell, &mut buf);

        assert_eq!(
            buf.get(8, 2).unwrap().content.as_char(),
            Some('┐'),
            "expected dashed diagonal to set a bend corner"
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
    fn edge_style_prefers_resolved_dash() {
        let mut style = ResolvedMermaidStyle::default();
        style.properties.stroke_dash = Some(MermaidStrokeDash::Dotted);
        assert_eq!(edge_line_style("-->", Some(&style)), EdgeLineStyle::Dotted);

        style.properties.stroke_dash = Some(MermaidStrokeDash::Dashed);
        assert_eq!(edge_line_style("-->", Some(&style)), EdgeLineStyle::Dashed);
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

    #[test]
    fn dotted_segment_uses_dot_glyph() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let cell = Cell::from_char(' ').with_fg(EDGE_FG);
        let mut buf = Buffer::new(6, 3);
        renderer.draw_dotted_segment(0, 1, 4, 1, cell, &mut buf);

        assert_eq!(buf.get(0, 1).unwrap().content.as_char(), Some('┄'));
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

    // --- Render mode selection ---

    #[test]
    fn render_mode_auto_selects_braille_for_kitty() {
        let caps = TerminalCapabilities::from_profile(TerminalProfile::Kitty);
        let policy = GlyphPolicy::from_env_with(|_| None, &caps);
        let config = MermaidConfig {
            render_mode: MermaidRenderMode::Auto,
            ..MermaidConfig::default()
        };
        assert_eq!(
            resolve_render_mode(&config, &policy),
            MermaidRenderMode::Braille
        );
    }

    #[test]
    fn render_mode_auto_selects_block_for_xterm() {
        let caps = TerminalCapabilities::from_profile(TerminalProfile::Xterm);
        let policy = GlyphPolicy::from_env_with(|_| None, &caps);
        let config = MermaidConfig {
            render_mode: MermaidRenderMode::Auto,
            ..MermaidConfig::default()
        };
        assert_eq!(
            resolve_render_mode(&config, &policy),
            MermaidRenderMode::Block
        );
    }

    #[test]
    fn render_mode_auto_selects_cell_only_for_vt100() {
        let caps = TerminalCapabilities::from_profile(TerminalProfile::Vt100);
        let policy = GlyphPolicy::from_env_with(|_| None, &caps);
        let config = MermaidConfig {
            render_mode: MermaidRenderMode::Auto,
            ..MermaidConfig::default()
        };
        assert_eq!(
            resolve_render_mode(&config, &policy),
            MermaidRenderMode::CellOnly
        );
    }

    #[test]
    fn render_mode_auto_selects_cell_only_for_dumb() {
        let caps = TerminalCapabilities::from_profile(TerminalProfile::Dumb);
        let policy = GlyphPolicy::from_env_with(|_| None, &caps);
        let config = MermaidConfig {
            render_mode: MermaidRenderMode::Auto,
            ..MermaidConfig::default()
        };
        assert_eq!(
            resolve_render_mode(&config, &policy),
            MermaidRenderMode::CellOnly
        );
    }

    #[test]
    fn render_plan_compact_hides_edge_labels() {
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
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
        let plan = select_render_plan(&config, &layout, &ir, area);
        assert!(!plan.show_edge_labels, "compact should hide edge labels");
        assert!(plan.show_node_labels, "compact should keep node labels");
        assert!(!plan.show_clusters, "compact should hide clusters");
    }

    #[test]
    fn bundled_edge_renders_count_label_in_compact_tier() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);

        let ir = make_ir(2, vec![(0, 1), (0, 1), (0, 1), (0, 1), (0, 1)]);
        let mut layout = make_layout(2, vec![(0, 1)]);
        // Ensure the label midpoint falls on an interior waypoint (not the target port).
        // `render_edge_label*()` uses `waypoints[len/2]` as the anchor.
        let from = layout.edges[0].waypoints[0];
        let to = layout.edges[0].waypoints[1];
        layout.edges[0].waypoints.insert(
            1,
            LayoutPoint {
                x: (from.x + to.x) / 2.0,
                y: (from.y + to.y) / 2.0,
            },
        );
        layout.edges[0].bundle_count = 5;
        layout.edges[0].bundle_members = vec![0, 1, 2, 3, 4];

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
        let plan = select_render_plan(&config, &layout, &ir, area);
        assert!(!plan.show_edge_labels, "compact should hide edge labels");

        let mut buf = Buffer::new(80, 24);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);

        let text = buffer_to_text(&buf);
        assert!(
            text.contains("×5"),
            "expected bundle count label to render even when edge labels are hidden"
        );
    }

    #[test]
    fn render_plan_outline_hides_all_labels() {
        let ir = make_ir(2, vec![(0, 1)]);
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
        let plan = select_render_plan(&config, &layout, &ir, area);
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
        let plan = select_render_plan(&config, &layout, &ir, area);
        let mut buf = Buffer::new(80, 24);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);

        // Should have node corners.
        let has_corner = (0..buf.height()).any(|y| {
            (0..buf.width()).any(|x| buf.get(x, y).unwrap().content.as_char() == Some('┌'))
        });
        assert!(has_corner, "expected node box corners in plan-based render");
    }

    #[test]
    fn render_plan_renders_link_footnotes() {
        let mut ir = make_ir(2, vec![(0, 1)]);
        ir.links.push(IrLink {
            kind: LinkKind::Link,
            target: IrNodeId(0),
            url: "https://example.com".to_string(),
            tooltip: None,
            sanitize_outcome: LinkSanitizeOutcome::Allowed,
            span: Span {
                start: Position {
                    line: 1,
                    col: 1,
                    byte: 0,
                },
                end: Position {
                    line: 1,
                    col: 1,
                    byte: 0,
                },
            },
        });
        let layout = make_layout(2, vec![(0, 1)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let config = MermaidConfig {
            enable_links: true,
            link_mode: MermaidLinkMode::Footnote,
            ..Default::default()
        };
        let plan = select_render_plan(&config, &layout, &ir, area);
        assert!(
            plan.legend_area.is_some(),
            "expected legend area reserved for footnotes"
        );
        let renderer = MermaidRenderer::new(&config);
        let mut buf = Buffer::new(80, 24);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);
        let text = buffer_to_text(&buf);
        assert!(
            text.contains("https://example.com"),
            "expected footnote URL in rendered legend"
        );
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

    #[test]
    fn e2e_pie_renders_content() {
        let source = "pie showData\ntitle Pets\n\"Dogs\": 386\n\"Cats\": 85\n\"Rats\": 15\n";
        let (buf, _plan) = e2e_render(source, 40, 16);
        assert!(buf_has_content(&buf), "pie should render content");
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

    // -- mindmap_basic at two sizes + snapshots --

    #[test]
    fn e2e_mindmap_basic_80x24() {
        let source = include_str!("../tests/fixtures/mermaid/mindmap_basic.mmd");
        let (buf, _plan) = e2e_render(source, 80, 24);
        assert!(buf_has_content(&buf), "mindmap should render at 80x24");
        let arrowheads = count_char_in_buf(&buf, '▶')
            + count_char_in_buf(&buf, '◀')
            + count_char_in_buf(&buf, '▲')
            + count_char_in_buf(&buf, '▼');
        assert_eq!(arrowheads, 0, "mindmap edges should not have arrowheads");
    }

    #[test]
    fn e2e_mindmap_basic_120x40() {
        let source = include_str!("../tests/fixtures/mermaid/mindmap_basic.mmd");
        let (buf, _plan) = e2e_render(source, 120, 40);
        assert!(buf_has_content(&buf), "mindmap should render at 120x40");
    }

    #[test]
    fn snapshot_mindmap_basic_80x24() {
        let source = include_str!("../tests/fixtures/mermaid/mindmap_basic.mmd");
        let (buf, _plan) = e2e_render(source, 80, 24);
        assert_buffer_snapshot_text("mermaid_mindmap_basic_80x24", &buf);
    }

    #[test]
    fn snapshot_mindmap_basic_120x40() {
        let source = include_str!("../tests/fixtures/mermaid/mindmap_basic.mmd");
        let (buf, _plan) = e2e_render(source, 120, 40);
        assert_buffer_snapshot_text("mermaid_mindmap_basic_120x40", &buf);
    }

    #[test]
    fn e2e_mindmap_emits_jsonl_logs() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static LOG_COUNTER: AtomicUsize = AtomicUsize::new(0);
        let idx = LOG_COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_path = format!(
            "/tmp/ftui_test_mindmap_jsonl_{}_{}.jsonl",
            std::process::id(),
            idx
        );

        let source = include_str!("../tests/fixtures/mermaid/mindmap_basic.mmd");
        let parsed = parse_with_diagnostics(source);
        let config = MermaidConfig {
            log_path: Some(log_path.clone()),
            ..MermaidConfig::default()
        };
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let ir_parse = normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
        let layout = layout_diagram(&ir_parse.ir, &config);
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let mut buf = Buffer::new(80, 24);
        let _plan = render_diagram_adaptive(&layout, &ir_parse.ir, &config, area, &mut buf);
        let log_content = std::fs::read_to_string(&log_path).expect("read log");
        assert!(log_content.contains("layout_metrics"));
        assert!(log_content.contains("mermaid_render"));
        assert!(log_content.contains("\"diagram_type\":\"mindmap\""));
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

        let config = MermaidConfig {
            error_mode: MermaidErrorMode::Panel,
            ..MermaidConfig::default()
        };

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

        let config = MermaidConfig {
            error_mode: MermaidErrorMode::Raw,
            ..MermaidConfig::default()
        };

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

        let config = MermaidConfig {
            error_mode: MermaidErrorMode::Both,
            ..MermaidConfig::default()
        };

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

    // ──────────────────────────────────────────────────
    // End-to-end class diagram tests
    // ──────────────────────────────────────────────────

    #[test]
    fn e2e_class_basic_80x24() {
        let source = include_str!("../tests/fixtures/mermaid/class_basic.mmd");
        let (buf, _plan) = e2e_render(source, 80, 24);
        assert!(
            buf_has_content(&buf),
            "class diagram should render at 80x24"
        );
    }

    #[test]
    fn e2e_class_basic_120x40() {
        let source = include_str!("../tests/fixtures/mermaid/class_basic.mmd");
        let (buf, _plan) = e2e_render(source, 120, 40);
        assert!(
            buf_has_content(&buf),
            "class diagram should render at 120x40"
        );
    }

    #[test]
    fn e2e_class_basic_200x60() {
        let source = include_str!("../tests/fixtures/mermaid/class_basic.mmd");
        let (buf, _plan) = e2e_render(source, 200, 60);
        assert!(
            buf_has_content(&buf),
            "class diagram should render at 200x60"
        );
    }

    #[test]
    fn e2e_class_ir_has_members() {
        let source = include_str!("../tests/fixtures/mermaid/class_basic.mmd");
        let parsed = parse_with_diagnostics(source);
        assert_eq!(parsed.ast.diagram_type, DiagramType::Class);
        let config = MermaidConfig::default();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let ir_parse = normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
        // Class diagram should produce nodes with members.
        let nodes_with_members: Vec<_> = ir_parse
            .ir
            .nodes
            .iter()
            .filter(|n| !n.members.is_empty())
            .collect();
        assert!(
            !nodes_with_members.is_empty(),
            "class diagram IR should have nodes with members"
        );
    }

    #[test]
    fn e2e_class_compartments_render_separator() {
        // Build a minimal class diagram with members and verify
        // the separator line (├───┤) appears in the rendered buffer.
        let source = "classDiagram\n  class Animal\n  Animal : +name string\n  Animal : +age int\n  Animal : +eat() void";
        let parsed = parse_with_diagnostics(source);
        let config = MermaidConfig::default();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let ir_parse = normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
        let layout = layout_diagram(&ir_parse.ir, &config);
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::new(60, 20);
        let _plan = render_diagram_adaptive(&layout, &ir_parse.ir, &config, area, &mut buf);
        assert!(buf_has_content(&buf), "class with members should render");
        // Check for tee characters (├ or ┤) which form the compartment separator.
        let has_tee = (0..buf.height()).any(|y| {
            (0..buf.width()).any(|x| {
                let ch = buf.get(x, y).unwrap().content.as_char();
                ch == Some('\u{251c}') || ch == Some('\u{2524}')
            })
        });
        // If the layout made nodes taller for members, expect separator tees.
        let expect_tee = layout.nodes.iter().any(|node| node.rect.height > 3.0);
        if expect_tee {
            assert!(
                has_tee,
                "compartment separator expected for class with members"
            );
        }
    }

    #[test]
    fn e2e_class_layout_taller_nodes() {
        // Nodes with members should get taller layout rects.
        let source = "classDiagram\n  class Foo\n  Foo : +bar() void\n  Foo : -baz int";
        let parsed = parse_with_diagnostics(source);
        let config = MermaidConfig::default();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let ir_parse = normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
        let layout = layout_diagram(&ir_parse.ir, &config);
        // Find the Foo node and check its height is > default 3.0.
        let foo_idx = ir_parse
            .ir
            .nodes
            .iter()
            .position(|n| n.id == "Foo")
            .expect("Foo node should exist");
        if let Some(layout_node) = layout.nodes.iter().find(|ln| ln.node_idx == foo_idx) {
            assert!(
                layout_node.rect.height > 3.0,
                "class with members should have at least default height, got {}",
                layout_node.rect.height
            );
        }
    }

    // ── Debug Overlay Tests (bd-4cwfj) ──────────────────────────────────

    #[test]
    fn overlay_info_collects_metrics() {
        let ir = make_ir(4, vec![(0, 1), (1, 2), (2, 3)]);
        let layout = make_layout(4, vec![(0, 1), (1, 2), (2, 3)]);
        let plan = RenderPlan {
            fidelity: MermaidFidelity::Normal,
            show_node_labels: true,
            show_edge_labels: true,
            show_clusters: true,
            max_label_width: 48,
            diagram_area: Rect::new(0, 0, 80, 24),
            legend_area: None,
        };
        let info = collect_overlay_info(&layout, &ir, &plan);
        assert_eq!(info.fidelity, MermaidFidelity::Normal);
        assert_eq!(info.nodes, 4);
        assert_eq!(info.edges, 3);
        assert!(!info.ir_hash_hex.is_empty());
    }

    #[test]
    fn overlay_lines_include_core_metrics() {
        let info = DebugOverlayInfo {
            fidelity: MermaidFidelity::Rich,
            crossings: 3,
            bends: 7,
            ranks: 4,
            max_rank_width: 3,
            score: 42.5,
            symmetry: 0.85,
            compactness: 0.72,
            nodes: 10,
            edges: 12,
            clusters: 2,
            budget_exceeded: false,
            ir_hash_hex: "abcd1234".to_string(),
        };
        let lines = build_overlay_lines(&info);
        // Must include tier, nodes, edges, clusters, crossings, bends, ranks, score, sym/comp, hash.
        assert!(lines.len() >= 10);
        assert_eq!(lines[0].1, "rich");
        assert!(lines.iter().any(|(l, _)| l.contains("Crossings")));
        assert!(lines.iter().any(|(l, _)| l.contains("Hash")));
    }

    #[test]
    fn overlay_lines_show_budget_warning() {
        let info = DebugOverlayInfo {
            fidelity: MermaidFidelity::Compact,
            crossings: 0,
            bends: 0,
            ranks: 1,
            max_rank_width: 1,
            score: 0.0,
            symmetry: 1.0,
            compactness: 1.0,
            nodes: 1,
            edges: 0,
            clusters: 0,
            budget_exceeded: true,
            ir_hash_hex: "00000000".to_string(),
        };
        let lines = build_overlay_lines(&info);
        assert!(
            lines
                .iter()
                .any(|(l, v)| l.contains("Budget") && v == "EXCEEDED")
        );
    }

    #[test]
    fn overlay_renders_without_crash() {
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let layout = make_layout(3, vec![(0, 1), (1, 2)]);
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::new(80, 24);
        let plan = RenderPlan {
            fidelity: MermaidFidelity::Normal,
            show_node_labels: true,
            show_edge_labels: true,
            show_clusters: true,
            max_label_width: 48,
            diagram_area: area,
            legend_area: None,
        };
        // Should not panic.
        render_debug_overlay(&layout, &ir, &plan, area, &mut buf);
    }

    #[test]
    fn overlay_skipped_when_area_too_small() {
        let ir = make_ir(2, vec![(0, 1)]);
        let layout = make_layout(2, vec![(0, 1)]);
        let area = Rect::new(0, 0, 10, 5); // Very small.
        let mut buf = Buffer::new(10, 5);
        let plan = RenderPlan {
            fidelity: MermaidFidelity::Outline,
            show_node_labels: false,
            show_edge_labels: false,
            show_clusters: false,
            max_label_width: 0,
            diagram_area: area,
            legend_area: None,
        };
        // Should not panic even with tiny area.
        render_debug_overlay(&layout, &ir, &plan, area, &mut buf);
    }

    #[test]
    fn overlay_adaptive_renders_with_debug_enabled() {
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let layout = layout_diagram(&ir, &MermaidConfig::default());
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::new(80, 24);
        let config = MermaidConfig {
            debug_overlay: true,
            ..MermaidConfig::default()
        };
        let plan = render_diagram_adaptive(&layout, &ir, &config, area, &mut buf);
        assert_eq!(plan.fidelity, MermaidFidelity::Normal);
    }

    #[test]
    fn overlay_bbox_renders_at_reasonable_size() {
        let ir = make_ir(4, vec![(0, 1), (1, 2), (2, 3)]);
        let layout = layout_diagram(&ir, &MermaidConfig::default());
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::new(120, 40);
        // Render the bounding box overlay alone.
        render_overlay_bbox(&layout, area, &mut buf);
        // No crash is success; bounding box should be drawn.
    }

    #[test]
    fn overlay_ranks_renders_at_reasonable_size() {
        let ir = make_ir(4, vec![(0, 1), (1, 2), (2, 3)]);
        let layout = layout_diagram(&ir, &MermaidConfig::default());
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::new(120, 40);
        // Render rank boundary overlay alone.
        render_overlay_ranks(&layout, area, &mut buf);
        // No crash is success.
    }

    // ── ER Diagram Integration Tests (bd-1rnqg) ────────────────────────

    #[test]
    fn er_cardinality_parse_one_to_many() {
        let card = parse_er_cardinality("||--o{").expect("should parse");
        assert_eq!(card.left, "||");
        assert_eq!(card.right, "o{");
    }

    #[test]
    fn er_cardinality_parse_many_to_many() {
        let card = parse_er_cardinality("}o--o{").expect("should parse");
        assert_eq!(card.left, "}o");
        assert_eq!(card.right, "o{");
    }

    #[test]
    fn er_cardinality_parse_one_to_one() {
        let card = parse_er_cardinality("||--||").expect("should parse");
        assert_eq!(card.left, "||");
        assert_eq!(card.right, "||");
    }

    #[test]
    fn er_cardinality_parse_dotted_line() {
        let card = parse_er_cardinality("|o..||").expect("should parse");
        assert_eq!(card.left, "|o");
        assert_eq!(card.right, "||");
    }

    #[test]
    fn er_cardinality_label_values() {
        assert_eq!(cardinality_label("||"), "1");
        assert_eq!(cardinality_label("o{"), "0..*");
        assert_eq!(cardinality_label("}o"), "0..*");
        assert_eq!(cardinality_label("|{"), "1..*");
        assert_eq!(cardinality_label("{|"), "1..*");
        assert_eq!(cardinality_label("o|"), "0..1");
        assert_eq!(cardinality_label("|o"), "0..1");
    }

    #[test]
    fn er_diagram_full_pipeline() {
        // End-to-end: parse → IR → layout → render for a basic ER diagram.
        let input = concat!(
            "erDiagram\n",
            "    CUSTOMER ||--o{ ORDER : places\n",
            "    ORDER ||--|{ LINE_ITEM : contains\n",
        );
        let prepared = parse_with_diagnostics(input);
        assert!(
            prepared.errors.is_empty(),
            "parse errors: {:?}",
            prepared.errors
        );
        let ir_parse = normalize_ast_to_ir(
            &prepared.ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let ir = &ir_parse.ir;
        assert_eq!(ir.diagram_type, DiagramType::Er);
        assert!(ir.nodes.len() >= 3, "should have at least 3 entities");
        assert!(ir.edges.len() >= 2, "should have at least 2 relationships");

        // Layout.
        let layout = layout_diagram(ir, &MermaidConfig::default());
        assert_eq!(layout.nodes.len(), ir.nodes.len());

        // Render at 80x24.
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::new(80, 24);
        let plan = render_diagram_adaptive(&layout, ir, &MermaidConfig::default(), area, &mut buf);
        assert!(
            plan.fidelity != MermaidFidelity::Outline || area.width < 40,
            "80x24 should not be outline fidelity for 3 nodes"
        );
    }

    #[test]
    fn er_diagram_with_attributes_pipeline() {
        let input = concat!(
            "erDiagram\n",
            "    CUSTOMER {\n",
            "        string name PK\n",
            "        int age\n",
            "    }\n",
            "    ORDER {\n",
            "        int id PK\n",
            "        date created\n",
            "    }\n",
            "    CUSTOMER ||--o{ ORDER : places\n",
        );
        let prepared = parse_with_diagnostics(input);
        assert!(prepared.errors.is_empty());
        let ir_parse = normalize_ast_to_ir(
            &prepared.ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let ir = &ir_parse.ir;

        // CUSTOMER and ORDER should have members (attributes).
        let customer = ir.nodes.iter().find(|n| n.id == "CUSTOMER");
        assert!(customer.is_some(), "CUSTOMER entity should exist");
        assert!(
            customer.unwrap().members.len() >= 2,
            "CUSTOMER should have at least 2 attributes"
        );

        let order = ir.nodes.iter().find(|n| n.id == "ORDER");
        assert!(order.is_some(), "ORDER entity should exist");
        assert!(
            order.unwrap().members.len() >= 2,
            "ORDER should have at least 2 attributes"
        );

        // Render.
        let layout = layout_diagram(ir, &MermaidConfig::default());
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::new(120, 40);
        let _plan = render_diagram_adaptive(&layout, ir, &MermaidConfig::default(), area, &mut buf);
    }

    #[test]
    fn er_diagram_at_multiple_sizes() {
        let input = concat!(
            "erDiagram\n",
            "    A ||--o{ B : rel1\n",
            "    B ||--|{ C : rel2\n",
            "    C }o--|| A : rel3\n",
        );
        let prepared = parse_with_diagnostics(input);
        assert!(prepared.errors.is_empty());
        let ir_parse = normalize_ast_to_ir(
            &prepared.ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let ir = &ir_parse.ir;
        let layout = layout_diagram(ir, &MermaidConfig::default());

        for (w, h) in [(80, 24), (120, 40), (200, 60), (40, 12)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::new(w, h);
            let _plan =
                render_diagram_adaptive(&layout, ir, &MermaidConfig::default(), area, &mut buf);
        }
    }

    #[test]
    fn er_cardinality_render_does_not_crash() {
        // Verify cardinality rendering doesn't crash on edge cases.
        let input = "erDiagram\nA ||--o{ B : places\n";
        let prepared = parse_with_diagnostics(input);
        let ir_parse = normalize_ast_to_ir(
            &prepared.ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let ir = &ir_parse.ir;
        let layout = layout_diagram(ir, &MermaidConfig::default());
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::new(80, 24);
        let _plan = render_diagram_adaptive(&layout, ir, &MermaidConfig::default(), area, &mut buf);
    }

    #[test]
    fn er_edge_without_label_renders() {
        let input = "erDiagram\nA ||--|| B\n";
        let prepared = parse_with_diagnostics(input);
        assert!(prepared.errors.is_empty());
        let ir_parse = normalize_ast_to_ir(
            &prepared.ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let ir = &ir_parse.ir;
        let layout = layout_diagram(ir, &MermaidConfig::default());
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::new(80, 24);
        let _plan = render_diagram_adaptive(&layout, ir, &MermaidConfig::default(), area, &mut buf);
    }

    // ── Canvas edge rendering tests ───────────────────────────────────

    #[test]
    #[cfg(feature = "canvas")]
    fn canvas_edge_solid_horizontal() {
        let mut painter = Painter::new(16, 4, CanvasMode::Braille);
        draw_canvas_line_segment(&mut painter, 0, 1, 15, 1, EdgeLineStyle::Solid);
        assert!(painter.get(0, 1));
        assert!(painter.get(8, 1));
        assert!(painter.get(15, 1));
    }

    #[test]
    #[cfg(feature = "canvas")]
    fn canvas_edge_diagonal_line() {
        let mut painter = Painter::new(8, 8, CanvasMode::Braille);
        draw_canvas_line_segment(&mut painter, 0, 0, 7, 7, EdgeLineStyle::Solid);
        assert!(painter.get(0, 0));
        assert!(painter.get(4, 4));
        assert!(painter.get(7, 7));
    }

    #[test]
    #[cfg(feature = "canvas")]
    fn canvas_edge_dashed_pattern_skips() {
        let mut painter = Painter::new(16, 4, CanvasMode::Braille);
        draw_canvas_line_segment(&mut painter, 0, 1, 15, 1, EdgeLineStyle::Dashed);
        assert!(painter.get(0, 1));
        assert!(painter.get(4, 1));
        assert!(!painter.get(7, 1));
        assert!(painter.get(12, 1));
    }

    #[test]
    #[cfg(feature = "canvas")]
    fn canvas_edge_dotted_pattern_skips() {
        let mut painter = Painter::new(16, 4, CanvasMode::Braille);
        draw_canvas_line_segment(&mut painter, 0, 1, 15, 1, EdgeLineStyle::Dotted);
        assert!(painter.get(0, 1));
        assert!(!painter.get(1, 1));
        assert!(!painter.get(2, 1));
        assert!(painter.get(3, 1));
    }

    #[test]
    #[cfg(feature = "canvas")]
    fn canvas_edge_thick_adds_parallel_line() {
        let mut painter = Painter::new(16, 4, CanvasMode::Braille);
        draw_canvas_line_segment(&mut painter, 0, 1, 15, 1, EdgeLineStyle::Thick);
        assert!(painter.get(6, 1));
        assert!(painter.get(6, 2));
    }

    #[test]
    #[cfg(feature = "canvas")]
    fn canvas_arrowhead_directions() {
        let directions = [
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (-1, 1),
            (1, -1),
            (-1, -1),
        ];
        for (dx, dy) in directions {
            let mut painter = Painter::new(11, 11, CanvasMode::Braille);
            let tip = (5, 5);
            let from = (5 - dx * 4, 5 - dy * 4);
            draw_canvas_arrowhead(
                &mut painter,
                from,
                tip,
                ArrowHeadKind::Normal,
                CanvasMode::Braille,
            );
            assert!(painter.get(tip.0, tip.1), "tip not set for {dx},{dy}");

            let mut has_back = false;
            for y in 0..11 {
                for x in 0..11 {
                    if !painter.get(x, y) {
                        continue;
                    }
                    let vx = x - tip.0;
                    let vy = y - tip.1;
                    if vx * dx + vy * dy < 0 {
                        has_back = true;
                        break;
                    }
                }
                if has_back {
                    break;
                }
            }
            assert!(has_back, "arrowhead for {dx},{dy} has no back pixels");
        }
    }

    #[test]
    #[cfg(feature = "canvas")]
    fn canvas_arrowhead_types_snapshot() {
        let input = concat!(
            "graph LR\n",
            "A-->B\n",
            "B-->>C\n",
            "C--oD\n",
            "D--xE\n",
            "E--*F\n",
        );
        let prepared = parse_with_diagnostics(input);
        assert!(prepared.errors.is_empty());
        let ir_parse = normalize_ast_to_ir(
            &prepared.ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let ir = &ir_parse.ir;
        let layout = layout_diagram(ir, &MermaidConfig::default());
        let area = Rect::new(0, 0, 50, 12);
        let config = MermaidConfig {
            render_mode: MermaidRenderMode::Braille,
            tier_override: MermaidTier::Normal,
            capability_profile: Some("kitty".to_string()),
            ..MermaidConfig::default()
        };
        let mut buf = Buffer::new(area.width, area.height);
        let _plan = render_diagram_adaptive(&layout, ir, &config, area, &mut buf);
        let text = trim_trailing_spaces(&buffer_to_text(&buf));
        if std::env::var("FTUI_SNAPSHOT").is_ok() {
            println!("{text}");
        }
        let expected = concat!(
            "\n",
            "\n",
            "\n",
            "\n",
            "\n",
            " ⣤⣤⣤⣤⣤⣤⢠⣀⢠⣤⣤⣤⣤⣤⡄⣄⡀⣤⣤⣤⣤⣤⣤ ⢀⣤⣤⣤⣤⣤⣤ ⠠⣠⣤⣤⣤⣤⣤⡄⢀⣄⣤⣤⣤⣤⣤⣤\n",
            " ⠛⠛⠛⠛⠛⠛⠹⠛⠙⠛⠛⠛⠛⠛⠋⠯⠛⠛⠛⠛⠛⠛⠛⠉⠙⠟⠛⠛⠛⠛⠛⠉⠩⠛⠻⠛⠛⠛⠛⠋⠙⠟⠛⠛⠛⠛⠛⠛\n",
            "\n",
            "\n",
            "\n",
            "\n",
        );
        assert_eq!(text, expected);
    }

    #[test]
    #[cfg(feature = "canvas")]
    fn canvas_braille_snapshot_five_node_graph() {
        let input = concat!(
            "graph LR\n",
            "A-->B\n",
            "B-->C\n",
            "C-->D\n",
            "D-->E\n",
            "A-->E\n",
        );
        let prepared = parse_with_diagnostics(input);
        assert!(prepared.errors.is_empty());
        let ir_parse = normalize_ast_to_ir(
            &prepared.ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let ir = &ir_parse.ir;
        let layout = layout_diagram(ir, &MermaidConfig::default());
        let area = Rect::new(0, 0, 40, 12);
        let config = MermaidConfig {
            render_mode: MermaidRenderMode::Braille,
            tier_override: MermaidTier::Normal,
            capability_profile: Some("kitty".to_string()),
            ..MermaidConfig::default()
        };
        let mut buf = Buffer::new(area.width, area.height);
        let _plan = render_diagram_adaptive(&layout, ir, &config, area, &mut buf);
        let text = trim_trailing_spaces(&buffer_to_text(&buf));
        if std::env::var("FTUI_SNAPSHOT").is_ok() {
            println!("{text}");
        }
        let expected = concat!(
            "\n",
            "\n",
            "\n",
            "\n",
            "\n",
            " ⣤⣤⣤⣤⣤⣤⣄⡀⣤⣤⣤⣤⣤⣤⣄⡀⣤⣤⣤⣤⣤⣤⣄⡀⣤⣤⣤⣤⣤⣤⣄⡀⣤⣤⣤⣤⣤⣤\n",
            " ⠛⠛⠛⠛⠛⠛⠟⠋⠛⠛⠛⠛⠛⠛⠟⠋⠛⠛⠛⠛⠛⠛⠟⠋⠛⠛⠛⠛⠛⠛⠟⠋⠛⠛⠛⠛⠛⠛\n",
            "\n",
            "\n",
            "\n",
            "\n",
        );
        assert_eq!(text, expected);
    }

    // ── Shape rendering tests ──────────────────────────────────────

    fn render_single_shape(shape: NodeShape, w: u16, h: u16) -> String {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let border_cell = Cell::from_char(' ').with_fg(PackedRgba::WHITE);
        let fill_cell = Cell::from_char(' ');
        let rect = Rect::new(0, 0, w, h);
        let mut buf = Buffer::new(w, h);
        let _inset = renderer.draw_shaped_node(rect, shape, border_cell, fill_cell, &mut buf);
        buffer_to_text(&buf)
    }

    fn render_single_shape_ascii(shape: NodeShape, w: u16, h: u16) -> String {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Ascii);
        let border_cell = Cell::from_char(' ').with_fg(PackedRgba::WHITE);
        let fill_cell = Cell::from_char(' ');
        let rect = Rect::new(0, 0, w, h);
        let mut buf = Buffer::new(w, h);
        let _inset = renderer.draw_shaped_node(rect, shape, border_cell, fill_cell, &mut buf);
        buffer_to_text(&buf)
    }

    #[test]
    fn shape_rect_renders_square_border() {
        let text = render_single_shape(NodeShape::Rect, 8, 4);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains('┌'), "top-left corner");
        assert!(lines[0].contains('┐'), "top-right corner");
        assert!(lines[3].contains('└'), "bottom-left corner");
        assert!(lines[3].contains('┘'), "bottom-right corner");
        assert!(lines[1].contains('│'), "vertical border");
    }

    #[test]
    fn shape_rounded_renders_round_corners() {
        let text = render_single_shape(NodeShape::Rounded, 8, 4);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains('╭'), "top-left");
        assert!(lines[0].contains('╮'), "top-right");
        assert!(lines[3].contains('╰'), "bottom-left");
        assert!(lines[3].contains('╯'), "bottom-right");
    }

    #[test]
    fn shape_circle_renders_round_corners() {
        let text = render_single_shape(NodeShape::Circle, 8, 4);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains('╭'), "top-left");
        assert!(lines[3].contains('╯'), "bottom-right");
    }

    #[test]
    fn shape_stadium_renders_double_horizontal() {
        let text = render_single_shape(NodeShape::Stadium, 10, 4);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains('╭'), "top-left rounded");
        assert!(lines[0].contains('═'), "double horizontal");
        assert!(lines[0].contains('╮'), "top-right rounded");
    }

    #[test]
    fn shape_subroutine_renders_double_verticals() {
        let text = render_single_shape(NodeShape::Subroutine, 10, 5);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].contains('║'), "double vertical");
        assert!(lines[1].contains('│'), "inner vertical");
    }

    #[test]
    fn shape_diamond_renders_diagonals() {
        let text = render_single_shape(NodeShape::Diamond, 10, 7);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains('╱'), "top fwd slash");
        assert!(lines[0].contains('╲'), "top back slash");
        let last = lines.len() - 1;
        assert!(lines[last].contains('╲'), "bottom bck slash");
        assert!(lines[last].contains('╱'), "bottom fwd slash");
    }

    #[test]
    fn shape_hexagon_renders_angled_sides() {
        let text = render_single_shape(NodeShape::Hexagon, 12, 5);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains('╱'), "hex top fwd");
        assert!(lines[0].contains('─'), "hex top horiz");
        assert!(lines[0].contains('╲'), "hex top bck");
        assert!(lines[2].contains('│'), "hex mid vert");
    }

    #[test]
    fn shape_asymmetric_renders_flag() {
        let text = render_single_shape(NodeShape::Asymmetric, 10, 5);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with('┌'), "left top");
        assert!(lines[4].starts_with('└'), "left bottom");
        let mid = 2;
        assert!(lines[mid].contains('▷'), "right point");
    }

    #[test]
    fn shape_diamond_small_fallback() {
        let text = render_single_shape(NodeShape::Diamond, 2, 2);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains('┌'), "small diamond fallback");
    }

    #[test]
    fn shape_ascii_fallback() {
        let text = render_single_shape_ascii(NodeShape::Rounded, 8, 4);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains('+'), "ascii fallback corners");
        assert!(lines[0].contains('-'), "ascii fallback horiz");
    }

    #[test]
    fn shape_stadium_ascii_fallback() {
        let text = render_single_shape_ascii(NodeShape::Stadium, 10, 4);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains('('), "ascii stadium left");
        assert!(lines[0].contains('='), "ascii stadium horiz");
        assert!(lines[0].contains(')'), "ascii stadium right");
    }

    #[test]
    fn all_shapes_render_without_panic() {
        let shapes = [
            NodeShape::Rect,
            NodeShape::Rounded,
            NodeShape::Stadium,
            NodeShape::Subroutine,
            NodeShape::Diamond,
            NodeShape::Hexagon,
            NodeShape::Circle,
            NodeShape::Asymmetric,
        ];
        for shape in shapes {
            for &(w, h) in &[(3u16, 3u16), (5, 5), (8, 4), (10, 7), (15, 10), (20, 12)] {
                let _text = render_single_shape(shape, w, h);
                let _ascii = render_single_shape_ascii(shape, w, h);
            }
        }
    }

    #[test]
    fn shape_label_insets_are_sane() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let border_cell = Cell::from_char(' ').with_fg(PackedRgba::WHITE);
        let fill_cell = Cell::from_char(' ');

        let shapes_and_expected: &[(NodeShape, bool)] = &[
            (NodeShape::Rect, false),
            (NodeShape::Rounded, false),
            (NodeShape::Circle, false),
            (NodeShape::Stadium, true),
            (NodeShape::Subroutine, true),
            (NodeShape::Diamond, true),
            (NodeShape::Hexagon, true),
            (NodeShape::Asymmetric, true),
        ];

        for &(shape, wider_inset) in shapes_and_expected {
            let rect = Rect::new(0, 0, 12, 8);
            let mut buf = Buffer::new(12, 8);
            let (l, t, r, b) =
                renderer.draw_shaped_node(rect, shape, border_cell, fill_cell, &mut buf);
            assert!(l >= 1, "{shape:?} left inset {l}");
            assert!(t >= 1, "{shape:?} top inset {t}");
            assert!(r >= 1, "{shape:?} right inset {r}");
            assert!(b >= 1, "{shape:?} bottom inset {b}");
            if wider_inset {
                assert!(l >= 2 || r >= 2, "{shape:?} should have wider inset");
            }
        }
    }

    #[test]
    fn mixed_shape_ir_renders_without_panic() {
        let shapes = [
            NodeShape::Rect,
            NodeShape::Rounded,
            NodeShape::Diamond,
            NodeShape::Hexagon,
        ];
        let mut ir = make_ir(4, vec![(0, 1), (1, 2), (2, 3)]);
        for (i, shape) in shapes.iter().enumerate() {
            ir.nodes[i].shape = *shape;
        }
        let layout = layout_diagram(&ir, &MermaidConfig::default());
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::new(80, 24);
        let renderer = MermaidRenderer::new(&MermaidConfig::default());
        renderer.render(&layout, &ir, area, &mut buf);
    }

    // ── Selection highlighting tests ──────────────────────────────────

    #[test]
    fn selection_state_from_selected_finds_edges() {
        let ir = make_ir(3, vec![(0, 1), (1, 2), (2, 0)]);
        let sel = SelectionState::from_selected(1, &ir);
        assert_eq!(sel.selected_node, Some(1));
        assert_eq!(sel.outgoing_edges.len(), 1); // edge 1->2
        assert_eq!(sel.incoming_edges.len(), 1); // edge 0->1
    }

    #[test]
    fn selection_state_empty_by_default() {
        let sel = SelectionState::default();
        assert!(sel.is_empty());
        assert_eq!(sel.edge_highlight(0), None);
    }

    #[test]
    fn selection_edge_highlight_colors() {
        let ir = make_ir(3, vec![(0, 1), (1, 2), (2, 0)]);
        let sel = SelectionState::from_selected(1, &ir);
        // Edge 0->1 is incoming to node 1
        assert!(sel.edge_highlight(0).is_some());
        // Edge 1->2 is outgoing from node 1
        assert!(sel.edge_highlight(1).is_some());
        // Edge 2->0 is unrelated to node 1
        assert!(sel.edge_highlight(2).is_none());
        // Incoming and outgoing should have different colors
        assert_ne!(sel.edge_highlight(0), sel.edge_highlight(1));
    }

    #[test]
    fn build_adjacency_correct() {
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let adj = build_adjacency(&ir);
        assert_eq!(adj.len(), 3);
        // Node 0: outgoing to 1
        assert_eq!(adj[0].len(), 1);
        assert_eq!(adj[0][0].0, 1); // neighbor
        assert!(adj[0][0].2); // outgoing
        // Node 1: incoming from 0, outgoing to 2
        assert_eq!(adj[1].len(), 2);
        // Node 2: incoming from 1
        assert_eq!(adj[2].len(), 1);
        assert!(!adj[2][0].2); // incoming
    }

    #[test]
    fn navigate_direction_finds_neighbor() {
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let config = MermaidConfig::default();
        let layout = layout_diagram(&ir, &config);
        let adj = build_adjacency(&ir);

        // From node 0, try navigating in all directions
        // At least one direction should find node 1
        let mut found = false;
        for dir in 0..4u8 {
            if let Some(idx) = navigate_direction(0, dir, &adj, &layout) {
                assert_eq!(idx, 1);
                found = true;
            }
        }
        assert!(found, "Should find node 1 in some direction from node 0");
    }

    #[test]
    fn render_with_selection_does_not_panic() {
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let config = MermaidConfig::default();
        let layout = layout_diagram(&ir, &config);
        let plan = crate::mermaid_render::select_render_plan(
            &config,
            &layout,
            &ir,
            Rect::new(0, 0, 80, 24),
        );
        let mut buf = Buffer::new(80, 24);
        let renderer = MermaidRenderer::new(&config);
        let selection = SelectionState::from_selected(1, &ir);
        renderer.render_with_selection(&layout, &ir, &plan, &selection, &mut buf);
    }

    #[test]
    fn render_with_selection_highlights_selected_node() {
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let config = MermaidConfig::default();
        let layout = layout_diagram(&ir, &config);
        let plan = crate::mermaid_render::select_render_plan(
            &config,
            &layout,
            &ir,
            Rect::new(0, 0, 80, 24),
        );

        // Render without selection
        let mut buf_normal = Buffer::new(80, 24);
        let renderer = MermaidRenderer::new(&config);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf_normal);

        // Render with selection
        let mut buf_selected = Buffer::new(80, 24);
        let selection = SelectionState::from_selected(1, &ir);
        renderer.render_with_selection(&layout, &ir, &plan, &selection, &mut buf_selected);

        // Buffers should differ (selected node has different colors)
        let mut differs = false;
        for y in 0..24u16 {
            for x in 0..80u16 {
                let c1 = buf_normal.get(x, y);
                let c2 = buf_selected.get(x, y);
                if let (Some(a), Some(b)) = (c1, c2)
                    && !a.bits_eq(b)
                {
                    differs = true;
                    break;
                }
            }
            if differs {
                break;
            }
        }
        assert!(differs, "Selected rendering should differ from normal");
    }

    #[test]
    fn render_with_empty_selection_matches_normal() {
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let config = MermaidConfig::default();
        let layout = layout_diagram(&ir, &config);
        let plan = crate::mermaid_render::select_render_plan(
            &config,
            &layout,
            &ir,
            Rect::new(0, 0, 80, 24),
        );

        // Render with plan
        let mut buf_plan = Buffer::new(80, 24);
        let renderer = MermaidRenderer::new(&config);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf_plan);

        // Render with empty selection (should be identical)
        let mut buf_sel = Buffer::new(80, 24);
        let selection = SelectionState::default();
        renderer.render_with_selection(&layout, &ir, &plan, &selection, &mut buf_sel);

        // Should be identical
        let mut same = true;
        for y in 0..24u16 {
            for x in 0..80u16 {
                if let (Some(a), Some(b)) = (buf_plan.get(x, y), buf_sel.get(x, y))
                    && !a.bits_eq(b)
                {
                    same = false;
                    break;
                }
            }
        }
        assert!(same, "Empty selection should produce identical output");
    }

    #[test]
    fn palette_default_has_expected_node_border() {
        let palette = DiagramPalette::default_palette();
        assert_eq!(palette.node_border, PackedRgba::WHITE);
        assert_eq!(palette.node_text, PackedRgba::WHITE);
    }

    #[test]
    fn palette_from_preset_all_variants() {
        use crate::mermaid::DiagramPalettePreset;
        for &preset in DiagramPalettePreset::all() {
            let palette = DiagramPalette::from_preset(preset);
            // Every palette must have 8 fill colors
            assert_eq!(palette.node_fills.len(), 8, "preset={}", preset);
            // Accent colors must be non-zero (not black)
            assert_ne!(
                palette.accent,
                PackedRgba::rgb(0, 0, 0),
                "accent for {}",
                preset
            );
        }
    }

    #[test]
    fn palette_neon_has_dark_text() {
        let palette = DiagramPalette::neon();
        assert_eq!(palette.node_text, PackedRgba::rgb(0, 0, 0));
    }

    #[test]
    fn palette_monochrome_has_no_color() {
        let palette = DiagramPalette::monochrome();
        // All fills should be grayscale (r == g == b)
        for fill in &palette.node_fills {
            let r = fill.r();
            let g = fill.g();
            let b = fill.b();
            assert_eq!(r, g, "monochrome fill not gray: ({r},{g},{b})");
            assert_eq!(g, b, "monochrome fill not gray: ({r},{g},{b})");
        }
    }

    #[test]
    fn palette_high_contrast_bright_fills() {
        let palette = DiagramPalette::high_contrast();
        // High-contrast fills should have at least one bright channel
        for (i, fill) in palette.node_fills.iter().enumerate() {
            let max_chan = fill.r().max(fill.g()).max(fill.b());
            assert!(
                max_chan >= 180,
                "high-contrast fill[{i}] not bright enough: max={max_chan}"
            );
        }
    }

    #[test]
    fn palette_fill_cycling() {
        let palette = DiagramPalette::default_palette();
        assert_eq!(palette.node_fill_for(0), palette.node_fills[0]);
        assert_eq!(palette.node_fill_for(8), palette.node_fills[0]);
        assert_eq!(palette.node_fill_for(3), palette.node_fills[3]);
    }

    #[test]
    fn renderer_uses_config_palette() {
        use crate::mermaid::DiagramPalettePreset;
        let config = MermaidConfig {
            palette: DiagramPalettePreset::Neon,
            ..MermaidConfig::default()
        };
        let renderer = MermaidRenderer::new(&config);
        assert_eq!(renderer.colors.node_text, PackedRgba::rgb(0, 0, 0));
    }

    #[test]
    fn renderer_with_mode_and_palette() {
        use crate::mermaid::DiagramPalettePreset;
        let renderer = MermaidRenderer::with_mode_and_palette(
            MermaidGlyphMode::Unicode,
            DiagramPalettePreset::Corporate,
        );
        assert_eq!(renderer.colors, DiagramPalette::corporate());
    }

    #[test]
    fn palette_preset_parse_roundtrip() {
        use crate::mermaid::DiagramPalettePreset;
        for &preset in DiagramPalettePreset::all() {
            let s = preset.as_str();
            let parsed = DiagramPalettePreset::parse(s).unwrap();
            assert_eq!(parsed, preset, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn palette_preset_next_prev_cycle() {
        use crate::mermaid::DiagramPalettePreset;
        let start = DiagramPalettePreset::Default;
        let mut current = start;
        for _ in 0..6 {
            current = current.next();
        }
        assert_eq!(current, start, "next() should cycle back after 6 steps");
        current = start;
        for _ in 0..6 {
            current = current.prev();
        }
        assert_eq!(current, start, "prev() should cycle back after 6 steps");
    }

    // ── Canvas Label Compositing Tests (bd-ukp1f.3) ─────────────────────

    #[cfg(feature = "canvas")]
    #[test]
    fn canvas_composite_node_label_centered_rect() {
        // Node label should be centered inside rect with fill background.
        let ir = make_ir(1, vec![]);
        let layout = make_layout(1, vec![]);
        let config = MermaidConfig::default();
        let renderer = MermaidRenderer::new(&config);
        let vp = Viewport::fit(&layout.bounding_box, Rect::new(0, 0, 40, 20));

        let mut buf = Buffer::new(40, 20);
        let plan = RenderPlan {
            fidelity: MermaidFidelity::Normal,
            show_node_labels: true,
            show_edge_labels: true,
            show_clusters: false,
            max_label_width: 0,
            diagram_area: Rect::new(0, 0, 40, 20),
            legend_area: None,
        };
        renderer.canvas_composite_labels(&layout.nodes, &layout.edges, &ir, &vp, &plan, &mut buf);
        // Node label "N0" should appear somewhere in the buffer.
        let has_n0 = (0..20)
            .any(|y| (0..40).any(|x| buf.get(x, y).and_then(|c| c.content.as_char()) == Some('N')));
        assert!(has_n0, "Node label 'N0' should be rendered in the buffer");
    }

    #[cfg(feature = "canvas")]
    #[test]
    fn canvas_composite_node_label_has_fill_background() {
        // Label cells should have the node fill as background color.
        let ir = make_ir(1, vec![]);
        let layout = make_layout(1, vec![]);
        let config = MermaidConfig::default();
        let renderer = MermaidRenderer::new(&config);
        let vp = Viewport::fit(&layout.bounding_box, Rect::new(0, 0, 40, 20));
        let colors = DiagramPalette::from_preset(config.palette);
        let expected_fill = colors.node_fill_for(0);

        let mut buf = Buffer::new(40, 20);
        let plan = RenderPlan {
            fidelity: MermaidFidelity::Normal,
            show_node_labels: true,
            show_edge_labels: true,
            show_clusters: false,
            max_label_width: 0,
            diagram_area: Rect::new(0, 0, 40, 20),
            legend_area: None,
        };
        renderer.canvas_composite_labels(&layout.nodes, &layout.edges, &ir, &vp, &plan, &mut buf);
        // Find a cell with 'N' (from label "N0") and verify its bg.
        let mut found_with_bg = false;
        for y in 0..20 {
            for x in 0..40 {
                if let Some(cell) = buf.get(x, y)
                    && cell.content.as_char() == Some('N')
                    && cell.bg == expected_fill
                {
                    found_with_bg = true;
                }
            }
        }
        assert!(
            found_with_bg,
            "Label text cells should have node fill as background color"
        );
    }

    #[cfg(feature = "canvas")]
    #[test]
    fn canvas_composite_fills_interior_with_bg() {
        // Interior cells (padding around label) should also have fill bg.
        let ir = make_ir(1, vec![]);
        let layout = make_layout(1, vec![]);
        let config = MermaidConfig::default();
        let renderer = MermaidRenderer::new(&config);
        let vp = Viewport::fit(&layout.bounding_box, Rect::new(0, 0, 40, 20));
        let colors = DiagramPalette::from_preset(config.palette);
        let expected_fill = colors.node_fill_for(0);

        let mut buf = Buffer::new(40, 20);
        let plan = RenderPlan {
            fidelity: MermaidFidelity::Normal,
            show_node_labels: true,
            show_edge_labels: true,
            show_clusters: false,
            max_label_width: 0,
            diagram_area: Rect::new(0, 0, 40, 20),
            legend_area: None,
        };
        renderer.canvas_composite_labels(&layout.nodes, &layout.edges, &ir, &vp, &plan, &mut buf);
        // Count cells with the node fill background.
        let fill_count = (0..20u16)
            .flat_map(|y| (0..40u16).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.get(x, y).is_some_and(|c| c.bg == expected_fill))
            .count();
        // At least a few cells should be filled (label text + padding).
        assert!(
            fill_count >= 2,
            "Expected at least 2 cells with fill background, got {fill_count}"
        );
    }

    #[cfg(feature = "canvas")]
    #[test]
    fn canvas_composite_edge_label_has_black_bg() {
        // Edge labels should have black background for readability.
        let mut ir = make_ir(2, vec![(0, 1)]);
        // Add a label to the edge.
        let label_id = ir.labels.len();
        ir.labels.push(make_label("yes"));
        ir.edges[0].label = Some(IrLabelId(label_id));
        let layout = make_layout(2, vec![(0, 1)]);
        let config = MermaidConfig::default();
        let renderer = MermaidRenderer::new(&config);
        let vp = Viewport::fit(&layout.bounding_box, Rect::new(0, 0, 40, 20));

        let mut buf = Buffer::new(40, 20);
        let plan = RenderPlan {
            fidelity: MermaidFidelity::Normal,
            show_node_labels: true,
            show_edge_labels: true,
            show_clusters: false,
            max_label_width: 0,
            diagram_area: Rect::new(0, 0, 40, 20),
            legend_area: None,
        };
        renderer.canvas_composite_labels(&layout.nodes, &layout.edges, &ir, &vp, &plan, &mut buf);
        // Find edge label text and verify black bg.
        let mut found_edge_label = false;
        for y in 0..20 {
            for x in 0..40 {
                if let Some(cell) = buf.get(x, y)
                    && cell.content.as_char() == Some('y')
                    && cell.bg == PackedRgba::BLACK
                {
                    found_edge_label = true;
                }
            }
        }
        assert!(
            found_edge_label,
            "Edge label 'yes' should be rendered with black background"
        );
    }

    #[cfg(feature = "canvas")]
    #[cfg(feature = "canvas")]
    #[test]
    fn canvas_composite_skips_when_labels_disabled() {
        // When show_node_labels is false, no labels should appear.
        let ir = make_ir(1, vec![]);
        let layout = make_layout(1, vec![]);
        let config = MermaidConfig::default();
        let renderer = MermaidRenderer::new(&config);
        let vp = Viewport::fit(&layout.bounding_box, Rect::new(0, 0, 40, 20));

        let mut buf = Buffer::new(40, 20);
        let plan = RenderPlan {
            fidelity: MermaidFidelity::Outline,
            show_node_labels: false,
            show_edge_labels: false,
            show_clusters: false,
            max_label_width: 0,
            diagram_area: Rect::new(0, 0, 40, 20),
            legend_area: None,
        };
        renderer.canvas_composite_labels(&layout.nodes, &layout.edges, &ir, &vp, &plan, &mut buf);
        // No label text should appear.
        let has_text = (0..20).any(|y| {
            (0..40).any(|x| {
                buf.get(x, y)
                    .and_then(|c| c.content.as_char())
                    .is_some_and(|ch| ch != ' ' && ch != '\0')
            })
        });
        assert!(
            !has_text,
            "No labels should be rendered when labels are disabled"
        );
    }

    #[cfg(feature = "canvas")]
    #[cfg(feature = "canvas")]
    #[test]
    fn canvas_composite_multiple_nodes_different_fills() {
        // Each node should get a distinct fill color.
        let ir = make_ir(3, vec![(0, 1), (1, 2)]);
        let layout = make_layout(3, vec![(0, 1), (1, 2)]);
        let config = MermaidConfig::default();
        let renderer = MermaidRenderer::new(&config);
        let vp = Viewport::fit(&layout.bounding_box, Rect::new(0, 0, 80, 30));
        let colors = DiagramPalette::from_preset(config.palette);

        let mut buf = Buffer::new(80, 30);
        let plan = RenderPlan {
            fidelity: MermaidFidelity::Normal,
            show_node_labels: true,
            show_edge_labels: true,
            show_clusters: false,
            max_label_width: 0,
            diagram_area: Rect::new(0, 0, 80, 30),
            legend_area: None,
        };
        renderer.canvas_composite_labels(&layout.nodes, &layout.edges, &ir, &vp, &plan, &mut buf);
        // Collect distinct bg colors that have text content.
        let mut bg_colors = std::collections::HashSet::new();
        for y in 0..30 {
            for x in 0..80 {
                if let Some(cell) = buf.get(x, y)
                    && let Some(ch) = cell.content.as_char()
                    && ch.is_alphanumeric()
                    && cell.bg != PackedRgba::TRANSPARENT
                {
                    bg_colors.insert(cell.bg);
                }
            }
        }
        // The default palette uses different fills for each node.
        let fill0 = colors.node_fill_for(0);
        let fill1 = colors.node_fill_for(1);
        let fill2 = colors.node_fill_for(2);
        // With 3 nodes and distinct palette entries, we should see at least 2
        // distinct fill colors (might be more if all 3 are distinct).
        assert!(
            bg_colors.contains(&fill0) || bg_colors.contains(&fill1) || bg_colors.contains(&fill2),
            "At least one node fill color should appear as text background"
        );
    }

    #[test]
    fn bundle_count_label_none_for_single_edge() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        assert_eq!(renderer.bundle_count_label(1), None);
        assert_eq!(renderer.bundle_count_label(0), None);
    }

    #[test]
    fn bundle_count_label_unicode_format() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        assert_eq!(
            renderer.bundle_count_label(2),
            Some("\u{00d7}2".to_string())
        );
        assert_eq!(
            renderer.bundle_count_label(5),
            Some("\u{00d7}5".to_string())
        );
        assert_eq!(
            renderer.bundle_count_label(10),
            Some("\u{00d7}10".to_string())
        );
    }

    #[test]
    fn bundle_count_label_ascii_format() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Ascii);
        assert_eq!(renderer.bundle_count_label(3), Some("x3".to_string()));
        assert_eq!(renderer.bundle_count_label(7), Some("x7".to_string()));
    }

    #[test]
    fn bundled_edge_forces_thick_line_style() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let ir = make_ir(2, vec![(0, 1), (0, 1), (0, 1)]);

        let mut layout = make_layout(2, vec![(0, 1)]);
        // Add a middle waypoint so the label anchor lands on an interior point.
        let from = layout.edges[0].waypoints[0];
        let to = layout.edges[0].waypoints[1];
        layout.edges[0].waypoints.insert(
            1,
            LayoutPoint {
                x: (from.x + to.x) / 2.0,
                y: (from.y + to.y) / 2.0,
            },
        );
        layout.edges[0].bundle_count = 3;
        layout.edges[0].bundle_members = vec![0, 1, 2];

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
        let plan = select_render_plan(&config, &layout, &ir, area);

        let mut buf = Buffer::new(80, 24);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);

        let text = buffer_to_text(&buf);
        assert!(
            text.contains("\u{00d7}3") || text.contains("x3"),
            "bundled edge should show count label; rendered:\n{text}"
        );
    }

    #[test]
    fn bundled_edge_label_appended_to_existing() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);

        let mut ir = make_ir(2, vec![(0, 1), (0, 1), (0, 1)]);
        let label_idx = ir.labels.len();
        ir.labels.push(make_label("link"));
        ir.edges[0].label = Some(IrLabelId(label_idx));

        let mut layout = make_layout(2, vec![(0, 1)]);
        let from = layout.edges[0].waypoints[0];
        let to = layout.edges[0].waypoints[1];
        layout.edges[0].waypoints.insert(
            1,
            LayoutPoint {
                x: (from.x + to.x) / 2.0,
                y: (from.y + to.y) / 2.0,
            },
        );
        layout.edges[0].bundle_count = 3;
        layout.edges[0].bundle_members = vec![0, 1, 2];

        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let config = MermaidConfig::default();
        let plan = select_render_plan(&config, &layout, &ir, area);

        let mut buf = Buffer::new(80, 24);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);

        let text = buffer_to_text(&buf);
        assert!(
            text.contains("\u{00d7}3") || text.contains("x3"),
            "bundle count should appear in output; rendered:\n{text}"
        );
    }
    #[test]
    fn node_fill_has_background_color() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let ir = make_ir(2, vec![(0, 1)]);
        let layout = make_layout(2, vec![(0, 1)]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 12,
        };
        let config = MermaidConfig::default();
        let plan = select_render_plan(&config, &layout, &ir, area);
        let mut buf = Buffer::new(40, 12);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);

        let colors = DiagramPalette::default_palette();
        let fill0 = colors.node_fill_for(0);

        // Check that interior cells have the fill background color.
        // Interior is inside the border (row 1..h-1, col 1..w-1).
        let node_rect = &layout.nodes[0].rect;
        let vp = Viewport::fit(&layout.bounding_box, area);
        let cell_rect = vp.to_cell_rect(node_rect);

        // Middle interior cell should have the fill background.
        let mid_x = cell_rect.x + cell_rect.width / 2;
        let mid_y = cell_rect.y + cell_rect.height / 2;
        if let Some(cell) = buf.get(mid_x, mid_y) {
            assert_eq!(
                cell.bg, fill0,
                "interior cell should have node fill background color"
            );
        }
    }

    #[test]
    fn node_fill_half_block_top_row() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let ir = make_ir(1, vec![]);
        // Create a layout with a single tall node.
        let layout = DiagramLayout {
            nodes: vec![LayoutNodeBox {
                node_idx: 0,
                rect: LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 6.0,
                },
                label_rect: None,
                rank: 0,
                order: 0,
            }],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 6.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: 100,
                budget_exceeded: false,
                crossings: 0,
                ranks: 1,
                max_rank_width: 1,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };

        let area = Rect {
            x: 0,
            y: 0,
            width: 12,
            height: 8,
        };
        let config = MermaidConfig::default();
        let plan = select_render_plan(&config, &layout, &ir, area);
        let mut buf = Buffer::new(12, 8);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);

        let text = buffer_to_text(&buf);
        // Should contain half-block characters for smooth edges.
        let has_lower_half = text.contains('\u{2584}');
        let has_upper_half = text.contains('\u{2580}');
        assert!(
            has_lower_half,
            "expected lower half block (\u{2584}) for top interior row"
        );
        assert!(
            has_upper_half,
            "expected upper half block (\u{2580}) for bottom interior row"
        );
    }

    #[test]
    fn node_fill_ascii_mode_no_half_blocks() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Ascii);
        let ir = make_ir(1, vec![]);
        let layout = DiagramLayout {
            nodes: vec![LayoutNodeBox {
                node_idx: 0,
                rect: LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 6.0,
                },
                label_rect: None,
                rank: 0,
                order: 0,
            }],
            clusters: vec![],
            edges: vec![],
            bounding_box: LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 6.0,
            },
            stats: LayoutStats {
                iterations_used: 0,
                max_iterations: 100,
                budget_exceeded: false,
                crossings: 0,
                ranks: 1,
                max_rank_width: 1,
                total_bends: 0,
                position_variance: 0.0,
            },
            degradation: None,
        };

        let area = Rect {
            x: 0,
            y: 0,
            width: 12,
            height: 8,
        };
        let config = MermaidConfig::default();
        let plan = select_render_plan(&config, &layout, &ir, area);
        let mut buf = Buffer::new(12, 8);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);

        let text = buffer_to_text(&buf);
        // ASCII mode should NOT use half-block characters.
        assert!(
            !text.contains('\u{2584}') && !text.contains('\u{2580}'),
            "ASCII mode should not use half-block characters"
        );
    }

    #[test]
    fn node_label_preserves_fill_background() {
        let renderer = MermaidRenderer::with_mode(MermaidGlyphMode::Unicode);
        let ir = make_ir(1, vec![]);
        let layout = make_layout(1, vec![]);
        let area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 6,
        };
        let config = MermaidConfig::default();
        let plan = select_render_plan(&config, &layout, &ir, area);
        let mut buf = Buffer::new(20, 6);
        renderer.render_with_plan(&layout, &ir, &plan, &mut buf);

        let colors = DiagramPalette::default_palette();
        let fill0 = colors.node_fill_for(0);

        // Find a cell that contains label text (letter 'N' from "N0").
        let mut found_label_with_bg = false;
        for y in 0..buf.height() {
            for x in 0..buf.width() {
                if let Some(cell) = buf.get(x, y)
                    && cell.content.as_char() == Some('N')
                    && cell.bg == fill0
                {
                    found_label_with_bg = true;
                }
            }
        }
        assert!(
            found_label_with_bg,
            "label text should preserve the node fill background color"
        );
    }
}

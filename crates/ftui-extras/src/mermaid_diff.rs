//! Diagram diffing: compare two Mermaid IRs and visualize differences.
//!
//! Given two `MermaidDiagramIr` structures (old and new), this module computes
//! which nodes and edges were added, removed, changed, or unchanged.
//! The result can be rendered with diff-aware coloring: green (added),
//! red (removed), yellow (changed), dimmed (unchanged).

use crate::mermaid::{IrEdge, IrEndpoint, IrNode, MermaidDiagramIr};

// ── Diff Types ──────────────────────────────────────────────────────────

/// Status of a node or edge in a diagram diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStatus {
    /// Present only in the new diagram.
    Added,
    /// Present only in the old diagram.
    Removed,
    /// Present in both but with changed attributes.
    Changed,
    /// Present in both with identical attributes.
    Unchanged,
}

/// A node in the diff result, tracking its status and which IR it came from.
#[derive(Debug, Clone)]
pub struct DiffNode {
    /// Semantic node ID (from `IrNode.id`).
    pub id: String,
    pub status: DiffStatus,
    /// Index into the NEW IR's nodes vec (or OLD if removed).
    pub node_idx: usize,
    /// Index into the OLD IR's nodes vec, if the node existed there.
    pub old_node_idx: Option<usize>,
}

/// An edge in the diff result.
#[derive(Debug, Clone)]
pub struct DiffEdge {
    /// Source node ID.
    pub from_id: String,
    /// Target node ID.
    pub to_id: String,
    pub status: DiffStatus,
    /// Index into the NEW IR's edges vec (or OLD if removed).
    pub edge_idx: usize,
    /// Index into the OLD IR's edges vec, if the edge existed there.
    pub old_edge_idx: Option<usize>,
}

/// Result of comparing two Mermaid diagram IRs.
#[derive(Debug, Clone)]
pub struct DiagramDiff {
    /// Node diffs, ordered by new IR index (added/changed/unchanged first, then removed).
    pub nodes: Vec<DiffNode>,
    /// Edge diffs, ordered similarly.
    pub edges: Vec<DiffEdge>,
    /// The new IR (primary for layout/rendering).
    pub new_ir: MermaidDiagramIr,
    /// The old IR (used for removed ghost nodes).
    pub old_ir: MermaidDiagramIr,
    /// Summary counts.
    pub added_nodes: usize,
    pub removed_nodes: usize,
    pub changed_nodes: usize,
    pub added_edges: usize,
    pub removed_edges: usize,
    pub changed_edges: usize,
}

impl DiagramDiff {
    /// True if the two diagrams are structurally identical.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added_nodes == 0
            && self.removed_nodes == 0
            && self.changed_nodes == 0
            && self.added_edges == 0
            && self.removed_edges == 0
            && self.changed_edges == 0
    }
}

// ── Diff Algorithm ──────────────────────────────────────────────────────

/// Resolve the node ID for an endpoint, returning the string ID.
fn endpoint_node_id(ep: &IrEndpoint, ir: &MermaidDiagramIr) -> String {
    match ep {
        IrEndpoint::Node(nid) => ir
            .nodes
            .get(nid.0)
            .map_or_else(|| format!("?{}", nid.0), |n| n.id.clone()),
        IrEndpoint::Port(pid) => ir
            .ports
            .get(pid.0)
            .and_then(|p| ir.nodes.get(p.node.0))
            .map_or_else(|| format!("?port{}", pid.0), |n| n.id.clone()),
    }
}

/// Compare two `MermaidDiagramIr` structures and produce a diff.
///
/// Nodes are matched by their semantic `id` field.
/// Edges are matched by `(from_node_id, to_node_id)` pairs.
#[must_use]
pub fn diff_diagrams(old: &MermaidDiagramIr, new: &MermaidDiagramIr) -> DiagramDiff {
    use std::collections::{HashMap, HashSet};

    let old_node_map: HashMap<&str, usize> = old
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();

    let new_node_map: HashMap<&str, usize> = new
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();

    let mut diff_nodes = Vec::new();
    let mut added_nodes = 0usize;
    let mut removed_nodes = 0usize;
    let mut changed_nodes = 0usize;

    // Process new nodes: check if added, changed, or unchanged
    for (new_idx, new_node) in new.nodes.iter().enumerate() {
        if let Some(&old_idx) = old_node_map.get(new_node.id.as_str()) {
            let old_node = &old.nodes[old_idx];
            let changed = node_attrs_differ(old_node, old, new_node, new);
            let status = if changed {
                changed_nodes += 1;
                DiffStatus::Changed
            } else {
                DiffStatus::Unchanged
            };
            diff_nodes.push(DiffNode {
                id: new_node.id.clone(),
                status,
                node_idx: new_idx,
                old_node_idx: Some(old_idx),
            });
        } else {
            added_nodes += 1;
            diff_nodes.push(DiffNode {
                id: new_node.id.clone(),
                status: DiffStatus::Added,
                node_idx: new_idx,
                old_node_idx: None,
            });
        }
    }

    // Process removed nodes (in old but not in new)
    for (old_idx, old_node) in old.nodes.iter().enumerate() {
        if !new_node_map.contains_key(old_node.id.as_str()) {
            removed_nodes += 1;
            diff_nodes.push(DiffNode {
                id: old_node.id.clone(),
                status: DiffStatus::Removed,
                node_idx: old_idx,
                old_node_idx: Some(old_idx),
            });
        }
    }

    // ── Edge diffing ──
    type EdgeKey = (String, String);

    let old_edge_map: HashMap<EdgeKey, Vec<usize>> = {
        let mut m: HashMap<EdgeKey, Vec<usize>> = HashMap::new();
        for (i, edge) in old.edges.iter().enumerate() {
            let key = (
                endpoint_node_id(&edge.from, old),
                endpoint_node_id(&edge.to, old),
            );
            m.entry(key).or_default().push(i);
        }
        m
    };

    let mut diff_edges = Vec::new();
    let mut added_edges = 0usize;
    let mut removed_edges = 0usize;
    let mut changed_edges = 0usize;
    let mut matched_old_edges: HashSet<usize> = HashSet::new();

    for (new_idx, new_edge) in new.edges.iter().enumerate() {
        let key = (
            endpoint_node_id(&new_edge.from, new),
            endpoint_node_id(&new_edge.to, new),
        );
        let old_match = old_edge_map.get(&key).and_then(|indices| {
            indices
                .iter()
                .copied()
                .find(|i| !matched_old_edges.contains(i))
        });

        if let Some(old_idx) = old_match {
            matched_old_edges.insert(old_idx);
            let old_edge = &old.edges[old_idx];
            let changed = edge_attrs_differ(old_edge, old, new_edge, new);
            let status = if changed {
                changed_edges += 1;
                DiffStatus::Changed
            } else {
                DiffStatus::Unchanged
            };
            diff_edges.push(DiffEdge {
                from_id: key.0,
                to_id: key.1,
                status,
                edge_idx: new_idx,
                old_edge_idx: Some(old_idx),
            });
        } else {
            added_edges += 1;
            diff_edges.push(DiffEdge {
                from_id: key.0,
                to_id: key.1,
                status: DiffStatus::Added,
                edge_idx: new_idx,
                old_edge_idx: None,
            });
        }
    }

    // Removed edges (in old but not matched)
    for (old_idx, old_edge) in old.edges.iter().enumerate() {
        if !matched_old_edges.contains(&old_idx) {
            removed_edges += 1;
            diff_edges.push(DiffEdge {
                from_id: endpoint_node_id(&old_edge.from, old),
                to_id: endpoint_node_id(&old_edge.to, old),
                status: DiffStatus::Removed,
                edge_idx: old_idx,
                old_edge_idx: Some(old_idx),
            });
        }
    }

    DiagramDiff {
        nodes: diff_nodes,
        edges: diff_edges,
        new_ir: new.clone(),
        old_ir: old.clone(),
        added_nodes,
        removed_nodes,
        changed_nodes,
        added_edges,
        removed_edges,
        changed_edges,
    }
}

/// Check if two nodes differ in their visible attributes.
fn node_attrs_differ(
    old: &IrNode,
    old_ir: &MermaidDiagramIr,
    new: &IrNode,
    new_ir: &MermaidDiagramIr,
) -> bool {
    if old.shape != new.shape {
        return true;
    }
    let old_label = old
        .label
        .and_then(|lid| old_ir.labels.get(lid.0))
        .map(|l| l.text.as_str());
    let new_label = new
        .label
        .and_then(|lid| new_ir.labels.get(lid.0))
        .map(|l| l.text.as_str());
    if old_label != new_label {
        return true;
    }
    if old.classes != new.classes {
        return true;
    }
    if old.members != new.members {
        return true;
    }
    false
}

/// Check if two edges differ in their visible attributes.
fn edge_attrs_differ(
    old: &IrEdge,
    old_ir: &MermaidDiagramIr,
    new: &IrEdge,
    new_ir: &MermaidDiagramIr,
) -> bool {
    if old.arrow != new.arrow {
        return true;
    }
    let old_label = old
        .label
        .and_then(|lid| old_ir.labels.get(lid.0))
        .map(|l| l.text.as_str());
    let new_label = new
        .label
        .and_then(|lid| new_ir.labels.get(lid.0))
        .map(|l| l.text.as_str());
    old_label != new_label
}

// ── Diff Rendering ──────────────────────────────────────────────────────

use crate::mermaid::MermaidConfig;
use crate::mermaid_layout::DiagramLayout;
use crate::mermaid_render::{Viewport, render_diagram};
use ftui_core::geometry::Rect;
use ftui_core::text_width::display_width;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};

/// Diff-specific color constants.
pub struct DiffColors;

impl DiffColors {
    /// Green for added nodes/edges.
    pub const ADDED: PackedRgba = PackedRgba::rgb(46, 204, 113);
    /// Red for removed nodes/edges.
    pub const REMOVED: PackedRgba = PackedRgba::rgb(231, 76, 60);
    /// Yellow for changed nodes/edges.
    pub const CHANGED: PackedRgba = PackedRgba::rgb(241, 196, 15);
    /// Dim gray for unchanged nodes/edges.
    pub const UNCHANGED: PackedRgba = PackedRgba::rgb(100, 100, 100);
}

/// Render a diagram diff into a buffer with color-coded highlighting.
///
/// Renders the NEW diagram as the base, then overlays diff colors:
/// - **Added** nodes/edges: green border with `+` marker
/// - **Changed** nodes/edges: yellow border with `~` marker
/// - **Unchanged** nodes/edges: dimmed (gray)
/// - **Removed** items: listed in a legend footer (red text)
///
/// For removed nodes, a compact legend is rendered at the bottom of the area
/// since their positions exist only in the old layout coordinate space.
pub fn render_diff(
    diff: &DiagramDiff,
    new_layout: &DiagramLayout,
    config: &MermaidConfig,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.is_empty() {
        return;
    }

    // Reserve bottom rows for removed-items legend if needed
    let has_removed = diff.removed_nodes > 0 || diff.removed_edges > 0;
    let legend_rows = if has_removed {
        2u16.min(area.height.saturating_sub(4))
    } else {
        0
    };
    let diagram_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height.saturating_sub(legend_rows),
    };

    // 1. Render the base (new) diagram
    render_diagram(new_layout, &diff.new_ir, config, diagram_area, buf);

    // 2. Compute viewport for coordinate mapping
    let vp = Viewport::fit(&new_layout.bounding_box, diagram_area);

    // 3. Build index maps for O(1) lookup of layout nodes/edges by their idx
    let node_by_idx: std::collections::HashMap<usize, usize> = new_layout
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.node_idx, i))
        .collect();
    let edge_by_idx: std::collections::HashMap<usize, usize> = new_layout
        .edges
        .iter()
        .enumerate()
        .map(|(i, e)| (e.edge_idx, i))
        .collect();

    // 4. Overlay node diff colors
    for dn in &diff.nodes {
        let (color, marker) = match dn.status {
            DiffStatus::Added => (DiffColors::ADDED, Some('+')),
            DiffStatus::Changed => (DiffColors::CHANGED, Some('~')),
            DiffStatus::Unchanged => (DiffColors::UNCHANGED, None),
            DiffStatus::Removed => continue, // handled in legend
        };

        if let Some(&layout_idx) = node_by_idx.get(&dn.node_idx) {
            let node_box = &new_layout.nodes[layout_idx];
            let cell_rect = vp.to_cell_rect(&node_box.rect);
            recolor_rect_border(cell_rect, color, buf);

            // Dim interior text for unchanged nodes
            if dn.status == DiffStatus::Unchanged {
                dim_rect_interior(cell_rect, color, buf);
            }

            // Place status marker in top-right corner
            if let Some(m) = marker {
                let mx = cell_rect.x + cell_rect.width.saturating_sub(1);
                let my = cell_rect.y;
                buf.set_fast(mx, my, Cell::from_char(m).with_fg(color));
            }
        }
    }

    // 5. Overlay edge diff colors
    for de in &diff.edges {
        let color = match de.status {
            DiffStatus::Added => DiffColors::ADDED,
            DiffStatus::Changed => DiffColors::CHANGED,
            DiffStatus::Unchanged => DiffColors::UNCHANGED,
            DiffStatus::Removed => continue, // handled in legend
        };

        if let Some(&layout_idx) = edge_by_idx.get(&de.edge_idx) {
            for wp in &new_layout.edges[layout_idx].waypoints {
                let (cx, cy) = vp.to_cell(wp.x, wp.y);
                if let Some(c) = buf.get(cx, cy) {
                    buf.set_fast(cx, cy, c.with_fg(color));
                }
            }
        }
    }

    // 6. Render removed-items legend
    if has_removed && legend_rows > 0 {
        render_removed_legend(diff, area, legend_rows, buf);
    }
}

/// Recolor the border cells of a rectangle.
fn recolor_rect_border(rect: Rect, color: PackedRgba, buf: &mut Buffer) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = x0 + rect.width.saturating_sub(1);
    let y1 = y0 + rect.height.saturating_sub(1);

    // Top and bottom edges
    for col in x0..=x1 {
        if let Some(c) = buf.get(col, y0) {
            buf.set_fast(col, y0, c.with_fg(color));
        }
        if let Some(c) = buf.get(col, y1) {
            buf.set_fast(col, y1, c.with_fg(color));
        }
    }
    // Left and right edges
    for row in y0..=y1 {
        if let Some(c) = buf.get(x0, row) {
            buf.set_fast(x0, row, c.with_fg(color));
        }
        if let Some(c) = buf.get(x1, row) {
            buf.set_fast(x1, row, c.with_fg(color));
        }
    }
}

/// Dim interior cells of a rectangle (for unchanged nodes).
fn dim_rect_interior(rect: Rect, color: PackedRgba, buf: &mut Buffer) {
    if rect.width < 3 || rect.height < 3 {
        return;
    }
    for row in (rect.y + 1)..(rect.y + rect.height.saturating_sub(1)) {
        for col in (rect.x + 1)..(rect.x + rect.width.saturating_sub(1)) {
            if let Some(c) = buf.get(col, row)
                && c.content.as_char().unwrap_or(' ') != ' '
            {
                buf.set_fast(col, row, c.with_fg(color));
            }
        }
    }
}

/// Render a compact legend for removed nodes/edges at the bottom of the area.
fn render_removed_legend(diff: &DiagramDiff, area: Rect, rows: u16, buf: &mut Buffer) {
    let legend_y = area.y + area.height.saturating_sub(rows);
    let max_w = area.width as usize;

    // Collect removed node IDs
    let removed_names: Vec<&str> = diff
        .nodes
        .iter()
        .filter(|n| n.status == DiffStatus::Removed)
        .map(|n| n.id.as_str())
        .collect();

    // Build legend text
    let mut parts = Vec::new();
    if !removed_names.is_empty() {
        let names = removed_names.join(", ");
        parts.push(format!("-nodes: {names}"));
    }
    if diff.removed_edges > 0 {
        parts.push(format!("-edges: {}", diff.removed_edges));
    }
    let legend_text = parts.join(" | ");

    // Truncate to fit
    let display_text = if display_width(&legend_text) > max_w {
        let mut result = String::new();
        let mut w = 0;
        for ch in legend_text.chars() {
            let cw = display_width(&ch.to_string());
            if w + cw + 1 > max_w {
                result.push('…');
                break;
            }
            result.push(ch);
            w += cw;
        }
        result
    } else {
        legend_text
    };

    // Write legend text in red, tracking cumulative display width
    // so wide characters (CJK etc.) advance the cursor correctly.
    let cell = Cell::from_char(' ').with_fg(DiffColors::REMOVED);
    let mut col = 0u16;
    for ch in display_text.chars() {
        let x = area.x + col;
        if x >= area.x + area.width {
            break;
        }
        buf.set_fast(x, legend_y, cell.with_char(ch));
        let cw = display_width(&ch.to_string());
        col += cw as u16;
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mermaid::{
        DiagramType, GraphDirection, IrEndpoint, IrLabel, IrLabelId, IrNodeId, MermaidDiagramMeta,
        MermaidGuardReport, MermaidInitParse, MermaidSupportLevel, MermaidThemeOverrides,
        NodeShape, Position, Span,
    };

    fn make_test_span() -> Span {
        Span {
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
        }
    }

    fn make_test_ir(node_ids: &[&str], edges: &[(usize, usize)]) -> MermaidDiagramIr {
        let labels: Vec<IrLabel> = node_ids
            .iter()
            .map(|id| IrLabel {
                text: id.to_string(),
                span: make_test_span(),
            })
            .collect();

        let nodes: Vec<IrNode> = node_ids
            .iter()
            .enumerate()
            .map(|(i, id)| IrNode {
                id: id.to_string(),
                label: Some(IrLabelId(i)),
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: make_test_span(),
                span_all: vec![],
                implicit: false,
                members: vec![],
                annotation: None,
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .map(|(from, to)| IrEdge {
                from: IrEndpoint::Node(IrNodeId(*from)),
                to: IrEndpoint::Node(IrNodeId(*to)),
                arrow: "-->".to_string(),
                label: None,
                style_ref: None,
                span: make_test_span(),
            })
            .collect();

        MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction: GraphDirection::TD,
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
                direction: GraphDirection::TD,
                support_level: MermaidSupportLevel::Supported,
                init: MermaidInitParse::default(),
                theme_overrides: MermaidThemeOverrides::default(),
                guard: MermaidGuardReport::default(),
            },
            constraints: vec![],
            quadrant_points: Vec::new(),
            quadrant_title: None,
            quadrant_x_axis: None,
            quadrant_y_axis: None,
            quadrant_labels: [None, None, None, None],
            packet_fields: Vec::new(),
            packet_title: None,
            packet_bits_per_row: 32,
            sequence_participants: Vec::new(),
            sequence_controls: Vec::new(),
            sequence_notes: Vec::new(),
            sequence_activations: Vec::new(),
            sequence_autonumber: false,
            gantt_title: None,
            gantt_sections: Vec::new(),
            gantt_tasks: Vec::new(),
        }
    }

    #[test]
    fn diff_identical_diagrams_is_empty() {
        let ir = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        let diff = diff_diagrams(&ir, &ir);
        assert!(
            diff.is_empty(),
            "identical diagrams should produce empty diff"
        );
        assert_eq!(diff.added_nodes, 0);
        assert_eq!(diff.removed_nodes, 0);
        assert_eq!(diff.changed_nodes, 0);
        assert_eq!(diff.added_edges, 0);
        assert_eq!(diff.removed_edges, 0);
        assert_eq!(diff.changed_edges, 0);
        assert_eq!(diff.nodes.len(), 3);
        assert_eq!(diff.edges.len(), 2);
        for dn in &diff.nodes {
            assert_eq!(dn.status, DiffStatus::Unchanged);
        }
        for de in &diff.edges {
            assert_eq!(de.status, DiffStatus::Unchanged);
        }
    }

    #[test]
    fn diff_detects_added_node() {
        let old = make_test_ir(&["A", "B"], &[(0, 1)]);
        let new = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.added_nodes, 1);
        assert_eq!(diff.removed_nodes, 0);
        assert_eq!(diff.changed_nodes, 0);
        let added = diff
            .nodes
            .iter()
            .find(|n| n.status == DiffStatus::Added)
            .unwrap();
        assert_eq!(added.id, "C");
    }

    #[test]
    fn diff_detects_removed_node() {
        let old = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        let new = make_test_ir(&["A", "B"], &[(0, 1)]);
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.added_nodes, 0);
        assert_eq!(diff.removed_nodes, 1);
        let removed = diff
            .nodes
            .iter()
            .find(|n| n.status == DiffStatus::Removed)
            .unwrap();
        assert_eq!(removed.id, "C");
    }

    #[test]
    fn diff_detects_changed_node_shape() {
        let old = make_test_ir(&["A", "B"], &[(0, 1)]);
        let mut new = make_test_ir(&["A", "B"], &[(0, 1)]);
        new.nodes[1].shape = NodeShape::Diamond;
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.changed_nodes, 1);
        let changed = diff
            .nodes
            .iter()
            .find(|n| n.status == DiffStatus::Changed)
            .unwrap();
        assert_eq!(changed.id, "B");
    }

    #[test]
    fn diff_detects_changed_node_label() {
        let old = make_test_ir(&["A", "B"], &[(0, 1)]);
        let mut new = make_test_ir(&["A", "B"], &[(0, 1)]);
        new.labels[1].text = "New Label".to_string();
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.changed_nodes, 1);
        let changed = diff
            .nodes
            .iter()
            .find(|n| n.status == DiffStatus::Changed)
            .unwrap();
        assert_eq!(changed.id, "B");
    }

    #[test]
    fn diff_detects_added_edge() {
        let old = make_test_ir(&["A", "B", "C"], &[(0, 1)]);
        let new = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.added_edges, 1);
        assert_eq!(diff.removed_edges, 0);
        let added = diff
            .edges
            .iter()
            .find(|e| e.status == DiffStatus::Added)
            .unwrap();
        assert_eq!(added.from_id, "B");
        assert_eq!(added.to_id, "C");
    }

    #[test]
    fn diff_detects_removed_edge() {
        let old = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        let new = make_test_ir(&["A", "B", "C"], &[(0, 1)]);
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.removed_edges, 1);
        let removed = diff
            .edges
            .iter()
            .find(|e| e.status == DiffStatus::Removed)
            .unwrap();
        assert_eq!(removed.from_id, "B");
        assert_eq!(removed.to_id, "C");
    }

    #[test]
    fn diff_detects_changed_edge_arrow() {
        let old = make_test_ir(&["A", "B"], &[(0, 1)]);
        let mut new = make_test_ir(&["A", "B"], &[(0, 1)]);
        new.edges[0].arrow = "-.->".to_string();
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.changed_edges, 1);
        let changed = diff
            .edges
            .iter()
            .find(|e| e.status == DiffStatus::Changed)
            .unwrap();
        assert_eq!(changed.from_id, "A");
        assert_eq!(changed.to_id, "B");
    }

    #[test]
    fn diff_complex_scenario() {
        // Old: A -> B -> C
        let old = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        // New: A -> B -> D, B -> E (C removed, D and E added, B changed shape)
        let mut new = make_test_ir(&["A", "B", "D", "E"], &[(0, 1), (1, 2), (1, 3)]);
        new.nodes[1].shape = NodeShape::Rounded;

        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.added_nodes, 2, "D and E added");
        assert_eq!(diff.removed_nodes, 1, "C removed");
        assert_eq!(diff.changed_nodes, 1, "B changed shape");
        assert_eq!(diff.added_edges, 2);
        assert_eq!(diff.removed_edges, 1);
    }

    #[test]
    fn diff_empty_diagrams() {
        let old = make_test_ir(&[], &[]);
        let new = make_test_ir(&[], &[]);
        let diff = diff_diagrams(&old, &new);
        assert!(diff.is_empty());
        assert_eq!(diff.nodes.len(), 0);
        assert_eq!(diff.edges.len(), 0);
    }

    #[test]
    fn diff_from_empty_to_populated() {
        let old = make_test_ir(&[], &[]);
        let new = make_test_ir(&["A", "B"], &[(0, 1)]);
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.added_nodes, 2);
        assert_eq!(diff.added_edges, 1);
        assert_eq!(diff.removed_nodes, 0);
    }

    #[test]
    fn diff_from_populated_to_empty() {
        let old = make_test_ir(&["A", "B"], &[(0, 1)]);
        let new = make_test_ir(&[], &[]);
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.removed_nodes, 2);
        assert_eq!(diff.removed_edges, 1);
        assert_eq!(diff.added_nodes, 0);
    }

    #[test]
    fn diff_node_members_change() {
        let old = make_test_ir(&["A"], &[]);
        let mut new = make_test_ir(&["A"], &[]);
        new.nodes[0].members = vec!["field1".to_string(), "method()".to_string()];
        let diff = diff_diagrams(&old, &new);
        assert_eq!(diff.changed_nodes, 1);
    }

    #[test]
    fn diff_preserves_node_indices() {
        let old = make_test_ir(&["A", "B"], &[(0, 1)]);
        let new = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        let diff = diff_diagrams(&old, &new);
        let a = diff.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(a.node_idx, 0);
        assert_eq!(a.old_node_idx, Some(0));
        let c = diff.nodes.iter().find(|n| n.id == "C").unwrap();
        assert_eq!(c.node_idx, 2);
        assert_eq!(c.old_node_idx, None);
    }

    // ── render_diff tests ─────────────────────────────────────────

    use super::{DiffColors, render_diff};
    use crate::mermaid::MermaidConfig;
    use crate::mermaid_layout::layout_diagram as mermaid_layout_diagram;
    use ftui_core::geometry::Rect;
    use ftui_render::buffer::Buffer;

    fn make_test_buffer(w: u16, h: u16) -> Buffer {
        Buffer::new(w, h)
    }

    #[test]
    fn render_diff_empty_diff_produces_output() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)]);
        let diff = diff_diagrams(&ir, &ir);
        let config = MermaidConfig::default();
        let layout = mermaid_layout_diagram(&ir, &config);
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let mut buf = make_test_buffer(40, 20);
        render_diff(&diff, &layout, &config, area, &mut buf);
        // Should render something (not all empty)
        let has_content = (0..40).any(|x| {
            (0..20).any(|y| {
                buf.get(x, y)
                    .and_then(|c| c.content.as_char())
                    .unwrap_or(' ')
                    != ' '
            })
        });
        assert!(has_content, "render_diff should produce visible output");
    }

    #[test]
    fn render_diff_added_nodes_get_green_border() {
        let old = make_test_ir(&["A"], &[]);
        let new = make_test_ir(&["A", "B"], &[(0, 1)]);
        let diff = diff_diagrams(&old, &new);
        let config = MermaidConfig::default();
        let layout = mermaid_layout_diagram(&new, &config);
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        let mut buf = make_test_buffer(60, 30);
        render_diff(&diff, &layout, &config, area, &mut buf);
        // Check that at least one cell has the green added color
        let has_green = (0..60)
            .any(|x| (0..30).any(|y| buf.get(x, y).is_some_and(|c| c.fg == DiffColors::ADDED)));
        assert!(has_green, "added node should have green-colored cells");
    }

    #[test]
    fn render_diff_unchanged_nodes_are_dimmed() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)]);
        let diff = diff_diagrams(&ir, &ir);
        let config = MermaidConfig::default();
        let layout = mermaid_layout_diagram(&ir, &config);
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        let mut buf = make_test_buffer(60, 30);
        render_diff(&diff, &layout, &config, area, &mut buf);
        // Check that at least one cell has the dim unchanged color
        let has_dim = (0..60)
            .any(|x| (0..30).any(|y| buf.get(x, y).is_some_and(|c| c.fg == DiffColors::UNCHANGED)));
        assert!(has_dim, "unchanged nodes should have dimmed cells");
    }

    #[test]
    fn render_diff_changed_node_has_yellow() {
        let old = make_test_ir(&["A", "B"], &[(0, 1)]);
        let mut new = make_test_ir(&["A", "B"], &[(0, 1)]);
        new.nodes[1].shape = NodeShape::Diamond;
        let diff = diff_diagrams(&old, &new);
        let config = MermaidConfig::default();
        let layout = mermaid_layout_diagram(&new, &config);
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        let mut buf = make_test_buffer(60, 30);
        render_diff(&diff, &layout, &config, area, &mut buf);
        let has_yellow = (0..60)
            .any(|x| (0..30).any(|y| buf.get(x, y).is_some_and(|c| c.fg == DiffColors::CHANGED)));
        assert!(has_yellow, "changed node should have yellow cells");
    }

    #[test]
    fn render_diff_removed_legend_shown() {
        let old = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        let new = make_test_ir(&["A", "B"], &[(0, 1)]);
        let diff = diff_diagrams(&old, &new);
        let config = MermaidConfig::default();
        let layout = mermaid_layout_diagram(&new, &config);
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        let mut buf = make_test_buffer(60, 30);
        render_diff(&diff, &layout, &config, area, &mut buf);
        // Check bottom rows for red removed-legend text
        let has_red_bottom = (0..60)
            .any(|x| (28..30).any(|y| buf.get(x, y).is_some_and(|c| c.fg == DiffColors::REMOVED)));
        assert!(
            has_red_bottom,
            "removed nodes should show red legend at bottom"
        );
    }

    #[test]
    fn render_diff_zero_area_does_not_panic() {
        let ir = make_test_ir(&["A"], &[]);
        let diff = diff_diagrams(&ir, &ir);
        let config = MermaidConfig::default();
        let layout = mermaid_layout_diagram(&ir, &config);
        let area = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        let mut buf = make_test_buffer(1, 1);
        render_diff(&diff, &layout, &config, area, &mut buf);
        // Should not panic
    }
}

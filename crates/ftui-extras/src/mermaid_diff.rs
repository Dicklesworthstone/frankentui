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
}

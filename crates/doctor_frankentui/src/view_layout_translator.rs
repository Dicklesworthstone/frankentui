// SPDX-License-Identifier: Apache-2.0
//! Translate view/layout semantics into ftui-layout and widget composition.
//!
//! Consumes a [`MigrationIr`] (view tree, style intent) plus an optional
//! [`IntentInferenceResult`] (inferred layout, focus, interaction patterns)
//! and produces a [`TranslatedView`] — a structured description of the
//! generated widget hierarchy:
//!
//! - **Widget nodes**: one per view node, mapped to ftui-widgets types
//! - **Layout strategy**: per-node constraint description for ftui-layout
//! - **Focus groups**: preserving keyboard traversal from intent inference
//! - **Conditional rendering**: guard/list/switch conditions preserved
//!
//! Design invariants:
//! - **Deterministic tree ordering**: children sorted by IR node id so
//!   generated widget code is reproducible across runs.
//! - **Policy-aware fallbacks**: when layout intent is ambiguous or
//!   unsupported, the translator falls back to `Flex::vertical()`.
//! - **Readable output**: generated widget names and hierarchy are kept
//!   human-readable for maintenance.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::intent_inference::{FocusTraversalGraph, IntentInferenceResult, LayoutConstraintGraph};
use crate::migration_ir::{
    ConditionKind, IrNodeId, LayoutKind, MigrationIr, Provenance, ViewNode, ViewNodeKind,
};

// ── Constants ──────────────────────────────────────────────────────────

/// Module version tag.
pub const VIEW_TRANSLATOR_VERSION: &str = "view-layout-translator-v1";

// ── Core Output Types ──────────────────────────────────────────────────

/// The complete translated view hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatedView {
    /// Schema version.
    pub version: String,
    /// Source IR run id.
    pub run_id: String,
    /// Root widget nodes (entry points for rendering).
    pub roots: Vec<String>,
    /// All widget nodes keyed by widget id.
    pub widgets: BTreeMap<String, WidgetNode>,
    /// Focus group declarations.
    pub focus_groups: Vec<TranslatedFocusGroup>,
    /// Top-level layout pattern detected.
    pub layout_pattern: String,
    /// Diagnostics.
    pub diagnostics: Vec<ViewDiagnostic>,
    /// Statistics.
    pub stats: ViewTranslationStats,
}

/// A translated widget node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetNode {
    /// Widget id (derived from IR node id).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// The ftui-widgets type to use.
    pub widget_type: WidgetType,
    /// Children widget ids (deterministic order).
    pub children: Vec<String>,
    /// Layout strategy for this node's children.
    pub layout: LayoutDecl,
    /// Properties / configuration for the widget.
    pub props: Vec<WidgetProp>,
    /// Conditional rendering guard.
    pub condition: Option<RenderConditionDecl>,
    /// Focus configuration.
    pub focus: Option<FocusConfig>,
    /// Source IR node id.
    pub source_id: IrNodeId,
    /// Source provenance.
    pub provenance: Provenance,
}

/// The ftui-widgets type a view node maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetType {
    /// Container with optional border/title (Block).
    Block,
    /// Multi-line text (Paragraph).
    Paragraph,
    /// Selectable list (List).
    List,
    /// Data table (Table).
    Table,
    /// Tab container (Tabs).
    Tabs,
    /// Text input field (TextInput).
    TextInput,
    /// Progress indicator (ProgressBar).
    ProgressBar,
    /// Scrollbar overlay.
    Scrollbar,
    /// Loading spinner (Spinner).
    Spinner,
    /// Horizontal divider (Rule).
    Rule,
    /// Compact label (Badge).
    Badge,
    /// Pure layout container (no visual, just arranges children).
    LayoutContainer,
    /// Fragment (invisible grouping).
    Fragment,
    /// Custom/unmapped widget.
    Custom,
}

/// Layout strategy for a widget's children.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutDecl {
    /// Layout kind.
    pub kind: LayoutDeclKind,
    /// Direction (for Flex layouts).
    pub direction: Option<String>,
    /// Alignment strategy.
    pub alignment: Option<String>,
    /// Per-child constraints.
    pub constraints: Vec<ConstraintDecl>,
    /// Gap between children.
    pub gap: Option<u16>,
}

/// Kind of layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutDeclKind {
    Flex,
    Grid,
    Stack,
    None,
}

/// A constraint for a child slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstraintDecl {
    /// Target child widget id.
    pub child_id: String,
    /// Constraint specification.
    pub constraint: String,
}

/// A widget property.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetProp {
    /// Property name.
    pub name: String,
    /// Value expression.
    pub value: String,
}

/// A conditional rendering declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderConditionDecl {
    /// Condition kind.
    pub kind: String,
    /// Expression snippet.
    pub expression: String,
    /// State dependencies.
    pub state_deps: Vec<IrNodeId>,
}

/// Focus configuration for a widget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusConfig {
    /// Tab order index.
    pub tab_index: Option<usize>,
    /// Focus group name.
    pub group: Option<String>,
    /// Whether this is a focus trap (modal).
    pub traps_focus: bool,
    /// Keyboard shortcut.
    pub shortcut: Option<String>,
}

/// A translated focus group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatedFocusGroup {
    /// Group name.
    pub name: String,
    /// Member widget ids.
    pub members: Vec<String>,
    /// Whether this group traps focus.
    pub traps_focus: bool,
}

/// A diagnostic from view translation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewDiagnostic {
    pub level: ViewDiagLevel,
    pub message: String,
    pub related_ids: Vec<IrNodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewDiagLevel {
    Info,
    Warning,
    Error,
}

/// Statistics about view translation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewTranslationStats {
    pub total_widgets: usize,
    pub by_type: BTreeMap<String, usize>,
    pub focus_groups: usize,
    pub conditional_widgets: usize,
    pub layout_containers: usize,
    pub diagnostics_by_level: BTreeMap<String, usize>,
}

// ── Public API ─────────────────────────────────────────────────────────

/// Translate view and layout semantics from the IR.
pub fn translate_view_layout(
    ir: &MigrationIr,
    intents: Option<&IntentInferenceResult>,
) -> TranslatedView {
    let mut diagnostics = Vec::new();

    // Build layout context from intents.
    let layout_ctx = intents.map(|i| &i.layout);
    let focus_ctx = intents.map(|i| &i.focus);

    // Detect top-level layout pattern.
    let layout_pattern = layout_ctx
        .map(|l| format!("{:?}", l.pattern))
        .unwrap_or_else(|| "Unclassified".to_string());

    // Step 1: Translate each view node into a widget node.
    let mut widgets = BTreeMap::new();
    let mut sorted_nodes: Vec<_> = ir.view_tree.nodes.iter().collect();
    sorted_nodes.sort_by_key(|(id, _)| (*id).clone());

    for (id, node) in &sorted_nodes {
        let widget = translate_node(id, node, ir, layout_ctx, &mut diagnostics);
        widgets.insert(widget.id.clone(), widget);
    }

    // Step 2: Build root list.
    let roots: Vec<String> = ir.view_tree.roots.iter().map(widget_id).collect();

    // Step 3: Build focus groups.
    let focus_groups = translate_focus_groups(focus_ctx);

    // Apply focus configuration to widgets.
    if let Some(focus) = focus_ctx {
        apply_focus_to_widgets(&mut widgets, focus);
    }

    let stats = compute_view_stats(&widgets, &focus_groups, &diagnostics);

    TranslatedView {
        version: VIEW_TRANSLATOR_VERSION.to_string(),
        run_id: ir.run_id.clone(),
        roots,
        widgets,
        focus_groups,
        layout_pattern,
        diagnostics,
        stats,
    }
}

// ── Node Translation ───────────────────────────────────────────────────

fn translate_node(
    id: &IrNodeId,
    node: &ViewNode,
    ir: &MigrationIr,
    layout_ctx: Option<&LayoutConstraintGraph>,
    diagnostics: &mut Vec<ViewDiagnostic>,
) -> WidgetNode {
    let widget_type = infer_widget_type(node, ir);
    let layout = infer_layout(id, node, ir, layout_ctx, diagnostics);
    let props = extract_props(id, node, widget_type, diagnostics);
    let condition = translate_conditions(node);
    let children: Vec<String> = node.children.iter().map(widget_id).collect();

    WidgetNode {
        id: widget_id(id),
        name: node.name.clone(),
        widget_type,
        children,
        layout,
        props,
        condition,
        focus: None, // Applied later from focus context.
        source_id: id.clone(),
        provenance: node.provenance.clone(),
    }
}

fn infer_widget_type(node: &ViewNode, ir: &MigrationIr) -> WidgetType {
    match node.kind {
        ViewNodeKind::Fragment => WidgetType::Fragment,
        ViewNodeKind::Portal => WidgetType::Block, // Portal → Block overlay.
        ViewNodeKind::Provider | ViewNodeKind::Consumer => WidgetType::Fragment,
        ViewNodeKind::Route => WidgetType::LayoutContainer,
        ViewNodeKind::Component => {
            // Try to infer from name and props.
            infer_component_widget(&node.name, node, ir)
        }
        ViewNodeKind::Element => {
            // Try to infer from element name and structure.
            infer_element_widget(&node.name, node)
        }
    }
}

fn infer_component_widget(name: &str, node: &ViewNode, _ir: &MigrationIr) -> WidgetType {
    let lower = name.to_lowercase();

    // Heuristic: detect common component patterns by name.
    if lower.contains("list") || lower.contains("menu") {
        WidgetType::List
    } else if lower.contains("table") || lower.contains("grid") || lower.contains("datagrid") {
        WidgetType::Table
    } else if lower.contains("tab") {
        WidgetType::Tabs
    } else if lower.contains("input") || lower.contains("textfield") || lower.contains("search") {
        WidgetType::TextInput
    } else if lower.contains("spinner") || lower.contains("loading") {
        WidgetType::Spinner
    } else if lower.contains("progress") {
        WidgetType::ProgressBar
    } else if lower.contains("badge") || lower.contains("tag") || lower.contains("chip") {
        WidgetType::Badge
    } else if lower.contains("divider") || lower.contains("separator") {
        WidgetType::Rule
    } else if lower.contains("scroll") {
        WidgetType::Scrollbar
    } else if node.children.is_empty() {
        WidgetType::Paragraph
    } else {
        WidgetType::Block
    }
}

fn infer_element_widget(name: &str, node: &ViewNode) -> WidgetType {
    let lower = name.to_lowercase();

    match lower.as_str() {
        "div" | "section" | "main" | "article" | "aside" | "header" | "footer" | "nav" => {
            if node.children.is_empty() {
                WidgetType::Paragraph
            } else {
                WidgetType::Block
            }
        }
        "p" | "span" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "label" | "text" => {
            WidgetType::Paragraph
        }
        "ul" | "ol" | "select" => WidgetType::List,
        "table" | "thead" | "tbody" => WidgetType::Table,
        "input" | "textarea" => WidgetType::TextInput,
        "button" | "a" => WidgetType::Badge,
        "hr" => WidgetType::Rule,
        "progress" => WidgetType::ProgressBar,
        "form" => WidgetType::Block,
        _ => {
            if node.children.is_empty() {
                WidgetType::Paragraph
            } else {
                WidgetType::Block
            }
        }
    }
}

// ── Layout Inference ───────────────────────────────────────────────────

fn infer_layout(
    id: &IrNodeId,
    node: &ViewNode,
    ir: &MigrationIr,
    layout_ctx: Option<&LayoutConstraintGraph>,
    diagnostics: &mut Vec<ViewDiagnostic>,
) -> LayoutDecl {
    // First try IR style intent.
    if let Some(layout_intent) = ir.style_intent.layouts.get(id) {
        return layout_from_intent(id, layout_intent, node, diagnostics);
    }

    // Then try inferred layout from intent inference.
    if let Some(ctx) = layout_ctx
        && let Some(region) = ctx.regions.get(id)
    {
        return layout_from_inferred(region, node, diagnostics);
    }

    // Fallback: vertical flex for containers, none for leaves.
    if node.children.is_empty() {
        LayoutDecl {
            kind: LayoutDeclKind::None,
            direction: None,
            alignment: None,
            constraints: Vec::new(),
            gap: None,
        }
    } else {
        LayoutDecl {
            kind: LayoutDeclKind::Flex,
            direction: Some("Vertical".to_string()),
            alignment: Some("Start".to_string()),
            constraints: node
                .children
                .iter()
                .map(|child_id| ConstraintDecl {
                    child_id: widget_id(child_id),
                    constraint: "Fill".to_string(),
                })
                .collect(),
            gap: None,
        }
    }
}

fn layout_from_intent(
    id: &IrNodeId,
    intent: &crate::migration_ir::LayoutIntent,
    node: &ViewNode,
    diagnostics: &mut Vec<ViewDiagnostic>,
) -> LayoutDecl {
    let kind = match intent.kind {
        LayoutKind::Flex => LayoutDeclKind::Flex,
        LayoutKind::Grid => LayoutDeclKind::Grid,
        LayoutKind::Stack => LayoutDeclKind::Stack,
        LayoutKind::Absolute => LayoutDeclKind::Flex, // Absolute → flex fallback.
        LayoutKind::Flow => LayoutDeclKind::Flex,     // Flow → flex fallback.
    };

    let direction = intent
        .direction
        .as_deref()
        .map(translate_direction)
        .or(Some("Vertical".to_string()));

    let alignment = intent.alignment.as_deref().map(translate_alignment);

    let constraints = node
        .children
        .iter()
        .map(|child_id| {
            let constraint = match intent.sizing.as_deref() {
                Some(raw) => match translate_sizing_checked(raw) {
                    Ok(translated) => translated,
                    Err(reason) => {
                        diagnostics.push(ViewDiagnostic {
                            level: ViewDiagLevel::Error,
                            message: reason,
                            related_ids: vec![id.clone(), child_id.clone()],
                        });
                        "FitContent".to_string()
                    }
                },
                None => "Fill".to_string(),
            };
            ConstraintDecl {
                child_id: widget_id(child_id),
                constraint,
            }
        })
        .collect();

    LayoutDecl {
        kind,
        direction,
        alignment,
        constraints,
        gap: None,
    }
}

fn layout_from_inferred(
    region: &crate::intent_inference::LayoutRegion,
    node: &ViewNode,
    _diagnostics: &mut Vec<ViewDiagnostic>,
) -> LayoutDecl {
    use crate::intent_inference::InferredConstraintKind;

    let kind = match region.constraint.kind {
        InferredConstraintKind::Flex => LayoutDeclKind::Flex,
        InferredConstraintKind::Grid => LayoutDeclKind::Grid,
        InferredConstraintKind::Stack => LayoutDeclKind::Stack,
        InferredConstraintKind::Absolute | InferredConstraintKind::Flow => LayoutDeclKind::Flex,
        InferredConstraintKind::Unknown => LayoutDeclKind::Flex,
    };

    let direction = region
        .constraint
        .direction
        .as_ref()
        .map(|d| format!("{d:?}"));

    let alignment = region
        .constraint
        .alignment
        .as_ref()
        .map(|a| format!("{a:?}"));

    let constraints = node
        .children
        .iter()
        .map(|child_id| {
            let constraint = region
                .constraint
                .sizing
                .as_ref()
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "Fill".to_string());
            ConstraintDecl {
                child_id: widget_id(child_id),
                constraint,
            }
        })
        .collect();

    LayoutDecl {
        kind,
        direction,
        alignment,
        constraints,
        gap: None,
    }
}

fn translate_direction(dir: &str) -> String {
    match dir.to_lowercase().as_str() {
        "row" | "horizontal" | "ltr" | "rtl" => "Horizontal".to_string(),
        "column" | "vertical" | "ttb" | "btt" => "Vertical".to_string(),
        _ => "Vertical".to_string(),
    }
}

fn translate_alignment(align: &str) -> String {
    match align.to_lowercase().as_str() {
        "start" | "flex-start" | "left" | "top" => "Start".to_string(),
        "center" | "middle" => "Center".to_string(),
        "end" | "flex-end" | "right" | "bottom" => "End".to_string(),
        "space-between" => "SpaceBetween".to_string(),
        "space-around" => "SpaceAround".to_string(),
        "stretch" => "Start".to_string(), // Stretch → Start + Fill constraints.
        _ => "Start".to_string(),
    }
}

fn parse_positive_fixed_dimension(raw: &str) -> Option<u16> {
    let trimmed = raw.trim();
    let candidate = trimmed
        .strip_prefix("fixed:")
        .or_else(|| {
            trimmed
                .strip_prefix("fixed(")
                .and_then(|s| s.strip_suffix(')'))
        })
        .or_else(|| trimmed.strip_suffix("px"))
        .or_else(|| trimmed.strip_suffix("ch"))
        .unwrap_or(trimmed)
        .trim();
    let value = candidate.parse::<u16>().ok()?;
    (value > 0).then_some(value)
}

fn translate_sizing_checked(sizing: &str) -> Result<String, String> {
    let lower = sizing.trim().to_lowercase();
    match lower.as_str() {
        "auto" | "fit-content" | "content" => Ok("FitContent".to_string()),
        "fill" | "stretch" | "1fr" => Ok("Fill".to_string()),
        s if s.ends_with('%') => {
            let pct = s.trim_end_matches('%').parse::<f32>().unwrap_or(100.0);
            Ok(format!("Percentage({pct})"))
        }
        s if s == "fixed"
            || s.starts_with("fixed:")
            || s.starts_with("fixed(")
            || s.ends_with("px")
            || s.ends_with("ch")
            || s.chars().all(|c| c.is_ascii_digit()) =>
        {
            if let Some(cells) = parse_positive_fixed_dimension(&lower) {
                Ok(format!("Fixed({cells})"))
            } else {
                Err(format!(
                    "Unsupported fixed sizing '{sizing}' without a positive cell dimension; falling back to FitContent"
                ))
            }
        }
        _ => Ok("Fill".to_string()),
    }
}

#[cfg(test)]
fn translate_sizing(sizing: &str) -> String {
    translate_sizing_checked(sizing).unwrap_or_else(|_| "FitContent".to_string())
}

// ── Prop Extraction ────────────────────────────────────────────────────

fn extract_props(
    id: &IrNodeId,
    node: &ViewNode,
    widget_type: WidgetType,
    diagnostics: &mut Vec<ViewDiagnostic>,
) -> Vec<WidgetProp> {
    let mut props = Vec::new();

    // Common props from IR.
    for prop in &node.props {
        if let Some(default_value) = &prop.default_value {
            props.push(WidgetProp {
                name: prop.name.clone(),
                value: default_value.clone(),
            });
        } else {
            diagnostics.push(ViewDiagnostic {
                level: ViewDiagLevel::Error,
                message: format!(
                    "Unsupported prop '{}' on node '{}' has no static default value",
                    prop.name, node.name
                ),
                related_ids: vec![id.clone()],
            });
        }
    }

    // Widget-type-specific defaults.
    match widget_type {
        WidgetType::Block => {
            if !props.iter().any(|p| p.name == "borders") {
                props.push(WidgetProp {
                    name: "borders".to_string(),
                    value: "Borders::ALL".to_string(),
                });
            }
            if !props.iter().any(|p| p.name == "title") && !node.name.is_empty() {
                props.push(WidgetProp {
                    name: "title".to_string(),
                    value: format!("\"{}\"", node.name),
                });
            }
        }
        WidgetType::Paragraph if !props.iter().any(|p| p.name == "wrap") => {
            props.push(WidgetProp {
                name: "wrap".to_string(),
                value: "WrapMode::Word".to_string(),
            });
        }
        _ => {}
    }

    props
}

// ── Condition Translation ──────────────────────────────────────────────

fn translate_conditions(node: &ViewNode) -> Option<RenderConditionDecl> {
    node.conditions.first().map(|cond| RenderConditionDecl {
        kind: match cond.kind {
            ConditionKind::Guard => "guard".to_string(),
            ConditionKind::List => "list".to_string(),
            ConditionKind::Switch => "switch".to_string(),
            ConditionKind::Suspense => "suspense".to_string(),
        },
        expression: cond.expression_snippet.clone(),
        state_deps: cond.state_dependencies.clone(),
    })
}

// ── Focus Translation ──────────────────────────────────────────────────

fn translate_focus_groups(focus_ctx: Option<&FocusTraversalGraph>) -> Vec<TranslatedFocusGroup> {
    let Some(focus) = focus_ctx else {
        return Vec::new();
    };

    focus
        .groups
        .iter()
        .map(|group| TranslatedFocusGroup {
            name: group.name.clone(),
            members: group.members.iter().map(widget_id).collect(),
            traps_focus: group.traps_focus,
        })
        .collect()
}

fn apply_focus_to_widgets(widgets: &mut BTreeMap<String, WidgetNode>, focus: &FocusTraversalGraph) {
    // Apply tab order.
    for (idx, node_id) in focus.tab_order.iter().enumerate() {
        let wid = widget_id(node_id);
        if let Some(widget) = widgets.get_mut(&wid) {
            let focus_node = focus.nodes.get(node_id);
            widget.focus = Some(FocusConfig {
                tab_index: Some(idx),
                group: focus_node
                    .and_then(|n| n.group_index)
                    .and_then(|gi| focus.groups.get(gi))
                    .map(|g| g.name.clone()),
                traps_focus: false,
                shortcut: focus_node.and_then(|n| n.shortcut.clone()),
            });
        }
    }

    // Mark focus-trapping groups.
    for group in &focus.groups {
        if group.traps_focus {
            for member_id in &group.members {
                let wid = widget_id(member_id);
                if let Some(widget) = widgets.get_mut(&wid)
                    && let Some(ref mut fc) = widget.focus
                {
                    fc.traps_focus = true;
                }
            }
        }
    }
}

// ── Utilities ──────────────────────────────────────────────────────────

fn widget_id(id: &IrNodeId) -> String {
    format!("w-{}", id.0)
}

fn compute_view_stats(
    widgets: &BTreeMap<String, WidgetNode>,
    focus_groups: &[TranslatedFocusGroup],
    diagnostics: &[ViewDiagnostic],
) -> ViewTranslationStats {
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut conditional = 0;
    let mut containers = 0;

    for widget in widgets.values() {
        *by_type
            .entry(format!("{:?}", widget.widget_type))
            .or_insert(0) += 1;
        if widget.condition.is_some() {
            conditional += 1;
        }
        if widget.widget_type == WidgetType::LayoutContainer
            || widget.widget_type == WidgetType::Block
        {
            containers += 1;
        }
    }

    let mut diagnostics_by_level: BTreeMap<String, usize> = BTreeMap::new();
    for d in diagnostics {
        *diagnostics_by_level
            .entry(format!("{:?}", d.level))
            .or_insert(0) += 1;
    }

    ViewTranslationStats {
        total_widgets: widgets.len(),
        by_type,
        focus_groups: focus_groups.len(),
        conditional_widgets: conditional,
        layout_containers: containers,
        diagnostics_by_level,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration_ir::{IrBuilder, ViewNode};

    fn test_provenance() -> Provenance {
        Provenance {
            file: "test.tsx".to_string(),
            line: 1,
            column: None,
            source_name: None,
            policy_category: None,
        }
    }

    fn minimal_ir() -> MigrationIr {
        let mut builder = IrBuilder::new("test-view".to_string(), "view-app".to_string());
        let root = IrNodeId("ir-root".to_string());
        let child1 = IrNodeId("ir-child-list".to_string());
        let child2 = IrNodeId("ir-child-text".to_string());

        builder.add_root(root.clone());
        builder.add_view_node(ViewNode {
            id: root.clone(),
            kind: ViewNodeKind::Component,
            name: "App".to_string(),
            children: vec![child1.clone(), child2.clone()],
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        });
        builder.add_view_node(ViewNode {
            id: child1,
            kind: ViewNodeKind::Element,
            name: "ul".to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        });
        builder.add_view_node(ViewNode {
            id: child2,
            kind: ViewNodeKind::Element,
            name: "p".to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        });
        builder.build()
    }

    #[test]
    fn translate_produces_valid_output() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        assert_eq!(result.version, VIEW_TRANSLATOR_VERSION);
        assert_eq!(result.run_id, ir.run_id);
    }

    #[test]
    fn roots_match_ir() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        assert_eq!(result.roots.len(), 1);
        assert_eq!(result.roots[0], "w-ir-root");
    }

    #[test]
    fn all_nodes_translated() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        assert_eq!(result.widgets.len(), 3);
    }

    #[test]
    fn list_element_maps_to_list_widget() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        let list_widget = result.widgets.get("w-ir-child-list");
        assert!(list_widget.is_some());
        assert_eq!(list_widget.unwrap().widget_type, WidgetType::List);
    }

    #[test]
    fn paragraph_element_maps_to_paragraph() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        let p_widget = result.widgets.get("w-ir-child-text");
        assert!(p_widget.is_some());
        assert_eq!(p_widget.unwrap().widget_type, WidgetType::Paragraph);
    }

    #[test]
    fn container_has_children() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        let root = result.widgets.get("w-ir-root").unwrap();
        assert_eq!(root.children.len(), 2);
    }

    #[test]
    fn container_has_flex_layout() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        let root = result.widgets.get("w-ir-root").unwrap();
        assert_eq!(root.layout.kind, LayoutDeclKind::Flex);
        assert_eq!(root.layout.direction.as_deref(), Some("Vertical"));
    }

    #[test]
    fn leaf_has_no_layout() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        let leaf = result.widgets.get("w-ir-child-text").unwrap();
        assert_eq!(leaf.layout.kind, LayoutDeclKind::None);
    }

    #[test]
    fn fragment_maps_correctly() {
        let mut builder = IrBuilder::new("test-frag".to_string(), "frag-app".to_string());
        let frag = IrNodeId("ir-frag".to_string());
        builder.add_root(frag.clone());
        builder.add_view_node(ViewNode {
            id: frag,
            kind: ViewNodeKind::Fragment,
            name: "".to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        });
        let ir = builder.build();
        let result = translate_view_layout(&ir, None);
        let widget = result.widgets.get("w-ir-frag").unwrap();
        assert_eq!(widget.widget_type, WidgetType::Fragment);
    }

    #[test]
    fn stats_are_consistent() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        assert_eq!(result.stats.total_widgets, result.widgets.len());
        let type_sum: usize = result.stats.by_type.values().sum();
        assert_eq!(type_sum, result.stats.total_widgets);
    }

    #[test]
    fn empty_ir_produces_empty_view() {
        let ir = IrBuilder::new("test-empty".to_string(), "empty".to_string()).build();
        let result = translate_view_layout(&ir, None);
        assert!(result.widgets.is_empty());
        assert!(result.roots.is_empty());
    }

    #[test]
    fn conditional_rendering_preserved() {
        use crate::migration_ir::RenderCondition;

        let mut builder = IrBuilder::new("test-cond".to_string(), "cond-app".to_string());
        let node = IrNodeId("ir-cond".to_string());
        builder.add_root(node.clone());
        builder.add_view_node(ViewNode {
            id: node,
            kind: ViewNodeKind::Element,
            name: "div".to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: vec![RenderCondition {
                kind: ConditionKind::Guard,
                expression_snippet: "isVisible".to_string(),
                state_dependencies: vec![IrNodeId("ir-state-visible".to_string())],
            }],
            provenance: test_provenance(),
        });
        let ir = builder.build();
        let result = translate_view_layout(&ir, None);
        let widget = result.widgets.get("w-ir-cond").unwrap();
        assert!(widget.condition.is_some());
        let cond = widget.condition.as_ref().unwrap();
        assert_eq!(cond.kind, "guard");
        assert_eq!(cond.expression, "isVisible");
    }

    #[test]
    fn input_element_maps_to_text_input() {
        let mut builder = IrBuilder::new("test-input".to_string(), "input-app".to_string());
        let node = IrNodeId("ir-input".to_string());
        builder.add_root(node.clone());
        builder.add_view_node(ViewNode {
            id: node,
            kind: ViewNodeKind::Element,
            name: "input".to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        });
        let ir = builder.build();
        let result = translate_view_layout(&ir, None);
        let widget = result.widgets.get("w-ir-input").unwrap();
        assert_eq!(widget.widget_type, WidgetType::TextInput);
    }

    #[test]
    fn translate_direction_conversions() {
        assert_eq!(translate_direction("row"), "Horizontal");
        assert_eq!(translate_direction("column"), "Vertical");
        assert_eq!(translate_direction("horizontal"), "Horizontal");
        assert_eq!(translate_direction("vertical"), "Vertical");
        assert_eq!(translate_direction("unknown"), "Vertical");
    }

    #[test]
    fn translate_alignment_conversions() {
        assert_eq!(translate_alignment("center"), "Center");
        assert_eq!(translate_alignment("flex-start"), "Start");
        assert_eq!(translate_alignment("flex-end"), "End");
        assert_eq!(translate_alignment("space-between"), "SpaceBetween");
    }

    #[test]
    fn translate_sizing_conversions() {
        assert_eq!(translate_sizing("fill"), "Fill");
        assert_eq!(translate_sizing("auto"), "FitContent");
        assert_eq!(translate_sizing("50%"), "Percentage(50)");
    }

    #[test]
    fn component_name_heuristics() {
        let make_node = |name: &str| ViewNode {
            id: IrNodeId("ir-test".to_string()),
            kind: ViewNodeKind::Component,
            name: name.to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        };
        let ir = IrBuilder::new("t".to_string(), "t".to_string()).build();

        assert_eq!(
            infer_widget_type(&make_node("ItemList"), &ir),
            WidgetType::List
        );
        assert_eq!(
            infer_widget_type(&make_node("DataTable"), &ir),
            WidgetType::Table
        );
        assert_eq!(
            infer_widget_type(&make_node("TabGroup"), &ir),
            WidgetType::Tabs
        );
        assert_eq!(
            infer_widget_type(&make_node("SearchInput"), &ir),
            WidgetType::TextInput
        );
        assert_eq!(
            infer_widget_type(&make_node("LoadingSpinner"), &ir),
            WidgetType::Spinner
        );
    }

    #[test]
    fn layout_pattern_defaults_to_unclassified() {
        let ir = minimal_ir();
        let result = translate_view_layout(&ir, None);
        assert_eq!(result.layout_pattern, "Unclassified");
    }

    #[test]
    fn deterministic_output() {
        let ir = minimal_ir();
        let r1 = translate_view_layout(&ir, None);
        let r2 = translate_view_layout(&ir, None);
        assert_eq!(r1.widgets.len(), r2.widgets.len());
        for (k1, v1) in &r1.widgets {
            let v2 = r2.widgets.get(k1).unwrap();
            assert_eq!(v1.name, v2.name);
            assert_eq!(v1.widget_type, v2.widget_type);
        }
    }
}

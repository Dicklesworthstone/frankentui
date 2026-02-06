//! Tree widget for hierarchical display.
//!
//! Renders a tree of labeled nodes with configurable guide characters
//! and styles, suitable for file trees or structured views.
//!
//! # Example
//!
//! ```
//! use ftui_widgets::tree::{Tree, TreeNode, TreeGuides};
//!
//! let tree = Tree::new(TreeNode::new("root")
//!     .child(TreeNode::new("src")
//!         .child(TreeNode::new("main.rs"))
//!         .child(TreeNode::new("lib.rs")))
//!     .child(TreeNode::new("Cargo.toml")));
//!
//! assert_eq!(tree.root().label(), "root");
//! assert_eq!(tree.root().children().len(), 2);
//! ```

use crate::mouse::MouseResult;
use crate::stateful::Stateful;
use crate::undo_support::{TreeUndoExt, UndoSupport, UndoWidgetId};
use crate::{Widget, draw_text_span};
use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::Style;
use std::any::Any;
use std::collections::HashSet;

/// Guide character styles for tree rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TreeGuides {
    /// ASCII guides: `|`, `+--`, `` `-- ``.
    Ascii,
    /// Unicode box-drawing characters (default).
    #[default]
    Unicode,
    /// Bold Unicode box-drawing characters.
    Bold,
    /// Double-line Unicode characters.
    Double,
    /// Rounded Unicode characters.
    Rounded,
}

impl TreeGuides {
    /// Vertical continuation (item has siblings below).
    #[must_use]
    pub const fn vertical(&self) -> &str {
        match self {
            Self::Ascii => "|   ",
            Self::Unicode | Self::Rounded => "\u{2502}   ",
            Self::Bold => "\u{2503}   ",
            Self::Double => "\u{2551}   ",
        }
    }

    /// Branch guide (item has siblings below).
    #[must_use]
    pub const fn branch(&self) -> &str {
        match self {
            Self::Ascii => "+-- ",
            Self::Unicode => "\u{251C}\u{2500}\u{2500} ",
            Self::Bold => "\u{2523}\u{2501}\u{2501} ",
            Self::Double => "\u{2560}\u{2550}\u{2550} ",
            Self::Rounded => "\u{251C}\u{2500}\u{2500} ",
        }
    }

    /// Last-item guide (no siblings below).
    #[must_use]
    pub const fn last(&self) -> &str {
        match self {
            Self::Ascii => "`-- ",
            Self::Unicode => "\u{2514}\u{2500}\u{2500} ",
            Self::Bold => "\u{2517}\u{2501}\u{2501} ",
            Self::Double => "\u{255A}\u{2550}\u{2550} ",
            Self::Rounded => "\u{2570}\u{2500}\u{2500} ",
        }
    }

    /// Empty indentation (no guide needed).
    #[must_use]
    pub const fn space(&self) -> &str {
        "    "
    }

    /// Width in columns of each guide segment.
    #[must_use]
    pub fn width(&self) -> usize {
        4
    }
}

/// A node in the tree hierarchy.
#[derive(Debug, Clone)]
pub struct TreeNode {
    label: String,
    /// Child nodes (crate-visible for undo support).
    pub(crate) children: Vec<TreeNode>,
    /// Whether this node is expanded (crate-visible for undo support).
    pub(crate) expanded: bool,
}

impl TreeNode {
    /// Create a new tree node with the given label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            children: Vec::new(),
            expanded: true,
        }
    }

    /// Add a child node.
    #[must_use]
    pub fn child(mut self, node: TreeNode) -> Self {
        self.children.push(node);
        self
    }

    /// Set children from a vec.
    #[must_use]
    pub fn with_children(mut self, nodes: Vec<TreeNode>) -> Self {
        self.children = nodes;
        self
    }

    /// Set whether this node is expanded.
    #[must_use]
    pub fn with_expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// Get the label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Get the children.
    #[must_use]
    pub fn children(&self) -> &[TreeNode] {
        &self.children
    }

    /// Whether this node is expanded.
    #[must_use]
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Toggle the expanded state.
    pub fn toggle_expanded(&mut self) {
        self.expanded = !self.expanded;
    }

    /// Count all visible (expanded) nodes, including this one.
    #[must_use]
    pub fn visible_count(&self) -> usize {
        let mut count = 1;
        if self.expanded {
            for child in &self.children {
                count += child.visible_count();
            }
        }
        count
    }

    /// Collect all expanded node paths into a set.
    #[allow(dead_code)]
    pub(crate) fn collect_expanded(&self, prefix: &str, out: &mut HashSet<String>) {
        let path = if prefix.is_empty() {
            self.label.clone()
        } else {
            format!("{}/{}", prefix, self.label)
        };

        if self.expanded && !self.children.is_empty() {
            out.insert(path.clone());
        }

        for child in &self.children {
            child.collect_expanded(&path, out);
        }
    }

    /// Apply expanded state from a set of paths.
    #[allow(dead_code)]
    pub(crate) fn apply_expanded(&mut self, prefix: &str, expanded_paths: &HashSet<String>) {
        let path = if prefix.is_empty() {
            self.label.clone()
        } else {
            format!("{}/{}", prefix, self.label)
        };

        if !self.children.is_empty() {
            self.expanded = expanded_paths.contains(&path);
        }

        for child in &mut self.children {
            child.apply_expanded(&path, expanded_paths);
        }
    }
}

/// Tree widget for rendering hierarchical data.
#[derive(Debug, Clone)]
pub struct Tree {
    /// Unique ID for undo tracking.
    undo_id: UndoWidgetId,
    root: TreeNode,
    /// Whether to show the root node.
    show_root: bool,
    /// Guide character style.
    guides: TreeGuides,
    /// Style for guide characters.
    guide_style: Style,
    /// Style for node labels.
    label_style: Style,
    /// Style for the root node label.
    root_style: Style,
    /// Optional persistence ID for state saving/restoration.
    persistence_id: Option<String>,
    /// Optional hit ID for mouse interaction.
    hit_id: Option<HitId>,
}

impl Tree {
    /// Create a tree widget with the given root node.
    #[must_use]
    pub fn new(root: TreeNode) -> Self {
        Self {
            undo_id: UndoWidgetId::new(),
            root,
            show_root: true,
            guides: TreeGuides::default(),
            guide_style: Style::default(),
            label_style: Style::default(),
            root_style: Style::default(),
            persistence_id: None,
            hit_id: None,
        }
    }

    /// Set whether to show the root node.
    #[must_use]
    pub fn with_show_root(mut self, show: bool) -> Self {
        self.show_root = show;
        self
    }

    /// Set the guide character style.
    #[must_use]
    pub fn with_guides(mut self, guides: TreeGuides) -> Self {
        self.guides = guides;
        self
    }

    /// Set the style for guide characters.
    #[must_use]
    pub fn with_guide_style(mut self, style: Style) -> Self {
        self.guide_style = style;
        self
    }

    /// Set the style for node labels.
    #[must_use]
    pub fn with_label_style(mut self, style: Style) -> Self {
        self.label_style = style;
        self
    }

    /// Set the style for the root label.
    #[must_use]
    pub fn with_root_style(mut self, style: Style) -> Self {
        self.root_style = style;
        self
    }

    /// Set a persistence ID for state saving.
    #[must_use]
    pub fn with_persistence_id(mut self, id: impl Into<String>) -> Self {
        self.persistence_id = Some(id.into());
        self
    }

    /// Get the persistence ID, if set.
    #[must_use]
    pub fn persistence_id(&self) -> Option<&str> {
        self.persistence_id.as_deref()
    }

    /// Set a hit ID for mouse interaction.
    #[must_use]
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.hit_id = Some(id);
        self
    }

    /// Get a reference to the root node.
    #[must_use]
    pub fn root(&self) -> &TreeNode {
        &self.root
    }

    /// Get a mutable reference to the root node.
    pub fn root_mut(&mut self) -> &mut TreeNode {
        &mut self.root
    }

    #[allow(clippy::too_many_arguments)]
    fn render_node(
        &self,
        node: &TreeNode,
        depth: usize,
        is_last: &mut Vec<bool>,
        area: Rect,
        frame: &mut Frame,
        current_row: &mut usize,
        deg: ftui_render::budget::DegradationLevel,
    ) {
        if *current_row >= area.height as usize {
            return;
        }

        let y = area.y.saturating_add(*current_row as u16);
        let mut x = area.x;
        let max_x = area.right();

        // Draw guide characters for each depth level
        if depth > 0 && deg.apply_styling() {
            for d in 0..depth {
                let is_last_at_depth = is_last.get(d).copied().unwrap_or(false);
                let guide = if d == depth - 1 {
                    // This is the immediate parent level
                    if is_last_at_depth {
                        self.guides.last()
                    } else {
                        self.guides.branch()
                    }
                } else {
                    // Ancestor level: show vertical line or blank
                    if is_last_at_depth {
                        self.guides.space()
                    } else {
                        self.guides.vertical()
                    }
                };

                x = draw_text_span(frame, x, y, guide, self.guide_style, max_x);
            }
        } else if depth > 0 {
            // Minimal rendering: indent with spaces
            let indent = "    ".repeat(depth);
            x = draw_text_span(frame, x, y, &indent, Style::default(), max_x);
        }

        // Draw label
        let style = if depth == 0 && self.show_root {
            self.root_style
        } else {
            self.label_style
        };

        if deg.apply_styling() {
            draw_text_span(frame, x, y, &node.label, style, max_x);
        } else {
            draw_text_span(frame, x, y, &node.label, Style::default(), max_x);
        }

        // Register hit region for the row
        if let Some(id) = self.hit_id {
            let row_area = Rect::new(area.x, y, area.width, 1);
            frame.register_hit(row_area, id, HitRegion::Content, *current_row as u64);
        }

        *current_row += 1;

        if !node.expanded {
            return;
        }

        let child_count = node.children.len();
        for (i, child) in node.children.iter().enumerate() {
            is_last.push(i == child_count - 1);
            self.render_node(child, depth + 1, is_last, area, frame, current_row, deg);
            is_last.pop();
        }
    }
}

impl Widget for Tree {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        let mut current_row = 0;
        let mut is_last = Vec::with_capacity(8);

        if self.show_root {
            self.render_node(
                &self.root,
                0,
                &mut is_last,
                area,
                frame,
                &mut current_row,
                deg,
            );
        } else if self.root.expanded {
            // If root is hidden but expanded, render children as top-level nodes.
            // We do NOT push to is_last for the root level, effectively shifting
            // the hierarchy up by one level.
            let child_count = self.root.children.len();
            for (i, child) in self.root.children.iter().enumerate() {
                is_last.push(i == child_count - 1);
                self.render_node(
                    child,
                    0, // Children become depth 0
                    &mut is_last,
                    area,
                    frame,
                    &mut current_row,
                    deg,
                );
                is_last.pop();
            }
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

// ============================================================================
// Stateful Persistence Implementation
// ============================================================================

/// Persistable state for a [`Tree`] widget.
///
/// Stores the set of expanded node paths to restore tree expansion state.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(
    feature = "state-persistence",
    derive(serde::Serialize, serde::Deserialize)
)]
pub struct TreePersistState {
    /// Set of expanded node paths (e.g., "root/src/main.rs").
    pub expanded_paths: HashSet<String>,
}

impl crate::stateful::Stateful for Tree {
    type State = TreePersistState;

    fn state_key(&self) -> crate::stateful::StateKey {
        crate::stateful::StateKey::new("Tree", self.persistence_id.as_deref().unwrap_or("default"))
    }

    fn save_state(&self) -> TreePersistState {
        let mut expanded_paths = HashSet::new();
        self.root.collect_expanded("", &mut expanded_paths);
        TreePersistState { expanded_paths }
    }

    fn restore_state(&mut self, state: TreePersistState) {
        self.root.apply_expanded("", &state.expanded_paths);
    }
}

// ============================================================================
// Undo Support Implementation
// ============================================================================

impl UndoSupport for Tree {
    fn undo_widget_id(&self) -> UndoWidgetId {
        self.undo_id
    }

    fn create_snapshot(&self) -> Box<dyn Any + Send> {
        Box::new(self.save_state())
    }

    fn restore_snapshot(&mut self, snapshot: &dyn Any) -> bool {
        if let Some(snap) = snapshot.downcast_ref::<TreePersistState>() {
            self.restore_state(snap.clone());
            true
        } else {
            false
        }
    }
}

impl TreeUndoExt for Tree {
    fn is_node_expanded(&self, path: &[usize]) -> bool {
        self.get_node_at_path(path)
            .map(|node| node.is_expanded())
            .unwrap_or(false)
    }

    fn expand_node(&mut self, path: &[usize]) {
        if let Some(node) = self.get_node_at_path_mut(path) {
            node.expanded = true;
        }
    }

    fn collapse_node(&mut self, path: &[usize]) {
        if let Some(node) = self.get_node_at_path_mut(path) {
            node.expanded = false;
        }
    }
}

impl Tree {
    /// Get the undo widget ID for this tree.
    #[must_use]
    pub fn undo_id(&self) -> UndoWidgetId {
        self.undo_id
    }

    /// Get a reference to a node at the given path (indices from root).
    fn get_node_at_path(&self, path: &[usize]) -> Option<&TreeNode> {
        let mut current = &self.root;
        for &idx in path {
            current = current.children.get(idx)?;
        }
        Some(current)
    }

    /// Get a mutable reference to a node at the given path (indices from root).
    fn get_node_at_path_mut(&mut self, path: &[usize]) -> Option<&mut TreeNode> {
        let mut current = &mut self.root;
        for &idx in path {
            current = current.children.get_mut(idx)?;
        }
        Some(current)
    }

    /// Handle a mouse event for this tree.
    ///
    /// # Hit data convention
    ///
    /// The hit data (`u64`) encodes the flattened visible row index. When the
    /// tree renders with a `hit_id`, each visible row registers
    /// `HitRegion::Content` with `data = visible_row_index as u64`.
    ///
    /// Clicking a parent node (one with children) toggles its expanded state
    /// and returns `Activated`. Clicking a leaf returns `Selected`.
    ///
    /// # Arguments
    ///
    /// * `event` — the mouse event from the terminal
    /// * `hit` — result of `frame.hit_test(event.x, event.y)`, if available
    /// * `expected_id` — the `HitId` this tree was rendered with
    pub fn handle_mouse(
        &mut self,
        event: &MouseEvent,
        hit: Option<(HitId, HitRegion, u64)>,
        expected_id: HitId,
    ) -> MouseResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((id, HitRegion::Content, data)) = hit && id == expected_id {
                    let index = data as usize;
                    if let Some(node) = self.node_at_visible_index_mut(index) {
                        if node.children.is_empty() {
                            return MouseResult::Selected(index);
                        }
                        node.toggle_expanded();
                        return MouseResult::Activated(index);
                    }
                }
                MouseResult::Ignored
            }
            _ => MouseResult::Ignored,
        }
    }

    /// Get a mutable reference to the node at the given visible (flattened) index.
    ///
    /// The traversal order matches `render_node()`: if `show_root` is true the
    /// root is row 0; otherwise children of the root are the top-level rows.
    /// Only expanded nodes' children are visited.
    pub fn node_at_visible_index_mut(&mut self, target: usize) -> Option<&mut TreeNode> {
        let mut counter = 0usize;
        if self.show_root {
            Self::walk_visible_mut(&mut self.root, target, &mut counter)
        } else if self.root.expanded {
            for child in &mut self.root.children {
                if let Some(node) = Self::walk_visible_mut(child, target, &mut counter) {
                    return Some(node);
                }
            }
            None
        } else {
            None
        }
    }

    /// Recursive helper that walks the visible tree to find the node at the
    /// given flattened index. Returns `Some` if found, `None` otherwise.
    fn walk_visible_mut<'a>(
        node: &'a mut TreeNode,
        target: usize,
        counter: &mut usize,
    ) -> Option<&'a mut TreeNode> {
        if *counter == target {
            return Some(node);
        }
        *counter += 1;
        if node.expanded {
            for child in &mut node.children {
                if let Some(found) = Self::walk_visible_mut(child, target, counter) {
                    return Some(found);
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Test-only flatten helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FlatNode {
    label: String,
    depth: usize,
}

#[cfg(test)]
fn flatten_visible(node: &TreeNode, depth: usize, out: &mut Vec<FlatNode>) {
    out.push(FlatNode {
        label: node.label.clone(),
        depth,
    });
    if node.expanded {
        for child in &node.children {
            flatten_visible(child, depth + 1, out);
        }
    }
}

#[cfg(test)]
impl Tree {
    fn flatten(&self) -> Vec<FlatNode> {
        let mut out = Vec::new();
        if self.show_root {
            flatten_visible(&self.root, 0, &mut out);
        } else if self.root.expanded {
            for child in &self.root.children {
                flatten_visible(child, 0, &mut out);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    fn simple_tree() -> TreeNode {
        TreeNode::new("root")
            .child(
                TreeNode::new("a")
                    .child(TreeNode::new("a1"))
                    .child(TreeNode::new("a2")),
            )
            .child(TreeNode::new("b"))
    }

    #[test]
    fn tree_node_basics() {
        let node = TreeNode::new("hello");
        assert_eq!(node.label(), "hello");
        assert!(node.children().is_empty());
        assert!(node.is_expanded());
    }

    #[test]
    fn tree_node_children() {
        let root = simple_tree();
        assert_eq!(root.children().len(), 2);
        assert_eq!(root.children()[0].label(), "a");
        assert_eq!(root.children()[0].children().len(), 2);
    }

    #[test]
    fn tree_node_visible_count() {
        let root = simple_tree();
        // root + a + a1 + a2 + b = 5
        assert_eq!(root.visible_count(), 5);
    }

    #[test]
    fn tree_node_collapsed() {
        let root = TreeNode::new("root")
            .child(
                TreeNode::new("a")
                    .with_expanded(false)
                    .child(TreeNode::new("a1"))
                    .child(TreeNode::new("a2")),
            )
            .child(TreeNode::new("b"));
        // root + a (collapsed, so no a1/a2) + b = 3
        assert_eq!(root.visible_count(), 3);
    }

    #[test]
    fn tree_node_toggle() {
        let mut node = TreeNode::new("x");
        assert!(node.is_expanded());
        node.toggle_expanded();
        assert!(!node.is_expanded());
        node.toggle_expanded();
        assert!(node.is_expanded());
    }

    #[test]
    fn tree_guides_unicode() {
        let g = TreeGuides::Unicode;
        assert!(g.branch().contains('├'));
        assert!(g.last().contains('└'));
        assert!(g.vertical().contains('│'));
    }

    #[test]
    fn tree_guides_ascii() {
        let g = TreeGuides::Ascii;
        assert!(g.branch().contains('+'));
        assert!(g.vertical().contains('|'));
    }

    #[test]
    fn tree_guides_width() {
        for g in [
            TreeGuides::Ascii,
            TreeGuides::Unicode,
            TreeGuides::Bold,
            TreeGuides::Double,
            TreeGuides::Rounded,
        ] {
            assert_eq!(g.width(), 4);
        }
    }

    #[test]
    fn tree_render_basic() {
        let tree = Tree::new(simple_tree());

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);
        tree.render(area, &mut frame);

        // Root label at (0, 0)
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('r'));
    }

    #[test]
    fn tree_render_guides_present() {
        let tree = Tree::new(simple_tree()).with_guides(TreeGuides::Ascii);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);
        tree.render(area, &mut frame);

        // Row 1 should be child "a" with branch guide "+-- "
        // First char of guide at (0, 1)
        let cell = frame.buffer.get(0, 1).unwrap();
        assert_eq!(cell.content.as_char(), Some('+'));
    }

    #[test]
    fn tree_render_last_guide() {
        let tree = Tree::new(
            TreeNode::new("root")
                .child(TreeNode::new("a"))
                .child(TreeNode::new("b")),
        )
        .with_guides(TreeGuides::Ascii);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);
        tree.render(area, &mut frame);

        // Row 1: "+-- a" (not last)
        let cell = frame.buffer.get(0, 1).unwrap();
        assert_eq!(cell.content.as_char(), Some('+'));

        // Row 2: "`-- b" (last child)
        let cell = frame.buffer.get(0, 2).unwrap();
        assert_eq!(cell.content.as_char(), Some('`'));
    }

    #[test]
    fn tree_render_zero_area() {
        let tree = Tree::new(simple_tree());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        tree.render(Rect::new(0, 0, 0, 0), &mut frame); // No panic
    }

    #[test]
    fn tree_render_truncated_height() {
        let tree = Tree::new(simple_tree());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 2, &mut pool);
        let area = Rect::new(0, 0, 40, 2);
        tree.render(area, &mut frame); // Only first 2 rows render, no panic
    }

    #[test]
    fn is_not_essential() {
        let tree = Tree::new(TreeNode::new("x"));
        assert!(!tree.is_essential());
    }

    #[test]
    fn tree_root_access() {
        let mut tree = Tree::new(TreeNode::new("root"));
        assert_eq!(tree.root().label(), "root");
        tree.root_mut().toggle_expanded();
        assert!(!tree.root().is_expanded());
    }

    #[test]
    fn tree_guides_default() {
        let g = TreeGuides::default();
        assert_eq!(g, TreeGuides::Unicode);
    }

    #[test]
    fn tree_guides_rounded() {
        let g = TreeGuides::Rounded;
        assert!(g.last().contains('╰'));
    }

    #[test]
    fn tree_deep_nesting() {
        let node = TreeNode::new("d3");
        let node = TreeNode::new("d2").child(node);
        let node = TreeNode::new("d1").child(node);
        let root = TreeNode::new("root").child(node);

        let tree = Tree::new(root);
        let flat = tree.flatten();
        assert_eq!(flat.len(), 4);
        assert_eq!(flat[3].depth, 3);
    }

    #[test]
    fn tree_node_with_children_vec() {
        let root = TreeNode::new("root").with_children(vec![
            TreeNode::new("a"),
            TreeNode::new("b"),
            TreeNode::new("c"),
        ]);
        assert_eq!(root.children().len(), 3);
    }

    // --- Stateful Persistence tests ---

    use crate::stateful::Stateful;

    #[test]
    fn tree_with_persistence_id() {
        let tree = Tree::new(TreeNode::new("root")).with_persistence_id("file-tree");
        assert_eq!(tree.persistence_id(), Some("file-tree"));
    }

    #[test]
    fn tree_default_no_persistence_id() {
        let tree = Tree::new(TreeNode::new("root"));
        assert_eq!(tree.persistence_id(), None);
    }

    #[test]
    fn tree_save_restore_round_trip() {
        // Create tree with some nodes expanded, some collapsed
        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(
                    TreeNode::new("src")
                        .child(TreeNode::new("main.rs"))
                        .child(TreeNode::new("lib.rs")),
                )
                .child(TreeNode::new("tests").with_expanded(false)),
        )
        .with_persistence_id("test");

        // Verify initial state: root and src expanded, tests collapsed
        assert!(tree.root().is_expanded());
        assert!(tree.root().children()[0].is_expanded()); // src
        assert!(!tree.root().children()[1].is_expanded()); // tests

        let saved = tree.save_state();

        // Verify saved state captures expanded nodes
        assert!(saved.expanded_paths.contains("root"));
        assert!(saved.expanded_paths.contains("root/src"));
        assert!(!saved.expanded_paths.contains("root/tests"));

        // Modify tree state (collapse src)
        tree.root_mut().children[0].toggle_expanded();
        assert!(!tree.root().children()[0].is_expanded());

        // Restore
        tree.restore_state(saved);

        // Verify restored state
        assert!(tree.root().is_expanded());
        assert!(tree.root().children()[0].is_expanded()); // src restored
        assert!(!tree.root().children()[1].is_expanded()); // tests still collapsed
    }

    #[test]
    fn tree_state_key_uses_persistence_id() {
        let tree = Tree::new(TreeNode::new("root")).with_persistence_id("project-explorer");
        let key = tree.state_key();
        assert_eq!(key.widget_type, "Tree");
        assert_eq!(key.instance_id, "project-explorer");
    }

    #[test]
    fn tree_state_key_default_when_no_id() {
        let tree = Tree::new(TreeNode::new("root"));
        let key = tree.state_key();
        assert_eq!(key.widget_type, "Tree");
        assert_eq!(key.instance_id, "default");
    }

    #[test]
    fn tree_persist_state_default() {
        let persist = TreePersistState::default();
        assert!(persist.expanded_paths.is_empty());
    }

    #[test]
    fn tree_collect_expanded_only_includes_nodes_with_children() {
        let tree = Tree::new(
            TreeNode::new("root").child(TreeNode::new("leaf")), // leaf has no children
        );

        let saved = tree.save_state();

        // Only root is expanded (and has children)
        assert!(saved.expanded_paths.contains("root"));
        // leaf has no children, so it's not tracked
        assert!(!saved.expanded_paths.contains("root/leaf"));
    }

    // ============================================================================
    // Undo Support Tests
    // ============================================================================

    #[test]
    fn tree_undo_widget_id_unique() {
        let tree1 = Tree::new(TreeNode::new("root1"));
        let tree2 = Tree::new(TreeNode::new("root2"));
        assert_ne!(tree1.undo_id(), tree2.undo_id());
    }

    #[test]
    fn tree_undo_snapshot_and_restore() {
        // Nodes must have children for their expanded state to be tracked
        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(
                    TreeNode::new("a")
                        .with_expanded(true)
                        .child(TreeNode::new("a_child")),
                )
                .child(
                    TreeNode::new("b")
                        .with_expanded(false)
                        .child(TreeNode::new("b_child")),
                ),
        );

        // Create snapshot
        let snapshot = tree.create_snapshot();

        // Verify initial state
        assert!(tree.is_node_expanded(&[0])); // a
        assert!(!tree.is_node_expanded(&[1])); // b

        // Modify state
        tree.collapse_node(&[0]); // collapse a
        tree.expand_node(&[1]); // expand b
        assert!(!tree.is_node_expanded(&[0]));
        assert!(tree.is_node_expanded(&[1]));

        // Restore snapshot
        assert!(tree.restore_snapshot(&*snapshot));

        // Verify restored state
        assert!(tree.is_node_expanded(&[0])); // a back to expanded
        assert!(!tree.is_node_expanded(&[1])); // b back to collapsed
    }

    #[test]
    fn tree_expand_collapse_node() {
        let mut tree =
            Tree::new(TreeNode::new("root").child(TreeNode::new("child").with_expanded(true)));

        // Initial state
        assert!(tree.is_node_expanded(&[0]));

        // Collapse
        tree.collapse_node(&[0]);
        assert!(!tree.is_node_expanded(&[0]));

        // Expand again
        tree.expand_node(&[0]);
        assert!(tree.is_node_expanded(&[0]));
    }

    #[test]
    fn tree_node_path_navigation() {
        let tree = Tree::new(
            TreeNode::new("root")
                .child(
                    TreeNode::new("a")
                        .child(TreeNode::new("a1"))
                        .child(TreeNode::new("a2")),
                )
                .child(TreeNode::new("b")),
        );

        // Test path navigation
        assert_eq!(tree.get_node_at_path(&[]).map(|n| n.label()), Some("root"));
        assert_eq!(tree.get_node_at_path(&[0]).map(|n| n.label()), Some("a"));
        assert_eq!(tree.get_node_at_path(&[1]).map(|n| n.label()), Some("b"));
        assert_eq!(
            tree.get_node_at_path(&[0, 0]).map(|n| n.label()),
            Some("a1")
        );
        assert_eq!(
            tree.get_node_at_path(&[0, 1]).map(|n| n.label()),
            Some("a2")
        );
        assert!(tree.get_node_at_path(&[5]).is_none()); // Invalid path
    }

    #[test]
    fn tree_restore_wrong_snapshot_type_fails() {
        use std::any::Any;
        let mut tree = Tree::new(TreeNode::new("root"));
        let wrong_snapshot: Box<dyn Any + Send> = Box::new(42i32);
        assert!(!tree.restore_snapshot(&*wrong_snapshot));
    }

    // --- Mouse handling tests ---

    use crate::mouse::MouseResult;
    use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};

    #[test]
    fn tree_click_expands_parent() {
        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(
                    TreeNode::new("a")
                        .child(TreeNode::new("a1"))
                        .child(TreeNode::new("a2")),
                )
                .child(TreeNode::new("b")),
        );
        assert!(tree.root().children()[0].is_expanded());

        // Click on row 1 which is node "a" (a parent node)
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 1);
        let hit = Some((HitId::new(1), HitRegion::Content, 1u64));
        let result = tree.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Activated(1));
        assert!(!tree.root().children()[0].is_expanded()); // toggled to collapsed
    }

    #[test]
    fn tree_click_selects_leaf() {
        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(
                    TreeNode::new("a")
                        .child(TreeNode::new("a1"))
                        .child(TreeNode::new("a2")),
                )
                .child(TreeNode::new("b")),
        );

        // Row 4 is "b" (a leaf): root=0, a=1, a1=2, a2=3, b=4
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 4);
        let hit = Some((HitId::new(1), HitRegion::Content, 4u64));
        let result = tree.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Selected(4));
    }

    #[test]
    fn tree_click_wrong_id_ignored() {
        let mut tree = Tree::new(TreeNode::new("root").child(TreeNode::new("a")));
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let hit = Some((HitId::new(99), HitRegion::Content, 0u64));
        let result = tree.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn tree_click_no_hit_ignored() {
        let mut tree = Tree::new(TreeNode::new("root"));
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let result = tree.handle_mouse(&event, None, HitId::new(1));
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn tree_right_click_ignored() {
        let mut tree = Tree::new(TreeNode::new("root").child(TreeNode::new("a")));
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Right), 0, 0);
        let hit = Some((HitId::new(1), HitRegion::Content, 0u64));
        let result = tree.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn tree_node_at_visible_index_with_show_root() {
        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(
                    TreeNode::new("a")
                        .child(TreeNode::new("a1"))
                        .child(TreeNode::new("a2")),
                )
                .child(TreeNode::new("b")),
        );

        // Visible order: root=0, a=1, a1=2, a2=3, b=4
        assert_eq!(
            tree.node_at_visible_index_mut(0)
                .map(|n| n.label().to_string()),
            Some("root".to_string())
        );
        assert_eq!(
            tree.node_at_visible_index_mut(1)
                .map(|n| n.label().to_string()),
            Some("a".to_string())
        );
        assert_eq!(
            tree.node_at_visible_index_mut(2)
                .map(|n| n.label().to_string()),
            Some("a1".to_string())
        );
        assert_eq!(
            tree.node_at_visible_index_mut(3)
                .map(|n| n.label().to_string()),
            Some("a2".to_string())
        );
        assert_eq!(
            tree.node_at_visible_index_mut(4)
                .map(|n| n.label().to_string()),
            Some("b".to_string())
        );
        assert!(tree.node_at_visible_index_mut(5).is_none());
    }

    #[test]
    fn tree_node_at_visible_index_hidden_root() {
        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(TreeNode::new("a").child(TreeNode::new("a1")))
                .child(TreeNode::new("b")),
        )
        .with_show_root(false);

        // Root hidden: a=0, a1=1, b=2
        assert_eq!(
            tree.node_at_visible_index_mut(0)
                .map(|n| n.label().to_string()),
            Some("a".to_string())
        );
        assert_eq!(
            tree.node_at_visible_index_mut(1)
                .map(|n| n.label().to_string()),
            Some("a1".to_string())
        );
        assert_eq!(
            tree.node_at_visible_index_mut(2)
                .map(|n| n.label().to_string()),
            Some("b".to_string())
        );
        assert!(tree.node_at_visible_index_mut(3).is_none());
    }

    #[test]
    fn tree_node_at_visible_index_collapsed() {
        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(
                    TreeNode::new("a")
                        .with_expanded(false)
                        .child(TreeNode::new("a1"))
                        .child(TreeNode::new("a2")),
                )
                .child(TreeNode::new("b")),
        );

        // root=0, a=1 (collapsed, so a1/a2 hidden), b=2
        assert_eq!(
            tree.node_at_visible_index_mut(0)
                .map(|n| n.label().to_string()),
            Some("root".to_string())
        );
        assert_eq!(
            tree.node_at_visible_index_mut(1)
                .map(|n| n.label().to_string()),
            Some("a".to_string())
        );
        assert_eq!(
            tree.node_at_visible_index_mut(2)
                .map(|n| n.label().to_string()),
            Some("b".to_string())
        );
        assert!(tree.node_at_visible_index_mut(3).is_none());
    }

    #[test]
    fn tree_click_toggles_collapsed_node() {
        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(
                    TreeNode::new("a")
                        .with_expanded(false)
                        .child(TreeNode::new("a1")),
                )
                .child(TreeNode::new("b")),
        );
        assert!(!tree.root().children()[0].is_expanded());

        // Click on "a" (row 1) to expand it
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 0, 1);
        let hit = Some((HitId::new(1), HitRegion::Content, 1u64));
        let result = tree.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Activated(1));
        assert!(tree.root().children()[0].is_expanded()); // now expanded
    }
}

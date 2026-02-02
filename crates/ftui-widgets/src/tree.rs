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

use crate::{Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;


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
    children: Vec<TreeNode>,
    expanded: bool,
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

    /// Flatten the visible tree into a list of (depth, is_last_at_each_depth, label) tuples.
    fn flatten_visible(&self, depth: usize, is_last: &[bool], out: &mut Vec<FlatNode>) {
        out.push(FlatNode {
            label: self.label.clone(),
            depth,
            is_last: is_last.to_vec(),
        });

        if !self.expanded {
            return;
        }

        let child_count = self.children.len();
        for (i, child) in self.children.iter().enumerate() {
            let mut child_is_last = is_last.to_vec();
            child_is_last.push(i == child_count - 1);
            child.flatten_visible(depth + 1, &child_is_last, out);
        }
    }
}

/// Flattened representation of a visible tree node for rendering.
#[derive(Debug, Clone)]
struct FlatNode {
    label: String,
    depth: usize,
    /// For each ancestor depth, whether it was the last child at that depth.
    is_last: Vec<bool>,
}

/// Tree widget for rendering hierarchical data.
#[derive(Debug, Clone)]
pub struct Tree {
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
}

impl Tree {
    /// Create a tree widget with the given root node.
    #[must_use]
    pub fn new(root: TreeNode) -> Self {
        Self {
            root,
            show_root: true,
            guides: TreeGuides::default(),
            guide_style: Style::default(),
            label_style: Style::default(),
            root_style: Style::default(),
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

    /// Get a reference to the root node.
    #[must_use]
    pub fn root(&self) -> &TreeNode {
        &self.root
    }

    /// Get a mutable reference to the root node.
    pub fn root_mut(&mut self) -> &mut TreeNode {
        &mut self.root
    }

    /// Flatten the tree into a list of visible nodes for rendering.
    fn flatten(&self) -> Vec<FlatNode> {
        let mut out = Vec::new();
        self.root.flatten_visible(0, &[], &mut out);
        if !self.show_root && !out.is_empty() {
            out.remove(0);
            // Decrease depth of remaining nodes
            for node in &mut out {
                node.depth = node.depth.saturating_sub(1);
                if !node.is_last.is_empty() {
                    node.is_last.remove(0);
                }
            }
        }
        out
    }
}

impl Widget for Tree {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        let flat = self.flatten();
        let max_x = area.right();

        for (row_idx, node) in flat.iter().enumerate() {
            if row_idx as u16 >= area.height {
                break;
            }

            let y = area.y + row_idx as u16;
            let mut x = area.x;

            // Draw guide characters for each depth level
            if node.depth > 0 && deg.apply_styling() {
                for d in 0..node.depth {
                    let is_last_at_depth = node.is_last.get(d).copied().unwrap_or(false);
                    let guide = if d == node.depth - 1 {
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
            } else if node.depth > 0 {
                // Minimal rendering: indent with spaces
                let indent = "    ".repeat(node.depth);
                x = draw_text_span(frame, x, y, &indent, Style::default(), max_x);
            }

            // Draw label
            let style = if node.depth == 0 && self.show_root {
                self.root_style
            } else {
                self.label_style
            };

            if deg.apply_styling() {
                draw_text_span(frame, x, y, &node.label, style, max_x);
            } else {
                draw_text_span(frame, x, y, &node.label, Style::default(), max_x);
            }
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    fn simple_tree() -> TreeNode {
        TreeNode::new("root")
            .child(TreeNode::new("a")
                .child(TreeNode::new("a1"))
                .child(TreeNode::new("a2")))
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
            .child(TreeNode::new("a").with_expanded(false)
                .child(TreeNode::new("a1"))
                .child(TreeNode::new("a2")))
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
        for g in [TreeGuides::Ascii, TreeGuides::Unicode, TreeGuides::Bold, TreeGuides::Double, TreeGuides::Rounded] {
            assert_eq!(g.width(), 4);
        }
    }

    #[test]
    fn tree_flatten_with_root() {
        let tree = Tree::new(simple_tree());
        let flat = tree.flatten();
        assert_eq!(flat.len(), 5);
        assert_eq!(flat[0].label, "root");
        assert_eq!(flat[0].depth, 0);
        assert_eq!(flat[1].label, "a");
        assert_eq!(flat[1].depth, 1);
        assert_eq!(flat[2].label, "a1");
        assert_eq!(flat[2].depth, 2);
        assert_eq!(flat[3].label, "a2");
        assert_eq!(flat[3].depth, 2);
        assert_eq!(flat[4].label, "b");
        assert_eq!(flat[4].depth, 1);
    }

    #[test]
    fn tree_flatten_without_root() {
        let tree = Tree::new(simple_tree()).with_show_root(false);
        let flat = tree.flatten();
        assert_eq!(flat.len(), 4);
        assert_eq!(flat[0].label, "a");
        assert_eq!(flat[0].depth, 0);
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
                .child(TreeNode::new("b"))
        ).with_guides(TreeGuides::Ascii);

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
}

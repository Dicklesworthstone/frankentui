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
use ftui_core::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::Style;
use std::any::Any;
use std::collections::HashSet;
#[cfg(feature = "tracing")]
use web_time::Instant;

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
    #[inline]
    #[must_use]
    pub fn width(&self) -> usize {
        4
    }
}

/// A node in the tree hierarchy.
#[derive(Debug, Clone)]
pub struct TreeNode {
    label: String,
    icon: Option<String>,
    /// Child nodes (crate-visible for undo support).
    pub(crate) children: Vec<TreeNode>,
    /// Lazily materialized children.
    lazy_children: Option<Vec<TreeNode>>,
    /// Whether this node is expanded (crate-visible for undo support).
    pub(crate) expanded: bool,
}

impl TreeNode {
    /// Create a new tree node with the given label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            children: Vec::new(),
            lazy_children: None,
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

    /// Set an icon prefix rendered before the label.
    #[must_use]
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Configure lazily materialized children.
    ///
    /// The node starts collapsed and children are attached when first expanded.
    #[must_use]
    pub fn with_lazy_children(mut self, nodes: Vec<TreeNode>) -> Self {
        self.lazy_children = Some(nodes);
        self.expanded = false;
        self
    }

    /// Set whether this node is expanded.
    #[must_use]
    pub fn with_expanded(mut self, expanded: bool) -> Self {
        if expanded {
            self.materialize_lazy_children();
        }
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

    /// Optional icon rendered before label.
    #[must_use]
    pub fn icon(&self) -> Option<&str> {
        self.icon.as_deref()
    }

    /// Whether this node has loaded or lazy children.
    #[must_use]
    pub fn has_children(&self) -> bool {
        !self.children.is_empty()
            || self
                .lazy_children
                .as_ref()
                .is_some_and(|children| !children.is_empty())
    }

    /// Whether this node is expanded.
    #[must_use]
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Toggle the expanded state.
    pub fn toggle_expanded(&mut self) {
        if !self.expanded {
            self.materialize_lazy_children();
        }
        self.expanded = !self.expanded;
    }

    fn materialize_lazy_children(&mut self) {
        if let Some(mut lazy) = self.lazy_children.take() {
            self.children.append(&mut lazy);
        }
    }

    #[cfg(feature = "tracing")]
    fn total_count(&self) -> usize {
        let mut count = 1usize;
        for child in &self.children {
            count = count.saturating_add(child.total_count());
        }
        if let Some(lazy) = &self.lazy_children {
            for child in lazy {
                count = count.saturating_add(child.total_count());
            }
        }
        count
    }

    #[cfg(feature = "tracing")]
    fn expanded_count(&self) -> usize {
        let mut count = usize::from(self.expanded && self.has_children());
        for child in &self.children {
            count = count.saturating_add(child.expanded_count());
        }
        if let Some(lazy) = &self.lazy_children {
            for child in lazy {
                count = count.saturating_add(child.expanded_count());
            }
        }
        count
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

        if self.expanded && self.has_children() {
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

        if self.has_children() {
            self.expanded = expanded_paths.contains(&path);
            if self.expanded {
                self.materialize_lazy_children();
            }
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
    /// Optional case-insensitive search query.
    search_query: Option<String>,
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
            search_query: None,
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

    /// Apply a case-insensitive search query filter.
    #[must_use]
    pub fn with_search_query(mut self, query: impl Into<String>) -> Self {
        let query = query.into();
        self.search_query = if query.trim().is_empty() {
            None
        } else {
            Some(query)
        };
        self
    }

    /// Clear search filtering.
    #[must_use]
    pub fn without_search_query(mut self) -> Self {
        self.search_query = None;
        self
    }

    #[cfg(feature = "tracing")]
    fn total_nodes(&self) -> usize {
        if self.show_root {
            self.root.total_count()
        } else if self.root.expanded {
            self.root
                .children
                .iter()
                .fold(0usize, |acc, child| acc.saturating_add(child.total_count()))
        } else {
            0
        }
    }

    #[cfg(feature = "tracing")]
    fn visible_nodes(&self) -> usize {
        if self.show_root {
            self.root.visible_count()
        } else if self.root.expanded {
            self.root.children.iter().fold(0usize, |acc, child| {
                acc.saturating_add(child.visible_count())
            })
        } else {
            0
        }
    }

    #[cfg(feature = "tracing")]
    fn expanded_nodes(&self) -> usize {
        if self.show_root {
            self.root.expanded_count()
        } else if self.root.expanded {
            self.root.children.iter().fold(0usize, |acc, child| {
                acc.saturating_add(child.expanded_count())
            })
        } else {
            0
        }
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
            // Avoid allocation by drawing chunks iteratively
            for _ in 0..depth {
                x = draw_text_span(frame, x, y, "    ", Style::default(), max_x);
                if x >= max_x {
                    break;
                }
            }
        }

        // Draw label
        let style = if depth == 0 && self.show_root {
            self.root_style
        } else {
            self.label_style
        };
        if let Some(icon) = node.icon() {
            let icon_style = if deg.apply_styling() {
                style
            } else {
                Style::default()
            };
            x = draw_text_span(frame, x, y, icon, icon_style, max_x);
            if x < max_x {
                x = draw_text_span(frame, x, y, " ", icon_style, max_x);
            }
        }

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
            if *current_row >= area.height as usize {
                break;
            }
            is_last.push(i == child_count - 1);
            self.render_node(child, depth + 1, is_last, area, frame, current_row, deg);
            is_last.pop();
        }
    }
}

fn filter_node(node: &TreeNode, query_lower: &str) -> Option<TreeNode> {
    let label_matches = node.label.to_lowercase().contains(query_lower)
        || node
            .icon
            .as_deref()
            .is_some_and(|icon| icon.to_lowercase().contains(query_lower));

    let mut filtered_children = Vec::new();
    for child in &node.children {
        if let Some(filtered) = filter_node(child, query_lower) {
            filtered_children.push(filtered);
        }
    }

    let mut filtered_lazy = Vec::new();
    if let Some(lazy) = &node.lazy_children {
        for child in lazy {
            if let Some(filtered) = filter_node(child, query_lower) {
                filtered_lazy.push(filtered);
            }
        }
    }

    if !label_matches && filtered_children.is_empty() && filtered_lazy.is_empty() {
        return None;
    }

    let mut filtered = node.clone();
    if !label_matches {
        // Materialize filtered lazy matches into `children` so render/flatten traversal,
        // which walks `children`, includes lazy descendants that matched the query.
        filtered.children = filtered_children;
        filtered.children.extend(filtered_lazy);
        filtered.lazy_children = None;
        filtered.expanded = true;
    }
    Some(filtered)
}

impl Widget for Tree {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        #[cfg(feature = "tracing")]
        let render_start = Instant::now();
        #[cfg(feature = "tracing")]
        let total_nodes = self.total_nodes();
        #[cfg(feature = "tracing")]
        let visible_nodes = self.visible_nodes();
        #[cfg(feature = "tracing")]
        let expanded_count = self.expanded_nodes();
        #[cfg(feature = "tracing")]
        let render_span = tracing::debug_span!(
            "tree.render",
            total_nodes,
            visible_nodes,
            expanded_count,
            render_duration_us = tracing::field::Empty,
        );
        #[cfg(feature = "tracing")]
        let _render_guard = render_span.enter();

        let deg = frame.buffer.degradation;
        let mut current_row = 0;
        let mut is_last = Vec::with_capacity(8);

        let filtered_root = self.search_query.as_deref().and_then(|query| {
            let query = query.trim();
            if query.is_empty() {
                return Some(self.root.clone());
            }
            let query_lower = query.to_lowercase();
            filter_node(&self.root, &query_lower)
        });
        let root = filtered_root.as_ref().unwrap_or(&self.root);

        if self.show_root {
            self.render_node(root, 0, &mut is_last, area, frame, &mut current_row, deg);
        } else if root.expanded {
            // If root is hidden but expanded, render children as top-level nodes.
            // We do NOT push to is_last for the root level, effectively shifting
            // the hierarchy up by one level.
            let child_count = root.children.len();
            for (i, child) in root.children.iter().enumerate() {
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

        #[cfg(feature = "tracing")]
        {
            let elapsed = render_start.elapsed();
            let elapsed_us = elapsed.as_micros() as u64;
            render_span.record("render_duration_us", elapsed_us);
            tracing::debug!(
                message = "tree.metrics",
                tree_render_duration_us = elapsed_us,
                total_nodes,
                visible_nodes,
                expanded_count
            );
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
            node.materialize_lazy_children();
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

    #[cfg(feature = "tracing")]
    fn log_expand_collapse(action: &str, source: &str, index: usize, label: &str) {
        tracing::debug!(
            message = "tree.toggle",
            action,
            source,
            visible_index = index,
            label
        );
    }

    fn toggle_node_at_visible_index(&mut self, index: usize, source: &str) -> bool {
        #[cfg(not(feature = "tracing"))]
        let _ = source;
        let Some(node) = self.node_at_visible_index_mut(index) else {
            return false;
        };
        if !node.has_children() {
            return false;
        }
        #[cfg(feature = "tracing")]
        let action = if node.is_expanded() {
            "collapse"
        } else {
            "expand"
        };
        #[cfg(feature = "tracing")]
        let label = node.label().to_owned();
        node.toggle_expanded();
        #[cfg(feature = "tracing")]
        Self::log_expand_collapse(action, source, index, &label);
        true
    }

    /// Handle keyboard expand/collapse at the currently selected visible row.
    ///
    /// Returns `true` when an expand/collapse action was applied.
    pub fn handle_key(&mut self, key: &KeyEvent, selected_visible_index: usize) -> bool {
        match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.toggle_node_at_visible_index(selected_visible_index, "keyboard")
            }
            _ => false,
        }
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
                if let Some((id, HitRegion::Content, data)) = hit
                    && id == expected_id
                {
                    let index = data as usize;
                    if let Some(node) = self.node_at_visible_index_mut(index)
                        && !node.has_children()
                    {
                        return MouseResult::Selected(index);
                    }
                    if self.toggle_node_at_visible_index(index, "mouse") {
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
        let path = self.find_path_indices_at_visible_index(target)?;
        
        let mut current = &mut self.root;
        for &idx in &path {
            current.materialize_lazy_children();
            current = current.children.get_mut(idx)?;
        }
        Some(current)
    }

    fn find_path_indices_at_visible_index(&self, target: usize) -> Option<Vec<usize>> {
        let filtered_root = self.search_query.as_deref().and_then(|query| {
            let query = query.trim();
            if query.is_empty() {
                return None;
            }
            let query_lower = query.to_lowercase();
            filter_node(&self.root, &query_lower)
        });
        let root = filtered_root.as_ref().unwrap_or(&self.root);

        let mut counter = 0usize;
        let mut path = Vec::new();
        
        if self.show_root {
            Self::walk_visible_index_path(root, target, &mut counter, &mut path)
        } else if root.expanded {
            for (idx, child) in root.children.iter().enumerate() {
                path.push(idx);
                if let Some(p) = Self::walk_visible_index_path(child, target, &mut counter, &mut path) {
                    return Some(p);
                }
                path.pop();
            }
            None
        } else {
            None
        }
    }

    fn walk_visible_index_path(
        node: &TreeNode,
        target: usize,
        counter: &mut usize,
        current_path: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        if *counter == target {
            return Some(current_path.clone());
        }
        *counter += 1;
        if node.expanded {
            for (idx, child) in node.children.iter().enumerate() {
                current_path.push(idx);
                if let Some(found) = Self::walk_visible_index_path(child, target, counter, current_path) {
                    return Some(found);
                }
                current_path.pop();
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
        let filtered_root = self.search_query.as_deref().and_then(|query| {
            let query = query.trim();
            if query.is_empty() {
                return Some(self.root.clone());
            }
            let query_lower = query.to_lowercase();
            filter_node(&self.root, &query_lower)
        });
        let root = filtered_root.as_ref().unwrap_or(&self.root);
        if self.show_root {
            flatten_visible(root, 0, &mut out);
        } else if root.expanded {
            for child in &root.children {
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
    #[cfg(feature = "tracing")]
    use std::sync::{Arc, Mutex};
    #[cfg(feature = "tracing")]
    use tracing::Subscriber;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::Layer;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::layer::{Context, SubscriberExt};

    fn simple_tree() -> TreeNode {
        TreeNode::new("root")
            .child(
                TreeNode::new("a")
                    .child(TreeNode::new("a1"))
                    .child(TreeNode::new("a2")),
            )
            .child(TreeNode::new("b"))
    }

    #[cfg(feature = "tracing")]
    #[derive(Debug, Default)]
    struct TreeTraceState {
        tree_render_seen: bool,
        has_total_nodes_field: bool,
        has_visible_nodes_field: bool,
        has_expanded_count_field: bool,
        render_duration_recorded: bool,
        toggle_events: usize,
    }

    #[cfg(feature = "tracing")]
    struct TreeTraceCapture {
        state: Arc<Mutex<TreeTraceState>>,
    }

    #[cfg(feature = "tracing")]
    impl<S> Layer<S> for TreeTraceCapture
    where
        S: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::Id,
            _ctx: Context<'_, S>,
        ) {
            if attrs.metadata().name() != "tree.render" {
                return;
            }
            let fields = attrs.metadata().fields();
            let mut state = self.state.lock().expect("tree trace state lock");
            state.tree_render_seen = true;
            state.has_total_nodes_field |= fields.field("total_nodes").is_some();
            state.has_visible_nodes_field |= fields.field("visible_nodes").is_some();
            state.has_expanded_count_field |= fields.field("expanded_count").is_some();
        }

        fn on_record(
            &self,
            id: &tracing::Id,
            values: &tracing::span::Record<'_>,
            ctx: Context<'_, S>,
        ) {
            let Some(span) = ctx.span(id) else {
                return;
            };
            if span.metadata().name() != "tree.render" {
                return;
            }

            struct DurationVisitor {
                saw_duration: bool,
            }
            impl tracing::field::Visit for DurationVisitor {
                fn record_u64(&mut self, field: &tracing::field::Field, _value: u64) {
                    if field.name() == "render_duration_us" {
                        self.saw_duration = true;
                    }
                }

                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    _value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "render_duration_us" {
                        self.saw_duration = true;
                    }
                }
            }

            let mut visitor = DurationVisitor {
                saw_duration: false,
            };
            values.record(&mut visitor);
            if visitor.saw_duration {
                self.state
                    .lock()
                    .expect("tree trace state lock")
                    .render_duration_recorded = true;
            }
        }

        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            struct MessageVisitor {
                message: Option<String>,
            }
            impl tracing::field::Visit for MessageVisitor {
                fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                    if field.name() == "message" {
                        self.message = Some(value.to_owned());
                    }
                }

                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.message = Some(format!("{value:?}").trim_matches('"').to_owned());
                    }
                }
            }

            let mut visitor = MessageVisitor { message: None };
            event.record(&mut visitor);
            if visitor.message.as_deref() == Some("tree.toggle") {
                let mut state = self.state.lock().expect("tree trace state lock");
                state.toggle_events = state.toggle_events.saturating_add(1);
            }
        }
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
    fn tree_node_lazy_children_materialize_on_expand() {
        let mut node = TreeNode::new("root")
            .with_lazy_children(vec![TreeNode::new("child"), TreeNode::new("child2")]);
        assert!(!node.is_expanded());
        assert_eq!(node.children().len(), 0);
        assert!(node.has_children());

        node.toggle_expanded();
        assert!(node.is_expanded());
        assert_eq!(node.children().len(), 2);
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
    fn tree_render_icon_before_label() {
        let tree = Tree::new(TreeNode::new("root").with_icon(">"));
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 2, &mut pool);
        tree.render(Rect::new(0, 0, 12, 2), &mut frame);

        assert_eq!(
            frame.buffer.get(0, 0).and_then(|c| c.content.as_char()),
            Some('>')
        );
        assert_eq!(
            frame.buffer.get(2, 0).and_then(|c| c.content.as_char()),
            Some('r')
        );
    }

    #[test]
    fn tree_search_query_filters_to_matching_branches() {
        let tree = Tree::new(
            TreeNode::new("root")
                .child(TreeNode::new("alpha").child(TreeNode::new("target-file")))
                .child(TreeNode::new("beta")),
        )
        .with_search_query("target");

        let flat = tree.flatten();
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].label, "root");
        assert_eq!(flat[1].label, "alpha");
        assert_eq!(flat[2].label, "target-file");
    }

    #[test]
    fn tree_search_query_includes_lazy_matching_descendants() {
        let tree =
            Tree::new(TreeNode::new("root").child(
                TreeNode::new("alpha").with_lazy_children(vec![TreeNode::new("target-file")]),
            ))
            .with_search_query("target");

        let flat = tree.flatten();
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].label, "root");
        assert_eq!(flat[1].label, "alpha");
        assert_eq!(flat[2].label, "target-file");
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
    use ftui_core::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

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

    #[test]
    fn tree_handle_key_enter_toggles_parent() {
        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(TreeNode::new("a").child(TreeNode::new("a1")))
                .child(TreeNode::new("b")),
        );

        // root=0, a=1, a1=2, b=3
        assert!(tree.root().children()[0].is_expanded());
        assert!(tree.handle_key(&KeyEvent::new(KeyCode::Enter), 1));
        assert!(!tree.root().children()[0].is_expanded());
        assert!(tree.handle_key(&KeyEvent::new(KeyCode::Char(' ')), 1));
        assert!(tree.root().children()[0].is_expanded());
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn tree_tracing_span_and_toggle_events_are_emitted() {
        let trace_state = Arc::new(Mutex::new(TreeTraceState::default()));
        let subscriber = tracing_subscriber::registry().with(TreeTraceCapture {
            state: Arc::clone(&trace_state),
        });
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let mut tree = Tree::new(
            TreeNode::new("root")
                .child(TreeNode::new("a").child(TreeNode::new("a1")))
                .child(TreeNode::new("b")),
        );
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 6, &mut pool);
        tree.render(Rect::new(0, 0, 20, 6), &mut frame);
        assert!(tree.handle_key(&KeyEvent::new(KeyCode::Enter), 1));

        tracing::callsite::rebuild_interest_cache();
        let snapshot = trace_state.lock().expect("tree trace state lock");
        assert!(snapshot.tree_render_seen, "expected tree.render span");
        assert!(
            snapshot.has_total_nodes_field,
            "tree.render missing total_nodes"
        );
        assert!(
            snapshot.has_visible_nodes_field,
            "tree.render missing visible_nodes"
        );
        assert!(
            snapshot.has_expanded_count_field,
            "tree.render missing expanded_count"
        );
        assert!(
            snapshot.render_duration_recorded,
            "tree.render did not record render_duration_us"
        );
        assert!(
            snapshot.toggle_events >= 1,
            "expected tree.toggle debug event"
        );
    }
}

//! Accessibility tree construction and diffing.
//!
//! During each render pass, widgets push [`A11yNodeInfo`] into an
//! [`A11yTreeBuilder`]. At the end of the pass the builder is consumed
//! to produce an immutable [`A11yTree`] snapshot. Consecutive snapshots
//! can be diffed to produce an [`A11yTreeDiff`] describing exactly what
//! changed -- this is the data a platform accessibility bridge would
//! push to the OS.

use ahash::{AHashMap, AHashSet};

use crate::node::{A11yNodeInfo, A11yRole, LiveRegion};

const MAX_TRAVERSAL_DEPTH: usize = 1000;

// ── Builder ────────────────────────────────────────────────────────────

/// Accumulates accessibility nodes during a render pass.
///
/// Usage:
/// 1. Create with [`A11yTreeBuilder::new`].
/// 2. Add nodes via [`add_node`](Self::add_node).
/// 3. Optionally designate the root and focused node.
/// 4. Call [`build`](Self::build) to freeze.
#[derive(Debug, Clone)]
pub struct A11yTreeBuilder {
    nodes: AHashMap<u64, A11yNodeInfo>,
    root: Option<u64>,
    focused: Option<u64>,
}

impl Default for A11yTreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl A11yTreeBuilder {
    /// Create an empty builder.
    #[inline]
    pub fn new() -> Self {
        Self {
            nodes: AHashMap::new(),
            root: None,
            focused: None,
        }
    }

    /// The explicitly focused node, if `set_focused` was called.
    #[inline]
    #[must_use]
    pub fn focused(&self) -> Option<u64> {
        self.focused
    }

    /// Create a builder pre-sized for `capacity` nodes (avoids reallocs).
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            nodes: AHashMap::with_capacity(capacity),
            root: None,
            focused: None,
        }
    }

    /// Insert or replace a node.
    #[inline]
    pub fn add_node(&mut self, node: A11yNodeInfo) {
        self.nodes.insert(node.id, node);
    }

    /// Designate the root node ID.
    #[inline]
    pub fn set_root(&mut self, id: u64) {
        self.root = Some(id);
    }

    /// Designate the currently focused node.
    #[inline]
    pub fn set_focused(&mut self, id: Option<u64>) {
        self.focused = id;
    }

    /// The root id chosen so far, if any.
    #[inline]
    #[must_use]
    pub fn root(&self) -> Option<u64> {
        self.root
    }

    /// A node added so far.
    #[inline]
    #[must_use]
    pub fn node(&self, id: u64) -> Option<&A11yNodeInfo> {
        self.nodes.get(&id)
    }

    /// Mutable access to a node added so far (used to wire children in).
    #[inline]
    pub fn node_mut(&mut self, id: u64) -> Option<&mut A11yNodeInfo> {
        self.nodes.get_mut(&id)
    }

    /// Number of nodes added so far.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether no node has been added.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Consume the builder and produce an immutable tree snapshot.
    #[inline]
    pub fn build(self) -> A11yTree {
        A11yTree {
            nodes: self.nodes,
            root: self.root,
            focused: self.focused,
        }
    }
}

// ── Immutable tree ─────────────────────────────────────────────────────

/// Immutable snapshot of the accessibility tree after a render pass.
///
/// The tree is a flat map keyed by node ID; parent/child relationships
/// are encoded inside each [`A11yNodeInfo`]. This makes traversal O(1)
/// per hop and diffing O(n) in the number of nodes.
#[derive(Debug, Clone)]
pub struct A11yTree {
    nodes: AHashMap<u64, A11yNodeInfo>,
    root: Option<u64>,
    focused: Option<u64>,
}

impl Default for A11yTree {
    fn default() -> Self {
        Self::empty()
    }
}

impl A11yTree {
    /// Create an empty tree (no nodes, no root, no focus).
    #[inline]
    pub fn empty() -> Self {
        Self {
            nodes: AHashMap::new(),
            root: None,
            focused: None,
        }
    }

    /// Look up a node by ID.
    #[inline]
    pub fn node(&self, id: u64) -> Option<&A11yNodeInfo> {
        self.nodes.get(&id)
    }

    /// The root node, if set and present.
    #[inline]
    pub fn root(&self) -> Option<&A11yNodeInfo> {
        self.root.and_then(|id| self.nodes.get(&id))
    }

    /// The root node ID, if set.
    #[inline]
    pub fn root_id(&self) -> Option<u64> {
        self.root
    }

    /// The focused node, if set and present.
    #[inline]
    pub fn focused(&self) -> Option<&A11yNodeInfo> {
        self.focused.and_then(|id| self.nodes.get(&id))
    }

    /// The focused node ID, if set.
    #[inline]
    pub fn focused_id(&self) -> Option<u64> {
        self.focused
    }

    /// Iterate over all nodes in unspecified order.
    #[inline]
    pub fn nodes(&self) -> impl Iterator<Item = &A11yNodeInfo> {
        self.nodes.values()
    }

    /// Total number of nodes.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree contains zero nodes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Get all children of a given node ID, in the order stored.
    pub fn children_of(&self, id: u64) -> Vec<&A11yNodeInfo> {
        self.nodes
            .get(&id)
            .map(|n| {
                n.children
                    .iter()
                    .filter_map(|cid| self.nodes.get(cid))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// One text line per node in `order` (document order as collected by the
    /// frame): `{indent}{role:?} "{name}" [{states}] @{x},{y} {w}x{h}`, with
    /// the indent equal to the node's depth. This is the format the
    /// accessibility panel and the tree snapshots share. Ids missing from the
    /// tree are skipped.
    #[must_use]
    pub fn dump_text(&self, order: &[u64]) -> String {
        let mut out = String::new();
        for &id in order {
            let Some(node) = self.node(id) else {
                continue;
            };
            // `ancestors` includes the node itself, so depth = chain - 1.
            let depth = self.ancestors(id).len().saturating_sub(1);
            let mut states: Vec<&str> = Vec::new();
            if node.state.focused {
                states.push("focused");
            }
            if node.state.disabled {
                states.push("disabled");
            }
            if node.state.selected {
                states.push("selected");
            }
            match node.state.checked {
                Some(true) => states.push("checked"),
                Some(false) => states.push("unchecked"),
                None => {}
            }
            match node.state.expanded {
                Some(true) => states.push("expanded"),
                Some(false) => states.push("collapsed"),
                None => {}
            }
            if node.state.readonly {
                states.push("readonly");
            }
            if node.state.required {
                states.push("required");
            }
            if node.state.busy {
                states.push("busy");
            }
            let name = node.name.as_deref().unwrap_or("");
            let b = node.bounds;
            out.push_str(&format!(
                "{}{:?} \"{}\" [{}] @{},{} {}x{}\n",
                "  ".repeat(depth),
                node.role,
                name,
                states.join(","),
                b.x,
                b.y,
                b.width,
                b.height
            ));
        }
        out
    }

    /// Walk ancestors from `id` up to the root (inclusive), returning
    /// their IDs. Returns an empty vec if `id` is not found.
    ///
    /// Includes cycle protection: stops after visiting 1000 nodes to
    /// prevent infinite loops on malformed cyclic parent chains.
    pub fn ancestors(&self, id: u64) -> Vec<u64> {
        let mut path = Vec::new();
        let mut visited = AHashSet::new();
        let mut current = Some(id);
        while let Some(cid) = current {
            if path.len() >= MAX_TRAVERSAL_DEPTH || !visited.insert(cid) {
                break;
            }
            if let Some(node) = self.nodes.get(&cid) {
                path.push(cid);
                current = node.parent;
            } else {
                break;
            }
        }
        path
    }

    /// Produce deterministic screen-reader mirror lines for this snapshot.
    ///
    /// Traversal starts at the root and follows explicit child order. Any
    /// disconnected nodes are appended by sorted node ID so malformed partial
    /// trees still produce stable diagnostics. Presentational nodes are skipped.
    pub fn screen_reader_mirror(&self, policy: ScreenReaderPolicy) -> ScreenReaderMirror {
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut visited = AHashSet::with_capacity(self.nodes.len());

        if let Some(root) = self.root {
            self.collect_mirror_order(root, 0, &mut visited, &mut order);
        }

        let mut disconnected: Vec<u64> = self
            .nodes
            .keys()
            .copied()
            .filter(|id| !visited.contains(id))
            .collect();
        disconnected.sort_unstable();

        for id in disconnected {
            self.collect_mirror_order(id, 0, &mut visited, &mut order);
        }

        let mut lines = Vec::new();
        let mut omitted_nodes = 0;

        for (id, depth) in order {
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            if node.role == A11yRole::Presentation {
                continue;
            }
            if lines.len() >= policy.max_mirror_nodes {
                omitted_nodes += 1;
                continue;
            }

            let indent = "  ".repeat(depth.min(16));
            let summary = node_summary(node, self.focused == Some(id), true);
            let mut line = String::with_capacity(indent.len() + summary.len());
            line.push_str(&indent);
            line.push_str(&summary);
            lines.push(limit_text(line, policy.max_text_chars));
        }

        ScreenReaderMirror {
            lines,
            omitted_nodes,
        }
    }

    /// Extract bounded screen-reader announcements from the previous snapshot.
    pub fn screen_reader_announcements_since(
        &self,
        previous: &A11yTree,
        policy: ScreenReaderPolicy,
    ) -> ScreenReaderAnnouncements {
        self.diff(previous)
            .screen_reader_announcements(self, policy)
    }

    fn collect_mirror_order(
        &self,
        id: u64,
        depth: usize,
        visited: &mut AHashSet<u64>,
        order: &mut Vec<(u64, usize)>,
    ) {
        if depth >= MAX_TRAVERSAL_DEPTH || !visited.insert(id) {
            return;
        }

        let Some(node) = self.nodes.get(&id) else {
            return;
        };

        order.push((id, depth));
        for child_id in &node.children {
            self.collect_mirror_order(*child_id, depth + 1, visited, order);
        }
    }

    /// Diff this tree against a previous snapshot to find changes.
    ///
    /// Returns an [`A11yTreeDiff`] describing additions, removals,
    /// property changes, and focus transitions.
    pub fn diff(&self, previous: &A11yTree) -> A11yTreeDiff {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();

        // Nodes in self but not in previous => added.
        // Nodes in both => check for property changes.
        for (&id, node) in &self.nodes {
            match previous.nodes.get(&id) {
                None => added.push(id),
                Some(old) => {
                    let changes = diff_node(old, node);
                    if !changes.is_empty() {
                        changed.push((id, changes));
                    }
                }
            }
        }

        // Nodes in previous but not in self => removed.
        for &id in previous.nodes.keys() {
            if !self.nodes.contains_key(&id) {
                removed.push(id);
            }
        }
        added.sort_unstable();
        removed.sort_unstable();
        changed.sort_unstable_by_key(|(id, _)| *id);

        let focus_changed = if self.focused != previous.focused {
            Some((previous.focused, self.focused))
        } else {
            None
        };

        A11yTreeDiff {
            added,
            removed,
            changed,
            focus_changed,
        }
    }
}

// ── Diff types ─────────────────────────────────────────────────────────

/// Changes between two accessibility tree snapshots.
///
/// Produced by [`A11yTree::diff`]. A platform bridge would translate
/// these into OS accessibility events.
#[derive(Debug, Clone)]
pub struct A11yTreeDiff {
    /// Node IDs that are new in the current tree.
    pub added: Vec<u64>,
    /// Node IDs that were in the previous tree but are gone now.
    pub removed: Vec<u64>,
    /// Node IDs whose properties changed, with details.
    pub changed: Vec<(u64, Vec<A11yChange>)>,
    /// Focus transition: `Some((old_focus, new_focus))`.
    /// Either side may be `None` if focus was gained/lost entirely.
    pub focus_changed: Option<(Option<u64>, Option<u64>)>,
}

impl A11yTreeDiff {
    /// Returns `true` if nothing changed between the two snapshots.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && self.focus_changed.is_none()
    }

    /// Convert this diff into bounded screen-reader announcements.
    ///
    /// Focus changes are announced first. Live-region content changes then
    /// follow in deterministic node-ID order, with assertive announcements
    /// ahead of polite announcements within the same reason class.
    pub fn screen_reader_announcements(
        &self,
        current: &A11yTree,
        policy: ScreenReaderPolicy,
    ) -> ScreenReaderAnnouncements {
        let mut candidates = Vec::new();

        if let Some((_, Some(new_focus))) = self.focus_changed
            && let Some(node) = current.node(new_focus)
            && node.role != A11yRole::Presentation
            && let Some(text) = announcement_text(node, current.focused == Some(new_focus), false)
        {
            candidates.push(ScreenReaderAnnouncement {
                node_id: Some(new_focus),
                urgency: LiveRegion::Polite,
                reason: AnnouncementReason::FocusChanged,
                text,
            });
        }

        for id in &self.added {
            if let Some(node) = current.node(*id)
                && let Some(urgency) = node.live_region
                && let Some(text) = announcement_text(node, current.focused == Some(*id), true)
            {
                candidates.push(ScreenReaderAnnouncement {
                    node_id: Some(*id),
                    urgency,
                    reason: AnnouncementReason::LiveRegionAdded,
                    text,
                });
            }
        }

        for (id, changes) in &self.changed {
            if let Some(node) = current.node(*id)
                && let Some(urgency) = node.live_region
                && let Some(reason) = announcement_reason(changes)
                && let Some(text) = announcement_text(node, current.focused == Some(*id), true)
            {
                candidates.push(ScreenReaderAnnouncement {
                    node_id: Some(*id),
                    urgency,
                    reason,
                    text,
                });
            }
        }

        candidates
            .sort_by(|left, right| announcement_sort_key(left).cmp(&announcement_sort_key(right)));

        let max = policy.max_announcements;
        let dropped_count = candidates.len().saturating_sub(max);
        let announcements = candidates
            .into_iter()
            .take(max)
            .map(|mut announcement| {
                announcement.text = limit_text(announcement.text, policy.max_text_chars);
                announcement
            })
            .collect();

        ScreenReaderAnnouncements {
            announcements,
            dropped_count,
        }
    }
}

/// Bounded output policy for screen-reader mirrors and announcements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenReaderPolicy {
    /// Maximum non-presentational nodes to include in mirror output.
    pub max_mirror_nodes: usize,
    /// Maximum announcements to emit for one diff.
    pub max_announcements: usize,
    /// Maximum Unicode scalar values in each mirror line or announcement.
    pub max_text_chars: usize,
}

impl Default for ScreenReaderPolicy {
    fn default() -> Self {
        Self {
            max_mirror_nodes: 128,
            max_announcements: 8,
            max_text_chars: 240,
        }
    }
}

/// Deterministic text mirror for assistive-technology bridges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenReaderMirror {
    /// Pre-order, human-readable lines for the accessible tree.
    pub lines: Vec<String>,
    /// Number of non-presentational nodes dropped by the mirror cap.
    pub omitted_nodes: usize,
}

impl ScreenReaderMirror {
    /// Join mirror lines with newlines for platform bridges that expect text.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// Bounded announcement batch for one tree transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenReaderAnnouncements {
    /// Announcements retained after applying [`ScreenReaderPolicy`].
    pub announcements: Vec<ScreenReaderAnnouncement>,
    /// Number of otherwise valid announcements dropped by the batch cap.
    pub dropped_count: usize,
}

/// One screen-reader announcement derived from focus or live-region changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenReaderAnnouncement {
    /// Node that caused the announcement, when known.
    pub node_id: Option<u64>,
    /// Politeness / interruption level.
    pub urgency: LiveRegion,
    /// Why this announcement was emitted.
    pub reason: AnnouncementReason,
    /// Bounded, normalized text.
    pub text: String,
}

/// Reason a screen-reader announcement was emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnouncementReason {
    /// Keyboard focus moved to this node.
    FocusChanged,
    /// A live region appeared in the current tree.
    LiveRegionAdded,
    /// Live-region text or user-facing state changed.
    LiveContentChanged,
    /// The live-region policy itself changed.
    LiveRegionChanged,
}

/// A single property change on an accessibility node.
#[derive(Debug, Clone, PartialEq)]
pub enum A11yChange {
    /// The accessible name changed.
    NameChanged {
        old: Option<String>,
        new: Option<String>,
    },
    /// The role changed (unusual but possible during dynamic UIs).
    RoleChanged { old: A11yRole, new: A11yRole },
    /// A state flag changed. `field` is the flag name, `description`
    /// is a human-readable summary of the new value.
    StateChanged { field: String, description: String },
    /// The bounding rectangle moved or resized.
    BoundsChanged,
    /// The set of child IDs changed.
    ChildrenChanged,
    /// The live-region policy changed.
    LiveRegionChanged {
        old: Option<LiveRegion>,
        new: Option<LiveRegion>,
    },
    /// The accessible description changed.
    DescriptionChanged {
        old: Option<String>,
        new: Option<String>,
    },
    /// The keyboard shortcut hint changed.
    ShortcutChanged {
        old: Option<String>,
        new: Option<String>,
    },
    /// The parent node ID changed.
    ParentChanged { old: Option<u64>, new: Option<u64> },
}

// ── Internal diff helpers ──────────────────────────────────────────────

fn diff_node(old: &A11yNodeInfo, new: &A11yNodeInfo) -> Vec<A11yChange> {
    let mut changes = Vec::new();

    if old.name != new.name {
        changes.push(A11yChange::NameChanged {
            old: old.name.clone(),
            new: new.name.clone(),
        });
    }

    if old.role != new.role {
        changes.push(A11yChange::RoleChanged {
            old: old.role,
            new: new.role,
        });
    }

    if old.bounds != new.bounds {
        changes.push(A11yChange::BoundsChanged);
    }

    if old.children != new.children {
        changes.push(A11yChange::ChildrenChanged);
    }

    if old.live_region != new.live_region {
        changes.push(A11yChange::LiveRegionChanged {
            old: old.live_region,
            new: new.live_region,
        });
    }

    if old.description != new.description {
        changes.push(A11yChange::DescriptionChanged {
            old: old.description.clone(),
            new: new.description.clone(),
        });
    }

    if old.shortcut != new.shortcut {
        changes.push(A11yChange::ShortcutChanged {
            old: old.shortcut.clone(),
            new: new.shortcut.clone(),
        });
    }

    if old.parent != new.parent {
        changes.push(A11yChange::ParentChanged {
            old: old.parent,
            new: new.parent,
        });
    }

    // Diff individual state fields.
    diff_state(&old.state, &new.state, &mut changes);

    changes
}

fn diff_state(
    old: &crate::node::A11yState,
    new: &crate::node::A11yState,
    changes: &mut Vec<A11yChange>,
) {
    macro_rules! check_bool {
        ($field:ident) => {
            if old.$field != new.$field {
                changes.push(A11yChange::StateChanged {
                    field: stringify!($field).to_owned(),
                    description: new.$field.to_string(),
                });
            }
        };
    }

    macro_rules! check_option {
        ($field:ident) => {
            if old.$field != new.$field {
                changes.push(A11yChange::StateChanged {
                    field: stringify!($field).to_owned(),
                    description: format!("{:?}", new.$field),
                });
            }
        };
    }

    check_bool!(focused);
    check_bool!(disabled);
    check_option!(checked);
    check_option!(expanded);
    check_bool!(selected);
    check_bool!(readonly);
    check_bool!(required);
    check_bool!(busy);
    check_option!(value_now);
    check_option!(value_min);
    check_option!(value_max);

    if old.value_text != new.value_text {
        changes.push(A11yChange::StateChanged {
            field: "value_text".to_owned(),
            description: new.value_text.as_deref().unwrap_or("<none>").to_owned(),
        });
    }
}

fn announcement_reason(changes: &[A11yChange]) -> Option<AnnouncementReason> {
    if changes
        .iter()
        .any(|change| matches!(change, A11yChange::LiveRegionChanged { .. }))
    {
        return Some(AnnouncementReason::LiveRegionChanged);
    }

    changes
        .iter()
        .any(|change| {
            matches!(
                change,
                A11yChange::NameChanged { .. }
                    | A11yChange::DescriptionChanged { .. }
                    | A11yChange::RoleChanged { .. }
            ) || matches!(
                change,
                A11yChange::StateChanged { field, .. }
                    if matches!(
                        field.as_str(),
                        "busy" | "checked" | "expanded" | "selected" | "value_now" | "value_text"
                    )
            )
        })
        .then_some(AnnouncementReason::LiveContentChanged)
}

fn announcement_sort_key(announcement: &ScreenReaderAnnouncement) -> (u8, u8, Option<u64>, &str) {
    let reason_rank = match announcement.reason {
        AnnouncementReason::FocusChanged => 0,
        AnnouncementReason::LiveRegionChanged => 1,
        AnnouncementReason::LiveRegionAdded | AnnouncementReason::LiveContentChanged => 2,
    };
    let urgency_rank = match announcement.urgency {
        LiveRegion::Assertive => 0,
        LiveRegion::Polite => 1,
    };
    (
        reason_rank,
        urgency_rank,
        announcement.node_id,
        announcement.text.as_str(),
    )
}

fn announcement_text(node: &A11yNodeInfo, focused: bool, require_content: bool) -> Option<String> {
    if node.role == A11yRole::Presentation {
        return None;
    }
    if require_content && !has_announcement_content(node) {
        return None;
    }

    let text = node_summary(node, focused, false);
    normalized_text(&text)
}

fn has_announcement_content(node: &A11yNodeInfo) -> bool {
    normalized_option(node.name.as_deref()).is_some()
        || normalized_option(node.description.as_deref()).is_some()
        || !state_summaries(&node.state).is_empty()
}

fn node_summary(node: &A11yNodeInfo, focused: bool, include_live_region: bool) -> String {
    let mut parts = Vec::new();
    let mut heading = node.role.to_string();

    if let Some(name) = normalized_option(node.name.as_deref()) {
        heading.push_str(": ");
        heading.push_str(&name);
    }
    parts.push(heading);

    if let Some(description) = normalized_option(node.description.as_deref())
        && normalized_option(node.name.as_deref()) != Some(description.clone())
    {
        parts.push(description);
    }

    let mut states = state_summaries(&node.state);
    if focused || node.state.focused {
        states.insert(0, "focused".to_owned());
    }
    if !states.is_empty() {
        parts.push(states.join(", "));
    }

    if let Some(shortcut) = normalized_option(node.shortcut.as_deref()) {
        parts.push(format!("shortcut {shortcut}"));
    }

    if include_live_region && let Some(region) = node.live_region {
        parts.push(format!("live {region}"));
    }

    parts.join(". ")
}

fn state_summaries(state: &crate::node::A11yState) -> Vec<String> {
    let mut states = Vec::new();
    if state.disabled {
        states.push("disabled".to_owned());
    }
    if let Some(checked) = state.checked {
        states.push(if checked { "checked" } else { "not checked" }.to_owned());
    }
    if let Some(expanded) = state.expanded {
        states.push(if expanded { "expanded" } else { "collapsed" }.to_owned());
    }
    if state.selected {
        states.push("selected".to_owned());
    }
    if state.readonly {
        states.push("read only".to_owned());
    }
    if state.required {
        states.push("required".to_owned());
    }
    if state.busy {
        states.push("busy".to_owned());
    }
    if let Some(value_text) = normalized_option(state.value_text.as_deref()) {
        states.push(format!("value {value_text}"));
    } else if let Some(value_now) = state.value_now {
        states.push(format!("value {value_now}"));
    }
    states
}

fn normalized_option(value: Option<&str>) -> Option<String> {
    value.and_then(normalized_text)
}

fn normalized_text(value: &str) -> Option<String> {
    let mut normalized = String::new();
    for word in value.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    (!normalized.is_empty()).then_some(normalized)
}

fn limit_text(text: String, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text;
    }
    text.chars().take(max_chars).collect()
}

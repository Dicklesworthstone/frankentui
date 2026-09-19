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
            let summary = node_summary(node, self.focused == Some(id), true, &[]);
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
    /// Focus changes are announced first, followed by retained-focus control
    /// state changes, live-region policy changes, and live content. Within a
    /// reason class, assertive announcements precede polite ones, then node ID
    /// determines the order.
    ///
    /// When the tree's focused ID stays on an existing non-presentational
    /// node, changes to `disabled`, `readonly`, and `required` produce one
    /// polite announcement of only the changed flags. Both directions are
    /// explicit: disabled/enabled, read only/not read only, required/not
    /// required. These independent states do not move focus or change widget
    /// behavior. A node's `state.focused` flag alone does not establish focus.
    ///
    /// Changes to just these flags off focus remain silent, even on live regions; the
    /// complete changes remain available in the raw diff. Entering a control
    /// uses its normal focus summary, not an additional state announcement.
    /// Same-node live updates are coalesced before applying the batch cap:
    /// focus/state reasons take precedence, but live content and urgency are
    /// retained. Cleared flags are included explicitly in a coalesced summary;
    /// default false flags are not added to ordinary focus or mirror output.
    ///
    /// This is a text-bridge policy, not a requirement to add ARIA live
    /// regions. WAI-ARIA defines the [state semantics], while [Core-AAM]
    /// describes native accessibility events. A bridge using native events
    /// should not also replay this text for the same transition (bd-inkss).
    ///
    /// [state semantics]: https://www.w3.org/TR/wai-aria-1.2/#aria-disabled
    /// [Core-AAM]: https://www.w3.org/TR/core-aam-1.2/#mapping_events_state-change
    pub fn screen_reader_announcements(
        &self,
        current: &A11yTree,
        policy: ScreenReaderPolicy,
    ) -> ScreenReaderAnnouncements {
        let mut candidates = Vec::new();

        if let Some((_, Some(new_focus))) = self.focus_changed
            && let Some(node) = current.node(new_focus)
            && node.role != A11yRole::Presentation
            && let Some(text) =
                announcement_text(node, current.focused == Some(new_focus), false, &[])
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
                && let Some(text) = announcement_text(node, current.focused == Some(*id), true, &[])
            {
                push_announcement(
                    &mut candidates,
                    ScreenReaderAnnouncement {
                        node_id: Some(*id),
                        urgency,
                        reason: AnnouncementReason::LiveRegionAdded,
                        text,
                    },
                );
            }
        }

        for (id, changes) in &self.changed {
            let Some(node) = current.node(*id) else {
                continue;
            };
            if node.role == A11yRole::Presentation {
                continue;
            }

            let control_changes = if self.focus_changed.is_none() && current.focused == Some(*id) {
                changed_control_states(node, changes)
            } else {
                Vec::new()
            };
            // Positive flags are already in the full live summary. Only add
            // cleared flags there, so neither direction is lost or repeated.
            let cleared_states: Vec<_> = control_changes
                .iter()
                .filter_map(|(set, text)| (!*set).then_some(*text))
                .collect();

            if let Some(urgency) = node.live_region
                && let Some(reason) = announcement_reason(changes)
                && let Some(text) =
                    announcement_text(node, current.focused == Some(*id), true, &cleared_states)
            {
                push_announcement(
                    &mut candidates,
                    ScreenReaderAnnouncement {
                        node_id: Some(*id),
                        urgency,
                        reason: if control_changes.is_empty() {
                            reason
                        } else {
                            AnnouncementReason::FocusedStateChanged
                        },
                        text,
                    },
                );
            } else if !control_changes.is_empty() {
                let states: Vec<_> = control_changes.iter().map(|(_, text)| *text).collect();
                candidates.push(ScreenReaderAnnouncement {
                    node_id: Some(*id),
                    urgency: LiveRegion::Polite,
                    reason: AnnouncementReason::FocusedStateChanged,
                    text: format!("{}. {}", node_heading(node), states.join(", ")),
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

/// One screen-reader announcement derived from focus, control state, or live-region changes.
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
    /// Disabled, read-only, or required state changed while this node kept focus.
    /// May include a simultaneous live update, retaining its urgency.
    FocusedStateChanged,
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

/// Changed control flags in stable order, with their current polarity and
/// spoken text. Read typed state rather than parsing diff debug descriptions.
fn changed_control_states(node: &A11yNodeInfo, changes: &[A11yChange]) -> Vec<(bool, &'static str)> {
    [
        ("disabled", node.state.disabled, "disabled", "enabled"),
        ("readonly", node.state.readonly, "read only", "not read only"),
        ("required", node.state.required, "required", "not required"),
    ]
    .into_iter()
    .filter(|(field, _, _, _)| {
        changes.iter().any(|change| {
            matches!(
                change,
                A11yChange::StateChanged { field: changed, .. } if changed.as_str() == *field
            )
        })
    })
    .map(|(_, set, on, off)| (set, if set { on } else { off }))
    .collect()
}

/// The optional focus candidate is inserted first, before either node loop.
/// Added and changed IDs are disjoint in a tree diff, so only that candidate
/// can overlap a live announcement. Coalesce by node, never by text, in O(1).
fn push_announcement(
    candidates: &mut Vec<ScreenReaderAnnouncement>,
    announcement: ScreenReaderAnnouncement,
) {
    if let Some(focus) = candidates.first_mut()
        && focus.reason == AnnouncementReason::FocusChanged
        && focus.node_id == announcement.node_id
    {
        if announcement.urgency == LiveRegion::Assertive {
            focus.urgency = LiveRegion::Assertive;
        }
    } else {
        candidates.push(announcement);
    }
}

fn announcement_sort_key(announcement: &ScreenReaderAnnouncement) -> (u8, u8, Option<u64>, &str) {
    let reason_rank = match announcement.reason {
        AnnouncementReason::FocusChanged => 0,
        AnnouncementReason::FocusedStateChanged => 1,
        AnnouncementReason::LiveRegionChanged => 2,
        AnnouncementReason::LiveRegionAdded | AnnouncementReason::LiveContentChanged => 3,
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

fn announcement_text(
    node: &A11yNodeInfo,
    focused: bool,
    require_content: bool,
    cleared_states: &[&str],
) -> Option<String> {
    if node.role == A11yRole::Presentation {
        return None;
    }
    if require_content && !has_announcement_content(node) && cleared_states.is_empty() {
        return None;
    }

    let text = node_summary(node, focused, false, cleared_states);
    normalized_text(&text)
}

fn has_announcement_content(node: &A11yNodeInfo) -> bool {
    normalized_option(node.name.as_deref()).is_some()
        || normalized_option(node.description.as_deref()).is_some()
        || !state_summaries(&node.state).is_empty()
}

fn node_heading(node: &A11yNodeInfo) -> String {
    let mut heading = node.role.to_string();
    if let Some(name) = normalized_option(node.name.as_deref()) {
        heading.push_str(": ");
        heading.push_str(&name);
    }
    heading
}

fn node_summary(
    node: &A11yNodeInfo,
    focused: bool,
    include_live_region: bool,
    cleared_states: &[&str],
) -> String {
    let mut parts = vec![node_heading(node)];

    if let Some(description) = normalized_option(node.description.as_deref())
        && normalized_option(node.name.as_deref()) != Some(description.clone())
    {
        parts.push(description);
    }

    let mut states = state_summaries(&node.state);
    states.extend(cleared_states.iter().map(|text| (*text).to_owned()));
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

#[cfg(test)]
mod focused_state_tests {
    use super::{A11yChange, A11yTree, A11yTreeBuilder, AnnouncementReason, ScreenReaderPolicy};
    use crate::node::{A11yNodeInfo, A11yRole, A11yState, LiveRegion};
    use crate::preferences::AccessibilityPreferences;
    use ftui_core::geometry::Rect;

    fn control(bits: u8) -> A11yNodeInfo {
        let mut node = A11yNodeInfo::new(1, A11yRole::TextInput, Rect::new(0, 0, 20, 1))
            .with_name(" Email \n address ")
            .with_description("Help")
            .with_shortcut("Alt+E");
        node.state = A11yState {
            disabled: bits & 1 != 0,
            readonly: bits & 2 != 0,
            required: bits & 4 != 0,
            ..A11yState::default()
        };
        node
    }

    fn snapshot(nodes: Vec<A11yNodeInfo>, focus: Option<u64>) -> A11yTree {
        let mut builder = A11yTreeBuilder::new();
        for node in nodes {
            builder.add_node(node);
        }
        builder.set_focused(focus);
        builder.build()
    }

    #[test]
    fn focused_control_flags_announce_both_directions_without_requiring_live_regions() {
        for region in [None, Some(LiveRegion::Polite), Some(LiveRegion::Assertive)] {
            for (from, to, field, text) in [
                (0, 1, "disabled", "disabled"),
                (1, 0, "disabled", "enabled"),
                (0, 2, "readonly", "read only"),
                (2, 0, "readonly", "not read only"),
                (0, 4, "required", "required"),
                (4, 0, "required", "not required"),
            ] {
                let mut old = control(from);
                let mut new = control(to);
                old.live_region = region;
                new.live_region = region;
                // The tree ID is authoritative, even with state.focused false.
                assert!(!old.state.focused && !new.state.focused);
                let before = snapshot(vec![old], Some(1));
                let after = snapshot(vec![new], Some(1));
                let diff = after.diff(&before);
                assert!(diff.focus_changed.is_none());
                let expected_value = (to != 0).to_string();
                assert!(diff.changed[0].1.iter().any(|change| {
                    matches!(
                        change,
                        A11yChange::StateChanged { field: actual, description }
                            if actual == field && description == &expected_value
                    )
                }));

                let batch = diff.screen_reader_announcements(&after, ScreenReaderPolicy::default());
                assert_eq!(batch.announcements.len(), 1, "{field}: {from} -> {to}");
                assert_eq!(batch.dropped_count, 0);
                let announcement = &batch.announcements[0];
                assert_eq!(announcement.node_id, Some(1));
                assert_eq!(announcement.reason, AnnouncementReason::FocusedStateChanged);
                // A state-only change is not an assertive live-content update.
                assert_eq!(announcement.urgency, LiveRegion::Polite);
                assert_eq!(announcement.text, format!("textInput: Email address. {text}"));
            }
        }
    }

    #[test]
    fn focused_control_flags_coalesce_and_do_not_repeat_on_unchanged_frames() {
        let before = snapshot(vec![control(0)], Some(1));
        let after = snapshot(vec![control(7)], Some(1));
        for (old, new, expected) in [
            (&before, &after, "disabled, read only, required"),
            (&after, &before, "enabled, not read only, not required"),
        ] {
            let batch = new.screen_reader_announcements_since(old, ScreenReaderPolicy::default());
            assert_eq!(batch.announcements.len(), 1);
            assert_eq!(
                batch.announcements[0].text,
                format!("textInput: Email address. {expected}")
            );
            let unchanged =
                new.screen_reader_announcements_since(new, ScreenReaderPolicy::default());
            assert!(unchanged.announcements.is_empty());
            assert_eq!(unchanged.dropped_count, 0);
        }
        // Clearing states must not make default false states verbose in snapshots.
        assert_eq!(
            before
                .screen_reader_mirror(ScreenReaderPolicy::default())
                .text(),
            "textInput: Email address. Help. focused. shortcut Alt+E"
        );
    }

    #[test]
    fn focused_control_delta_does_not_repeat_unchanged_flags_or_imply_editability() {
        let before = snapshot(vec![control(7)], Some(1));
        let after = snapshot(vec![control(5)], Some(1));
        let batch = after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
        assert_eq!(batch.announcements.len(), 1);
        // Still disabled and required: removing readonly does not mean operable.
        assert_eq!(
            batch.announcements[0].text,
            "textInput: Email address. not read only"
        );
    }

    #[test]
    fn off_focus_and_presentational_control_flags_are_diffed_but_not_spoken() {
        for region in [None, Some(LiveRegion::Polite), Some(LiveRegion::Assertive)] {
            for (role, focus) in [
                (A11yRole::TextInput, None),
                (A11yRole::TextInput, Some(2)),
                (A11yRole::TextInput, Some(999)),
                (A11yRole::Presentation, Some(1)),
            ] {
                for (from, to) in [(0, 7), (7, 0)] {
                    let mut old = control(from);
                    let mut new = control(to);
                    old.role = role;
                    new.role = role;
                    old.live_region = region;
                    new.live_region = region;
                    // A stale per-node focus flag must not opt into state speech.
                    old.state.focused = true;
                    new.state.focused = true;
                    let other = A11yNodeInfo::new(2, A11yRole::Button, Rect::new(0, 1, 5, 1));
                    let before = snapshot(vec![old, other.clone()], focus);
                    let after = snapshot(vec![new, other], focus);
                    let diff = after.diff(&before);
                    assert_eq!(diff.changed.len(), 1);
                    assert_eq!(diff.changed[0].1.len(), 3);
                    let batch =
                        diff.screen_reader_announcements(&after, ScreenReaderPolicy::default());
                    assert!(
                        batch.announcements.is_empty(),
                        "{role:?}, {focus:?}, {region:?}"
                    );
                    assert_eq!(batch.dropped_count, 0);
                }
            }
        }
    }

    #[test]
    fn focus_handoff_uses_destination_summary_and_silences_outgoing_state_changes() {
        let mut other_before = control(0);
        other_before.id = 2;
        let mut other_after = control(7);
        other_after.id = 2;
        let before = snapshot(vec![control(0), other_before], Some(2));
        let after = snapshot(vec![control(7), other_after], Some(1));
        let batch = after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
        assert_eq!(batch.announcements.len(), 1);
        assert_eq!(
            batch.announcements[0].reason,
            AnnouncementReason::FocusChanged
        );
        assert_eq!(batch.announcements[0].node_id, Some(1));
        assert_eq!(
            batch.announcements[0].text,
            "textInput: Email address. Help. focused, disabled, read only, required. shortcut Alt+E"
        );

        let lost = snapshot(vec![control(0)], None);
        let removed = snapshot(Vec::new(), Some(1));
        for next in [lost, removed] {
            let batch =
                next.screen_reader_announcements_since(&after, ScreenReaderPolicy::default());
            assert!(batch.announcements.is_empty());
            assert_eq!(batch.dropped_count, 0);
        }
    }

    #[test]
    fn new_focus_and_live_region_addition_coalesce_before_counting_drops() {
        let focused = control(7).with_live_region(LiveRegion::Assertive);
        let other = A11yNodeInfo::new(2, A11yRole::Label, Rect::new(0, 1, 20, 1))
            .with_name("Other update")
            .with_live_region(LiveRegion::Polite);
        let after = snapshot(vec![focused, other], Some(1));
        for cap in [0, 1, 2] {
            let batch = after.screen_reader_announcements_since(
                &A11yTree::empty(),
                ScreenReaderPolicy {
                    max_announcements: cap,
                    ..ScreenReaderPolicy::default()
                },
            );
            assert_eq!(batch.announcements.len(), cap);
            assert_eq!(batch.dropped_count, 2 - cap);
            if cap > 0 {
                assert_eq!(batch.announcements[0].node_id, Some(1));
                assert_eq!(
                    batch.announcements[0].reason,
                    AnnouncementReason::FocusChanged
                );
                assert_eq!(batch.announcements[0].urgency, LiveRegion::Assertive);
            }
        }

        // Equal text from different nodes is not a duplicate focus/live event.
        let first = control(0).with_live_region(LiveRegion::Polite);
        let mut second = first.clone();
        second.id = 2;
        let equal_text = snapshot(vec![second, first], None)
            .screen_reader_announcements_since(&A11yTree::empty(), ScreenReaderPolicy::default());
        assert_eq!(equal_text.announcements.len(), 2);
        assert_eq!(
            equal_text.announcements[0].text,
            equal_text.announcements[1].text
        );
        assert_eq!(equal_text.announcements[0].node_id, Some(1));
        assert_eq!(equal_text.announcements[1].node_id, Some(2));
    }

    #[test]
    fn new_focus_and_existing_live_content_or_policy_changes_are_announced_once() {
        for change_policy in [false, true] {
            let old = control(0).with_live_region(LiveRegion::Polite);
            let mut new = control(7).with_live_region(if change_policy {
                LiveRegion::Assertive
            } else {
                LiveRegion::Polite
            });
            new.description = Some("Updated help".to_owned());
            let before = snapshot(vec![old], None);
            let after = snapshot(vec![new], Some(1));
            let batch =
                after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
            assert_eq!(batch.announcements.len(), 1);
            assert_eq!(batch.dropped_count, 0);
            assert_eq!(
                batch.announcements[0].reason,
                AnnouncementReason::FocusChanged
            );
            assert_eq!(
                batch.announcements[0].urgency,
                if change_policy {
                    LiveRegion::Assertive
                } else {
                    LiveRegion::Polite
                }
            );
            assert_eq!(
                batch.announcements[0].text,
                "textInput: Email address. Updated help. focused, disabled, read only, required. shortcut Alt+E"
            );
        }
    }

    #[test]
    fn retained_focus_and_live_content_keep_both_directions_content_and_urgency_once() {
        for region in [LiveRegion::Polite, LiveRegion::Assertive] {
            for (from, to, states) in [
                (0, 7, "disabled, read only, required, value new"),
                (7, 0, "value new, enabled, not read only, not required"),
            ] {
                let old = control(from).with_live_region(region);
                let mut new = control(to)
                    .with_live_region(region)
                    .with_name(" Updated \n email ")
                    .with_description("New help");
                new.state.value_text = Some("new".to_owned());
                let before = snapshot(vec![old], Some(1));
                let after = snapshot(vec![new], Some(1));
                let batch =
                    after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
                assert_eq!(batch.announcements.len(), 1);
                assert_eq!(batch.dropped_count, 0);
                assert_eq!(
                    batch.announcements[0].reason,
                    AnnouncementReason::FocusedStateChanged
                );
                assert_eq!(batch.announcements[0].urgency, region);
                assert_eq!(
                    batch.announcements[0].text,
                    format!("textInput: Updated email. New help. focused, {states}. shortcut Alt+E")
                );
                // Actionable focused-state changes are not motion-like churn.
                let filtered = AccessibilityPreferences::all()
                    .motion_profile()
                    .filter_announcements(&batch);
                assert_eq!(filtered.announcements, batch.announcements);
                assert_eq!(filtered.coalesced_count, 0);
                assert_eq!(filtered.downgraded_count, 0);
            }
        }
    }

    #[test]
    fn clearing_unnamed_focused_states_survives_live_policy_changes() {
        for (old_region, new_region) in [
            (None, Some(LiveRegion::Assertive)),
            (Some(LiveRegion::Polite), Some(LiveRegion::Assertive)),
            (Some(LiveRegion::Assertive), None),
        ] {
            let mut old = control(7);
            let mut new = control(0);
            for node in [&mut old, &mut new] {
                node.name = None;
                node.description = None;
                node.shortcut = None;
            }
            old.live_region = old_region;
            new.live_region = new_region;
            let before = snapshot(vec![old], Some(1));
            let after = snapshot(vec![new], Some(1));
            let batch =
                after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
            assert_eq!(batch.announcements.len(), 1);
            assert_eq!(
                batch.announcements[0].reason,
                AnnouncementReason::FocusedStateChanged
            );
            assert_eq!(
                batch.announcements[0].urgency,
                new_region.unwrap_or(LiveRegion::Polite)
            );
            assert_eq!(
                batch.announcements[0].text,
                if new_region.is_some() {
                    "textInput. focused, enabled, not read only, not required"
                } else {
                    "textInput. enabled, not read only, not required"
                }
            );
        }
    }

    #[test]
    fn focused_state_order_and_caps_are_deterministic_and_unicode_safe() {
        let before = snapshot(vec![control(0)], Some(1));
        let focused = control(7).with_name("\u{00c9}mail \u{754c} \u{1f980}");
        let live = A11yNodeInfo::new(2, A11yRole::Label, Rect::new(0, 1, 20, 1))
            .with_name("Background update")
            .with_live_region(LiveRegion::Assertive);
        let after = snapshot(vec![live, focused], Some(1));
        let full = after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
        assert_eq!(full.announcements.len(), 2);
        assert_eq!(
            full.announcements[0].reason,
            AnnouncementReason::FocusedStateChanged
        );
        assert_eq!(full.announcements[0].node_id, Some(1));
        assert_eq!(full.announcements[1].node_id, Some(2));
        for cap in [0, 1, 2] {
            for chars in [0, 1, 20, 240] {
                let policy = ScreenReaderPolicy {
                    max_announcements: cap,
                    max_text_chars: chars,
                    ..ScreenReaderPolicy::default()
                };
                let batch = after.screen_reader_announcements_since(&before, policy);
                assert_eq!(
                    batch,
                    after.screen_reader_announcements_since(&before, policy)
                );
                assert_eq!(batch.announcements.len(), cap);
                assert_eq!(batch.dropped_count, 2 - cap);
                for (actual, expected) in batch.announcements.iter().zip(&full.announcements) {
                    assert_eq!(actual.node_id, expected.node_id);
                    assert_eq!(
                        actual.text,
                        expected.text.chars().take(chars).collect::<String>()
                    );
                }
            }
        }
    }

    #[test]
    fn unrelated_focused_layout_and_hint_changes_stay_quiet() {
        let before = snapshot(vec![control(0)], Some(1));
        let mut new = control(0);
        new.state.focused = true;
        new.bounds = Rect::new(4, 5, 30, 2);
        new.shortcut = Some("Ctrl+E".to_owned());
        let after = snapshot(vec![new], Some(1));
        assert!(!after.diff(&before).is_empty());
        let batch = after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
        assert!(batch.announcements.is_empty());
        assert_eq!(batch.dropped_count, 0);
    }
}

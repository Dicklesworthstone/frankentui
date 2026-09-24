#![forbid(unsafe_code)]

//! Focus manager coordinating focus traversal, history, and traps.
//!
//! The manager tracks the current focus, maintains a navigation history,
//! and enforces focus traps for modal dialogs. It also provides a
//! configurable [`FocusIndicator`] for styling the focused widget.

use ahash::AHashMap;

use super::indicator::FocusIndicator;
use super::spatial;
use super::{FocusGraph, FocusId, NavDirection};

/// Focus change events emitted by the manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusEvent {
    FocusGained { id: FocusId },
    FocusLost { id: FocusId },
    FocusMoved { from: FocusId, to: FocusId },
}

/// Group of focusable widgets for tab traversal.
#[derive(Debug, Clone)]
pub struct FocusGroup {
    pub id: u32,
    pub members: Vec<FocusId>,
    pub wrap: bool,
}

impl FocusGroup {
    #[must_use]
    pub fn new(id: u32, members: Vec<FocusId>) -> Self {
        Self {
            id,
            members,
            wrap: true,
        }
    }

    #[must_use]
    pub fn with_wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    fn contains(&self, id: FocusId) -> bool {
        self.members.contains(&id)
    }
}

/// Active focus trap (e.g., modal).
#[derive(Debug, Clone, Copy)]
pub struct FocusTrap {
    pub group_id: u32,
    pub return_focus: Option<FocusId>,
}

/// Focus changes [`FocusManager::focus_back`] can step back through.
///
/// Every recorded change adds an entry, so a long session tabbing between
/// widgets grew the history without bound. Each graph repair also filters
/// all of it. The oldest entry is dropped at the cap.
const MAX_FOCUS_HISTORY: usize = 64;

/// Central focus coordinator.
///
/// Tracks focus state, navigation history, focus traps (for modals),
/// and focus indicator styling. Emits [`FocusEvent`]s on focus changes.
#[derive(Debug)]
pub struct FocusManager {
    graph: FocusGraph,
    current: Option<FocusId>,
    host_focused: bool,
    pending_focus_on_host_gain: Option<FocusId>,
    history: Vec<FocusId>,
    trap_stack: Vec<FocusTrap>,
    groups: AHashMap<u32, FocusGroup>,
    last_event: Option<FocusEvent>,
    indicator: FocusIndicator,
    /// Running count of focus changes for metrics.
    focus_change_count: u64,
}

impl Default for FocusManager {
    fn default() -> Self {
        Self {
            graph: FocusGraph::default(),
            current: None,
            host_focused: true,
            pending_focus_on_host_gain: None,
            history: Vec::new(),
            trap_stack: Vec::new(),
            groups: AHashMap::new(),
            last_event: None,
            indicator: FocusIndicator::default(),
            focus_change_count: 0,
        }
    }
}

impl FocusManager {
    /// Create a new focus manager.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Access the underlying focus graph.
    #[must_use]
    pub fn graph(&self) -> &FocusGraph {
        &self.graph
    }

    /// Mutably access the underlying focus graph.
    pub fn graph_mut(&mut self) -> &mut FocusGraph {
        &mut self.graph
    }

    /// Get currently focused widget.
    #[inline]
    #[must_use]
    pub fn current(&self) -> Option<FocusId> {
        self.current
    }

    #[must_use]
    pub(crate) fn host_focused(&self) -> bool {
        self.host_focused
    }

    pub(crate) fn set_host_focused(&mut self, focused: bool) {
        self.host_focused = focused;
        if focused {
            self.pending_focus_on_host_gain = None;
        }
    }

    /// Check if a widget is focused.
    #[must_use]
    pub fn is_focused(&self, id: FocusId) -> bool {
        self.current == Some(id)
    }

    /// Set focus to widget, returns previous focus.
    pub fn focus(&mut self, id: FocusId) -> Option<FocusId> {
        if !self.can_focus(id) || !self.allowed_by_trap(id) {
            return None;
        }
        let prev = self.active_focus_target();
        if prev == Some(id) {
            return prev;
        }
        self.set_focus(id);
        prev
    }

    /// Remove focus from current widget.
    pub fn blur(&mut self) -> Option<FocusId> {
        let prev = self.current.take();
        if let Some(id) = prev {
            #[cfg(feature = "tracing")]
            tracing::debug!(from_widget = id, trigger = "blur", "focus.change");
            self.last_event = Some(FocusEvent::FocusLost { id });
            self.focus_change_count += 1;
        }
        prev
    }

    /// Apply host/window focus state to the widget focus graph.
    ///
    /// Deterministic policy:
    /// - `focused = false` clears current focus.
    /// - `focused = true` restores the last valid logical focus target when
    ///   possible, otherwise falls back to the first allowed node (respecting
    ///   active traps).
    ///
    /// Returns `true` when focus state changed.
    pub fn apply_host_focus(&mut self, focused: bool) -> bool {
        if !focused {
            if let Some(current) = self.current {
                self.pending_focus_on_host_gain = Some(current);
            }
            self.host_focused = false;
            return self.blur().is_some();
        }

        self.host_focused = true;
        let had_current = self.current.is_some();
        if let Some(current) = self.current
            && self.can_focus(current)
            && self.allowed_by_trap(current)
        {
            self.pending_focus_on_host_gain = None;
            return false;
        }

        let pending_focus = self.pending_focus_on_host_gain.take();
        if let Some(id) = pending_focus
            && self.can_focus(id)
            && self.allowed_by_trap(id)
        {
            return self.set_focus_without_history(id);
        }

        if let Some(group_id) = self.active_trap_group()
            && self.focus_first_in_group_without_history(group_id)
        {
            return true;
        }

        if self.focus_first_without_history() {
            return true;
        }

        if had_current {
            return self.blur().is_some();
        }

        false
    }

    /// Move focus in direction.
    pub fn navigate(&mut self, dir: NavDirection) -> bool {
        match dir {
            NavDirection::Next => self.focus_next(),
            NavDirection::Prev => self.focus_prev(),
            _ => {
                let Some(current) = self.active_focus_target() else {
                    return false;
                };
                // Explicit edges take precedence; fall back to spatial navigation.
                let target = self
                    .graph
                    .navigate(current, dir)
                    .or_else(|| spatial::spatial_navigate(&self.graph, current, dir));
                let Some(target) = target else {
                    return false;
                };
                if !self.allowed_by_trap(target) {
                    return false;
                }
                self.set_focus(target)
            }
        }
    }

    /// Move to next in tab order.
    pub fn focus_next(&mut self) -> bool {
        self.move_in_tab_order(true)
    }

    /// Move to previous in tab order.
    pub fn focus_prev(&mut self) -> bool {
        self.move_in_tab_order(false)
    }

    /// Focus first focusable widget.
    pub fn focus_first(&mut self) -> bool {
        let order = self.active_tab_order();
        let Some(first) = order.first().copied() else {
            return false;
        };
        self.set_focus(first)
    }

    /// Focus last focusable widget.
    pub fn focus_last(&mut self) -> bool {
        let order = self.active_tab_order();
        let Some(last) = order.last().copied() else {
            return false;
        };
        self.set_focus(last)
    }

    /// Go back to previous focus.
    pub fn focus_back(&mut self) -> bool {
        let active_focus = self.active_focus_target();
        while let Some(id) = self.history.pop() {
            if active_focus == Some(id) {
                continue;
            }
            if self.can_focus(id) && self.allowed_by_trap(id) {
                if !self.host_focused {
                    return self.set_pending_focus_target(id);
                }
                // Set focus directly without pushing current to history
                // (going back shouldn't create a forward entry).
                let prev = self.current;
                self.current = Some(id);
                self.last_event = Some(match prev {
                    Some(from) => FocusEvent::FocusMoved { from, to: id },
                    None => FocusEvent::FocusGained { id },
                });
                self.focus_change_count += 1;
                return true;
            }
        }
        false
    }

    /// Clear focus history.
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Push focus trap (for modals).
    ///
    /// If the group doesn't exist or has no focusable members, the trap is
    /// **not** pushed and the method returns `false`. This prevents a deadlock
    /// where `allowed_by_trap` would deny focus to every widget because the
    /// group is empty/missing.
    pub fn push_trap(&mut self, group_id: u32) -> bool {
        let return_focus = if self.host_focused {
            self.current
        } else {
            self.current.or(self.deferred_focus_target())
        };
        if !self.push_trap_with_return_focus(group_id, return_focus) {
            #[cfg(feature = "tracing")]
            tracing::warn!(group_id, "focus.trap_push rejected: group missing or empty");
            return false;
        }

        if self.host_focused && !self.is_current_focusable_in_group(group_id) {
            self.focus_first_in_group_without_history(group_id);
        } else if !self.host_focused {
            self.pending_focus_on_host_gain = self.group_primary_focus_target(group_id);
        }
        true
    }

    /// Pop focus trap, restore previous focus.
    pub fn pop_trap(&mut self) -> bool {
        let Some(trap) = self.trap_stack.pop() else {
            return false;
        };
        let had_current = self.current.is_some();
        #[cfg(feature = "tracing")]
        tracing::debug!(
            group_id = trap.group_id,
            return_focus = ?trap.return_focus,
            "focus.trap_pop"
        );

        if !self.host_focused {
            self.pending_focus_on_host_gain = trap
                .return_focus
                .filter(|id| self.can_focus(*id) && self.allowed_by_trap(*id))
                .or_else(|| {
                    self.active_trap_group()
                        .and_then(|group_id| self.group_primary_focus_target(group_id))
                });
            return if had_current {
                self.blur().is_some()
            } else {
                false
            };
        }

        if let Some(id) = trap.return_focus
            && self.can_focus(id)
            && self.allowed_by_trap(id)
        {
            return self.set_focus_without_history(id);
        }

        if let Some(active) = self.active_trap_group() {
            return self.focus_first_in_group_without_history(active);
        }

        if trap.return_focus.is_none() {
            return if had_current {
                self.blur().is_some()
            } else {
                false
            };
        }

        if self.focus_first_without_history() {
            return true;
        }

        if had_current && self.current.is_some_and(|id| !self.can_focus(id)) {
            return self.blur().is_some();
        }

        false
    }

    /// Check if focus is currently trapped.
    #[must_use]
    pub fn is_trapped(&self) -> bool {
        self.active_trap_group().is_some()
    }

    /// Remove all active focus traps without changing focus groups.
    pub fn clear_traps(&mut self) {
        self.trap_stack.clear();
    }

    /// Create focus group.
    pub fn create_group(&mut self, id: u32, members: Vec<FocusId>) {
        let members = self.filter_focusable(members);
        self.groups.insert(id, FocusGroup::new(id, members));
        self.repair_focus_after_group_change();
    }

    pub(crate) fn create_group_preserving_members(&mut self, id: u32, members: Vec<FocusId>) {
        let members = self.dedup_members(members);
        self.groups.insert(id, FocusGroup::new(id, members));
        self.repair_focus_after_group_change();
    }

    /// Add widget to group.
    pub fn add_to_group(&mut self, group_id: u32, widget_id: FocusId) {
        if !self.can_focus(widget_id) {
            return;
        }
        let group = self
            .groups
            .entry(group_id)
            .or_insert_with(|| FocusGroup::new(group_id, Vec::new()));
        if !group.contains(widget_id) {
            group.members.push(widget_id);
        }
        self.repair_focus_after_group_change();
    }

    /// Remove widget from group.
    pub fn remove_from_group(&mut self, group_id: u32, widget_id: FocusId) {
        let Some(group) = self.groups.get_mut(&group_id) else {
            return;
        };
        group.members.retain(|id| *id != widget_id);
        self.repair_focus_after_group_change();
    }

    /// Remove an entire focus group.
    pub fn remove_group(&mut self, group_id: u32) {
        if self.groups.remove(&group_id).is_none() {
            return;
        }
        self.trap_stack.retain(|trap| trap.group_id != group_id);
        self.repair_focus_after_group_change();
    }

    /// Get the last focus event.
    #[must_use]
    pub fn focus_event(&self) -> Option<&FocusEvent> {
        self.last_event.as_ref()
    }

    /// Take and clear the last focus event.
    #[must_use]
    pub fn take_focus_event(&mut self) -> Option<FocusEvent> {
        self.last_event.take()
    }

    /// Get the focus indicator configuration.
    #[inline]
    #[must_use]
    pub fn indicator(&self) -> &FocusIndicator {
        &self.indicator
    }

    /// Set the focus indicator configuration.
    pub fn set_indicator(&mut self, indicator: FocusIndicator) {
        self.indicator = indicator;
    }

    /// Total number of focus changes since creation (for metrics).
    #[inline]
    #[must_use]
    pub fn focus_change_count(&self) -> u64 {
        self.focus_change_count
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn group_count(&self) -> usize {
        self.groups.len()
    }

    #[must_use]
    pub(crate) fn has_group(&self, group_id: u32) -> bool {
        self.groups.contains_key(&group_id)
    }

    #[must_use]
    pub(crate) fn group_members(&self, group_id: u32) -> Vec<FocusId> {
        self.groups
            .get(&group_id)
            .map(|group| group.members.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn base_trap_return_focus(&self) -> Option<Option<FocusId>> {
        self.trap_stack.first().map(|trap| trap.return_focus)
    }

    #[must_use]
    pub(crate) fn deferred_focus_target(&self) -> Option<FocusId> {
        if let Some(id) = self.active_focus_target() {
            return Some(id);
        }

        self.active_trap_group()
            .and_then(|group_id| self.group_primary_focus_target(group_id))
    }

    #[must_use]
    pub(crate) fn logical_focus_target(&self) -> Option<FocusId> {
        self.active_focus_target()
    }

    pub(crate) fn focus_without_history(&mut self, id: FocusId) -> bool {
        self.set_focus_without_history(id)
    }

    pub(crate) fn focus_first_without_history_for_restore(&mut self) -> bool {
        self.focus_first_without_history()
    }

    pub(crate) fn replace_deferred_focus_target(&mut self, target: Option<FocusId>) {
        self.current = None;
        self.pending_focus_on_host_gain =
            target.filter(|id| self.can_focus(*id) && self.allowed_by_trap(*id));
    }

    pub(crate) fn remove_group_without_repair(&mut self, group_id: u32) -> bool {
        if self.groups.remove(&group_id).is_none() {
            return false;
        }
        self.trap_stack.retain(|trap| trap.group_id != group_id);
        true
    }

    pub(crate) fn push_trap_with_return_focus(
        &mut self,
        group_id: u32,
        return_focus: Option<FocusId>,
    ) -> bool {
        if !self.group_has_focusable_member(group_id) {
            return false;
        }

        #[cfg(feature = "tracing")]
        tracing::debug!(
            group_id,
            return_focus = ?return_focus,
            "focus.trap_push"
        );
        self.trap_stack.push(FocusTrap {
            group_id,
            return_focus,
        });
        true
    }

    pub(crate) fn repair_focus_after_excluding_ids(&mut self, excluded: &[FocusId]) {
        self.history.retain(|id| !excluded.contains(id));

        if !self.host_focused {
            if self.current.is_some_and(|id| excluded.contains(&id)) {
                let _ = self.blur();
            }
            return;
        }

        if self.is_trapped() || !self.current.is_some_and(|id| excluded.contains(&id)) {
            return;
        }

        for id in self.graph.tab_order() {
            if excluded.contains(&id) {
                continue;
            }
            if self.set_focus_without_history(id) {
                return;
            }
        }

        let _ = self.blur();
    }

    pub(crate) fn clear_deferred_focus_if_excluded(&mut self, excluded: &[FocusId]) {
        if self
            .pending_focus_on_host_gain
            .is_some_and(|id| excluded.contains(&id))
        {
            self.pending_focus_on_host_gain = None;
        }
    }

    pub(crate) fn restore_focus_after_invalid_current(&mut self) {
        if !self.host_focused {
            return;
        }

        if let Some(group_id) = self.active_trap_group()
            && self.focus_first_in_group_without_history(group_id)
        {
            return;
        }

        let _ = self.focus_first_without_history();
    }

    fn set_focus(&mut self, id: FocusId) -> bool {
        self.set_focus_target(id, true)
    }

    fn set_focus_without_history(&mut self, id: FocusId) -> bool {
        self.set_focus_target(id, false)
    }

    fn set_focus_target(&mut self, id: FocusId, record_history: bool) -> bool {
        if !self.host_focused {
            return self.set_pending_focus_target(id);
        }
        self.set_focus_internal(id, record_history)
    }

    fn set_focus_internal(&mut self, id: FocusId, record_history: bool) -> bool {
        if !self.can_focus(id) || !self.allowed_by_trap(id) {
            return false;
        }
        if self.current == Some(id) {
            return false;
        }

        let prev = self.current;
        if let Some(prev_id) = prev {
            if record_history && Some(prev_id) != self.history.last().copied() {
                if self.history.len() >= MAX_FOCUS_HISTORY {
                    self.history.remove(0);
                }
                self.history.push(prev_id);
            }
            let event = FocusEvent::FocusMoved {
                from: prev_id,
                to: id,
            };
            #[cfg(feature = "tracing")]
            tracing::debug!(
                from_widget = prev_id,
                to_widget = id,
                trigger = "navigate",
                "focus.change"
            );
            self.last_event = Some(event);
        } else {
            #[cfg(feature = "tracing")]
            tracing::debug!(to_widget = id, trigger = "initial", "focus.change");
            self.last_event = Some(FocusEvent::FocusGained { id });
        }

        self.current = Some(id);
        self.focus_change_count += 1;
        true
    }

    fn can_focus(&self, id: FocusId) -> bool {
        self.graph.get(id).map(|n| n.is_focusable).unwrap_or(false)
    }

    fn active_focus_target(&self) -> Option<FocusId> {
        if let Some(current) = self.current
            && self.can_focus(current)
            && self.allowed_by_trap(current)
        {
            return Some(current);
        }

        if self.host_focused {
            return None;
        }

        self.pending_focus_on_host_gain
            .filter(|id| self.can_focus(*id) && self.allowed_by_trap(*id))
    }

    fn set_pending_focus_target(&mut self, id: FocusId) -> bool {
        if !self.can_focus(id) || !self.allowed_by_trap(id) {
            return false;
        }

        let prev = self.active_focus_target();
        self.current = None;
        if prev == Some(id) {
            return false;
        }

        self.pending_focus_on_host_gain = Some(id);
        true
    }

    fn active_trap_group(&self) -> Option<u32> {
        self.trap_stack
            .iter()
            .rev()
            .find(|trap| self.group_has_focusable_member(trap.group_id))
            .map(|trap| trap.group_id)
    }

    fn allowed_by_trap(&self, id: FocusId) -> bool {
        let Some(group_id) = self.active_trap_group() else {
            return true;
        };
        self.groups
            .get(&group_id)
            .map(|g| g.contains(id))
            .unwrap_or(false)
    }

    fn group_has_focusable_member(&self, group_id: u32) -> bool {
        self.groups
            .get(&group_id)
            .is_some_and(|group| group.members.iter().any(|id| self.can_focus(*id)))
    }

    fn repair_focus_after_group_change(&mut self) {
        if !self.host_focused {
            if self.current.is_some() {
                let _ = self.blur();
            }
            return;
        }

        match self.active_trap_group() {
            Some(group_id) => {
                let current_allowed = self
                    .current
                    .is_some_and(|id| self.can_focus(id) && self.allowed_by_trap(id));
                if !current_allowed {
                    let _ = self.focus_first_in_group_without_history(group_id);
                }
            }
            None => {
                if self.current.is_some_and(|id| !self.can_focus(id))
                    && !self.focus_first_without_history()
                {
                    let _ = self.blur();
                }
            }
        }
    }

    fn is_current_focusable_in_group(&self, group_id: u32) -> bool {
        let Some(current) = self.current else {
            return false;
        };
        self.can_focus(current)
            && self
                .groups
                .get(&group_id)
                .map(|g| g.contains(current))
                .unwrap_or(false)
    }

    fn active_tab_order(&self) -> Vec<FocusId> {
        if let Some(group_id) = self.active_trap_group() {
            return self.group_tab_order(group_id);
        }
        self.graph.tab_order()
    }

    fn group_tab_order(&self, group_id: u32) -> Vec<FocusId> {
        let Some(group) = self.groups.get(&group_id) else {
            return Vec::new();
        };
        let order = self.graph.tab_order();
        order.into_iter().filter(|id| group.contains(*id)).collect()
    }

    pub(crate) fn group_primary_focus_target(&self, group_id: u32) -> Option<FocusId> {
        self.group_tab_order(group_id).first().copied().or_else(|| {
            self.groups
                .get(&group_id)
                .and_then(|group| group.members.iter().copied().find(|id| self.can_focus(*id)))
        })
    }

    fn focus_first_in_group_without_history(&mut self, group_id: u32) -> bool {
        let Some(first) = self.group_primary_focus_target(group_id) else {
            return false;
        };
        self.set_focus_without_history(first)
    }

    fn focus_first_without_history(&mut self) -> bool {
        let order = self.active_tab_order();
        let Some(first) = order.first().copied() else {
            return false;
        };
        self.set_focus_without_history(first)
    }

    fn move_in_tab_order(&mut self, forward: bool) -> bool {
        let order = self.active_tab_order();
        if order.is_empty() {
            return false;
        }
        let first = order[0];
        let last = order[order.len() - 1];
        let fallback = if forward { first } else { last };

        let wrap = self
            .active_trap_group()
            .and_then(|id| self.groups.get(&id).map(|g| g.wrap))
            .unwrap_or(true);

        let next = match self.active_focus_target() {
            None => fallback,
            Some(current) => {
                let pos = order.iter().position(|id| *id == current);
                match pos {
                    None => fallback,
                    Some(idx) if forward => {
                        if idx + 1 < order.len() {
                            order[idx + 1]
                        } else if wrap {
                            order[0]
                        } else {
                            return false;
                        }
                    }
                    Some(idx) => {
                        if idx > 0 {
                            order[idx - 1]
                        } else if wrap {
                            last
                        } else {
                            return false;
                        }
                    }
                }
            }
        };

        self.set_focus(next)
    }

    fn dedup_members(&self, ids: Vec<FocusId>) -> Vec<FocusId> {
        let mut out = Vec::new();
        for id in ids {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    fn filter_focusable(&self, ids: Vec<FocusId>) -> Vec<FocusId> {
        self.dedup_members(ids)
            .into_iter()
            .filter(|id| self.can_focus(*id))
            .collect()
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus::FocusNode;
    use ftui_core::geometry::Rect;

    fn node(id: FocusId, tab: i32) -> FocusNode {
        FocusNode::new(id, Rect::new(0, 0, 1, 1)).with_tab_index(tab)
    }

    #[test]
    fn focus_basic() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        assert!(fm.focus(1).is_none());
        assert_eq!(fm.current(), Some(1));

        assert_eq!(fm.focus(2), Some(1));
        assert_eq!(fm.current(), Some(2));

        assert_eq!(fm.blur(), Some(2));
        assert_eq!(fm.current(), None);
    }

    #[test]
    fn focus_history_back() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));

        fm.focus(1);
        fm.focus(2);
        fm.focus(3);

        assert!(fm.focus_back());
        assert_eq!(fm.current(), Some(2));

        assert!(fm.focus_back());
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn focus_history_is_bounded_and_keeps_the_newest_entries() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));

        for _ in 0..10_000 {
            fm.focus(1);
            fm.focus(2);
        }
        assert_eq!(fm.history.len(), MAX_FOCUS_HISTORY);

        fm.focus(3);
        assert!(fm.focus_back());
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn focus_back_skips_current_id_in_history() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        fm.focus(1);
        fm.focus(2);
        assert_eq!(fm.current(), Some(2));

        assert_eq!(fm.blur(), Some(2));
        assert_eq!(fm.current(), None);

        fm.focus(1);
        assert_eq!(fm.current(), Some(1));
        let _ = fm.take_focus_event();
        let before = fm.focus_change_count();

        assert!(!fm.focus_back());
        assert_eq!(fm.current(), Some(1));
        assert!(fm.take_focus_event().is_none());
        assert_eq!(fm.focus_change_count(), before);
    }

    #[test]
    fn focus_next_prev() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));

        assert!(fm.focus_next());
        assert_eq!(fm.current(), Some(1));

        assert!(fm.focus_next());
        assert_eq!(fm.current(), Some(2));

        assert!(fm.focus_prev());
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn apply_host_focus_loss_blurs_current() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);
        let _ = fm.take_focus_event();

        assert!(fm.apply_host_focus(false));
        assert_eq!(fm.current(), None);
        assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 1 }));
    }

    #[test]
    fn apply_host_focus_gain_focuses_first_when_unfocused() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(10, 1));
        fm.graph_mut().insert(node(5, 0));

        assert!(fm.apply_host_focus(true));
        assert_eq!(fm.current(), Some(5));
        assert_eq!(
            fm.take_focus_event(),
            Some(FocusEvent::FocusGained { id: 5 })
        );
    }

    #[test]
    fn apply_host_focus_gain_preserves_valid_current() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.focus(2);
        let _ = fm.take_focus_event();

        assert!(!fm.apply_host_focus(true));
        assert_eq!(fm.current(), Some(2));
        assert!(fm.take_focus_event().is_none());
    }

    #[test]
    fn apply_host_focus_gain_clears_invalid_current_when_restore_fails() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);
        let _ = fm.take_focus_event();
        let _ = fm.graph_mut().remove(1);

        assert!(fm.apply_host_focus(true));
        assert_eq!(fm.current(), None);
        assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 1 }));
    }

    #[test]
    fn apply_host_focus_gain_respects_trap_order() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.create_group(42, vec![2, 3]);
        fm.push_trap(42);
        let _ = fm.take_focus_event();
        fm.blur();
        let _ = fm.take_focus_event();

        assert!(fm.apply_host_focus(true));
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn apply_host_focus_gain_restores_previously_selected_trapped_focus() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.create_group(42, vec![2, 3]);
        assert!(fm.push_trap(42));
        assert_eq!(fm.current(), Some(2));
        assert_eq!(fm.focus(3), Some(2));
        let _ = fm.take_focus_event();

        assert!(fm.apply_host_focus(false));
        assert_eq!(fm.current(), None);
        let _ = fm.take_focus_event();

        assert!(fm.apply_host_focus(true));
        assert_eq!(fm.current(), Some(3));
        assert_eq!(
            fm.take_focus_event(),
            Some(FocusEvent::FocusGained { id: 3 })
        );
    }

    #[test]
    fn push_trap_while_host_blurred_without_prior_focus_restores_none_on_pop() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        assert!(!fm.apply_host_focus(false));

        fm.create_group(42, vec![2]);
        assert!(fm.push_trap(42));
        assert!(fm.apply_host_focus(true));
        assert_eq!(fm.current(), Some(2));

        assert!(fm.pop_trap());
        assert_eq!(fm.current(), None);
        assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 2 }));
    }

    #[test]
    fn push_trap_does_not_autofocus_while_host_blurred() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.focus(1);
        assert!(fm.apply_host_focus(false));

        fm.create_group(42, vec![2]);
        assert!(fm.push_trap(42));
        assert_eq!(fm.current(), None);

        assert!(fm.apply_host_focus(true));
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn focus_while_host_blurred_updates_deferred_target_without_restoring_current() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.focus(1);
        let _ = fm.take_focus_event();

        assert!(fm.apply_host_focus(false));
        assert_eq!(fm.current(), None);
        assert_eq!(fm.focus(3), Some(1));
        assert_eq!(fm.current(), None);
        assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 1 }));

        assert!(fm.apply_host_focus(true));
        assert_eq!(fm.current(), Some(3));
        assert_eq!(
            fm.take_focus_event(),
            Some(FocusEvent::FocusGained { id: 3 })
        );
    }

    #[test]
    fn focus_next_while_host_blurred_advances_deferred_target() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.focus(1);
        assert_eq!(fm.focus(2), Some(1));
        let _ = fm.take_focus_event();

        assert!(fm.apply_host_focus(false));
        assert_eq!(fm.current(), None);
        assert!(fm.focus_next());
        assert_eq!(fm.current(), None);
        assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 2 }));

        assert!(fm.apply_host_focus(true));
        assert_eq!(fm.current(), Some(3));
        assert_eq!(
            fm.take_focus_event(),
            Some(FocusEvent::FocusGained { id: 3 })
        );
    }

    #[test]
    fn focus_trap_push_pop() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));

        fm.focus(3);
        fm.create_group(7, vec![1, 2]);

        fm.push_trap(7);
        assert!(fm.is_trapped());
        assert_eq!(fm.current(), Some(1));

        fm.pop_trap();
        assert!(!fm.is_trapped());
        assert_eq!(fm.current(), Some(3));
    }

    #[test]
    fn focus_group_wrap_respected() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.create_group(9, vec![1, 2]);
        fm.groups.get_mut(&9).unwrap().wrap = false;

        fm.push_trap(9);
        fm.focus(2);
        assert!(!fm.focus_next());
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn focus_event_generation() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        fm.focus(1);
        assert_eq!(
            fm.take_focus_event(),
            Some(FocusEvent::FocusGained { id: 1 })
        );

        fm.focus(2);
        assert_eq!(
            fm.take_focus_event(),
            Some(FocusEvent::FocusMoved { from: 1, to: 2 })
        );

        fm.blur();
        assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 2 }));
    }

    #[test]
    fn trap_prevents_focus_outside_group() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.create_group(5, vec![1, 2]);

        fm.push_trap(5);
        assert_eq!(fm.current(), Some(1));

        // Attempt to focus outside trap should fail.
        assert!(fm.focus(3).is_none());
        assert_ne!(fm.current(), Some(3));
    }

    // --- Spatial navigation integration ---

    fn spatial_node(id: FocusId, x: u16, y: u16, w: u16, h: u16, tab: i32) -> FocusNode {
        FocusNode::new(id, Rect::new(x, y, w, h)).with_tab_index(tab)
    }

    #[test]
    fn navigate_spatial_fallback() {
        let mut fm = FocusManager::new();
        // Two nodes side by side — no explicit edges.
        fm.graph_mut().insert(spatial_node(1, 0, 0, 10, 3, 0));
        fm.graph_mut().insert(spatial_node(2, 20, 0, 10, 3, 1));

        fm.focus(1);
        assert!(fm.navigate(NavDirection::Right));
        assert_eq!(fm.current(), Some(2));

        assert!(fm.navigate(NavDirection::Left));
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn navigate_explicit_edge_overrides_spatial() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(spatial_node(1, 0, 0, 10, 3, 0));
        fm.graph_mut().insert(spatial_node(2, 20, 0, 10, 3, 1)); // spatially right
        fm.graph_mut().insert(spatial_node(3, 40, 0, 10, 3, 2)); // further right

        // Explicit edge overrides spatial: Right from 1 goes to 3, not 2.
        fm.graph_mut().connect(1, NavDirection::Right, 3);

        fm.focus(1);
        assert!(fm.navigate(NavDirection::Right));
        assert_eq!(fm.current(), Some(3));
    }

    #[test]
    fn navigate_spatial_respects_trap() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(spatial_node(1, 0, 0, 10, 3, 0));
        fm.graph_mut().insert(spatial_node(2, 20, 0, 10, 3, 1));
        fm.graph_mut().insert(spatial_node(3, 40, 0, 10, 3, 2));

        // Trap to group containing only 1 and 2.
        fm.create_group(1, vec![1, 2]);
        fm.focus(2);
        fm.push_trap(1);

        // Spatial would find 3 to the right of 2, but trap blocks it.
        assert!(!fm.navigate(NavDirection::Right));
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn navigate_spatial_grid_round_trip() {
        let mut fm = FocusManager::new();
        // 2x2 grid.
        fm.graph_mut().insert(spatial_node(1, 0, 0, 10, 3, 0));
        fm.graph_mut().insert(spatial_node(2, 20, 0, 10, 3, 1));
        fm.graph_mut().insert(spatial_node(3, 0, 6, 10, 3, 2));
        fm.graph_mut().insert(spatial_node(4, 20, 6, 10, 3, 3));

        fm.focus(1);

        // Navigate around the grid: right, down, left, up — back to start.
        assert!(fm.navigate(NavDirection::Right));
        assert_eq!(fm.current(), Some(2));

        assert!(fm.navigate(NavDirection::Down));
        assert_eq!(fm.current(), Some(4));

        assert!(fm.navigate(NavDirection::Left));
        assert_eq!(fm.current(), Some(3));

        assert!(fm.navigate(NavDirection::Up));
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn navigate_spatial_no_candidate() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(spatial_node(1, 0, 0, 10, 3, 0));
        fm.focus(1);

        // No other nodes, spatial should return false.
        assert!(!fm.navigate(NavDirection::Right));
        assert!(!fm.navigate(NavDirection::Up));
        assert_eq!(fm.current(), Some(1));
    }

    // --- FocusManager construction ---

    #[test]
    fn new_manager_has_no_focus() {
        let fm = FocusManager::new();
        assert_eq!(fm.current(), None);
        assert!(!fm.is_trapped());
    }

    #[test]
    fn default_and_new_are_equivalent() {
        let a = FocusManager::new();
        let b = FocusManager::default();
        assert_eq!(a.current(), b.current());
        assert_eq!(a.is_trapped(), b.is_trapped());
        assert_eq!(a.host_focused(), b.host_focused());
    }

    // --- is_focused ---

    #[test]
    fn is_focused_returns_true_for_current() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);
        assert!(fm.is_focused(1));
        assert!(!fm.is_focused(2));
    }

    #[test]
    fn is_focused_returns_false_when_no_focus() {
        let fm = FocusManager::new();
        assert!(!fm.is_focused(1));
    }

    // --- focus edge cases ---

    #[test]
    fn focus_non_existent_node_returns_none() {
        let mut fm = FocusManager::new();
        assert!(fm.focus(999).is_none());
        assert_eq!(fm.current(), None);
    }

    #[test]
    fn focus_already_focused_returns_same_id() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);
        // Focusing same node returns current (early exit)
        assert_eq!(fm.focus(1), Some(1));
        assert_eq!(fm.current(), Some(1));
    }

    // --- blur ---

    #[test]
    fn blur_when_no_focus_returns_none() {
        let mut fm = FocusManager::new();
        assert_eq!(fm.blur(), None);
    }

    #[test]
    fn blur_generates_focus_lost_event() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);
        let _ = fm.take_focus_event(); // clear
        fm.blur();
        assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 1 }));
    }

    // --- focus_first / focus_last ---

    #[test]
    fn focus_first_selects_lowest_tab_index() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(3, 2));
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        assert!(fm.focus_first());
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn focus_last_selects_highest_tab_index() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));

        assert!(fm.focus_last());
        assert_eq!(fm.current(), Some(3));
    }

    #[test]
    fn focus_first_on_empty_graph_returns_false() {
        let mut fm = FocusManager::new();
        assert!(!fm.focus_first());
    }

    #[test]
    fn focus_last_on_empty_graph_returns_false() {
        let mut fm = FocusManager::new();
        assert!(!fm.focus_last());
    }

    // --- Tab wrapping ---

    #[test]
    fn focus_next_wraps_at_end() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        fm.focus(2);
        assert!(fm.focus_next()); // wraps
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn focus_prev_wraps_at_start() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        fm.focus(1);
        assert!(fm.focus_prev()); // wraps
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn focus_next_with_no_current_selects_first() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        assert!(fm.focus_next());
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn focus_prev_with_no_current_selects_last() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        assert!(fm.focus_prev());
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn focus_prev_with_stale_current_selects_last() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));

        fm.focus(2);
        let _ = fm.graph_mut().remove(2);

        assert!(fm.focus_prev());
        assert_eq!(fm.current(), Some(3));
    }

    #[test]
    fn focus_next_on_empty_returns_false() {
        let mut fm = FocusManager::new();
        assert!(!fm.focus_next());
    }

    // --- History ---

    #[test]
    fn focus_back_on_empty_history_returns_false() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);
        assert!(!fm.focus_back());
    }

    #[test]
    fn clear_history_prevents_back() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        fm.focus(1);
        fm.focus(2);
        fm.clear_history();
        assert!(!fm.focus_back());
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn focus_back_skips_removed_nodes() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));

        fm.focus(1);
        fm.focus(2);
        fm.focus(3);

        // Remove node 2 from graph
        let _ = fm.graph_mut().remove(2);

        // focus_back should skip 2 and go to 1
        assert!(fm.focus_back());
        assert_eq!(fm.current(), Some(1));
    }

    // --- Groups ---

    #[test]
    fn create_group_filters_non_focusable() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        // Node 999 doesn't exist in the graph
        fm.create_group(1, vec![1, 999]);

        let group = fm.groups.get(&1).unwrap();
        assert_eq!(group.members.len(), 1);
        assert!(group.contains(1));
    }

    #[test]
    fn add_to_group_creates_group_if_needed() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.add_to_group(42, 1);
        assert!(fm.groups.contains_key(&42));
        assert!(fm.groups.get(&42).unwrap().contains(1));
    }

    #[test]
    fn add_to_group_skips_unfocusable() {
        let mut fm = FocusManager::new();
        fm.add_to_group(1, 999); // 999 not in graph
        // Group may or may not exist, but if it does, 999 is not in it
        if let Some(group) = fm.groups.get(&1) {
            assert!(!group.contains(999));
        }
    }

    #[test]
    fn add_to_group_no_duplicates() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.add_to_group(1, 1);
        fm.add_to_group(1, 1);
        assert_eq!(fm.groups.get(&1).unwrap().members.len(), 1);
    }

    #[test]
    fn remove_from_group() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.create_group(1, vec![1, 2]);
        fm.remove_from_group(1, 1);
        assert!(!fm.groups.get(&1).unwrap().contains(1));
        assert!(fm.groups.get(&1).unwrap().contains(2));
    }

    #[test]
    fn removing_focused_member_from_active_trap_refocuses_remaining_member() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.create_group(1, vec![1, 2]);

        fm.focus(2);
        assert!(fm.push_trap(1));
        assert_eq!(fm.current(), Some(2));

        fm.remove_from_group(1, 2);
        assert_eq!(fm.current(), Some(1));
        assert!(fm.is_trapped());
        assert!(fm.focus(3).is_none());
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn removing_last_member_from_active_trap_allows_focus_escape() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.create_group(1, vec![1]);

        fm.focus(1);
        assert!(fm.push_trap(1));
        assert!(fm.is_trapped());

        fm.remove_from_group(1, 1);
        assert!(!fm.is_trapped());
        assert_eq!(fm.current(), Some(1));
        assert_eq!(fm.focus(2), Some(1));
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn removing_active_inner_trap_member_falls_back_to_outer_trap() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.create_group(10, vec![1, 2]);
        fm.create_group(20, vec![3]);

        fm.focus(1);
        assert!(fm.push_trap(10));
        assert!(fm.push_trap(20));
        assert_eq!(fm.current(), Some(3));

        fm.remove_from_group(20, 3);
        assert!(fm.is_trapped());
        assert_eq!(fm.current(), Some(1));
        assert!(fm.focus(3).is_none());
        assert_eq!(fm.focus(2), Some(1));
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn adding_member_to_invalidated_trap_restores_confinement() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.create_group(1, vec![1]);

        fm.focus(1);
        assert!(fm.push_trap(1));

        fm.remove_from_group(1, 1);
        assert!(!fm.is_trapped());

        fm.add_to_group(1, 2);
        assert!(fm.is_trapped());
        assert_eq!(fm.current(), Some(2));
        assert!(fm.focus(1).is_none());
    }

    #[test]
    fn remove_from_nonexistent_group_is_noop() {
        let mut fm = FocusManager::new();
        fm.remove_from_group(999, 1); // should not panic
    }

    #[test]
    fn remove_group_deletes_group() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.create_group(42, vec![1]);

        fm.remove_group(42);
        assert!(!fm.groups.contains_key(&42));
    }

    #[test]
    fn remove_group_from_active_inner_trap_falls_back_to_outer_trap() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.create_group(10, vec![1, 2]);
        fm.create_group(20, vec![3]);

        fm.focus(1);
        assert!(fm.push_trap(10));
        assert!(fm.push_trap(20));
        assert_eq!(fm.current(), Some(3));

        fm.remove_group(20);
        assert!(fm.is_trapped());
        assert_eq!(fm.current(), Some(1));
        assert!(fm.focus(3).is_none());
    }

    #[test]
    fn remove_group_clears_stale_trap_entries() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.create_group(10, vec![1]);

        fm.focus(1);
        assert!(fm.push_trap(10));
        assert!(fm.is_trapped());

        fm.remove_group(10);
        assert!(!fm.is_trapped());
        assert!(!fm.pop_trap());
    }

    #[test]
    fn remove_group_blurs_invalid_current_when_no_fallback_exists() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.create_group(10, vec![1]);
        fm.focus(1);

        let _ = fm.graph_mut().remove(1);
        fm.remove_group(10);

        assert_eq!(fm.current(), None);
        assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 1 }));
    }

    // --- FocusGroup ---

    #[test]
    fn focus_group_with_wrap() {
        let group = FocusGroup::new(1, vec![1, 2]).with_wrap(false);
        assert!(!group.wrap);
    }

    #[test]
    fn focus_group_default_wraps() {
        let group = FocusGroup::new(1, vec![]);
        assert!(group.wrap);
    }

    // --- Trap stack ---

    #[test]
    fn nested_traps() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.graph_mut().insert(node(4, 3));

        fm.create_group(10, vec![1, 2]);
        fm.create_group(20, vec![3, 4]);

        fm.focus(1);
        fm.push_trap(10);
        assert!(fm.is_trapped());

        fm.push_trap(20);
        // Should be in inner trap, focused on first of group 20
        assert_eq!(fm.current(), Some(3));

        // Pop inner trap
        fm.pop_trap();
        // Should still be trapped (in group 10)
        assert!(fm.is_trapped());

        // Pop outer trap
        fm.pop_trap();
        assert!(!fm.is_trapped());
    }

    #[test]
    fn trap_push_pop_does_not_pollute_focus_history() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.graph_mut().insert(node(3, 2));
        fm.create_group(10, vec![2]);

        fm.focus(1);
        fm.focus(3);
        assert_eq!(fm.current(), Some(3));

        assert!(fm.push_trap(10));
        assert_eq!(fm.current(), Some(2));

        assert!(fm.pop_trap());
        assert_eq!(fm.current(), Some(3));

        assert!(fm.focus_back());
        assert_eq!(fm.current(), Some(1));
        assert!(!fm.focus_back());
    }

    #[test]
    fn pop_trap_restores_none_when_modal_opened_without_focus() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.create_group(10, vec![1]);

        assert!(fm.push_trap(10));
        assert_eq!(fm.current(), Some(1));

        assert!(fm.pop_trap());
        assert_eq!(fm.current(), None);
        assert_eq!(fm.take_focus_event(), Some(FocusEvent::FocusLost { id: 1 }));
    }

    #[test]
    fn pop_trap_on_empty_returns_false() {
        let mut fm = FocusManager::new();
        assert!(!fm.pop_trap());
    }

    #[test]
    fn push_trap_rejects_missing_group() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);

        // Group 999 doesn't exist — push_trap must refuse.
        assert!(!fm.push_trap(999));
        assert!(!fm.is_trapped());
        // Focus should remain unchanged (no deadlock).
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn push_trap_rejects_empty_group() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);

        // Create group with no members.
        fm.create_group(42, vec![]);
        assert!(!fm.push_trap(42));
        assert!(!fm.is_trapped());
        // Focus should remain unchanged (no deadlock).
        assert_eq!(fm.current(), Some(1));
    }

    #[test]
    fn push_trap_autofocuses_negative_tabindex_member_when_group_has_no_tabbable_nodes() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, -1));
        fm.focus(1);

        fm.create_group(42, vec![2]);
        assert!(fm.push_trap(42));
        assert!(fm.is_trapped());
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn push_trap_blurred_restores_negative_tabindex_member_on_focus_gain() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, -1));
        fm.focus(1);
        assert!(fm.apply_host_focus(false));

        fm.create_group(42, vec![2]);
        assert!(fm.push_trap(42));
        assert_eq!(fm.current(), None);

        assert!(fm.apply_host_focus(true));
        assert_eq!(fm.current(), Some(2));
    }

    #[test]
    fn push_trap_retargets_when_current_group_member_becomes_unfocusable() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));
        fm.focus(1);
        fm.create_group(10, vec![1, 2]);

        fm.graph_mut().insert(node(1, 0).with_focusable(false));

        assert!(fm.push_trap(10));
        assert_eq!(fm.current(), Some(2));
    }

    // --- Focus events ---

    #[test]
    fn take_focus_event_clears_it() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);

        assert!(fm.take_focus_event().is_some());
        assert!(fm.take_focus_event().is_none());
    }

    #[test]
    fn focus_event_accessor() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);

        assert_eq!(fm.focus_event(), Some(&FocusEvent::FocusGained { id: 1 }));
    }

    // --- Navigate with no current ---

    #[test]
    fn navigate_direction_with_no_current_returns_false() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(spatial_node(1, 0, 0, 10, 3, 0));
        assert!(!fm.navigate(NavDirection::Right));
    }

    // --- graph accessors ---

    #[test]
    fn graph_accessor_returns_reference() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        assert!(fm.graph().get(1).is_some());
    }

    // --- Focus indicator ---

    #[test]
    fn default_indicator_is_reverse() {
        let fm = FocusManager::new();
        assert!(fm.indicator().is_visible());
        assert_eq!(
            fm.indicator().kind(),
            crate::focus::FocusIndicatorKind::StyleOverlay
        );
    }

    #[test]
    fn set_indicator() {
        let mut fm = FocusManager::new();
        fm.set_indicator(crate::focus::FocusIndicator::underline());
        assert_eq!(
            fm.indicator().kind(),
            crate::focus::FocusIndicatorKind::Underline
        );
    }

    // --- Focus change count ---

    #[test]
    fn focus_change_count_increments() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        assert_eq!(fm.focus_change_count(), 0);

        fm.focus(1);
        assert_eq!(fm.focus_change_count(), 1);

        fm.focus(2);
        assert_eq!(fm.focus_change_count(), 2);

        fm.blur();
        assert_eq!(fm.focus_change_count(), 3);
    }

    #[test]
    fn focus_change_count_zero_on_no_op() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.focus(1);
        assert_eq!(fm.focus_change_count(), 1);

        // Focusing the same widget is a no-op
        fm.focus(1);
        assert_eq!(fm.focus_change_count(), 1);
    }

    #[test]
    fn focus_back_increments_focus_change_count() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(node(1, 0));
        fm.graph_mut().insert(node(2, 1));

        fm.focus(1);
        fm.focus(2);
        assert_eq!(fm.focus_change_count(), 2);

        assert!(fm.focus_back());
        assert_eq!(fm.current(), Some(1));
        assert_eq!(fm.focus_change_count(), 3);
    }
}

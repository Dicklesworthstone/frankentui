#![forbid(unsafe_code)]

//! Focus manager coordinating focus traversal, history, and traps.

use std::collections::HashMap;

use ftui_core::event::KeyCode;

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
    pub exit_key: Option<KeyCode>,
}

impl FocusGroup {
    #[must_use]
    pub fn new(id: u32, members: Vec<FocusId>) -> Self {
        Self {
            id,
            members,
            wrap: true,
            exit_key: None,
        }
    }

    #[must_use]
    pub fn with_wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    #[must_use]
    pub fn with_exit_key(mut self, key: KeyCode) -> Self {
        self.exit_key = Some(key);
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

/// Central focus coordinator.
#[derive(Debug, Default)]
pub struct FocusManager {
    graph: FocusGraph,
    current: Option<FocusId>,
    history: Vec<FocusId>,
    trap_stack: Vec<FocusTrap>,
    groups: HashMap<u32, FocusGroup>,
    last_event: Option<FocusEvent>,
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

    /// Check if a widget is focused.
    #[must_use]
    pub fn is_focused(&self, id: FocusId) -> bool {
        self.current == Some(id)
    }

    /// Set focus to widget, returns previous focus.
    #[must_use]
    pub fn focus(&mut self, id: FocusId) -> Option<FocusId> {
        if !self.can_focus(id) || !self.allowed_by_trap(id) {
            return None;
        }
        let prev = self.current;
        if prev == Some(id) {
            return prev;
        }
        self.set_focus(id);
        prev
    }

    /// Remove focus from current widget.
    #[must_use]
    pub fn blur(&mut self) -> Option<FocusId> {
        let prev = self.current.take();
        if let Some(id) = prev {
            self.last_event = Some(FocusEvent::FocusLost { id });
        }
        prev
    }

    /// Move focus in direction.
    pub fn navigate(&mut self, dir: NavDirection) -> bool {
        match dir {
            NavDirection::Next => self.focus_next(),
            NavDirection::Prev => self.focus_prev(),
            _ => {
                let Some(current) = self.current else {
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
        while let Some(id) = self.history.pop() {
            if self.can_focus(id) && self.allowed_by_trap(id) {
                // Set focus directly without pushing current to history
                // (going back shouldn't create a forward entry).
                let prev = self.current;
                self.current = Some(id);
                self.last_event = Some(match prev {
                    Some(from) => FocusEvent::FocusMoved { from, to: id },
                    None => FocusEvent::FocusGained { id },
                });
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
    pub fn push_trap(&mut self, group_id: u32) {
        let return_focus = self.current;
        self.trap_stack.push(FocusTrap {
            group_id,
            return_focus,
        });

        if !self.is_current_in_group(group_id) {
            self.focus_first_in_group(group_id);
        }
    }

    /// Pop focus trap, restore previous focus.
    pub fn pop_trap(&mut self) -> bool {
        let Some(trap) = self.trap_stack.pop() else {
            return false;
        };

        if let Some(id) = trap.return_focus
            && self.can_focus(id)
            && self.allowed_by_trap(id)
        {
            return self.set_focus(id);
        }

        if let Some(active) = self.active_trap_group() {
            return self.focus_first_in_group(active);
        }

        self.focus_first()
    }

    /// Check if focus is currently trapped.
    #[must_use]
    pub fn is_trapped(&self) -> bool {
        !self.trap_stack.is_empty()
    }

    /// Create focus group.
    pub fn create_group(&mut self, id: u32, members: Vec<FocusId>) {
        let members = self.filter_focusable(members);
        self.groups.insert(id, FocusGroup::new(id, members));
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
    }

    /// Remove widget from group.
    pub fn remove_from_group(&mut self, group_id: u32, widget_id: FocusId) {
        let Some(group) = self.groups.get_mut(&group_id) else {
            return;
        };
        group.members.retain(|id| *id != widget_id);
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

    fn set_focus(&mut self, id: FocusId) -> bool {
        if !self.can_focus(id) || !self.allowed_by_trap(id) {
            return false;
        }
        if self.current == Some(id) {
            return false;
        }

        let prev = self.current;
        if let Some(prev_id) = prev {
            if Some(prev_id) != self.history.last().copied() {
                self.history.push(prev_id);
            }
            self.last_event = Some(FocusEvent::FocusMoved {
                from: prev_id,
                to: id,
            });
        } else {
            self.last_event = Some(FocusEvent::FocusGained { id });
        }

        self.current = Some(id);
        true
    }

    fn can_focus(&self, id: FocusId) -> bool {
        self.graph.get(id).map(|n| n.is_focusable).unwrap_or(false)
    }

    fn active_trap_group(&self) -> Option<u32> {
        self.trap_stack.last().map(|t| t.group_id)
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

    fn is_current_in_group(&self, group_id: u32) -> bool {
        let Some(current) = self.current else {
            return false;
        };
        self.groups
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

    fn focus_first_in_group(&mut self, group_id: u32) -> bool {
        let order = self.group_tab_order(group_id);
        let Some(first) = order.first().copied() else {
            return false;
        };
        self.set_focus(first)
    }

    fn move_in_tab_order(&mut self, forward: bool) -> bool {
        let order = self.active_tab_order();
        if order.is_empty() {
            return false;
        }

        let wrap = self
            .active_trap_group()
            .and_then(|id| self.groups.get(&id).map(|g| g.wrap))
            .unwrap_or(true);

        let next = match self.current {
            None => order[0],
            Some(current) => {
                let pos = order.iter().position(|id| *id == current);
                match pos {
                    None => order[0],
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
                            *order.last().unwrap()
                        } else {
                            return false;
                        }
                    }
                }
            }
        };

        self.set_focus(next)
    }

    fn filter_focusable(&self, ids: Vec<FocusId>) -> Vec<FocusId> {
        let mut out = Vec::new();
        for id in ids {
            if self.can_focus(id) && !out.contains(&id) {
                out.push(id);
            }
        }
        out
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
        fm.take_focus_event(); // clear
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
    fn remove_from_nonexistent_group_is_noop() {
        let mut fm = FocusManager::new();
        fm.remove_from_group(999, 1); // should not panic
    }

    // --- FocusGroup ---

    #[test]
    fn focus_group_with_wrap() {
        let group = FocusGroup::new(1, vec![1, 2]).with_wrap(false);
        assert!(!group.wrap);
    }

    #[test]
    fn focus_group_with_exit_key() {
        let group = FocusGroup::new(1, vec![]).with_exit_key(KeyCode::Escape);
        assert_eq!(group.exit_key, Some(KeyCode::Escape));
    }

    #[test]
    fn focus_group_default_wraps() {
        let group = FocusGroup::new(1, vec![]);
        assert!(group.wrap);
        assert_eq!(group.exit_key, None);
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
    fn pop_trap_on_empty_returns_false() {
        let mut fm = FocusManager::new();
        assert!(!fm.pop_trap());
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
}

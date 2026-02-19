#![forbid(unsafe_code)]

//! Focus-aware modal integration for automatic focus trap management.
//!
//! This module provides `FocusAwareModalStack`, which combines [`ModalStack`]
//! with [`FocusManager`] integration for automatic focus trapping when modals
//! are opened and focus restoration when they close.
//!
//! # Invariants
//!
//! 1. **Auto-focus**: When a modal opens with a focus group, focus moves to the
//!    first focusable element in that group.
//! 2. **Focus trap**: Tab navigation is constrained to the modal's focus group.
//! 3. **Focus restoration**: When a modal closes, focus returns to where it was
//!    before the modal opened.
//! 4. **LIFO ordering**: Focus traps follow modal stack ordering (nested modals
//!    restore to the correct previous state).
//!
//! # Failure Modes
//!
//! - If the focus group has no focusable members, focus remains unchanged.
//! - If the original focus target is removed during modal display, focus moves
//!   to the first available focusable element.
//! - Focus trap with an empty group allows focus to escape (graceful degradation).
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::focus::FocusManager;
//! use ftui_widgets::modal::{ModalStack, WidgetModalEntry};
//! use ftui_widgets::modal::focus_integration::FocusAwareModalStack;
//!
//! let mut modals = FocusAwareModalStack::new();
//!
//! // Push modal with focus group members
//! let focus_ids = vec![ok_button_id, cancel_button_id];
//! let modal_id = modals.push_with_trap(
//!     Box::new(WidgetModalEntry::new(dialog)),
//!     focus_ids,
//! );
//!
//! // Handle event (focus trap active, Escape closes and restores focus)
//! if let Some(result) = modals.handle_event(&event) {
//!     // Modal closed, focus already restored
//! }
//! ```

use std::sync::atomic::{AtomicU32, Ordering};

use ftui_core::event::Event;
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;

use crate::focus::{FocusId, FocusManager};
use crate::modal::{ModalId, ModalResult, ModalStack, StackModal};

/// Global counter for unique focus group IDs.
static FOCUS_GROUP_COUNTER: AtomicU32 = AtomicU32::new(1_000_000);

/// Generate a unique focus group ID.
fn next_focus_group_id() -> u32 {
    FOCUS_GROUP_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Modal stack with integrated focus management.
///
/// This wrapper provides automatic focus trapping when modals open and
/// focus restoration when they close. It manages both the modal stack
/// and focus manager in a coordinated way.
///
/// # Invariants
///
/// - Focus trap stack depth equals the number of modals with focus groups.
/// - Each modal's focus group ID is unique and not reused.
/// - Pop operations always call `pop_trap` for modals with focus groups.
pub struct FocusAwareModalStack {
    stack: ModalStack,
    focus_manager: FocusManager,
}

impl Default for FocusAwareModalStack {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusAwareModalStack {
    /// Create a new focus-aware modal stack.
    pub fn new() -> Self {
        Self {
            stack: ModalStack::new(),
            focus_manager: FocusManager::new(),
        }
    }

    /// Create from existing stack and focus manager.
    ///
    /// Use this when you already have a `FocusManager` in your application
    /// and want to integrate modal focus trapping.
    pub fn with_focus_manager(focus_manager: FocusManager) -> Self {
        Self {
            stack: ModalStack::new(),
            focus_manager,
        }
    }

    // --- Modal Stack Delegation ---

    /// Push a modal without focus trapping.
    ///
    /// The modal will be rendered and receive events, but focus is not managed.
    pub fn push(&mut self, modal: Box<dyn StackModal>) -> ModalId {
        self.stack.push(modal)
    }

    /// Push a modal with automatic focus trapping.
    ///
    /// # Parameters
    /// - `modal`: The modal content
    /// - `focusable_ids`: The focus IDs of elements inside the modal
    ///
    /// # Behavior
    /// 1. Creates a focus group with the provided IDs
    /// 2. Pushes a focus trap (saving current focus)
    /// 3. Moves focus to the first element in the group
    pub fn push_with_trap(
        &mut self,
        modal: Box<dyn StackModal>,
        focusable_ids: Vec<FocusId>,
    ) -> ModalId {
        let group_id = next_focus_group_id();

        // Create focus group and push trap
        self.focus_manager.create_group(group_id, focusable_ids);
        self.focus_manager.push_trap(group_id);

        // Push modal with focus group tracking
        self.stack.push_with_focus(modal, Some(group_id))
    }

    /// Pop the top modal.
    ///
    /// If the modal had a focus group, the focus trap is popped and
    /// focus is restored to where it was before the modal opened.
    pub fn pop(&mut self) -> Option<ModalResult> {
        let result = self.stack.pop()?;
        if result.focus_group_id.is_some() {
            self.focus_manager.pop_trap();
        }
        Some(result)
    }

    /// Pop a specific modal by ID.
    ///
    /// **Warning**: Popping a non-top modal with a focus group will NOT restore
    /// focus correctly. The focus trap stack is LIFO, so only the top modal's
    /// trap can be safely popped. Prefer using `pop()` for correct focus handling.
    ///
    /// # Behavior
    /// - If the modal is the top modal and has a focus group, `pop_trap()` is called
    /// - If the modal is NOT the top modal, the focus trap is NOT popped (would corrupt state)
    pub fn pop_id(&mut self, id: ModalId) -> Option<ModalResult> {
        // Check if this is the top modal BEFORE popping
        let is_top = self.stack.top_id() == Some(id);

        let result = self.stack.pop_id(id)?;

        // Only pop the focus trap if this was the top modal
        // Popping a non-top modal's trap would corrupt the LIFO focus trap stack
        if is_top && result.focus_group_id.is_some() {
            self.focus_manager.pop_trap();
        }

        Some(result)
    }

    /// Pop all modals, restoring focus to the original state.
    pub fn pop_all(&mut self) -> Vec<ModalResult> {
        let results = self.stack.pop_all();
        for result in &results {
            if result.focus_group_id.is_some() {
                self.focus_manager.pop_trap();
            }
        }
        results
    }

    /// Handle an event, routing to the top modal.
    ///
    /// If the modal closes (via Escape, backdrop click, etc.), the focus
    /// trap is automatically popped and focus is restored.
    pub fn handle_event(&mut self, event: &Event) -> Option<ModalResult> {
        if let Event::Focus(focused) = event {
            self.focus_manager.apply_host_focus(*focused);
        }
        let result = self.stack.handle_event(event)?;
        if result.focus_group_id.is_some() {
            self.focus_manager.pop_trap();
        }
        Some(result)
    }

    /// Render all modals.
    pub fn render(&self, frame: &mut Frame, screen: Rect) {
        self.stack.render(frame, screen);
    }

    // --- State Queries ---

    /// Check if the modal stack is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// Get the number of open modals.
    #[inline]
    pub fn depth(&self) -> usize {
        self.stack.depth()
    }

    /// Check if focus is currently trapped in a modal.
    #[inline]
    pub fn is_focus_trapped(&self) -> bool {
        self.focus_manager.is_trapped()
    }

    /// Get a reference to the underlying modal stack.
    pub fn stack(&self) -> &ModalStack {
        &self.stack
    }

    /// Get a mutable reference to the underlying modal stack.
    ///
    /// **Warning**: Direct manipulation may desync focus state.
    pub fn stack_mut(&mut self) -> &mut ModalStack {
        &mut self.stack
    }

    /// Get a reference to the focus manager.
    pub fn focus_manager(&self) -> &FocusManager {
        &self.focus_manager
    }

    /// Get a mutable reference to the focus manager.
    pub fn focus_manager_mut(&mut self) -> &mut FocusManager {
        &mut self.focus_manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Widget;
    use crate::focus::FocusNode;
    use crate::modal::WidgetModalEntry;
    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use ftui_core::geometry::Rect;

    #[derive(Debug, Clone)]
    struct StubWidget;

    impl Widget for StubWidget {
        fn render(&self, _area: Rect, _frame: &mut Frame) {}
    }

    fn make_focus_node(id: FocusId) -> FocusNode {
        FocusNode::new(id, Rect::new(0, 0, 10, 3)).with_tab_index(id as i32)
    }

    #[test]
    fn push_with_trap_creates_focus_trap() {
        let mut modals = FocusAwareModalStack::new();

        // Add focusable nodes
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(1));
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(2));
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(3));

        // Focus node 3 before opening modal
        modals.focus_manager_mut().focus(3);
        assert_eq!(modals.focus_manager().current(), Some(3));

        // Push modal with trap containing nodes 1 and 2
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![1, 2]);

        // Focus should now be on node 1 (first in group)
        assert!(modals.is_focus_trapped());
        assert_eq!(modals.focus_manager().current(), Some(1));
    }

    #[test]
    fn pop_restores_focus() {
        let mut modals = FocusAwareModalStack::new();

        // Add focusable nodes
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(1));
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(2));
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(3));

        // Focus node 3 before opening modal
        modals.focus_manager_mut().focus(3);

        // Push modal with trap
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![1, 2]);
        assert_eq!(modals.focus_manager().current(), Some(1));

        // Pop modal - focus should return to node 3
        modals.pop();
        assert!(!modals.is_focus_trapped());
        assert_eq!(modals.focus_manager().current(), Some(3));
    }

    #[test]
    fn nested_modals_restore_correctly() {
        let mut modals = FocusAwareModalStack::new();

        // Add focusable nodes
        for id in 1..=6 {
            modals
                .focus_manager_mut()
                .graph_mut()
                .insert(make_focus_node(id));
        }

        // Initial focus
        modals.focus_manager_mut().focus(1);

        // First modal traps to nodes 2, 3
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![2, 3]);
        assert_eq!(modals.focus_manager().current(), Some(2));

        // Second modal traps to nodes 4, 5, 6
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![4, 5, 6]);
        assert_eq!(modals.focus_manager().current(), Some(4));

        // Pop second modal - back to first modal's focus (node 2)
        modals.pop();
        assert_eq!(modals.focus_manager().current(), Some(2));

        // Pop first modal - back to original focus (node 1)
        modals.pop();
        assert_eq!(modals.focus_manager().current(), Some(1));
        assert!(!modals.is_focus_trapped());
    }

    #[test]
    fn handle_event_escape_restores_focus() {
        let mut modals = FocusAwareModalStack::new();

        // Add focusable nodes
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(1));
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(2));

        // Focus node 2
        modals.focus_manager_mut().focus(2);

        // Push modal
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![1]);
        assert_eq!(modals.focus_manager().current(), Some(1));

        // Escape closes modal
        let escape = Event::Key(KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        let result = modals.handle_event(&escape);
        assert!(result.is_some());
        assert_eq!(modals.focus_manager().current(), Some(2));
    }

    #[test]
    fn handle_event_focus_loss_blurs_current_focus() {
        let mut modals = FocusAwareModalStack::new();
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(1));
        modals.focus_manager_mut().focus(1);
        let _ = modals.focus_manager_mut().take_focus_event();

        let result = modals.handle_event(&Event::Focus(false));
        assert!(result.is_none());
        assert_eq!(modals.focus_manager().current(), None);
        assert_eq!(
            modals.focus_manager_mut().take_focus_event(),
            Some(crate::focus::FocusEvent::FocusLost { id: 1 })
        );
    }

    #[test]
    fn handle_event_focus_gain_restores_trapped_focus() {
        let mut modals = FocusAwareModalStack::new();
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(1));
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(2));
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(3));
        modals.focus_manager_mut().focus(3);

        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![1, 2]);
        assert_eq!(modals.focus_manager().current(), Some(1));

        let _ = modals.handle_event(&Event::Focus(false));
        assert_eq!(modals.focus_manager().current(), None);

        let result = modals.handle_event(&Event::Focus(true));
        assert!(result.is_none());
        assert_eq!(modals.focus_manager().current(), Some(1));
    }

    #[test]
    fn push_without_trap_no_focus_change() {
        let mut modals = FocusAwareModalStack::new();

        // Add focusable nodes
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(1));
        modals
            .focus_manager_mut()
            .graph_mut()
            .insert(make_focus_node(2));

        // Focus node 2
        modals.focus_manager_mut().focus(2);

        // Push modal without trap
        modals.push(Box::new(WidgetModalEntry::new(StubWidget)));

        // Focus should not change
        assert!(!modals.is_focus_trapped());
        assert_eq!(modals.focus_manager().current(), Some(2));
    }

    #[test]
    fn pop_all_restores_all_focus() {
        let mut modals = FocusAwareModalStack::new();

        // Add focusable nodes
        for id in 1..=4 {
            modals
                .focus_manager_mut()
                .graph_mut()
                .insert(make_focus_node(id));
        }

        // Initial focus
        modals.focus_manager_mut().focus(1);

        // Push multiple modals
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![2]);
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![3]);
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![4]);

        assert_eq!(modals.depth(), 3);
        assert_eq!(modals.focus_manager().current(), Some(4));

        // Pop all
        let results = modals.pop_all();
        assert_eq!(results.len(), 3);
        assert!(modals.is_empty());
        assert!(!modals.is_focus_trapped());
        assert_eq!(modals.focus_manager().current(), Some(1));
    }

    #[test]
    fn tab_navigation_trapped_in_modal() {
        let mut modals = FocusAwareModalStack::new();

        // Add focusable nodes
        for id in 1..=5 {
            modals
                .focus_manager_mut()
                .graph_mut()
                .insert(make_focus_node(id));
        }

        // Push modal with nodes 2 and 3
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![2, 3]);

        // Focus should be on 2
        assert_eq!(modals.focus_manager().current(), Some(2));

        // Tab forward should go to 3
        modals.focus_manager_mut().focus_next();
        assert_eq!(modals.focus_manager().current(), Some(3));

        // Tab forward should wrap to 2 (trapped)
        modals.focus_manager_mut().focus_next();
        assert_eq!(modals.focus_manager().current(), Some(2));

        // Attempt to focus outside trap should fail
        assert!(modals.focus_manager_mut().focus(5).is_none());
        assert_eq!(modals.focus_manager().current(), Some(2));
    }

    #[test]
    fn empty_focus_group_no_panic() {
        let mut modals = FocusAwareModalStack::new();

        // Push modal with empty focus group (edge case)
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![]);

        // Should not panic, just have no focused element
        assert!(modals.is_focus_trapped());

        // Pop should still work
        modals.pop();
        assert!(!modals.is_focus_trapped());
    }

    #[test]
    fn pop_id_non_top_modal_does_not_corrupt_focus() {
        let mut modals = FocusAwareModalStack::new();

        // Add focusable nodes
        for id in 1..=6 {
            modals
                .focus_manager_mut()
                .graph_mut()
                .insert(make_focus_node(id));
        }

        // Initial focus
        modals.focus_manager_mut().focus(1);

        // Push three modals with focus traps
        // Trap stack will be: [(group1, return=1), (group2, return=2), (group3, return=3)]
        let id1 = modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![2]);
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![3]);
        let _id3 = modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![4]);

        // Focus should be on node 4 (top modal)
        assert_eq!(modals.focus_manager().current(), Some(4));

        // Pop the BOTTOM modal (id1) by ID - this is non-LIFO
        // This should NOT pop a focus trap since it's not the top
        // The trap for id1 becomes "orphaned" but doesn't corrupt the stack
        modals.pop_id(id1);

        // Focus should still be trapped (trap stack still has 3 traps, 2 modals remain)
        assert!(modals.is_focus_trapped());
        // Focus should still be on node 4 (top modal unchanged)
        assert_eq!(modals.focus_manager().current(), Some(4));
        assert_eq!(modals.depth(), 2);

        // Pop remaining modals normally
        modals.pop(); // Pops modal3, pops group3's trap, restores to 3
        assert_eq!(modals.focus_manager().current(), Some(3));

        modals.pop(); // Pops modal2, pops group2's trap, restores to 2
        // Note: We restore to 2, not 1, because group1's trap is orphaned
        assert_eq!(modals.focus_manager().current(), Some(2));

        // Stack is empty but there's still an orphaned trap
        assert!(modals.is_empty());
        // The orphaned trap means focus is still "trapped" to group1
        // This is a known limitation of using pop_id with focus groups
        assert!(modals.is_focus_trapped());
    }

    #[test]
    fn pop_id_top_modal_restores_focus_correctly() {
        let mut modals = FocusAwareModalStack::new();

        // Add focusable nodes
        for id in 1..=4 {
            modals
                .focus_manager_mut()
                .graph_mut()
                .insert(make_focus_node(id));
        }

        // Initial focus
        modals.focus_manager_mut().focus(1);

        // Push two modals
        modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![2]);
        let id2 = modals.push_with_trap(Box::new(WidgetModalEntry::new(StubWidget)), vec![3]);

        assert_eq!(modals.focus_manager().current(), Some(3));

        // Pop the TOP modal by ID - this should work correctly
        modals.pop_id(id2);

        // Focus should restore to modal1's focus (2)
        assert_eq!(modals.focus_manager().current(), Some(2));
        assert!(modals.is_focus_trapped()); // Still in modal1's trap

        // Pop the last modal
        modals.pop();
        assert_eq!(modals.focus_manager().current(), Some(1));
        assert!(!modals.is_focus_trapped());
    }

    #[test]
    fn default_creates_empty_stack() {
        let modals = FocusAwareModalStack::default();
        assert!(modals.is_empty());
        assert_eq!(modals.depth(), 0);
        assert!(!modals.is_focus_trapped());
    }

    #[test]
    fn with_focus_manager_uses_provided() {
        let mut fm = FocusManager::new();
        fm.graph_mut().insert(make_focus_node(42));
        fm.focus(42);

        let modals = FocusAwareModalStack::with_focus_manager(fm);
        assert!(modals.is_empty());
        assert_eq!(modals.focus_manager().current(), Some(42));
    }

    #[test]
    fn stack_accessors() {
        let mut modals = FocusAwareModalStack::new();
        assert!(modals.stack().is_empty());
        modals.push(Box::new(WidgetModalEntry::new(StubWidget)));
        assert!(!modals.stack().is_empty());
        assert_eq!(modals.stack_mut().depth(), 1);
    }

    #[test]
    fn depth_tracks_push_pop() {
        let mut modals = FocusAwareModalStack::new();
        assert_eq!(modals.depth(), 0);
        modals.push(Box::new(WidgetModalEntry::new(StubWidget)));
        assert_eq!(modals.depth(), 1);
        modals.push(Box::new(WidgetModalEntry::new(StubWidget)));
        assert_eq!(modals.depth(), 2);
        modals.pop();
        assert_eq!(modals.depth(), 1);
    }

    #[test]
    fn pop_empty_stack_returns_none() {
        let mut modals = FocusAwareModalStack::new();
        assert!(modals.pop().is_none());
    }
}

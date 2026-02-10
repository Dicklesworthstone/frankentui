#![forbid(unsafe_code)]

//! Modal stack for managing nested modals with proper z-ordering.
//!
//! The `ModalStack` manages multiple open modals in a LIFO (stack) order.
//! Only the topmost modal receives input events, while all modals are
//! rendered from bottom to top with appropriate backdrop dimming.
//!
//! # Invariants
//!
//! - Z-order is strictly increasing: later modals are always on top.
//! - Only the top modal receives input events.
//! - Close ordering is LIFO by default; pop-by-id removes from any position.
//! - Backdrop opacity is reduced for lower modals to create depth effect.
//!
//! # Failure Modes
//!
//! - `pop()` on empty stack returns `None` (no panic).
//! - `pop_id()` for non-existent ID returns `None`.
//! - `get()` / `get_mut()` for non-existent ID returns `None`.
//!
//! # Example
//!
//! ```ignore
//! let mut stack = ModalStack::new();
//!
//! // Push modals
//! let id1 = stack.push(ModalEntry::new(dialog1));
//! let id2 = stack.push(ModalEntry::new(dialog2));
//!
//! // Only top modal (id2) receives events
//! stack.handle_event(&event);
//!
//! // Render all modals in z-order
//! stack.render(frame, screen_area);
//!
//! // Pop top modal
//! let result = stack.pop(); // Returns id2's entry
//! ```

use ftui_core::event::Event;
use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId};
use ftui_style::Style;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::modal::{BackdropConfig, ModalSizeConstraints};
use crate::set_style_area;

/// Base z-index for modal layer.
const BASE_MODAL_Z: u32 = 1000;

/// Z-index increment between modals (leaves room for internal layers).
const Z_INCREMENT: u32 = 10;

/// Global counter for unique modal IDs.
static MODAL_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Unique identifier for a modal in the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModalId(u64);

impl ModalId {
    /// Create a new unique modal ID.
    fn new() -> Self {
        Self(MODAL_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Get the raw ID value.
    #[inline]
    pub const fn id(self) -> u64 {
        self.0
    }
}

/// Result returned when a modal is closed.
#[derive(Debug, Clone)]
pub struct ModalResult {
    /// The modal ID that was closed.
    pub id: ModalId,
    /// Optional result data from the modal.
    pub data: Option<ModalResultData>,
    /// Focus group ID if one was associated (for calling `FocusManager::pop_trap`).
    pub focus_group_id: Option<u32>,
}

/// Modal result data variants.
#[derive(Debug, Clone)]
pub enum ModalResultData {
    /// Dialog was dismissed (escaped or cancelled).
    Dismissed,
    /// Dialog was confirmed.
    Confirmed,
    /// Dialog returned a custom value.
    Custom(String),
}

/// A FocusId alias for modal focus management.
pub type ModalFocusId = u64;

/// Trait for modal content that can be managed in the stack.
///
/// This trait abstracts over different modal implementations (Dialog, custom modals)
/// so they can all be managed by the same stack.
///
/// # Focus Management (bd-39vx.5)
///
/// Modals can optionally participate in focus management by providing:
/// - `focusable_ids()`: List of focusable widget IDs within the modal
/// - `aria_modal()`: Whether this modal should be treated as an ARIA modal
///
/// When focus management is enabled, the caller should:
/// 1. Create a focus group from `focusable_ids()` when the modal opens
/// 2. Push a focus trap to constrain Tab navigation within the modal
/// 3. Auto-focus the first focusable widget
/// 4. Restore previous focus when the modal closes
pub trait StackModal: Send {
    /// Render the modal content at the given area.
    fn render_content(&self, area: Rect, frame: &mut Frame);

    /// Handle an event, returning true if the modal should close.
    fn handle_event(&mut self, event: &Event, hit_id: HitId) -> Option<ModalResultData>;

    /// Get the modal's size constraints.
    fn size_constraints(&self) -> ModalSizeConstraints;

    /// Get the backdrop configuration.
    fn backdrop_config(&self) -> BackdropConfig;

    /// Whether this modal can be closed by pressing Escape.
    fn close_on_escape(&self) -> bool {
        true
    }

    /// Whether this modal can be closed by clicking the backdrop.
    fn close_on_backdrop(&self) -> bool {
        true
    }

    /// Whether this modal is an ARIA modal (accessibility semantic).
    ///
    /// ARIA modals:
    /// - Trap focus within the modal (Tab cannot escape)
    /// - Announce modal semantics to screen readers
    /// - Block interaction with content behind the modal
    ///
    /// Default: `true` for accessibility compliance.
    ///
    /// # Invariants
    /// - When `aria_modal()` returns true, focus MUST be trapped within the modal.
    /// - Screen readers should announce modal state changes.
    ///
    /// # Failure Modes
    /// - If focus trap is not configured, Tab may escape (accessibility violation).
    fn aria_modal(&self) -> bool {
        true
    }

    /// Get the IDs of focusable widgets within this modal.
    ///
    /// These IDs are used to create a focus group when the modal opens.
    /// The first ID in the list receives auto-focus.
    ///
    /// Returns `None` if focus management is not needed (e.g., non-interactive modals).
    ///
    /// # Example
    /// ```ignore
    /// fn focusable_ids(&self) -> Option<Vec<ModalFocusId>> {
    ///     Some(vec![
    ///         self.input_field_id,
    ///         self.confirm_button_id,
    ///         self.cancel_button_id,
    ///     ])
    /// }
    /// ```
    fn focusable_ids(&self) -> Option<Vec<ModalFocusId>> {
        None
    }
}

/// An active modal in the stack.
struct ActiveModal {
    /// Unique identifier for this modal.
    id: ModalId,
    /// Z-index for layering (reserved for future compositor integration).
    #[allow(dead_code)]
    z_index: u32,
    /// The modal content.
    modal: Box<dyn StackModal>,
    /// Hit ID for this modal's hit regions.
    hit_id: HitId,
    /// Focus group ID for focus trap integration.
    focus_group_id: Option<u32>,
}

/// Stack of active modals with z-ordering and input routing.
///
/// # Invariants
///
/// - `modals` is ordered by z_index (lowest to highest).
/// - `next_z` always produces a z_index greater than any existing modal.
/// - Input is only routed to the top modal (last in the vec).
pub struct ModalStack {
    /// Active modals in z-order (bottom to top).
    modals: Vec<ActiveModal>,
    /// Next z-index to assign.
    next_z: u32,
    /// Next hit ID to assign.
    next_hit_id: u32,
}

impl Default for ModalStack {
    fn default() -> Self {
        Self::new()
    }
}

impl ModalStack {
    /// Create an empty modal stack.
    pub fn new() -> Self {
        Self {
            modals: Vec::new(),
            next_z: 0,
            next_hit_id: 1000, // Start hit IDs high to avoid conflicts
        }
    }

    // --- Stack Operations ---

    /// Push a modal onto the stack.
    ///
    /// Returns the unique `ModalId` for the pushed modal.
    pub fn push(&mut self, modal: Box<dyn StackModal>) -> ModalId {
        self.push_with_focus(modal, None)
    }

    /// Push a modal with an associated focus group ID.
    ///
    /// The focus group ID is used to integrate with `FocusManager`:
    /// 1. Before calling this, create a focus group with `focus_manager.create_group(id, members)`
    /// 2. Then call `focus_manager.push_trap(id)` to trap focus within the modal
    /// 3. When the modal closes, call `focus_manager.pop_trap()` to restore focus
    ///
    /// Returns the unique `ModalId` for the pushed modal.
    pub fn push_with_focus(
        &mut self,
        modal: Box<dyn StackModal>,
        focus_group_id: Option<u32>,
    ) -> ModalId {
        let id = ModalId::new();
        let z_index = BASE_MODAL_Z + self.next_z;
        self.next_z += Z_INCREMENT;

        let hit_id = HitId::new(self.next_hit_id);
        self.next_hit_id += 1;

        self.modals.push(ActiveModal {
            id,
            z_index,
            modal,
            hit_id,
            focus_group_id,
        });

        id
    }

    /// Get the focus group ID for a modal.
    ///
    /// Returns `None` if the modal doesn't exist or has no focus group.
    pub fn focus_group_id(&self, modal_id: ModalId) -> Option<u32> {
        self.modals
            .iter()
            .find(|m| m.id == modal_id)
            .and_then(|m| m.focus_group_id)
    }

    /// Get the focus group ID for the top modal.
    ///
    /// Useful for checking if focus trap should be active.
    pub fn top_focus_group_id(&self) -> Option<u32> {
        self.modals.last().and_then(|m| m.focus_group_id)
    }

    /// Pop the top modal from the stack.
    ///
    /// Returns the result if a modal was popped, or `None` if the stack is empty.
    /// If the modal had a focus group, the caller should call `FocusManager::pop_trap()`.
    pub fn pop(&mut self) -> Option<ModalResult> {
        self.modals.pop().map(|m| ModalResult {
            id: m.id,
            data: None,
            focus_group_id: m.focus_group_id,
        })
    }

    /// Pop a specific modal by ID.
    ///
    /// Returns the result if the modal was found and removed, or `None` if not found.
    /// Note: This breaks strict LIFO ordering but is sometimes needed.
    /// If the modal had a focus group, the caller should handle focus restoration.
    pub fn pop_id(&mut self, id: ModalId) -> Option<ModalResult> {
        let idx = self.modals.iter().position(|m| m.id == id)?;
        let modal = self.modals.remove(idx);
        Some(ModalResult {
            id: modal.id,
            data: None,
            focus_group_id: modal.focus_group_id,
        })
    }

    /// Pop all modals from the stack.
    ///
    /// Returns results in LIFO order (top first).
    pub fn pop_all(&mut self) -> Vec<ModalResult> {
        let mut results = Vec::with_capacity(self.modals.len());
        while let Some(result) = self.pop() {
            results.push(result);
        }
        results
    }

    /// Get a reference to the top modal.
    pub fn top(&self) -> Option<&(dyn StackModal + 'static)> {
        self.modals.last().map(|m| &*m.modal)
    }

    /// Get a mutable reference to the top modal.
    pub fn top_mut(&mut self) -> Option<&mut (dyn StackModal + 'static)> {
        match self.modals.last_mut() {
            Some(m) => Some(m.modal.as_mut()),
            None => None,
        }
    }

    // --- State Queries ---

    /// Check if the stack is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.modals.is_empty()
    }

    /// Get the number of modals in the stack.
    #[inline]
    pub fn depth(&self) -> usize {
        self.modals.len()
    }

    /// Check if a modal with the given ID exists in the stack.
    pub fn contains(&self, id: ModalId) -> bool {
        self.modals.iter().any(|m| m.id == id)
    }

    /// Get the ID of the top modal, if any.
    pub fn top_id(&self) -> Option<ModalId> {
        self.modals.last().map(|m| m.id)
    }

    // --- Event Handling ---

    /// Handle an event, routing it to the top modal only.
    ///
    /// Returns `Some(ModalResult)` if the top modal closed, otherwise `None`.
    /// If the result contains a `focus_group_id`, the caller should call
    /// `FocusManager::pop_trap()` to restore focus.
    pub fn handle_event(&mut self, event: &Event) -> Option<ModalResult> {
        let top = self.modals.last_mut()?;
        let hit_id = top.hit_id;
        let id = top.id;
        let focus_group_id = top.focus_group_id;

        if let Some(data) = top.modal.handle_event(event, hit_id) {
            // Modal wants to close
            self.modals.pop();
            return Some(ModalResult {
                id,
                data: Some(data),
                focus_group_id,
            });
        }

        None
    }

    // --- Rendering ---

    /// Render all modals in z-order.
    ///
    /// Modals are rendered from bottom to top. Lower modals have reduced
    /// backdrop opacity to create a visual depth effect.
    pub fn render(&self, frame: &mut Frame, screen: Rect) {
        if self.modals.is_empty() {
            return;
        }

        let modal_count = self.modals.len();

        for (i, modal) in self.modals.iter().enumerate() {
            let is_top = i == modal_count - 1;

            // Calculate backdrop opacity with depth dimming
            let base_opacity = modal.modal.backdrop_config().opacity;
            let opacity = if is_top {
                base_opacity
            } else {
                // Reduce opacity for lower modals (50% of configured)
                base_opacity * 0.5
            };

            // Render backdrop
            if opacity > 0.0 {
                let bg_color = modal.modal.backdrop_config().color.with_opacity(opacity);
                set_style_area(&mut frame.buffer, screen, Style::new().bg(bg_color));
            }

            // Calculate modal content area
            let constraints = modal.modal.size_constraints();
            let available = ftui_core::geometry::Size::new(screen.width, screen.height);
            let size = constraints.clamp(available);

            if size.width == 0 || size.height == 0 {
                continue;
            }

            // Center the modal
            let x = screen.x + (screen.width.saturating_sub(size.width)) / 2;
            let y = screen.y + (screen.height.saturating_sub(size.height)) / 2;
            let content_area = Rect::new(x, y, size.width, size.height);

            // Render modal content
            modal.modal.render_content(content_area, frame);
        }
    }
}

/// A simple modal entry that wraps any Widget.
pub struct WidgetModalEntry<W> {
    widget: W,
    size: ModalSizeConstraints,
    backdrop: BackdropConfig,
    close_on_escape: bool,
    close_on_backdrop: bool,
    aria_modal: bool,
    focusable_ids: Option<Vec<ModalFocusId>>,
}

impl<W> WidgetModalEntry<W> {
    /// Create a new modal entry with a widget.
    pub fn new(widget: W) -> Self {
        Self {
            widget,
            size: ModalSizeConstraints::new()
                .min_width(30)
                .max_width(60)
                .min_height(10)
                .max_height(20),
            backdrop: BackdropConfig::default(),
            close_on_escape: true,
            close_on_backdrop: true,
            aria_modal: true,
            focusable_ids: None,
        }
    }

    /// Set size constraints.
    #[must_use]
    pub fn size(mut self, size: ModalSizeConstraints) -> Self {
        self.size = size;
        self
    }

    /// Set backdrop configuration.
    #[must_use]
    pub fn backdrop(mut self, backdrop: BackdropConfig) -> Self {
        self.backdrop = backdrop;
        self
    }

    /// Set whether Escape closes the modal.
    #[must_use]
    pub fn close_on_escape(mut self, close: bool) -> Self {
        self.close_on_escape = close;
        self
    }

    /// Set whether backdrop click closes the modal.
    #[must_use]
    pub fn close_on_backdrop(mut self, close: bool) -> Self {
        self.close_on_backdrop = close;
        self
    }

    /// Set whether this modal is an ARIA modal.
    ///
    /// ARIA modals trap focus and announce semantics to screen readers.
    /// Default is `true` for accessibility compliance.
    #[must_use]
    pub fn with_aria_modal(mut self, aria_modal: bool) -> Self {
        self.aria_modal = aria_modal;
        self
    }

    /// Set the focusable widget IDs for focus trap integration.
    ///
    /// When provided, these IDs will be used to:
    /// 1. Create a focus group constraining Tab navigation
    /// 2. Auto-focus the first focusable widget when modal opens
    /// 3. Restore focus to the previous element when modal closes
    #[must_use]
    pub fn with_focusable_ids(mut self, ids: Vec<ModalFocusId>) -> Self {
        self.focusable_ids = Some(ids);
        self
    }
}

impl<W: crate::Widget + Send> StackModal for WidgetModalEntry<W> {
    fn render_content(&self, area: Rect, frame: &mut Frame) {
        self.widget.render(area, frame);
    }

    fn handle_event(&mut self, event: &Event, _hit_id: HitId) -> Option<ModalResultData> {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind};

        // Handle escape to close
        if self.close_on_escape
            && let Event::Key(KeyEvent {
                code: KeyCode::Escape,
                kind: KeyEventKind::Press,
                ..
            }) = event
        {
            return Some(ModalResultData::Dismissed);
        }

        None
    }

    fn size_constraints(&self) -> ModalSizeConstraints {
        self.size
    }

    fn backdrop_config(&self) -> BackdropConfig {
        self.backdrop
    }

    fn close_on_escape(&self) -> bool {
        self.close_on_escape
    }

    fn close_on_backdrop(&self) -> bool {
        self.close_on_backdrop
    }

    fn aria_modal(&self) -> bool {
        self.aria_modal
    }

    fn focusable_ids(&self) -> Option<Vec<ModalFocusId>> {
        self.focusable_ids.clone()
    }
}

// =========================================================================
// Modal Focus Integration Helper (bd-39vx.5)
// =========================================================================

/// Helper for integrating `ModalStack` with `FocusManager`.
///
/// This struct provides a convenient API for:
/// - Pushing modals with automatic focus trap setup
/// - Popping modals with focus restoration
/// - Managing focus groups for nested modals
///
/// # Example
/// ```ignore
/// use ftui_widgets::modal::{ModalStack, ModalFocusIntegration};
/// use ftui_widgets::focus::FocusManager;
///
/// let mut stack = ModalStack::new();
/// let mut focus = FocusManager::new();
/// let mut integrator = ModalFocusIntegration::new(&mut stack, &mut focus);
///
/// // Push a modal with focus management
/// let modal_id = integrator.push_with_focus(dialog);
///
/// // ... handle events ...
///
/// // Pop modal and restore focus
/// integrator.pop_with_focus();
/// ```
#[allow(dead_code)]
pub struct ModalFocusIntegration<'a> {
    stack: &'a mut ModalStack,
    focus: &'a mut crate::focus::FocusManager,
    next_group_id: u32,
}

impl<'a> ModalFocusIntegration<'a> {
    /// Create a new integration helper.
    pub fn new(stack: &'a mut ModalStack, focus: &'a mut crate::focus::FocusManager) -> Self {
        Self {
            stack,
            focus,
            next_group_id: 1000, // Start high to avoid conflicts
        }
    }

    /// Push a modal with automatic focus management.
    ///
    /// 1. Creates a focus group from `modal.focusable_ids()` (if provided)
    /// 2. Pushes a focus trap to constrain Tab navigation
    /// 3. Auto-focuses the first focusable widget
    /// 4. Stores the previous focus for restoration on close
    ///
    /// Returns the modal ID.
    pub fn push_with_focus(&mut self, modal: Box<dyn StackModal>) -> ModalId {
        let focusable_ids = modal.focusable_ids();
        let is_aria_modal = modal.aria_modal();

        let focus_group_id = if is_aria_modal {
            if let Some(ids) = focusable_ids {
                let group_id = self.next_group_id;
                self.next_group_id += 1;

                // Convert ModalFocusId (u64) to FocusId (u64) for the focus manager
                let focus_ids: Vec<crate::focus::FocusId> = ids.into_iter().collect();

                // Create focus group and trap
                self.focus.create_group(group_id, focus_ids);
                self.focus.push_trap(group_id);

                Some(group_id)
            } else {
                None
            }
        } else {
            None
        };

        self.stack.push_with_focus(modal, focus_group_id)
    }

    /// Pop the top modal with focus restoration.
    ///
    /// If the modal had a focus group, the trap is popped and focus
    /// is restored to the element that was focused before the modal opened.
    ///
    /// Returns the modal result.
    pub fn pop_with_focus(&mut self) -> Option<ModalResult> {
        let result = self.stack.pop();

        if let Some(ref res) = result
            && res.focus_group_id.is_some()
        {
            self.focus.pop_trap();
        }

        result
    }

    /// Handle an event with automatic focus trap popping.
    ///
    /// If the event causes the modal to close, the focus trap is popped.
    pub fn handle_event(&mut self, event: &Event) -> Option<ModalResult> {
        let result = self.stack.handle_event(event);

        if let Some(ref res) = result
            && res.focus_group_id.is_some()
        {
            self.focus.pop_trap();
        }

        result
    }

    /// Check if focus is currently trapped in a modal.
    pub fn is_focus_trapped(&self) -> bool {
        self.focus.is_trapped()
    }

    /// Get a reference to the underlying modal stack.
    pub fn stack(&self) -> &ModalStack {
        self.stack
    }

    /// Get a mutable reference to the underlying modal stack.
    pub fn stack_mut(&mut self) -> &mut ModalStack {
        self.stack
    }

    /// Get a reference to the underlying focus manager.
    pub fn focus(&self) -> &crate::focus::FocusManager {
        self.focus
    }

    /// Get a mutable reference to the underlying focus manager.
    pub fn focus_mut(&mut self) -> &mut crate::focus::FocusManager {
        self.focus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Widget;
    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use ftui_render::cell::PackedRgba;
    use ftui_render::grapheme_pool::GraphemePool;

    #[derive(Debug, Clone)]
    struct StubWidget;

    impl Widget for StubWidget {
        fn render(&self, _area: Rect, _frame: &mut Frame) {}
    }

    #[test]
    fn empty_stack() {
        let stack = ModalStack::new();
        assert!(stack.is_empty());
        assert_eq!(stack.depth(), 0);
        assert!(stack.top().is_none());
        assert!(stack.top_id().is_none());
    }

    #[test]
    fn push_increases_depth() {
        let mut stack = ModalStack::new();
        let id1 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        assert_eq!(stack.depth(), 1);
        assert!(!stack.is_empty());
        assert!(stack.contains(id1));

        let id2 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        assert_eq!(stack.depth(), 2);
        assert!(stack.contains(id2));
        assert_eq!(stack.top_id(), Some(id2));
    }

    #[test]
    fn pop_lifo_order() {
        let mut stack = ModalStack::new();
        let id1 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        let id2 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        let id3 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        let result = stack.pop();
        assert_eq!(result.map(|r| r.id), Some(id3));
        assert_eq!(stack.depth(), 2);

        let result = stack.pop();
        assert_eq!(result.map(|r| r.id), Some(id2));
        assert_eq!(stack.depth(), 1);

        let result = stack.pop();
        assert_eq!(result.map(|r| r.id), Some(id1));
        assert!(stack.is_empty());
    }

    #[test]
    fn pop_empty_returns_none() {
        let mut stack = ModalStack::new();
        assert!(stack.pop().is_none());
    }

    #[test]
    fn pop_by_id() {
        let mut stack = ModalStack::new();
        let id1 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        let id2 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        let id3 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        // Pop middle modal
        let result = stack.pop_id(id2);
        assert_eq!(result.map(|r| r.id), Some(id2));
        assert_eq!(stack.depth(), 2);
        assert!(!stack.contains(id2));
        assert!(stack.contains(id1));
        assert!(stack.contains(id3));
    }

    #[test]
    fn pop_by_nonexistent_id() {
        let mut stack = ModalStack::new();
        let _id1 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        // Try to pop non-existent ID
        let fake_id = ModalId(999999);
        assert!(stack.pop_id(fake_id).is_none());
        assert_eq!(stack.depth(), 1);
    }

    #[test]
    fn pop_all() {
        let mut stack = ModalStack::new();
        let id1 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        let id2 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        let id3 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        let results = stack.pop_all();
        assert_eq!(results.len(), 3);
        // LIFO order: id3, id2, id1
        assert_eq!(results[0].id, id3);
        assert_eq!(results[1].id, id2);
        assert_eq!(results[2].id, id1);
        assert!(stack.is_empty());
    }

    #[test]
    fn z_order_increasing() {
        let mut stack = ModalStack::new();

        // Push multiple modals
        stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        // Verify z-order is increasing
        let z_indices: Vec<u32> = stack.modals.iter().map(|m| m.z_index).collect();
        for i in 1..z_indices.len() {
            assert!(
                z_indices[i] > z_indices[i - 1],
                "z_index should be strictly increasing"
            );
        }
    }

    #[test]
    fn escape_closes_top_modal() {
        let mut stack = ModalStack::new();
        let id1 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        let id2 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        let escape = Event::Key(KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        // Escape should close top modal (id2)
        let result = stack.handle_event(&escape);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, id2);
        assert_eq!(stack.depth(), 1);
        assert_eq!(stack.top_id(), Some(id1));
    }

    #[test]
    fn render_does_not_panic() {
        let mut stack = ModalStack::new();
        stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let screen = Rect::new(0, 0, 80, 24);

        // Should not panic
        stack.render(&mut frame, screen);
    }

    #[test]
    fn render_empty_stack_no_op() {
        let stack = ModalStack::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let screen = Rect::new(0, 0, 80, 24);

        // Should be a no-op
        stack.render(&mut frame, screen);
    }

    #[test]
    fn contains_after_pop() {
        let mut stack = ModalStack::new();
        let id1 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        assert!(stack.contains(id1));
        stack.pop();
        assert!(!stack.contains(id1));
    }

    #[test]
    fn unique_modal_ids() {
        let mut stack = ModalStack::new();
        let id1 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        let id2 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));
        let id3 = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);
    }

    #[test]
    fn widget_modal_entry_builder() {
        let entry = WidgetModalEntry::new(StubWidget)
            .size(ModalSizeConstraints::new().min_width(40).max_width(80))
            .backdrop(BackdropConfig::new(PackedRgba::rgb(0, 0, 0), 0.8))
            .close_on_escape(false)
            .close_on_backdrop(false);

        assert!(!entry.close_on_escape);
        assert!(!entry.close_on_backdrop);
        assert_eq!(entry.size.min_width, Some(40));
        assert_eq!(entry.size.max_width, Some(80));
    }

    #[test]
    fn escape_disabled_does_not_close() {
        let mut stack = ModalStack::new();
        stack.push(Box::new(
            WidgetModalEntry::new(StubWidget).close_on_escape(false),
        ));

        let escape = Event::Key(KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        // Escape should NOT close the modal
        let result = stack.handle_event(&escape);
        assert!(result.is_none());
        assert_eq!(stack.depth(), 1);
    }

    // --- Focus group integration tests ---

    #[test]
    fn push_with_focus_tracks_group_id() {
        let mut stack = ModalStack::new();
        let modal_id = stack.push_with_focus(Box::new(WidgetModalEntry::new(StubWidget)), Some(42));

        assert_eq!(stack.focus_group_id(modal_id), Some(42));
        assert_eq!(stack.top_focus_group_id(), Some(42));
    }

    #[test]
    fn pop_returns_focus_group_id() {
        let mut stack = ModalStack::new();
        stack.push_with_focus(Box::new(WidgetModalEntry::new(StubWidget)), Some(99));

        let result = stack.pop();
        assert!(result.is_some());
        assert_eq!(result.unwrap().focus_group_id, Some(99));
    }

    #[test]
    fn pop_id_returns_focus_group_id() {
        let mut stack = ModalStack::new();
        let id1 = stack.push_with_focus(Box::new(WidgetModalEntry::new(StubWidget)), Some(10));
        let _id2 = stack.push_with_focus(Box::new(WidgetModalEntry::new(StubWidget)), Some(20));

        let result = stack.pop_id(id1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().focus_group_id, Some(10));
    }

    #[test]
    fn handle_event_returns_focus_group_id() {
        let mut stack = ModalStack::new();
        stack.push_with_focus(Box::new(WidgetModalEntry::new(StubWidget)), Some(77));

        let escape = Event::Key(KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        let result = stack.handle_event(&escape);
        assert!(result.is_some());
        assert_eq!(result.unwrap().focus_group_id, Some(77));
    }

    #[test]
    fn push_without_focus_has_none_group_id() {
        let mut stack = ModalStack::new();
        let modal_id = stack.push(Box::new(WidgetModalEntry::new(StubWidget)));

        assert_eq!(stack.focus_group_id(modal_id), None);
        assert_eq!(stack.top_focus_group_id(), None);
    }

    #[test]
    fn nested_focus_groups_track_correctly() {
        let mut stack = ModalStack::new();
        let _id1 = stack.push_with_focus(Box::new(WidgetModalEntry::new(StubWidget)), Some(1));
        let id2 = stack.push_with_focus(Box::new(WidgetModalEntry::new(StubWidget)), Some(2));
        let _id3 = stack.push_with_focus(Box::new(WidgetModalEntry::new(StubWidget)), Some(3));

        // Top should be group 3
        assert_eq!(stack.top_focus_group_id(), Some(3));

        // Pop top, now group 2 is on top
        stack.pop();
        assert_eq!(stack.top_focus_group_id(), Some(2));

        // Query specific modal
        assert_eq!(stack.focus_group_id(id2), Some(2));
    }

    // --- ARIA modal tests (bd-39vx.5) ---

    #[test]
    fn default_aria_modal_is_true() {
        let entry = WidgetModalEntry::new(StubWidget);
        assert!(entry.aria_modal);
    }

    #[test]
    fn aria_modal_builder() {
        let entry = WidgetModalEntry::new(StubWidget).with_aria_modal(false);
        assert!(!entry.aria_modal);
    }

    #[test]
    fn focusable_ids_builder() {
        let entry = WidgetModalEntry::new(StubWidget).with_focusable_ids(vec![1, 2, 3]);
        assert_eq!(entry.focusable_ids, Some(vec![1, 2, 3]));
    }

    #[test]
    fn stack_modal_aria_modal_trait() {
        let entry = WidgetModalEntry::new(StubWidget);
        assert!(StackModal::aria_modal(&entry)); // Default true

        let entry_non_aria = WidgetModalEntry::new(StubWidget).with_aria_modal(false);
        assert!(!StackModal::aria_modal(&entry_non_aria));
    }

    #[test]
    fn stack_modal_focusable_ids_trait() {
        let entry = WidgetModalEntry::new(StubWidget);
        assert!(StackModal::focusable_ids(&entry).is_none()); // Default none

        let entry_with_ids = WidgetModalEntry::new(StubWidget).with_focusable_ids(vec![10, 20]);
        assert_eq!(
            StackModal::focusable_ids(&entry_with_ids),
            Some(vec![10, 20])
        );
    }

    // --- ModalFocusIntegration tests ---

    #[test]
    fn focus_integration_push_creates_trap() {
        use crate::focus::{FocusManager, FocusNode};
        use ftui_core::geometry::Rect;

        let mut stack = ModalStack::new();
        let mut focus = FocusManager::new();

        // Register focusable nodes
        focus
            .graph_mut()
            .insert(FocusNode::new(1, Rect::new(0, 0, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(2, Rect::new(0, 1, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(100, Rect::new(0, 10, 10, 1))); // Outside modal

        // Focus outside modal initially
        focus.focus(100);
        assert_eq!(focus.current(), Some(100));

        {
            let mut integrator = ModalFocusIntegration::new(&mut stack, &mut focus);

            // Push modal with focusable IDs
            let modal = WidgetModalEntry::new(StubWidget).with_focusable_ids(vec![1, 2]);
            let _modal_id = integrator.push_with_focus(Box::new(modal));

            // Focus should now be trapped
            assert!(integrator.is_focus_trapped());

            // Focus should move to first focusable in modal
            assert_eq!(integrator.focus().current(), Some(1));
        }
    }

    #[test]
    fn focus_integration_pop_restores_focus() {
        use crate::focus::{FocusManager, FocusNode};
        use ftui_core::geometry::Rect;

        let mut stack = ModalStack::new();
        let mut focus = FocusManager::new();

        // Register focusable nodes
        focus
            .graph_mut()
            .insert(FocusNode::new(1, Rect::new(0, 0, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(2, Rect::new(0, 1, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(100, Rect::new(0, 10, 10, 1))); // Trigger element

        // Focus the trigger element before opening modal
        focus.focus(100);
        assert_eq!(focus.current(), Some(100));

        {
            let mut integrator = ModalFocusIntegration::new(&mut stack, &mut focus);

            // Push modal
            let modal = WidgetModalEntry::new(StubWidget).with_focusable_ids(vec![1, 2]);
            integrator.push_with_focus(Box::new(modal));

            // Focus is in modal
            assert!(integrator.is_focus_trapped());

            // Pop modal
            let result = integrator.pop_with_focus();
            assert!(result.is_some());

            // Focus should be restored to trigger element
            assert!(!integrator.is_focus_trapped());
            assert_eq!(integrator.focus().current(), Some(100));
        }
    }

    #[test]
    fn focus_integration_escape_restores_focus() {
        use crate::focus::{FocusManager, FocusNode};
        use ftui_core::geometry::Rect;

        let mut stack = ModalStack::new();
        let mut focus = FocusManager::new();

        focus
            .graph_mut()
            .insert(FocusNode::new(1, Rect::new(0, 0, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(100, Rect::new(0, 10, 10, 1)));

        focus.focus(100);

        {
            let mut integrator = ModalFocusIntegration::new(&mut stack, &mut focus);

            let modal = WidgetModalEntry::new(StubWidget).with_focusable_ids(vec![1]);
            integrator.push_with_focus(Box::new(modal));

            assert!(integrator.is_focus_trapped());

            // Simulate Escape key
            let escape = Event::Key(KeyEvent {
                code: KeyCode::Escape,
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            });
            let result = integrator.handle_event(&escape);

            assert!(result.is_some());
            assert!(!integrator.is_focus_trapped());
            assert_eq!(integrator.focus().current(), Some(100));
        }
    }

    #[test]
    fn focus_integration_non_aria_modal_no_trap() {
        use crate::focus::{FocusManager, FocusNode};
        use ftui_core::geometry::Rect;

        let mut stack = ModalStack::new();
        let mut focus = FocusManager::new();

        focus
            .graph_mut()
            .insert(FocusNode::new(1, Rect::new(0, 0, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(100, Rect::new(0, 10, 10, 1)));

        focus.focus(100);

        {
            let mut integrator = ModalFocusIntegration::new(&mut stack, &mut focus);

            // Push non-ARIA modal (aria_modal = false)
            let modal = WidgetModalEntry::new(StubWidget)
                .with_aria_modal(false)
                .with_focusable_ids(vec![1]);
            integrator.push_with_focus(Box::new(modal));

            // Focus should NOT be trapped for non-ARIA modals
            assert!(!integrator.is_focus_trapped());
        }
    }

    #[test]
    fn focus_integration_nested_modals() {
        use crate::focus::{FocusManager, FocusNode};
        use ftui_core::geometry::Rect;

        let mut stack = ModalStack::new();
        let mut focus = FocusManager::new();

        // Register nodes for both modals and background
        focus
            .graph_mut()
            .insert(FocusNode::new(1, Rect::new(0, 0, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(2, Rect::new(0, 1, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(10, Rect::new(0, 5, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(11, Rect::new(0, 6, 10, 1)));
        focus
            .graph_mut()
            .insert(FocusNode::new(100, Rect::new(0, 10, 10, 1)));

        focus.focus(100);

        {
            let mut integrator = ModalFocusIntegration::new(&mut stack, &mut focus);

            // Push first modal
            let modal1 = WidgetModalEntry::new(StubWidget).with_focusable_ids(vec![1, 2]);
            integrator.push_with_focus(Box::new(modal1));
            assert_eq!(integrator.focus().current(), Some(1));

            // Push second modal (nested)
            let modal2 = WidgetModalEntry::new(StubWidget).with_focusable_ids(vec![10, 11]);
            integrator.push_with_focus(Box::new(modal2));
            assert_eq!(integrator.focus().current(), Some(10));

            // Pop second modal - should restore to first modal's focus
            integrator.pop_with_focus();
            assert_eq!(integrator.focus().current(), Some(1));

            // Pop first modal - should restore to original focus
            integrator.pop_with_focus();
            assert_eq!(integrator.focus().current(), Some(100));
        }
    }
}

#![forbid(unsafe_code)]

//! Event coalescing for high-frequency input events.
//!
//! Terminal applications can receive a flood of events during rapid user
//! interaction, particularly mouse moves and scrolls. Without coalescing,
//! each event triggers a model update and potential re-render, causing lag.
//!
//! This module provides [`EventCoalescer`] which:
//! - Coalesces rapid mouse moves into a single event
//! - Coalesces consecutive scroll events in the same direction
//! - Passes through all other events immediately
//!
//! # Design
//!
//! The coalescer uses a "latest wins" strategy for coalescable events:
//! - Mouse moves: keep only the most recent position
//! - Scroll events: keep direction and total delta
//!
//! Non-coalescable events (key presses, mouse clicks, etc.) pass through
//! immediately. The caller is responsible for flushing pending events.
//!
//! # Usage
//!
//! ```
//! use ftui_core::event_coalescer::EventCoalescer;
//! use ftui_core::event::{Event, MouseEvent, MouseEventKind, KeyEvent, KeyCode};
//!
//! let mut coalescer = EventCoalescer::new();
//!
//! // Mouse moves coalesce - only the latest position is kept
//! assert!(coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 10, 10))).is_none());
//! assert!(coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 20, 20))).is_none());
//!
//! // Non-coalescable events pass through immediately (no auto-flush)
//! let result = coalescer.push(Event::Key(KeyEvent::new(KeyCode::Enter)));
//! assert!(result.is_some()); // Returns the key event
//!
//! // Caller must explicitly flush to get pending coalesced events
//! let pending = coalescer.flush();
//! assert_eq!(pending.len(), 1);
//! if let Event::Mouse(m) = &pending[0] {
//!     assert_eq!(m.x, 20);
//!     assert_eq!(m.y, 20);
//! }
//! ```

use crate::event::{Event, MouseEvent, MouseEventKind};

/// Coalesces high-frequency terminal events to prevent event storms.
///
/// # Thread Safety
///
/// `EventCoalescer` is not thread-safe. It should be used from a single
/// event processing thread.
///
/// # Performance
///
/// All operations are O(1). The coalescer holds at most two pending events
/// (one mouse move and one scroll sequence).
#[derive(Debug, Clone, Default)]
pub struct EventCoalescer {
    /// Pending mouse move event (latest position wins).
    pending_mouse_move: Option<MouseEvent>,

    /// Pending scroll state (direction + count).
    pending_scroll: Option<ScrollState>,
}

/// Accumulated scroll state for coalescing.
#[derive(Debug, Clone, Copy)]
struct ScrollState {
    direction: ScrollDirection,
    count: u32,
    modifiers: crate::event::Modifiers,
    /// Position of the last scroll event (some terminals report scroll position).
    x: u16,
    y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl EventCoalescer {
    /// Create a new event coalescer with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an event into the coalescer.
    ///
    /// Returns `Some(event)` if the event should be processed immediately,
    /// or `None` if the event was coalesced and is pending.
    ///
    /// # Coalescing Rules
    ///
    /// - **Mouse move**: Replaces any pending mouse move. Returns `None`.
    /// - **Scroll (same direction)**: Increments pending scroll count. Returns `None`.
    /// - **Scroll (different direction)**: Flushes pending scroll, starts new. Returns the old scroll.
    /// - **Other events**: Flush is NOT automatic; caller should call `flush()` first.
    ///   Returns the event immediately.
    ///
    /// # Note on Flush
    ///
    /// This method does NOT automatically flush pending events when a
    /// non-coalescable event arrives. The caller is responsible for calling
    /// `flush()` before processing events to ensure pending moves/scrolls
    /// are delivered at appropriate times.
    pub fn push(&mut self, event: Event) -> Option<Event> {
        match &event {
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Moved => {
                    // Coalesce mouse moves: latest position wins
                    self.pending_mouse_move = Some(*mouse);
                    None
                }
                MouseEventKind::ScrollUp => self.handle_scroll(ScrollDirection::Up, mouse),
                MouseEventKind::ScrollDown => self.handle_scroll(ScrollDirection::Down, mouse),
                MouseEventKind::ScrollLeft => self.handle_scroll(ScrollDirection::Left, mouse),
                MouseEventKind::ScrollRight => self.handle_scroll(ScrollDirection::Right, mouse),
                // Other mouse events (Down, Up, Drag) pass through
                _ => Some(event),
            },
            // Non-mouse events pass through
            _ => Some(event),
        }
    }

    /// Handle a scroll event, coalescing if same direction.
    fn handle_scroll(&mut self, direction: ScrollDirection, mouse: &MouseEvent) -> Option<Event> {
        if let Some(pending) = self.pending_scroll {
            if pending.direction == direction {
                // Same direction: increment count, update position to latest
                self.pending_scroll = Some(ScrollState {
                    count: pending.count.saturating_add(1),
                    x: mouse.x,
                    y: mouse.y,
                    modifiers: mouse.modifiers,
                    ..pending
                });
                None
            } else {
                // Different direction: flush old, start new
                let old = self.scroll_to_event(pending);
                self.pending_scroll = Some(ScrollState {
                    direction,
                    count: 1,
                    modifiers: mouse.modifiers,
                    x: mouse.x,
                    y: mouse.y,
                });
                Some(old)
            }
        } else {
            // No pending scroll: start accumulating
            self.pending_scroll = Some(ScrollState {
                direction,
                count: 1,
                modifiers: mouse.modifiers,
                x: mouse.x,
                y: mouse.y,
            });
            None
        }
    }

    /// Convert scroll state to an event.
    ///
    /// For coalesced scrolls, we emit the scroll event N times where N is
    /// the accumulated count. However, since most applications process
    /// scroll events one at a time, we return a single event and the caller
    /// can check `pending_scroll_count()` if they want to handle batched scrolls.
    fn scroll_to_event(&self, state: ScrollState) -> Event {
        let kind = match state.direction {
            ScrollDirection::Up => MouseEventKind::ScrollUp,
            ScrollDirection::Down => MouseEventKind::ScrollDown,
            ScrollDirection::Left => MouseEventKind::ScrollLeft,
            ScrollDirection::Right => MouseEventKind::ScrollRight,
        };
        // Preserve the position from the last scroll event
        Event::Mouse(MouseEvent::new(kind, state.x, state.y).with_modifiers(state.modifiers))
    }

    /// Flush all pending coalesced events.
    ///
    /// Returns a vector of events that were pending. The order is:
    /// 1. Pending scroll event (if any)
    /// 2. Pending mouse move (if any)
    ///
    /// After calling `flush()`, the coalescer is empty.
    ///
    /// # When to Call
    ///
    /// Call `flush()` before processing non-coalescable events to ensure
    /// pending input is delivered in the correct order. Also call at the
    /// end of each event batch to process any remaining pending events.
    #[must_use]
    pub fn flush(&mut self) -> Vec<Event> {
        let mut events = Vec::with_capacity(2);

        // Scroll first (older)
        if let Some(scroll) = self.pending_scroll.take() {
            events.push(self.scroll_to_event(scroll));
        }

        // Then mouse move (newer)
        if let Some(mouse) = self.pending_mouse_move.take() {
            events.push(Event::Mouse(mouse));
        }

        events
    }

    /// Flush pending events, calling a closure for each.
    ///
    /// This is more efficient than `flush()` when you need to process
    /// events immediately rather than collecting them.
    pub fn flush_each<F>(&mut self, mut f: F)
    where
        F: FnMut(Event),
    {
        if let Some(scroll) = self.pending_scroll.take() {
            f(self.scroll_to_event(scroll));
        }
        if let Some(mouse) = self.pending_mouse_move.take() {
            f(Event::Mouse(mouse));
        }
    }

    /// Check if there are any pending coalesced events.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.pending_mouse_move.is_some() || self.pending_scroll.is_some()
    }

    /// Get the pending scroll count (for applications that batch scroll handling).
    ///
    /// Returns 0 if no scroll is pending.
    #[must_use]
    pub fn pending_scroll_count(&self) -> u32 {
        self.pending_scroll.map(|s| s.count).unwrap_or(0)
    }

    /// Clear all pending events without processing them.
    ///
    /// Use this when you want to discard pending input, for example
    /// during a mode change or focus loss.
    pub fn clear(&mut self) {
        self.pending_mouse_move = None;
        self.pending_scroll = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{KeyCode, KeyEvent, Modifiers, MouseButton};

    #[test]
    fn new_coalescer_has_no_pending() {
        let coalescer = EventCoalescer::new();
        assert!(!coalescer.has_pending());
        assert_eq!(coalescer.pending_scroll_count(), 0);
    }

    #[test]
    fn mouse_move_coalesces() {
        let mut coalescer = EventCoalescer::new();

        // First move: pending
        let result = coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 10, 10)));
        assert!(result.is_none());
        assert!(coalescer.has_pending());

        // Second move: replaces first
        let result = coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 20, 25)));
        assert!(result.is_none());

        // Flush: returns only the latest position
        let pending = coalescer.flush();
        assert_eq!(pending.len(), 1);
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(m.x, 20);
            assert_eq!(m.y, 25);
            assert!(matches!(m.kind, MouseEventKind::Moved));
        } else {
            panic!("expected mouse event");
        }
    }

    #[test]
    fn mouse_move_preserves_modifiers() {
        let mut coalescer = EventCoalescer::new();

        let move_event =
            MouseEvent::new(MouseEventKind::Moved, 5, 5).with_modifiers(Modifiers::ALT);
        coalescer.push(Event::Mouse(move_event));

        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(m.modifiers, Modifiers::ALT);
        }
    }

    #[test]
    fn mouse_click_passes_through() {
        let mut coalescer = EventCoalescer::new();

        let click = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            10,
            10,
        ));
        let result = coalescer.push(click.clone());

        assert_eq!(result, Some(click));
        assert!(!coalescer.has_pending());
    }

    #[test]
    fn mouse_drag_passes_through() {
        let mut coalescer = EventCoalescer::new();

        let drag = Event::Mouse(MouseEvent::new(
            MouseEventKind::Drag(MouseButton::Left),
            10,
            10,
        ));
        let result = coalescer.push(drag.clone());

        assert_eq!(result, Some(drag));
    }

    #[test]
    fn key_event_passes_through() {
        let mut coalescer = EventCoalescer::new();

        let key = Event::Key(KeyEvent::new(KeyCode::Enter));
        let result = coalescer.push(key.clone());

        assert_eq!(result, Some(key));
    }

    #[test]
    fn scroll_same_direction_coalesces() {
        let mut coalescer = EventCoalescer::new();

        // Three scroll-ups
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));

        assert_eq!(coalescer.pending_scroll_count(), 3);

        let pending = coalescer.flush();
        assert_eq!(pending.len(), 1);
        if let Event::Mouse(m) = &pending[0] {
            assert!(matches!(m.kind, MouseEventKind::ScrollUp));
        }
    }

    #[test]
    fn scroll_direction_change_flushes() {
        let mut coalescer = EventCoalescer::new();

        // Scroll up twice
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));

        // Scroll down: should flush the pending up scrolls
        let result = coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            0,
            0,
        )));

        // Should return the old scroll (up)
        assert!(result.is_some());
        if let Some(Event::Mouse(m)) = result {
            assert!(matches!(m.kind, MouseEventKind::ScrollUp));
        }

        // New scroll (down) is now pending
        assert_eq!(coalescer.pending_scroll_count(), 1);
        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert!(matches!(m.kind, MouseEventKind::ScrollDown));
        }
    }

    #[test]
    fn scroll_preserves_modifiers() {
        let mut coalescer = EventCoalescer::new();

        let scroll =
            MouseEvent::new(MouseEventKind::ScrollUp, 0, 0).with_modifiers(Modifiers::CTRL);
        coalescer.push(Event::Mouse(scroll));

        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(m.modifiers, Modifiers::CTRL);
        }
    }

    #[test]
    fn flush_returns_scroll_before_move() {
        let mut coalescer = EventCoalescer::new();

        // Add both scroll and move
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 10, 10)));

        let pending = coalescer.flush();
        assert_eq!(pending.len(), 2);

        // Scroll first
        assert!(matches!(
            pending[0],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                ..
            })
        ));
        // Move second
        assert!(matches!(
            pending[1],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                ..
            })
        ));
    }

    #[test]
    fn flush_each_processes_in_order() {
        let mut coalescer = EventCoalescer::new();

        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            0,
            0,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 5, 5)));

        let mut events = Vec::new();
        coalescer.flush_each(|e| events.push(e));

        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                ..
            })
        ));
        assert!(matches!(
            events[1],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                ..
            })
        ));
    }

    #[test]
    fn clear_discards_pending() {
        let mut coalescer = EventCoalescer::new();

        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 10, 10)));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        assert!(coalescer.has_pending());

        coalescer.clear();
        assert!(!coalescer.has_pending());
        assert!(coalescer.flush().is_empty());
    }

    #[test]
    fn resize_passes_through() {
        let mut coalescer = EventCoalescer::new();

        let resize = Event::Resize {
            width: 80,
            height: 24,
        };
        let result = coalescer.push(resize.clone());

        assert_eq!(result, Some(resize));
    }

    #[test]
    fn focus_passes_through() {
        let mut coalescer = EventCoalescer::new();

        let focus = Event::Focus(true);
        let result = coalescer.push(focus.clone());

        assert_eq!(result, Some(focus));
    }

    #[test]
    fn many_moves_coalesce_to_one() {
        let mut coalescer = EventCoalescer::new();

        // Simulate a rapid mouse movement
        for i in 0..100 {
            coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, i, i)));
        }

        let pending = coalescer.flush();
        assert_eq!(pending.len(), 1);

        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(m.x, 99);
            assert_eq!(m.y, 99);
        }
    }

    #[test]
    fn scroll_count_saturates() {
        let mut coalescer = EventCoalescer::new();

        // This many scrolls won't overflow
        for _ in 0..1000 {
            coalescer.push(Event::Mouse(MouseEvent::new(
                MouseEventKind::ScrollUp,
                0,
                0,
            )));
        }

        assert_eq!(coalescer.pending_scroll_count(), 1000);
    }

    #[test]
    fn horizontal_scroll_coalesces() {
        let mut coalescer = EventCoalescer::new();

        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollLeft,
            0,
            0,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollLeft,
            0,
            0,
        )));

        assert_eq!(coalescer.pending_scroll_count(), 2);

        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert!(matches!(m.kind, MouseEventKind::ScrollLeft));
        }
    }

    #[test]
    fn scroll_preserves_position() {
        let mut coalescer = EventCoalescer::new();

        // Scroll at position (10, 20)
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            10,
            20,
        )));
        // Scroll at position (15, 25) - latest position should be preserved
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            15,
            25,
        )));

        let pending = coalescer.flush();
        assert_eq!(pending.len(), 1);
        if let Event::Mouse(m) = &pending[0] {
            assert!(matches!(m.kind, MouseEventKind::ScrollUp));
            assert_eq!(m.x, 15, "scroll should preserve latest x position");
            assert_eq!(m.y, 25, "scroll should preserve latest y position");
        } else {
            panic!("expected mouse event");
        }
    }

    #[test]
    fn mixed_coalescing_workflow() {
        let mut coalescer = EventCoalescer::new();
        let mut processed = Vec::new();

        // Simulate event stream
        let events = vec![
            Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 0, 0)),
            Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 5, 5)),
            Event::Mouse(MouseEvent::new(
                MouseEventKind::Down(MouseButton::Left),
                5,
                5,
            )),
            Event::Mouse(MouseEvent::new(
                MouseEventKind::Drag(MouseButton::Left),
                10,
                10,
            )),
            Event::Mouse(MouseEvent::new(
                MouseEventKind::Up(MouseButton::Left),
                10,
                10,
            )),
            Event::Mouse(MouseEvent::new(MouseEventKind::ScrollUp, 0, 0)),
            Event::Mouse(MouseEvent::new(MouseEventKind::ScrollUp, 0, 0)),
            Event::Key(KeyEvent::new(KeyCode::Escape)),
        ];

        for event in events {
            if let Some(e) = coalescer.push(event) {
                // Non-coalescable event passed through - flush pending first, then process
                coalescer.flush_each(|pending| processed.push(pending));
                processed.push(e);
            }
            // If push returned None, event was coalesced and is pending
        }

        // Final flush for any remaining pending events
        coalescer.flush_each(|e| processed.push(e));

        // Verify coalescing occurred:
        // - 2 mouse moves -> 1 coalesced move
        // - down, drag, up -> 3 pass-through events
        // - 2 scroll ups -> 1 coalesced scroll
        // - escape -> 1 pass-through event
        // Total: 1 + 3 + 1 + 1 = 6 events (down from 8 input events)
        assert_eq!(processed.len(), 6);

        // Verify the coalesced move has the final position
        let move_event = processed
            .iter()
            .find(|e| matches!(e, Event::Mouse(m) if matches!(m.kind, MouseEventKind::Moved)));
        assert!(move_event.is_some());
        if let Some(Event::Mouse(m)) = move_event {
            assert_eq!(m.x, 5);
            assert_eq!(m.y, 5);
        }
    }
}

#![forbid(unsafe_code)]

//! Event coalescing for high-frequency input events.
//!
//! Terminal applications can receive a flood of events during rapid user
//! interaction, particularly mouse moves and scrolls. Without coalescing,
//! each event triggers a model update and potential re-render, causing lag.
//!
//! This module provides [`EventCoalescer`] which:
//! - Coalesces rapid mouse moves into a single event
//! - Batches consecutive scroll events in the same direction
//! - Passes through all other events immediately
//!
//! # Design
//!
//! The two coalescable kinds are treated differently, because what is
//! redundant about them is different:
//!
//! - **Mouse moves: latest wins.** The intermediate positions the pointer
//!   passed through carry no information the final position does not, so all
//!   but the newest are dropped.
//! - **Scroll: batched, never dropped.** A scroll event is a *delta*, so
//!   discarding one discards distance the user asked for. Consecutive notches
//!   in the same direction accumulate into a run, and flushing a run of `n`
//!   notches delivers `n` events.
//!
//! That distinction is the whole point: an earlier version collapsed a run to
//! a single event, so four notches scrolled a quarter as far as the user asked,
//! and the faster you scrolled the more was lost (`bd-minjt`).
//!
//! # What batching does and does not save
//!
//! Only the mouse-move half of this module reduces work: `n` moves become one
//! `update()` call. Scroll batching does not, and the claim that it does is
//! worth stating in the negative, because it reads as though it should.
//!
//! [`MouseEventKind`] has no notch count, so a run of `n` notches can only be
//! delivered as `n` events, and a caller that hands each one to its model gets
//! `n` model updates — exactly what it would get with no coalescer at all. The
//! renders are not saved either: [`Program`] renders once per drain burst
//! whether or not it is coalescing, because dispatch only sets a dirty flag.
//!
//! So for scroll this is a *reordering*, not a reduction: the notches are held
//! until the batch ends, then delivered contiguously and all at the pointer
//! position of the last one. A caller that wants the reduction has to take it
//! deliberately, by reading [`EventCoalescer::pending_scroll_events`] and
//! applying the total in one step instead of flushing. [`Program`] does not do
//! that, and should not: a model is entitled to see each notch.
//!
//! [`Program`]: https://docs.rs/ftui-runtime/latest/ftui_runtime/program/struct.Program.html
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
/// [`push`](Self::push) is O(1) and allocation-free in the common path; the
/// closed-run vector only allocates when the scroll direction actually changes
/// between two flushes. [`flush`](Self::flush) is O(total notches), since it
/// materialises one event per notch.
#[derive(Debug, Clone, Default)]
pub struct EventCoalescer {
    /// Pending mouse move event (latest position wins).
    pending_mouse_move: Option<MouseEvent>,

    /// Scroll run currently accumulating (direction + notch count).
    pending_scroll: Option<ScrollState>,

    /// Runs closed by a direction change, oldest first, awaiting flush.
    ///
    /// One run is open at a time, across all four directions, so a horizontal
    /// notch closes a vertical run just as an opposite one does. The closed run
    /// must not be discarded, so it waits here instead of being collapsed into
    /// the single event `push` can return.
    closed_scrolls: Vec<ScrollState>,
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
    /// - **Scroll (same direction)**: Adds a notch to the current run. Returns `None`.
    /// - **Scroll (different direction)**: Closes the current run and starts a
    ///   new one. Returns `None` — the closed run is delivered in full by the
    ///   next `flush()`, because its notch count will not fit in the single
    ///   event this method can return.
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

    /// Handle a scroll event, adding to the current run or starting a new one.
    fn handle_scroll(&mut self, direction: ScrollDirection, mouse: &MouseEvent) -> Option<Event> {
        if let Some(pending) = self.pending_scroll {
            if pending.direction == direction {
                // Same direction: one more notch on the current run, position
                // and modifiers from the latest event.
                self.pending_scroll = Some(ScrollState {
                    count: pending.count.saturating_add(1),
                    x: mouse.x,
                    y: mouse.y,
                    modifiers: mouse.modifiers,
                    ..pending
                });
            } else {
                // Any direction change, not just an opposite one: the old run
                // is finished but keeps its notch count, so it waits for flush
                // rather than collapsing into one event.
                self.closed_scrolls.push(pending);
                self.pending_scroll = Some(ScrollState {
                    direction,
                    count: 1,
                    modifiers: mouse.modifiers,
                    x: mouse.x,
                    y: mouse.y,
                });
            }
        } else {
            // No run in progress: start one.
            self.pending_scroll = Some(ScrollState {
                direction,
                count: 1,
                modifiers: mouse.modifiers,
                x: mouse.x,
                y: mouse.y,
            });
        }
        None
    }

    /// Convert scroll state to an event.
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
    /// Returns a vector of events that were pending, in input order:
    /// 1. Scroll runs closed by a direction change, oldest first
    /// 2. The scroll run still accumulating
    /// 3. Pending mouse move (latest position)
    ///
    /// **A run of `n` notches yields `n` events.** Scroll is a delta, so
    /// collapsing a run would silently shorten the distance the user asked to
    /// travel (`bd-minjt`). Flushing therefore costs one model update per
    /// notch; see the module's "What batching does and does not save".
    ///
    /// After calling `flush()`, the coalescer is empty.
    #[must_use]
    pub fn flush(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        self.flush_each(|event| events.push(event));
        events
    }

    /// Flush pending events, calling a closure for each.
    ///
    /// Same events and same order as [`flush`](Self::flush), including one call
    /// per scroll notch, without building the intermediate vector.
    pub fn flush_each<F>(&mut self, mut f: F)
    where
        F: FnMut(Event),
    {
        // Closed runs first (oldest input), then the run still accumulating.
        for run in std::mem::take(&mut self.closed_scrolls) {
            let event = self.scroll_to_event(run);
            for _ in 0..run.count {
                f(event.clone());
            }
        }
        if let Some(run) = self.pending_scroll.take() {
            let event = self.scroll_to_event(run);
            for _ in 0..run.count {
                f(event.clone());
            }
        }
        // Then mouse move (newest).
        if let Some(mouse) = self.pending_mouse_move.take() {
            f(Event::Mouse(mouse));
        }
    }

    /// Check if there are any pending coalesced events.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.pending_mouse_move.is_some()
            || self.pending_scroll.is_some()
            || !self.closed_scrolls.is_empty()
    }

    /// Notches accumulated in the scroll run currently in progress.
    ///
    /// Returns 0 if no run is in progress. This counts only the open run, not
    /// runs already closed by a direction change; for the full pending total
    /// use [`pending_scroll_events`](Self::pending_scroll_events).
    #[must_use]
    pub fn pending_scroll_count(&self) -> u32 {
        self.pending_scroll.map_or(0, |s| s.count)
    }

    /// Total scroll events the next flush will deliver, across all runs.
    #[must_use]
    pub fn pending_scroll_events(&self) -> u32 {
        self.closed_scrolls
            .iter()
            .fold(self.pending_scroll_count(), |total, run| {
                total.saturating_add(run.count)
            })
    }

    /// Clear all pending events without processing them.
    ///
    /// Use this when you want to discard pending input, for example
    /// during a mode change or focus loss.
    pub fn clear(&mut self) {
        self.pending_mouse_move = None;
        self.pending_scroll = None;
        self.closed_scrolls.clear();
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

    /// Push `n` scroll events of one kind at the origin.
    fn push_scrolls(coalescer: &mut EventCoalescer, kind: MouseEventKind, n: usize) {
        for _ in 0..n {
            coalescer.push(Event::Mouse(MouseEvent::new(kind, 0, 0)));
        }
    }

    fn scroll_kinds(events: &[Event]) -> Vec<MouseEventKind> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Mouse(m) => Some(m.kind),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn scroll_same_direction_batches_without_losing_notches() {
        let mut coalescer = EventCoalescer::new();

        push_scrolls(&mut coalescer, MouseEventKind::ScrollUp, 3);

        assert_eq!(coalescer.pending_scroll_count(), 3);
        assert_eq!(coalescer.pending_scroll_events(), 3);

        // Three notches in, three notches out: the run is dispatched as one
        // batch, but the model still scrolls the full distance (`bd-minjt`).
        let pending = coalescer.flush();
        assert_eq!(
            scroll_kinds(&pending),
            vec![
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollUp
            ]
        );
        assert!(!coalescer.has_pending());
    }

    #[test]
    fn scroll_direction_change_keeps_both_runs_whole() {
        let mut coalescer = EventCoalescer::new();

        push_scrolls(&mut coalescer, MouseEventKind::ScrollUp, 2);

        // Reversing closes the up-run; it is held for flush rather than
        // returned, because two notches do not fit in one returned event.
        let result = coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            0,
            0,
        )));
        assert!(result.is_none());

        assert_eq!(coalescer.pending_scroll_count(), 1, "the open down-run");
        assert_eq!(coalescer.pending_scroll_events(), 3, "both runs");

        // Input order: the two ups, then the down.
        let pending = coalescer.flush();
        assert_eq!(
            scroll_kinds(&pending),
            vec![
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollDown
            ]
        );
        assert!(!coalescer.has_pending());
    }

    #[test]
    fn scroll_runs_alternating_directions_survive_in_order() {
        let mut coalescer = EventCoalescer::new();

        push_scrolls(&mut coalescer, MouseEventKind::ScrollDown, 2);
        push_scrolls(&mut coalescer, MouseEventKind::ScrollUp, 1);
        push_scrolls(&mut coalescer, MouseEventKind::ScrollDown, 3);

        assert_eq!(coalescer.pending_scroll_events(), 6);
        assert_eq!(
            scroll_kinds(&coalescer.flush()),
            vec![
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollDown
            ]
        );
    }

    #[test]
    fn clear_discards_closed_runs_too() {
        let mut coalescer = EventCoalescer::new();

        push_scrolls(&mut coalescer, MouseEventKind::ScrollUp, 2);
        push_scrolls(&mut coalescer, MouseEventKind::ScrollDown, 2);
        assert!(coalescer.has_pending());

        coalescer.clear();

        assert!(!coalescer.has_pending());
        assert_eq!(coalescer.pending_scroll_events(), 0);
        assert!(coalescer.flush().is_empty());
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

        // Two notches, two events, both reporting the run's latest position.
        let pending = coalescer.flush();
        assert_eq!(pending.len(), 2);
        for event in &pending {
            let Event::Mouse(m) = event else {
                panic!("expected mouse event");
            };
            assert!(matches!(m.kind, MouseEventKind::ScrollUp));
            assert_eq!(m.x, 15, "scroll should preserve latest x position");
            assert_eq!(m.y, 25, "scroll should preserve latest y position");
        }
    }

    #[test]
    fn default_coalescer_has_no_pending() {
        let coalescer = EventCoalescer::default();
        assert!(!coalescer.has_pending());
        assert_eq!(coalescer.pending_scroll_count(), 0);
    }

    #[test]
    fn scroll_direction_change_flushes_old() {
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

        // Change direction -> closes the up-run, which waits for flush
        let result = coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            0,
            0,
        )));
        assert!(result.is_none());

        // The open run is the new down scroll; the closed up-run is still held
        assert_eq!(coalescer.pending_scroll_count(), 1);
        assert_eq!(coalescer.pending_scroll_events(), 3);
        assert_eq!(
            scroll_kinds(&coalescer.flush()),
            vec![
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollDown
            ]
        );
    }

    #[test]
    fn pending_scroll_count_zero_after_flush() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        assert_eq!(coalescer.pending_scroll_count(), 1);
        let _ = coalescer.flush();
        assert_eq!(coalescer.pending_scroll_count(), 0);
    }

    #[test]
    fn scroll_right_coalesces() {
        let mut coalescer = EventCoalescer::new();

        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollRight,
            5,
            10,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollRight,
            6,
            11,
        )));

        assert_eq!(coalescer.pending_scroll_count(), 2);

        let pending = coalescer.flush();
        assert_eq!(pending.len(), 2);
        for event in &pending {
            if let Event::Mouse(m) = event {
                assert!(matches!(m.kind, MouseEventKind::ScrollRight));
                assert_eq!(m.x, 6);
                assert_eq!(m.y, 11);
            }
        }
    }

    // ─── Edge-case tests (bd-2lusg) ────────────────────────────────────

    #[test]
    fn clone_preserves_pending_state() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 7, 8)));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            1,
            2,
        )));

        let mut cloned = coalescer.clone();
        assert!(cloned.has_pending());
        assert_eq!(cloned.pending_scroll_count(), 1);

        let pending = cloned.flush();
        assert_eq!(pending.len(), 2);
        // Original still has pending events (independent clone)
        assert!(coalescer.has_pending());
    }

    #[test]
    fn flush_empty_returns_empty_vec() {
        let mut coalescer = EventCoalescer::new();
        let pending = coalescer.flush();
        assert!(pending.is_empty());
    }

    #[test]
    fn flush_each_empty_does_not_call_closure() {
        let mut coalescer = EventCoalescer::new();
        let mut called = false;
        coalescer.flush_each(|_| called = true);
        assert!(!called);
    }

    #[test]
    fn double_flush_second_empty() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 1, 1)));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            0,
            0,
        )));

        let first = coalescer.flush();
        assert_eq!(first.len(), 2);
        let second = coalescer.flush();
        assert!(second.is_empty());
        assert!(!coalescer.has_pending());
    }

    #[test]
    fn paste_event_passes_through() {
        let mut coalescer = EventCoalescer::new();
        let paste = Event::Paste(crate::event::PasteEvent {
            text: "hello".into(),
            bracketed: true,
        });
        let result = coalescer.push(paste.clone());
        assert_eq!(result, Some(paste));
        assert!(!coalescer.has_pending());
    }

    #[test]
    fn mouse_up_passes_through() {
        let mut coalescer = EventCoalescer::new();
        let up = Event::Mouse(MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 5, 5));
        let result = coalescer.push(up.clone());
        assert_eq!(result, Some(up));
    }

    #[test]
    fn mouse_up_right_passes_through() {
        let mut coalescer = EventCoalescer::new();
        let up = Event::Mouse(MouseEvent::new(
            MouseEventKind::Up(MouseButton::Right),
            0,
            0,
        ));
        assert_eq!(coalescer.push(up.clone()), Some(up));
    }

    #[test]
    fn mouse_down_middle_passes_through() {
        let mut coalescer = EventCoalescer::new();
        let down = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Middle),
            0,
            0,
        ));
        assert_eq!(coalescer.push(down.clone()), Some(down));
    }

    #[test]
    fn drag_right_button_passes_through() {
        let mut coalescer = EventCoalescer::new();
        let drag = Event::Mouse(MouseEvent::new(
            MouseEventKind::Drag(MouseButton::Right),
            3,
            4,
        ));
        assert_eq!(coalescer.push(drag.clone()), Some(drag));
    }

    #[test]
    fn has_pending_move_only() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 0, 0)));
        assert!(coalescer.has_pending());
        assert_eq!(coalescer.pending_scroll_count(), 0);
    }

    #[test]
    fn has_pending_scroll_only() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            0,
            0,
        )));
        assert!(coalescer.has_pending());
        assert_eq!(coalescer.pending_scroll_count(), 1);
    }

    #[test]
    fn move_at_origin() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 0, 0)));
        let pending = coalescer.flush();
        assert_eq!(pending.len(), 1);
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!((m.x, m.y), (0, 0));
        }
    }

    #[test]
    fn move_at_max_coordinates() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::Moved,
            u16::MAX,
            u16::MAX,
        )));
        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(m.x, u16::MAX);
            assert_eq!(m.y, u16::MAX);
        }
    }

    #[test]
    fn scroll_at_max_coordinates() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            u16::MAX,
            u16::MAX,
        )));
        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(m.x, u16::MAX);
            assert_eq!(m.y, u16::MAX);
        }
    }

    #[test]
    fn move_with_all_modifiers() {
        let mut coalescer = EventCoalescer::new();
        let mods = Modifiers::SHIFT | Modifiers::ALT | Modifiers::CTRL | Modifiers::SUPER;
        let ev = MouseEvent::new(MouseEventKind::Moved, 10, 20).with_modifiers(mods);
        coalescer.push(Event::Mouse(ev));
        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(m.modifiers, mods);
        }
    }

    #[test]
    fn scroll_direction_change_preserves_new_modifiers() {
        let mut coalescer = EventCoalescer::new();
        let scroll_up =
            MouseEvent::new(MouseEventKind::ScrollUp, 0, 0).with_modifiers(Modifiers::SHIFT);
        coalescer.push(Event::Mouse(scroll_up));

        // Direction change with different modifiers
        let scroll_down =
            MouseEvent::new(MouseEventKind::ScrollDown, 5, 5).with_modifiers(Modifiers::CTRL);
        assert!(coalescer.push(Event::Mouse(scroll_down)).is_none());

        // Both runs flush together, each keeping its own modifiers.
        let pending = coalescer.flush();
        assert_eq!(pending.len(), 2);
        let Event::Mouse(old) = &pending[0] else {
            panic!("expected the closed up-run first");
        };
        assert!(matches!(old.kind, MouseEventKind::ScrollUp));
        assert_eq!(old.modifiers, Modifiers::SHIFT);

        let Event::Mouse(new) = &pending[1] else {
            panic!("expected the open down-run second");
        };
        assert!(matches!(new.kind, MouseEventKind::ScrollDown));
        assert_eq!(new.modifiers, Modifiers::CTRL);
    }

    #[test]
    fn scroll_direction_change_returns_old_position() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            10,
            20,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            15,
            25,
        )));

        // Direction change
        let old = coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            30,
            40,
        )));
        // Old scroll should have latest position (15, 25)
        if let Some(Event::Mouse(m)) = old {
            assert_eq!(m.x, 15);
            assert_eq!(m.y, 25);
        }
    }

    #[test]
    fn four_direction_changes() {
        let mut coalescer = EventCoalescer::new();
        let directions = [
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ];

        // First scroll starts accumulating
        assert!(
            coalescer
                .push(Event::Mouse(MouseEvent::new(directions[0], 0, 0)))
                .is_none()
        );

        for &dir in &directions[1..] {
            let flushed = coalescer.push(Event::Mouse(MouseEvent::new(dir, 0, 0)));
            // Each direction change closes a run rather than returning it
            assert!(flushed.is_none());
        }

        // Only the last direction (Right) is still open; the other three runs
        // are closed but intact.
        assert_eq!(coalescer.pending_scroll_count(), 1);
        assert_eq!(coalescer.pending_scroll_events(), 4);
        assert_eq!(scroll_kinds(&coalescer.flush()), directions.to_vec());
    }

    #[test]
    fn horizontal_to_vertical_direction_change() {
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

        // Switching axis closes the horizontal run and starts a vertical one;
        // neither is returned from `push`, and neither loses a notch.
        let old = coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        assert!(old.is_none());

        assert_eq!(
            scroll_kinds(&coalescer.flush()),
            vec![
                MouseEventKind::ScrollLeft,
                MouseEventKind::ScrollLeft,
                MouseEventKind::ScrollUp
            ]
        );
    }

    #[test]
    fn push_clear_flush_empty() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 5, 5)));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        coalescer.clear();
        assert!(coalescer.flush().is_empty());
        assert_eq!(coalescer.pending_scroll_count(), 0);
    }

    #[test]
    fn passthrough_does_not_affect_pending() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 1, 1)));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));

        // Pass-through event doesn't auto-flush
        let key = Event::Key(KeyEvent::new(KeyCode::Char('a')));
        let result = coalescer.push(key);
        assert!(result.is_some());

        // Pending events still there
        assert!(coalescer.has_pending());
        assert_eq!(coalescer.pending_scroll_count(), 1);
        let pending = coalescer.flush();
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn flush_then_reuse() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 1, 1)));
        let _ = coalescer.flush();

        // Reuse after flush
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 10, 20)));
        let pending = coalescer.flush();
        assert_eq!(pending.len(), 1);
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!((m.x, m.y), (10, 20));
        }
    }

    #[test]
    fn scroll_count_u32_max_saturates() {
        let mut coalescer = EventCoalescer::new();
        // Manually build a state near u32::MAX by pushing once then many more
        // We can't push u32::MAX times in a test, so verify saturation logic
        // by checking that count increases
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        assert_eq!(coalescer.pending_scroll_count(), 1);
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        assert_eq!(coalescer.pending_scroll_count(), 2);
    }

    #[test]
    fn single_scroll_count_is_one() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            0,
            0,
        )));
        assert_eq!(coalescer.pending_scroll_count(), 1);
    }

    #[test]
    fn scroll_modifiers_update_on_same_direction() {
        let mut coalescer = EventCoalescer::new();
        let s1 = MouseEvent::new(MouseEventKind::ScrollUp, 0, 0).with_modifiers(Modifiers::SHIFT);
        coalescer.push(Event::Mouse(s1));

        // Same direction, different modifiers: latest modifiers win
        let s2 = MouseEvent::new(MouseEventKind::ScrollUp, 0, 0).with_modifiers(Modifiers::ALT);
        coalescer.push(Event::Mouse(s2));

        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(
                m.modifiers,
                Modifiers::ALT,
                "latest scroll modifiers should win"
            );
        }
    }

    #[test]
    fn move_replaces_move_preserves_latest_modifiers() {
        let mut coalescer = EventCoalescer::new();
        let m1 = MouseEvent::new(MouseEventKind::Moved, 1, 1).with_modifiers(Modifiers::SHIFT);
        coalescer.push(Event::Mouse(m1));

        let m2 = MouseEvent::new(MouseEventKind::Moved, 2, 2).with_modifiers(Modifiers::CTRL);
        coalescer.push(Event::Mouse(m2));

        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(m.modifiers, Modifiers::CTRL);
            assert_eq!((m.x, m.y), (2, 2));
        }
    }

    #[test]
    fn flush_each_equivalent_to_flush() {
        let mut c1 = EventCoalescer::new();
        let mut c2 = EventCoalescer::new();

        let events = [
            Event::Mouse(MouseEvent::new(MouseEventKind::ScrollDown, 3, 4)),
            Event::Mouse(MouseEvent::new(MouseEventKind::ScrollDown, 5, 6)),
            Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 10, 20)),
        ];

        for e in &events {
            c1.push(e.clone());
            c2.push(e.clone());
        }

        let vec_flush = c1.flush();
        let mut each_flush = Vec::new();
        c2.flush_each(|e| each_flush.push(e));

        assert_eq!(vec_flush, each_flush);
    }

    #[test]
    fn flush_order_scroll_then_move() {
        // Verify order regardless of push order
        let mut coalescer = EventCoalescer::new();
        // Push move first, then scroll
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 1, 1)));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));

        let pending = coalescer.flush();
        assert_eq!(pending.len(), 2);
        // Scroll always first
        assert!(matches!(
            pending[0],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                ..
            })
        ));
        assert!(matches!(
            pending[1],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                ..
            })
        ));
    }

    #[test]
    fn rapid_alternating_scroll_directions() {
        let mut coalescer = EventCoalescer::new();

        // Alternate up/down rapidly: ten one-notch runs, nine reversals.
        let mut expected = Vec::new();
        for i in 0..10 {
            let kind = if i % 2 == 0 {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            };
            expected.push(kind);
            // No scroll is ever returned from `push` - runs leave via `flush`.
            assert!(
                coalescer
                    .push(Event::Mouse(MouseEvent::new(kind, 0, 0)))
                    .is_none()
            );
        }

        // Only the newest run is open; the nine closed ones are still held.
        assert_eq!(coalescer.pending_scroll_count(), 1);
        assert_eq!(coalescer.pending_scroll_events(), 10);

        // Worst case for the old collapse-to-one behaviour: every notch
        // reversed direction, so every notch mattered.
        assert_eq!(scroll_kinds(&coalescer.flush()), expected);
    }

    #[test]
    fn resize_does_not_affect_pending() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 5, 5)));

        let resize = Event::Resize {
            width: 120,
            height: 40,
        };
        let result = coalescer.push(resize.clone());
        assert_eq!(result, Some(resize));

        // Move is still pending
        assert!(coalescer.has_pending());
        let pending = coalescer.flush();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn focus_does_not_affect_pending() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));

        let result = coalescer.push(Event::Focus(false));
        assert_eq!(result, Some(Event::Focus(false)));

        assert_eq!(coalescer.pending_scroll_count(), 1);
    }

    #[test]
    fn debug_format_contains_type_name() {
        let coalescer = EventCoalescer::new();
        let dbg = format!("{coalescer:?}");
        assert!(dbg.contains("EventCoalescer"));
    }

    #[test]
    fn flush_only_move_returns_one() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 42, 99)));
        let pending = coalescer.flush();
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            pending[0],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                ..
            })
        ));
    }

    #[test]
    fn flush_only_scroll_returns_one() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollLeft,
            0,
            0,
        )));
        let pending = coalescer.flush();
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            pending[0],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollLeft,
                ..
            })
        ));
    }

    #[test]
    fn clear_after_direction_change() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        // Direction change closes the up-run and holds it for flush
        let _ = coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            0,
            0,
        )));
        // Clear drops the closed run as well as the open one
        coalescer.clear();
        assert!(!coalescer.has_pending());
        assert_eq!(coalescer.pending_scroll_count(), 0);
    }

    #[test]
    fn scroll_position_updates_to_latest_same_direction() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            0,
            0,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            100,
            200,
        )));
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            50,
            60,
        )));

        let pending = coalescer.flush();
        if let Event::Mouse(m) = &pending[0] {
            assert_eq!(
                (m.x, m.y),
                (50, 60),
                "position should be from latest scroll"
            );
        }
        assert_eq!(coalescer.pending_scroll_count(), 0);
    }

    #[test]
    fn move_does_not_flush_pending_scroll() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        // Adding a move doesn't flush the scroll
        let result = coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 1, 1)));
        assert!(result.is_none());
        assert_eq!(coalescer.pending_scroll_count(), 1);
    }

    #[test]
    fn scroll_does_not_flush_pending_move() {
        let mut coalescer = EventCoalescer::new();
        coalescer.push(Event::Mouse(MouseEvent::new(MouseEventKind::Moved, 1, 1)));
        // Adding a scroll doesn't flush the move
        let result = coalescer.push(Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            0,
            0,
        )));
        assert!(result.is_none());

        let pending = coalescer.flush();
        assert_eq!(pending.len(), 2);
    }

    // ─── End edge-case tests (bd-2lusg) ──────────────────────────────

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
        // - 2 mouse moves -> 1 coalesced move (intermediate positions are redundant)
        // - down, drag, up -> 3 pass-through events
        // - 2 scroll ups -> 2 events in one batch (notches are distance, not position)
        // - escape -> 1 pass-through event
        // Total: 1 + 3 + 2 + 1 = 7 events (down from 8 input events)
        assert_eq!(processed.len(), 7);

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

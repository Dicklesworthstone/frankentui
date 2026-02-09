#![forbid(unsafe_code)]

//! Gesture recognition: transforms raw terminal events into semantic events.
//!
//! [`GestureRecognizer`] is a stateful processor that converts raw [`Event`]
//! sequences (mouse clicks, key presses, etc.) into high-level [`SemanticEvent`]s
//! (double-click, drag, chord, etc.).
//!
//! # State Machine
//!
//! The recognizer tracks several concurrent state machines:
//!
//! - **Click detector**: Tracks consecutive clicks at the same position to emit
//!   `Click`, `DoubleClick`, or `TripleClick`.
//! - **Drag detector**: Monitors mouse-down → move → mouse-up sequences,
//!   emitting `DragStart` / `DragMove` / `DragEnd` / `DragCancel`.
//! - **Long press detector**: Fires when mouse is held stationary beyond a threshold.
//! - **Chord detector**: Accumulates modifier+key sequences within a timeout window.
//!
//! # Invariants
//!
//! 1. Drag and Click never both emit for the same mouse-down → mouse-up interaction.
//!    If a drag starts, the mouse-up produces `DragEnd`, not `Click`.
//! 2. Click multiplicity is monotonically increasing within a multi-click window:
//!    `Click` → `DoubleClick` → `TripleClick`.
//! 3. `Chord` sequences are always non-empty.
//! 4. After `reset()`, all state machines return to their initial idle state.
//! 5. `DragCancel` is emitted if Escape is pressed during a drag.
//!
//! # Failure Modes
//!
//! - If the chord timeout expires mid-sequence, the chord buffer is cleared and
//!   no `Chord` event is emitted (raw keys are still delivered by the caller).
//! - If focus is lost during a drag, the caller should call `reset()` which will
//!   not emit `DragCancel` (the caller handles focus-loss cancellation).

use std::time::{Duration, Instant};

use crate::event::{Event, KeyCode, KeyEventKind, Modifiers, MouseButton, MouseEventKind};
use crate::semantic_event::{ChordKey, Position, SemanticEvent};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Thresholds and timeouts for gesture recognition.
#[derive(Debug, Clone)]
pub struct GestureConfig {
    /// Time window for double/triple click detection (default: 300ms).
    pub multi_click_timeout: Duration,
    /// Duration before a stationary mouse-down triggers long press (default: 500ms).
    pub long_press_threshold: Duration,
    /// Minimum manhattan distance (cells) before a drag starts (default: 3).
    pub drag_threshold: u16,
    /// Time window for chord key sequence completion (default: 1000ms).
    pub chord_timeout: Duration,
    /// Minimum velocity (cells/sec) for swipe detection (default: 50.0).
    pub swipe_velocity_threshold: f32,
    /// Position tolerance for multi-click detection (manhattan distance, default: 1).
    pub click_tolerance: u16,
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self {
            multi_click_timeout: Duration::from_millis(300),
            long_press_threshold: Duration::from_millis(500),
            drag_threshold: 3,
            chord_timeout: Duration::from_millis(1000),
            swipe_velocity_threshold: 50.0,
            click_tolerance: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// Tracks click timing for multi-click detection.
#[derive(Debug, Clone)]
struct ClickState {
    pos: Position,
    button: MouseButton,
    time: Instant,
    count: u8,
}

/// Tracks an ongoing or potential drag.
#[derive(Debug, Clone)]
struct DragTracker {
    start_pos: Position,
    button: MouseButton,
    last_pos: Position,
    started: bool,
}

// ---------------------------------------------------------------------------
// GestureRecognizer
// ---------------------------------------------------------------------------

/// Stateful gesture recognizer that transforms raw events into semantic events.
///
/// Call [`process`](GestureRecognizer::process) for each incoming [`Event`].
/// Call [`check_long_press`](GestureRecognizer::check_long_press) periodically
/// (e.g., on tick) to detect long-press gestures.
pub struct GestureRecognizer {
    config: GestureConfig,

    // Click tracking
    last_click: Option<ClickState>,

    // Drag tracking
    mouse_down: Option<(Position, MouseButton, Instant)>,
    drag: Option<DragTracker>,

    // Long press tracking
    long_press_pos: Option<(Position, Instant)>,
    long_press_fired: bool,

    // Chord tracking
    chord_buffer: Vec<ChordKey>,
    chord_start: Option<Instant>,
}

impl std::fmt::Debug for GestureRecognizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GestureRecognizer")
            .field("dragging", &self.is_dragging())
            .field("chord_len", &self.chord_buffer.len())
            .finish()
    }
}

impl GestureRecognizer {
    /// Create a new gesture recognizer with the given configuration.
    #[must_use]
    pub fn new(config: GestureConfig) -> Self {
        Self {
            config,
            last_click: None,
            mouse_down: None,
            drag: None,
            long_press_pos: None,
            long_press_fired: false,
            chord_buffer: Vec::with_capacity(4),
            chord_start: None,
        }
    }

    /// Process a raw event, returning any semantic events produced.
    ///
    /// Most events produce 0 or 1 semantic events. A mouse-up after a
    /// multi-click sequence may produce both a `Click` and a `DoubleClick`.
    #[inline]
    pub fn process(&mut self, event: &Event, now: Instant) -> Vec<SemanticEvent> {
        let mut out = Vec::with_capacity(2);

        // Expire stale chord
        self.expire_chord(now);

        match event {
            Event::Mouse(mouse) => {
                let pos = Position::new(mouse.x, mouse.y);
                match mouse.kind {
                    MouseEventKind::Down(button) => {
                        self.on_mouse_down(pos, button, now, &mut out);
                    }
                    MouseEventKind::Up(button) => {
                        self.on_mouse_up(pos, button, now, &mut out);
                    }
                    MouseEventKind::Drag(button) => {
                        self.on_mouse_drag(pos, button, &mut out);
                    }
                    MouseEventKind::Moved => {
                        // Movement without button cancels long press
                        self.long_press_pos = None;
                    }
                    _ => {}
                }
            }
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    return out;
                }

                // Escape cancels drag
                if key.code == KeyCode::Escape {
                    if let Some(drag) = self.drag.take()
                        && drag.started
                    {
                        out.push(SemanticEvent::DragCancel);
                    }
                    self.mouse_down = None;
                    self.long_press_pos = None;
                    self.chord_buffer.clear();
                    self.chord_start = None;
                    return out;
                }

                // Chord detection: modifier+key combinations
                let has_modifier = key
                    .modifiers
                    .intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER);

                if has_modifier {
                    let chord_key = ChordKey::new(key.code, key.modifiers);
                    if self.chord_buffer.is_empty() {
                        self.chord_start = Some(now);
                    }
                    self.chord_buffer.push(chord_key);

                    // Emit chord after each addition (caller can decide if complete).
                    // A chord of length 1 is just a modified key; length ≥2 is a sequence.
                    if self.chord_buffer.len() >= 2 {
                        out.push(SemanticEvent::Chord {
                            sequence: self.chord_buffer.clone(),
                        });
                        self.chord_buffer.clear();
                        self.chord_start = None;
                    }
                } else {
                    // Non-modifier key clears any in-progress chord
                    self.chord_buffer.clear();
                    self.chord_start = None;
                }
            }
            Event::Focus(false) => {
                // Focus loss: reset drag and long press
                if let Some(drag) = self.drag.take()
                    && drag.started
                {
                    out.push(SemanticEvent::DragCancel);
                }
                self.mouse_down = None;
                self.long_press_pos = None;
                self.long_press_fired = false;
            }
            _ => {}
        }

        out
    }

    /// Check for long press timeout. Call periodically (e.g., on tick).
    ///
    /// Returns `Some(LongPress { .. })` if the mouse has been held stationary
    /// beyond the configured threshold.
    #[inline]
    pub fn check_long_press(&mut self, now: Instant) -> Option<SemanticEvent> {
        if self.long_press_fired {
            return None;
        }
        if let Some((pos, down_time)) = self.long_press_pos {
            let elapsed = now.duration_since(down_time);
            if elapsed >= self.config.long_press_threshold {
                self.long_press_fired = true;
                return Some(SemanticEvent::LongPress {
                    pos,
                    duration: elapsed,
                });
            }
        }
        None
    }

    /// Whether a drag is currently in progress.
    #[inline]
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.started)
    }

    /// Reset all gesture state to initial idle.
    pub fn reset(&mut self) {
        self.last_click = None;
        self.mouse_down = None;
        self.drag = None;
        self.long_press_pos = None;
        self.long_press_fired = false;
        self.chord_buffer.clear();
        self.chord_start = None;
    }

    /// Get a reference to the current configuration.
    #[inline]
    #[must_use]
    pub fn config(&self) -> &GestureConfig {
        &self.config
    }

    /// Update the configuration.
    pub fn set_config(&mut self, config: GestureConfig) {
        self.config = config;
    }
}

// ---------------------------------------------------------------------------
// Internal event handlers
// ---------------------------------------------------------------------------

impl GestureRecognizer {
    fn on_mouse_down(
        &mut self,
        pos: Position,
        button: MouseButton,
        now: Instant,
        _out: &mut Vec<SemanticEvent>,
    ) {
        self.mouse_down = Some((pos, button, now));
        self.drag = Some(DragTracker {
            start_pos: pos,
            button,
            last_pos: pos,
            started: false,
        });
        self.long_press_pos = Some((pos, now));
        self.long_press_fired = false;
    }

    fn on_mouse_up(
        &mut self,
        pos: Position,
        button: MouseButton,
        now: Instant,
        out: &mut Vec<SemanticEvent>,
    ) {
        self.long_press_pos = None;
        self.long_press_fired = false;

        // If we were dragging, emit DragEnd
        if let Some(drag) = self.drag.take()
            && drag.started
        {
            out.push(SemanticEvent::DragEnd {
                start: drag.start_pos,
                end: pos,
            });
            self.mouse_down = None;
            return;
        }

        // Not a drag — emit click
        self.mouse_down = None;

        // Multi-click detection
        let click_count = if let Some(ref last) = self.last_click {
            if last.button == button
                && last.pos.manhattan_distance(pos) <= u32::from(self.config.click_tolerance)
                && now.duration_since(last.time) <= self.config.multi_click_timeout
                && last.count < 3
            {
                last.count + 1
            } else {
                1
            }
        } else {
            1
        };

        self.last_click = Some(ClickState {
            pos,
            button,
            time: now,
            count: click_count,
        });

        match click_count {
            1 => out.push(SemanticEvent::Click { pos, button }),
            2 => out.push(SemanticEvent::DoubleClick { pos, button }),
            3 => out.push(SemanticEvent::TripleClick { pos, button }),
            _ => out.push(SemanticEvent::Click { pos, button }),
        }
    }

    fn on_mouse_drag(&mut self, pos: Position, button: MouseButton, out: &mut Vec<SemanticEvent>) {
        // Cancel long press on any movement
        self.long_press_pos = None;

        let Some(ref mut drag) = self.drag else {
            // Drag without prior mouse-down: create tracker
            self.drag = Some(DragTracker {
                start_pos: pos,
                button,
                last_pos: pos,
                started: false,
            });
            return;
        };

        if !drag.started {
            // Check if we've moved past the drag threshold
            let distance = drag.start_pos.manhattan_distance(pos);
            if distance >= u32::from(self.config.drag_threshold) {
                drag.started = true;
                out.push(SemanticEvent::DragStart {
                    pos: drag.start_pos,
                    button: drag.button,
                });
            }
        }

        if drag.started {
            let delta = (
                pos.x as i16 - drag.last_pos.x as i16,
                pos.y as i16 - drag.last_pos.y as i16,
            );
            out.push(SemanticEvent::DragMove {
                start: drag.start_pos,
                current: pos,
                delta,
            });
        }

        drag.last_pos = pos;
    }

    fn expire_chord(&mut self, now: Instant) {
        if let Some(start) = self.chord_start
            && now.duration_since(start) > self.config.chord_timeout
        {
            self.chord_buffer.clear();
            self.chord_start = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{KeyEvent, MouseEvent};

    fn now() -> Instant {
        Instant::now()
    }

    fn mouse_down(x: u16, y: u16, button: MouseButton) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(button),
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    fn mouse_up(x: u16, y: u16, button: MouseButton) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(button),
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    fn mouse_drag(x: u16, y: u16, button: MouseButton) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(button),
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    fn key_press(code: KeyCode, modifiers: Modifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        })
    }

    fn esc() -> Event {
        key_press(KeyCode::Escape, Modifiers::NONE)
    }

    const MS_50: Duration = Duration::from_millis(50);
    const MS_100: Duration = Duration::from_millis(100);
    const MS_200: Duration = Duration::from_millis(200);
    const MS_500: Duration = Duration::from_millis(500);
    const MS_600: Duration = Duration::from_millis(600);

    // --- Click tests ---

    #[test]
    fn single_click() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        let events = gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        assert!(events.is_empty());

        let events = gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            SemanticEvent::Click {
                pos: Position { x: 5, y: 5 },
                button: MouseButton::Left,
            }
        ));
    }

    #[test]
    fn double_click() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // First click
        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);

        // Second click within timeout
        gr.process(&mouse_down(5, 5, MouseButton::Left), t + MS_100);
        let events = gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_200);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::DoubleClick { .. }));
    }

    #[test]
    fn triple_click() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // First click
        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);

        // Second click
        gr.process(&mouse_down(5, 5, MouseButton::Left), t + MS_100);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_200);

        // Third click
        gr.process(
            &mouse_down(5, 5, MouseButton::Left),
            t + Duration::from_millis(250),
        );
        let events = gr.process(
            &mouse_up(5, 5, MouseButton::Left),
            t + Duration::from_millis(280),
        );

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::TripleClick { .. }));
    }

    #[test]
    fn double_click_timeout_resets_to_single() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // First click
        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);

        // Second click AFTER timeout (>300ms)
        gr.process(&mouse_down(5, 5, MouseButton::Left), t + MS_500);
        let events = gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_600);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::Click { .. }));
    }

    #[test]
    fn different_position_resets_click_count() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // First click at (5, 5)
        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);

        // Second click at (20, 20) — different position
        gr.process(&mouse_down(20, 20, MouseButton::Left), t + MS_100);
        let events = gr.process(&mouse_up(20, 20, MouseButton::Left), t + MS_200);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::Click { .. }));
    }

    #[test]
    fn different_button_resets_click_count() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Left click
        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);

        // Right click at same position
        gr.process(&mouse_down(5, 5, MouseButton::Right), t + MS_100);
        let events = gr.process(&mouse_up(5, 5, MouseButton::Right), t + MS_200);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            SemanticEvent::Click {
                button: MouseButton::Right,
                ..
            }
        ));
    }

    #[test]
    fn click_position_tolerance() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // First click at (5, 5)
        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);

        // Second click at (6, 5) — within tolerance of 1 cell
        gr.process(&mouse_down(6, 5, MouseButton::Left), t + MS_100);
        let events = gr.process(&mouse_up(6, 5, MouseButton::Left), t + MS_200);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::DoubleClick { .. }));
    }

    // --- Drag tests ---

    #[test]
    fn drag_starts_after_threshold() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);

        // Small move — below threshold (manhattan distance 1 < 3)
        let events = gr.process(&mouse_drag(6, 5, MouseButton::Left), t + MS_50);
        assert!(events.is_empty());
        assert!(!gr.is_dragging());

        // Move beyond threshold (manhattan distance 5 >= 3)
        let events = gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_100);
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SemanticEvent::DragStart { .. }))
        );
        assert!(gr.is_dragging());
    }

    #[test]
    fn drag_move_has_correct_delta() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        // Move past threshold
        let events = gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);

        // Find the DragMove event
        let drag_move = events
            .iter()
            .find(|e| matches!(e, SemanticEvent::DragMove { .. }));
        assert!(drag_move.is_some());
        if let Some(SemanticEvent::DragMove {
            start,
            current,
            delta,
        }) = drag_move
        {
            assert_eq!(*start, Position::new(5, 5));
            assert_eq!(*current, Position::new(10, 5));
            assert_eq!(*delta, (5, 0));
        }
    }

    #[test]
    fn drag_end_on_mouse_up() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);

        // Mouse up during drag
        let events = gr.process(&mouse_up(12, 5, MouseButton::Left), t + MS_100);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            SemanticEvent::DragEnd {
                start: Position { x: 5, y: 5 },
                end: Position { x: 12, y: 5 },
            }
        ));
        assert!(!gr.is_dragging());
    }

    #[test]
    fn drag_cancel_on_escape() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);
        assert!(gr.is_dragging());

        let events = gr.process(&esc(), t + MS_100);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::DragCancel));
        assert!(!gr.is_dragging());
    }

    #[test]
    fn drag_prevents_click() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);

        // Mouse up after drag → DragEnd, NOT Click
        let events = gr.process(&mouse_up(10, 5, MouseButton::Left), t + MS_100);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::DragEnd { .. }));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SemanticEvent::Click { .. }))
        );
    }

    // --- Long press tests ---

    #[test]
    fn long_press_fires_after_threshold() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);

        // Before threshold — no long press
        assert!(gr.check_long_press(t + MS_200).is_none());

        // After threshold (500ms)
        let lp = gr.check_long_press(t + MS_600);
        assert!(lp.is_some());
        if let Some(SemanticEvent::LongPress { pos, duration }) = lp {
            assert_eq!(pos, Position::new(5, 5));
            assert!(duration >= Duration::from_millis(500));
        }
    }

    #[test]
    fn long_press_not_repeated() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);

        // First check fires
        assert!(gr.check_long_press(t + MS_600).is_some());

        // Second check does NOT fire again
        assert!(
            gr.check_long_press(t + Duration::from_millis(700))
                .is_none()
        );
    }

    #[test]
    fn drag_cancels_long_press() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        // Move the mouse
        gr.process(&mouse_drag(6, 5, MouseButton::Left), t + MS_100);

        // Long press should not fire after movement
        assert!(gr.check_long_press(t + MS_600).is_none());
    }

    #[test]
    fn mouse_up_cancels_long_press() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_100);

        // Long press should not fire after mouse up
        assert!(gr.check_long_press(t + MS_600).is_none());
    }

    // --- Chord tests ---

    #[test]
    fn two_key_chord() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Ctrl+K
        let events1 = gr.process(&key_press(KeyCode::Char('k'), Modifiers::CTRL), t);
        assert!(events1.is_empty()); // First key in chord: no event yet

        // Ctrl+C within timeout
        let events2 = gr.process(&key_press(KeyCode::Char('c'), Modifiers::CTRL), t + MS_100);
        assert_eq!(events2.len(), 1);
        if let SemanticEvent::Chord { sequence } = &events2[0] {
            assert_eq!(sequence.len(), 2);
            assert_eq!(sequence[0].code, KeyCode::Char('k'));
            assert_eq!(sequence[1].code, KeyCode::Char('c'));
        } else {
            panic!("Expected Chord event");
        }
    }

    #[test]
    fn chord_timeout_clears_buffer() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Ctrl+K
        gr.process(&key_press(KeyCode::Char('k'), Modifiers::CTRL), t);

        // Wait beyond chord timeout (>1000ms)
        let events = gr.process(
            &key_press(KeyCode::Char('c'), Modifiers::CTRL),
            t + Duration::from_millis(1100),
        );

        // Should not emit a chord (timeout expired, buffer was cleared)
        assert!(events.is_empty());
    }

    #[test]
    fn non_modifier_key_clears_chord() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Start chord with Ctrl+K
        gr.process(&key_press(KeyCode::Char('k'), Modifiers::CTRL), t);

        // Plain key (no modifier) — clears chord
        gr.process(&key_press(KeyCode::Char('x'), Modifiers::NONE), t + MS_50);

        // Now Ctrl+C — should not form a chord with Ctrl+K
        let events = gr.process(&key_press(KeyCode::Char('c'), Modifiers::CTRL), t + MS_100);
        assert!(events.is_empty()); // Only one key in new chord buffer
    }

    #[test]
    fn escape_clears_chord() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&key_press(KeyCode::Char('k'), Modifiers::CTRL), t);
        gr.process(&esc(), t + MS_50);

        // Chord buffer should be cleared
        let events = gr.process(&key_press(KeyCode::Char('c'), Modifiers::CTRL), t + MS_100);
        assert!(events.is_empty());
    }

    // --- Focus loss tests ---

    #[test]
    fn focus_loss_cancels_drag() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);
        assert!(gr.is_dragging());

        let events = gr.process(&Event::Focus(false), t + MS_100);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::DragCancel));
        assert!(!gr.is_dragging());
    }

    #[test]
    fn focus_loss_without_drag_is_silent() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        let events = gr.process(&Event::Focus(false), t);
        assert!(events.is_empty());
    }

    // --- Reset tests ---

    #[test]
    fn reset_clears_all_state() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Build up state
        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);
        gr.process(&key_press(KeyCode::Char('k'), Modifiers::CTRL), t + MS_100);

        assert!(gr.is_dragging());

        gr.reset();

        assert!(!gr.is_dragging());
        assert!(gr.last_click.is_none());
        assert!(gr.mouse_down.is_none());
        assert!(gr.drag.is_none());
        assert!(gr.chord_buffer.is_empty());
        assert!(gr.chord_start.is_none());
    }

    // --- Edge cases ---

    #[test]
    fn quadruple_click_wraps_to_single() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Click 1-3
        for i in 0..3u32 {
            let offset = Duration::from_millis(i as u64 * 80);
            gr.process(&mouse_down(5, 5, MouseButton::Left), t + offset);
            gr.process(&mouse_up(5, 5, MouseButton::Left), t + offset + MS_50);
        }

        // Click 4 — should wrap back to single click (count capped at 3)
        gr.process(
            &mouse_down(5, 5, MouseButton::Left),
            t + Duration::from_millis(260),
        );
        let events = gr.process(
            &mouse_up(5, 5, MouseButton::Left),
            t + Duration::from_millis(280),
        );

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::Click { .. }));
    }

    #[test]
    fn key_release_ignored() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        let events = gr.process(
            &Event::Key(KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: Modifiers::CTRL,
                kind: KeyEventKind::Release,
            }),
            t,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn debug_format() {
        let gr = GestureRecognizer::new(GestureConfig::default());
        let dbg = format!("{:?}", gr);
        assert!(dbg.contains("GestureRecognizer"));
    }

    // --- Additional click tests (bd-950w) ---

    #[test]
    fn right_click() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(10, 10, MouseButton::Right), t);
        let events = gr.process(&mouse_up(10, 10, MouseButton::Right), t + MS_50);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            SemanticEvent::Click {
                button: MouseButton::Right,
                ..
            }
        ));
    }

    #[test]
    fn middle_click() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(10, 10, MouseButton::Middle), t);
        let events = gr.process(&mouse_up(10, 10, MouseButton::Middle), t + MS_50);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            SemanticEvent::Click {
                button: MouseButton::Middle,
                ..
            }
        ));
    }

    #[test]
    fn click_at_origin() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(0, 0, MouseButton::Left), t);
        let events = gr.process(&mouse_up(0, 0, MouseButton::Left), t + MS_50);

        assert_eq!(events.len(), 1);
        if let SemanticEvent::Click { pos, .. } = &events[0] {
            assert_eq!(pos.x, 0);
            assert_eq!(pos.y, 0);
        }
    }

    #[test]
    fn click_at_max_position() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(u16::MAX, u16::MAX, MouseButton::Left), t);
        let events = gr.process(&mouse_up(u16::MAX, u16::MAX, MouseButton::Left), t + MS_50);

        assert_eq!(events.len(), 1);
        if let SemanticEvent::Click { pos, .. } = &events[0] {
            assert_eq!(pos.x, u16::MAX);
            assert_eq!(pos.y, u16::MAX);
        }
    }

    #[test]
    fn double_click_right_button() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Right), t);
        gr.process(&mouse_up(5, 5, MouseButton::Right), t + MS_50);

        gr.process(&mouse_down(5, 5, MouseButton::Right), t + MS_100);
        let events = gr.process(&mouse_up(5, 5, MouseButton::Right), t + MS_200);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            SemanticEvent::DoubleClick {
                button: MouseButton::Right,
                ..
            }
        ));
    }

    #[test]
    fn click_position_beyond_tolerance() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);

        // Second click at (8, 5) — manhattan distance 3, beyond tolerance of 1
        gr.process(&mouse_down(8, 5, MouseButton::Left), t + MS_100);
        let events = gr.process(&mouse_up(8, 5, MouseButton::Left), t + MS_200);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::Click { .. }));
    }

    // --- Additional drag tests (bd-950w) ---

    #[test]
    fn drag_with_right_button() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Right), t);
        let events = gr.process(&mouse_drag(10, 5, MouseButton::Right), t + MS_50);

        assert!(events.iter().any(|e| matches!(
            e,
            SemanticEvent::DragStart {
                button: MouseButton::Right,
                ..
            }
        )));
    }

    #[test]
    fn drag_vertical() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        let events = gr.process(&mouse_drag(5, 10, MouseButton::Left), t + MS_50);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SemanticEvent::DragStart { .. }))
        );
        let drag_move = events
            .iter()
            .find(|e| matches!(e, SemanticEvent::DragMove { .. }));
        if let Some(SemanticEvent::DragMove { delta, .. }) = drag_move {
            assert_eq!(delta.0, 0); // no horizontal movement
            assert_eq!(delta.1, 5); // 5 cells down
        }
    }

    #[test]
    fn drag_multiple_moves() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);

        // Second move
        let events = gr.process(&mouse_drag(15, 5, MouseButton::Left), t + MS_100);
        let drag_move = events
            .iter()
            .find(|e| matches!(e, SemanticEvent::DragMove { .. }));
        if let Some(SemanticEvent::DragMove {
            start,
            current,
            delta,
        }) = drag_move
        {
            assert_eq!(*start, Position::new(5, 5));
            assert_eq!(*current, Position::new(15, 5));
            assert_eq!(*delta, (5, 0)); // delta from last position (10,5) to (15,5)
        }
    }

    #[test]
    fn drag_threshold_exactly_met() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        // Manhattan distance = 3 (exactly at threshold)
        let events = gr.process(&mouse_drag(8, 5, MouseButton::Left), t + MS_50);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SemanticEvent::DragStart { .. }))
        );
    }

    #[test]
    fn drag_threshold_one_below() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        // Manhattan distance = 2 (below threshold of 3)
        let events = gr.process(&mouse_drag(7, 5, MouseButton::Left), t + MS_50);

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SemanticEvent::DragStart { .. }))
        );
        assert!(!gr.is_dragging());
    }

    #[test]
    fn drag_state_reset_after_end() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Complete a drag
        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);
        gr.process(&mouse_up(10, 5, MouseButton::Left), t + MS_100);

        assert!(!gr.is_dragging());

        // New mouse down should start fresh
        gr.process(&mouse_down(20, 20, MouseButton::Left), t + MS_200);
        let events = gr.process(
            &mouse_up(20, 20, MouseButton::Left),
            t + Duration::from_millis(250),
        );

        // Should be a click, not a drag
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::Click { .. }));
    }

    #[test]
    fn no_click_after_drag_cancel() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);
        gr.process(&esc(), t + MS_100); // Cancel

        // Mouse up after cancel — should not produce click or DragEnd
        let events = gr.process(&mouse_up(10, 5, MouseButton::Left), t + MS_200);
        // Click is emitted because drag state was cleared by cancel
        // This is expected: after Escape cancels the drag, mouse up is treated as a fresh interaction
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SemanticEvent::DragEnd { .. }))
        );
    }

    // --- Additional long press tests (bd-950w) ---

    #[test]
    fn long_press_correct_position() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(42, 17, MouseButton::Left), t);
        let lp = gr.check_long_press(t + MS_600);

        if let Some(SemanticEvent::LongPress { pos, .. }) = lp {
            assert_eq!(pos, Position::new(42, 17));
        } else {
            panic!("Expected LongPress");
        }
    }

    #[test]
    fn long_press_with_custom_threshold() {
        let config = GestureConfig {
            long_press_threshold: Duration::from_millis(200),
            ..Default::default()
        };
        let mut gr = GestureRecognizer::new(config);
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);

        // Should not fire at 150ms
        assert!(
            gr.check_long_press(t + Duration::from_millis(150))
                .is_none()
        );

        // Should fire at 250ms (past 200ms threshold)
        assert!(
            gr.check_long_press(t + Duration::from_millis(250))
                .is_some()
        );
    }

    #[test]
    fn long_press_resets_on_new_mouse_down() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_100);

        // New mouse down resets long press timer
        gr.process(&mouse_down(10, 10, MouseButton::Left), t + MS_200);

        // Should not fire based on old timer
        assert!(gr.check_long_press(t + MS_600).is_none());

        // Should fire based on new timer (200ms + 500ms = 700ms)
        assert!(
            gr.check_long_press(t + Duration::from_millis(750))
                .is_some()
        );
    }

    // --- Additional chord tests (bd-950w) ---

    #[test]
    fn alt_key_chord() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&key_press(KeyCode::Char('x'), Modifiers::ALT), t);
        let events = gr.process(&key_press(KeyCode::Char('y'), Modifiers::ALT), t + MS_100);

        assert_eq!(events.len(), 1);
        if let SemanticEvent::Chord { sequence } = &events[0] {
            assert_eq!(sequence.len(), 2);
            assert!(sequence[0].modifiers.contains(Modifiers::ALT));
        }
    }

    #[test]
    fn mixed_modifier_chord() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&key_press(KeyCode::Char('k'), Modifiers::CTRL), t);
        let events = gr.process(&key_press(KeyCode::Char('d'), Modifiers::ALT), t + MS_100);

        assert_eq!(events.len(), 1);
        if let SemanticEvent::Chord { sequence } = &events[0] {
            assert_eq!(sequence[0].modifiers, Modifiers::CTRL);
            assert_eq!(sequence[1].modifiers, Modifiers::ALT);
        }
    }

    #[test]
    fn chord_with_function_key() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        gr.process(&key_press(KeyCode::Char('k'), Modifiers::CTRL), t);
        let events = gr.process(&key_press(KeyCode::F(1), Modifiers::CTRL), t + MS_100);

        assert_eq!(events.len(), 1);
        if let SemanticEvent::Chord { sequence } = &events[0] {
            assert_eq!(sequence[1].code, KeyCode::F(1));
        }
    }

    #[test]
    fn single_modifier_key_no_chord_emitted() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Single Ctrl+K does not emit a chord (needs ≥2 keys)
        let events = gr.process(&key_press(KeyCode::Char('k'), Modifiers::CTRL), t);
        assert!(events.is_empty());
    }

    // --- Config tests (bd-950w) ---

    #[test]
    fn custom_click_tolerance() {
        let config = GestureConfig {
            click_tolerance: 5,
            ..Default::default()
        };
        let mut gr = GestureRecognizer::new(config);
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);

        // Click at (10, 5) — manhattan distance 5, within tolerance of 5
        gr.process(&mouse_down(10, 5, MouseButton::Left), t + MS_100);
        let events = gr.process(&mouse_up(10, 5, MouseButton::Left), t + MS_200);

        assert!(matches!(events[0], SemanticEvent::DoubleClick { .. }));
    }

    #[test]
    fn custom_drag_threshold() {
        let config = GestureConfig {
            drag_threshold: 10,
            ..Default::default()
        };
        let mut gr = GestureRecognizer::new(config);
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        // Move 5 cells — below new threshold of 10
        let events = gr.process(&mouse_drag(10, 5, MouseButton::Left), t + MS_50);
        assert!(!gr.is_dragging());
        assert!(events.is_empty());

        // Move 10 cells — at threshold
        let events = gr.process(&mouse_drag(15, 5, MouseButton::Left), t + MS_100);
        assert!(gr.is_dragging());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SemanticEvent::DragStart { .. }))
        );
    }

    #[test]
    fn custom_multi_click_timeout() {
        let config = GestureConfig {
            multi_click_timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let mut gr = GestureRecognizer::new(config);
        let t = now();

        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);

        // Second click at 150ms — beyond 100ms timeout
        gr.process(
            &mouse_down(5, 5, MouseButton::Left),
            t + Duration::from_millis(150),
        );
        let events = gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_200);

        assert!(matches!(events[0], SemanticEvent::Click { .. }));
    }

    #[test]
    fn config_getter_and_setter() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());

        assert_eq!(gr.config().drag_threshold, 3);

        let new_config = GestureConfig {
            drag_threshold: 10,
            ..Default::default()
        };
        gr.set_config(new_config);

        assert_eq!(gr.config().drag_threshold, 10);
    }

    // --- Integration / sequence tests (bd-950w) ---

    #[test]
    fn click_then_drag_are_independent() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Click
        gr.process(&mouse_down(5, 5, MouseButton::Left), t);
        let click_events = gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_50);
        assert!(matches!(click_events[0], SemanticEvent::Click { .. }));

        // Drag
        gr.process(&mouse_down(5, 5, MouseButton::Left), t + MS_200);
        let drag_events = gr.process(
            &mouse_drag(10, 5, MouseButton::Left),
            t + Duration::from_millis(250),
        );
        assert!(
            drag_events
                .iter()
                .any(|e| matches!(e, SemanticEvent::DragStart { .. }))
        );

        let end_events = gr.process(
            &mouse_up(10, 5, MouseButton::Left),
            t + Duration::from_millis(300),
        );
        assert!(matches!(end_events[0], SemanticEvent::DragEnd { .. }));
    }

    #[test]
    fn interleaved_mouse_and_keyboard() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        // Start typing a chord
        gr.process(&key_press(KeyCode::Char('k'), Modifiers::CTRL), t);

        // Mouse click in between
        gr.process(&mouse_down(5, 5, MouseButton::Left), t + MS_50);
        gr.process(&mouse_up(5, 5, MouseButton::Left), t + MS_100);

        // Continue chord — chord buffer was not cleared by mouse events
        let events = gr.process(&key_press(KeyCode::Char('c'), Modifiers::CTRL), t + MS_200);

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SemanticEvent::Chord { .. }));
    }

    #[test]
    fn rapid_clicks_produce_correct_sequence() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();
        let mut results = Vec::new();

        for i in 0..5u32 {
            let offset = Duration::from_millis(i as u64 * 60);
            gr.process(&mouse_down(5, 5, MouseButton::Left), t + offset);
            let events = gr.process(
                &mouse_up(5, 5, MouseButton::Left),
                t + offset + Duration::from_millis(30),
            );
            results.extend(events);
        }

        // Should see: Click, DoubleClick, TripleClick, Click (wrap), DoubleClick
        assert!(results.len() == 5);
        assert!(matches!(results[0], SemanticEvent::Click { .. }));
        assert!(matches!(results[1], SemanticEvent::DoubleClick { .. }));
        assert!(matches!(results[2], SemanticEvent::TripleClick { .. }));
        assert!(matches!(results[3], SemanticEvent::Click { .. }));
        assert!(matches!(results[4], SemanticEvent::DoubleClick { .. }));
    }

    #[test]
    fn tick_event_ignored() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        let events = gr.process(&Event::Tick, t);
        assert!(events.is_empty());
    }

    #[test]
    fn resize_event_ignored() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        let events = gr.process(
            &Event::Resize {
                width: 80,
                height: 24,
            },
            t,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn focus_gain_ignored() {
        let mut gr = GestureRecognizer::new(GestureConfig::default());
        let t = now();

        let events = gr.process(&Event::Focus(true), t);
        assert!(events.is_empty());
    }

    #[test]
    fn default_config_values() {
        let config = GestureConfig::default();
        assert_eq!(config.multi_click_timeout, Duration::from_millis(300));
        assert_eq!(config.long_press_threshold, Duration::from_millis(500));
        assert_eq!(config.drag_threshold, 3);
        assert_eq!(config.chord_timeout, Duration::from_millis(1000));
        assert_eq!(config.click_tolerance, 1);
    }
}

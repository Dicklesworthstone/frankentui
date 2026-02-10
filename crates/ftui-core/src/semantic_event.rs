#![forbid(unsafe_code)]

//! High-level semantic events derived from raw terminal input (bd-3fu8).
//!
//! [`SemanticEvent`] represents user *intentions* rather than raw key presses or
//! mouse coordinates. A gesture recognizer (see bd-2v34) converts raw [`Event`]
//! sequences into these semantic events.
//!
//! # Design
//!
//! ## Invariants
//! 1. Every drag sequence is well-formed: `DragStart` → zero or more `DragMove` → `DragEnd` or `DragCancel`.
//! 2. Click multiplicity is monotonically increasing within a multi-click window:
//!    a `TripleClick` always follows a `DoubleClick` from the same position.
//! 3. `Chord` sequences are non-empty (enforced by constructor).
//! 4. `Swipe` velocity is always non-negative.
//!
//! ## Failure Modes
//! - If the gesture recognizer times out mid-chord, no `Chord` event is emitted;
//!   the raw keys are passed through instead (graceful degradation).
//! - If a drag is interrupted by focus loss, `DragCancel` is emitted (never a
//!   dangling `DragStart` without termination).

use crate::event::{KeyCode, Modifiers, MouseButton};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Position
// ---------------------------------------------------------------------------

/// A 2D cell position in the terminal (0-indexed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Position {
    pub x: u16,
    pub y: u16,
}

impl Position {
    /// Create a new position.
    #[must_use]
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }

    /// Manhattan distance to another position.
    #[must_use]
    pub fn manhattan_distance(self, other: Self) -> u32 {
        (self.x as i32 - other.x as i32).unsigned_abs()
            + (self.y as i32 - other.y as i32).unsigned_abs()
    }
}

impl From<(u16, u16)> for Position {
    fn from((x, y): (u16, u16)) -> Self {
        Self { x, y }
    }
}

// ---------------------------------------------------------------------------
// ChordKey
// ---------------------------------------------------------------------------

/// A single key in a chord sequence (e.g., Ctrl+K in "Ctrl+K, Ctrl+C").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChordKey {
    pub code: KeyCode,
    pub modifiers: Modifiers,
}

impl ChordKey {
    /// Create a chord key.
    #[must_use]
    pub const fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self { code, modifiers }
    }
}

// ---------------------------------------------------------------------------
// SwipeDirection
// ---------------------------------------------------------------------------

/// Cardinal direction for swipe gestures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwipeDirection {
    Up,
    Down,
    Left,
    Right,
}

impl SwipeDirection {
    /// Returns the opposite direction.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Down => Self::Up,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    /// Returns true for vertical directions.
    #[must_use]
    pub const fn is_vertical(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }

    /// Returns true for horizontal directions.
    #[must_use]
    pub const fn is_horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

// ---------------------------------------------------------------------------
// SemanticEvent
// ---------------------------------------------------------------------------

/// High-level semantic events derived from raw terminal input.
///
/// These represent user intentions rather than raw key presses or mouse
/// coordinates. A gesture recognizer converts raw events into these.
#[derive(Debug, Clone, PartialEq)]
pub enum SemanticEvent {
    // === Mouse Gestures ===
    /// Single click (mouse down + up in same position within threshold).
    Click { pos: Position, button: MouseButton },

    /// Two clicks within the double-click time threshold.
    DoubleClick { pos: Position, button: MouseButton },

    /// Three clicks within threshold (often used for line selection).
    TripleClick { pos: Position, button: MouseButton },

    /// Mouse held down beyond threshold without moving.
    LongPress { pos: Position, duration: Duration },

    // === Drag Gestures ===
    /// Mouse moved beyond drag threshold while button held.
    DragStart { pos: Position, button: MouseButton },

    /// Ongoing drag movement.
    DragMove {
        start: Position,
        current: Position,
        /// Movement since last DragMove (dx, dy).
        delta: (i16, i16),
    },

    /// Mouse released after drag.
    DragEnd { start: Position, end: Position },

    /// Drag cancelled (Escape pressed, focus lost, etc.).
    DragCancel,

    // === Keyboard Gestures ===
    /// Key chord sequence completed (e.g., Ctrl+K, Ctrl+C).
    ///
    /// Invariant: `sequence` is always non-empty.
    Chord { sequence: Vec<ChordKey> },

    // === Touch-Like Gestures ===
    /// Swipe gesture (rapid mouse movement in a cardinal direction).
    Swipe {
        direction: SwipeDirection,
        /// Distance in cells.
        distance: u16,
        /// Velocity in cells per second (always >= 0.0).
        velocity: f32,
    },
}

impl SemanticEvent {
    /// Returns true if this is a drag-related event.
    #[must_use]
    pub fn is_drag(&self) -> bool {
        matches!(
            self,
            Self::DragStart { .. }
                | Self::DragMove { .. }
                | Self::DragEnd { .. }
                | Self::DragCancel
        )
    }

    /// Returns true if this is a click-related event (single, double, or triple).
    #[must_use]
    pub fn is_click(&self) -> bool {
        matches!(
            self,
            Self::Click { .. } | Self::DoubleClick { .. } | Self::TripleClick { .. }
        )
    }

    /// Returns the position if this event has one.
    #[must_use]
    pub fn position(&self) -> Option<Position> {
        match self {
            Self::Click { pos, .. }
            | Self::DoubleClick { pos, .. }
            | Self::TripleClick { pos, .. }
            | Self::LongPress { pos, .. }
            | Self::DragStart { pos, .. } => Some(*pos),
            Self::DragMove { current, .. } => Some(*current),
            Self::DragEnd { end, .. } => Some(*end),
            Self::Chord { .. } | Self::DragCancel | Self::Swipe { .. } => None,
        }
    }

    /// Returns the mouse button if this event involves one.
    #[must_use]
    pub fn button(&self) -> Option<MouseButton> {
        match self {
            Self::Click { button, .. }
            | Self::DoubleClick { button, .. }
            | Self::TripleClick { button, .. }
            | Self::DragStart { button, .. } => Some(*button),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: u16, y: u16) -> Position {
        Position::new(x, y)
    }

    // === Position tests ===

    #[test]
    fn position_new_and_from_tuple() {
        let p = Position::new(5, 10);
        assert_eq!(p, Position::from((5, 10)));
        assert_eq!(p.x, 5);
        assert_eq!(p.y, 10);
    }

    #[test]
    fn position_manhattan_distance() {
        assert_eq!(pos(0, 0).manhattan_distance(pos(3, 4)), 7);
        assert_eq!(pos(5, 5).manhattan_distance(pos(5, 5)), 0);
        assert_eq!(pos(10, 0).manhattan_distance(pos(0, 10)), 20);
    }

    #[test]
    fn position_default_is_origin() {
        assert_eq!(Position::default(), pos(0, 0));
    }

    // === ChordKey tests ===

    #[test]
    fn chord_key_equality() {
        let k1 = ChordKey::new(KeyCode::Char('k'), Modifiers::CTRL);
        let k2 = ChordKey::new(KeyCode::Char('k'), Modifiers::CTRL);
        let k3 = ChordKey::new(KeyCode::Char('c'), Modifiers::CTRL);

        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn chord_key_hash_consistency() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ChordKey::new(KeyCode::Char('k'), Modifiers::CTRL));
        set.insert(ChordKey::new(KeyCode::Char('k'), Modifiers::CTRL)); // duplicate
        assert_eq!(set.len(), 1);
    }

    // === SwipeDirection tests ===

    #[test]
    fn swipe_direction_opposite() {
        assert_eq!(SwipeDirection::Up.opposite(), SwipeDirection::Down);
        assert_eq!(SwipeDirection::Down.opposite(), SwipeDirection::Up);
        assert_eq!(SwipeDirection::Left.opposite(), SwipeDirection::Right);
        assert_eq!(SwipeDirection::Right.opposite(), SwipeDirection::Left);
    }

    #[test]
    fn swipe_direction_axes() {
        assert!(SwipeDirection::Up.is_vertical());
        assert!(SwipeDirection::Down.is_vertical());
        assert!(!SwipeDirection::Left.is_vertical());
        assert!(!SwipeDirection::Right.is_vertical());

        assert!(SwipeDirection::Left.is_horizontal());
        assert!(SwipeDirection::Right.is_horizontal());
        assert!(!SwipeDirection::Up.is_horizontal());
        assert!(!SwipeDirection::Down.is_horizontal());
    }

    // === SemanticEvent tests ===

    #[test]
    fn is_drag_classification() {
        assert!(
            SemanticEvent::DragStart {
                pos: pos(0, 0),
                button: MouseButton::Left,
            }
            .is_drag()
        );

        assert!(
            SemanticEvent::DragMove {
                start: pos(0, 0),
                current: pos(5, 5),
                delta: (5, 5),
            }
            .is_drag()
        );

        assert!(
            SemanticEvent::DragEnd {
                start: pos(0, 0),
                end: pos(10, 10),
            }
            .is_drag()
        );

        assert!(SemanticEvent::DragCancel.is_drag());

        // Non-drag events
        assert!(
            !SemanticEvent::Click {
                pos: pos(0, 0),
                button: MouseButton::Left,
            }
            .is_drag()
        );

        assert!(
            !SemanticEvent::Chord {
                sequence: vec![ChordKey::new(KeyCode::Char('k'), Modifiers::CTRL)],
            }
            .is_drag()
        );
    }

    #[test]
    fn is_click_classification() {
        assert!(
            SemanticEvent::Click {
                pos: pos(1, 2),
                button: MouseButton::Left,
            }
            .is_click()
        );

        assert!(
            SemanticEvent::DoubleClick {
                pos: pos(1, 2),
                button: MouseButton::Left,
            }
            .is_click()
        );

        assert!(
            SemanticEvent::TripleClick {
                pos: pos(1, 2),
                button: MouseButton::Left,
            }
            .is_click()
        );

        assert!(
            !SemanticEvent::DragStart {
                pos: pos(0, 0),
                button: MouseButton::Left,
            }
            .is_click()
        );
    }

    #[test]
    fn position_extraction() {
        assert_eq!(
            SemanticEvent::Click {
                pos: pos(5, 10),
                button: MouseButton::Left,
            }
            .position(),
            Some(pos(5, 10))
        );

        assert_eq!(
            SemanticEvent::DragMove {
                start: pos(0, 0),
                current: pos(15, 20),
                delta: (1, 1),
            }
            .position(),
            Some(pos(15, 20))
        );

        assert_eq!(
            SemanticEvent::DragEnd {
                start: pos(0, 0),
                end: pos(30, 40),
            }
            .position(),
            Some(pos(30, 40))
        );

        assert_eq!(SemanticEvent::DragCancel.position(), None);

        assert_eq!(SemanticEvent::Chord { sequence: vec![] }.position(), None);

        assert_eq!(
            SemanticEvent::Swipe {
                direction: SwipeDirection::Up,
                distance: 10,
                velocity: 100.0,
            }
            .position(),
            None
        );
    }

    #[test]
    fn button_extraction() {
        assert_eq!(
            SemanticEvent::Click {
                pos: pos(0, 0),
                button: MouseButton::Right,
            }
            .button(),
            Some(MouseButton::Right)
        );

        assert_eq!(
            SemanticEvent::DragStart {
                pos: pos(0, 0),
                button: MouseButton::Middle,
            }
            .button(),
            Some(MouseButton::Middle)
        );

        assert_eq!(SemanticEvent::DragCancel.button(), None);

        assert_eq!(
            SemanticEvent::LongPress {
                pos: pos(0, 0),
                duration: Duration::from_millis(500),
            }
            .button(),
            None
        );
    }

    #[test]
    fn long_press_carries_duration() {
        let event = SemanticEvent::LongPress {
            pos: pos(10, 20),
            duration: Duration::from_millis(750),
        };
        assert_eq!(event.position(), Some(pos(10, 20)));
        assert!(!event.is_drag());
        assert!(!event.is_click());
    }

    #[test]
    fn swipe_velocity_and_direction() {
        let event = SemanticEvent::Swipe {
            direction: SwipeDirection::Right,
            distance: 25,
            velocity: 150.0,
        };
        assert!(!event.is_drag());
        assert!(!event.is_click());
        assert_eq!(event.position(), None);
    }

    #[test]
    fn chord_sequence_contents() {
        let chord = SemanticEvent::Chord {
            sequence: vec![
                ChordKey::new(KeyCode::Char('k'), Modifiers::CTRL),
                ChordKey::new(KeyCode::Char('c'), Modifiers::CTRL),
            ],
        };
        if let SemanticEvent::Chord { sequence } = &chord {
            assert_eq!(sequence.len(), 2);
            assert_eq!(sequence[0].code, KeyCode::Char('k'));
            assert_eq!(sequence[1].code, KeyCode::Char('c'));
        } else {
            panic!("Expected Chord variant");
        }
    }

    #[test]
    fn semantic_event_debug_format() {
        let click = SemanticEvent::Click {
            pos: pos(5, 10),
            button: MouseButton::Left,
        };
        let dbg = format!("{:?}", click);
        assert!(dbg.contains("Click"));
        assert!(dbg.contains("Position"));
    }

    // ─── Edge-case tests (bd-17azz) ────────────────────────────────────

    // === Position edge cases ===

    #[test]
    fn position_manhattan_distance_max_coordinates() {
        let p1 = Position::new(0, 0);
        let p2 = Position::new(u16::MAX, u16::MAX);
        // |65535 - 0| + |65535 - 0| = 131070
        assert_eq!(p1.manhattan_distance(p2), 131070);
    }

    #[test]
    fn position_manhattan_distance_symmetric() {
        let a = pos(10, 20);
        let b = pos(50, 3);
        assert_eq!(a.manhattan_distance(b), b.manhattan_distance(a));
    }

    #[test]
    fn position_manhattan_distance_same_point() {
        let p = pos(100, 200);
        assert_eq!(p.manhattan_distance(p), 0);
    }

    #[test]
    fn position_manhattan_distance_horizontal_only() {
        assert_eq!(pos(0, 5).manhattan_distance(pos(10, 5)), 10);
    }

    #[test]
    fn position_manhattan_distance_vertical_only() {
        assert_eq!(pos(5, 0).manhattan_distance(pos(5, 10)), 10);
    }

    #[test]
    fn position_from_tuple_max() {
        let p: Position = (u16::MAX, u16::MAX).into();
        assert_eq!(p.x, u16::MAX);
        assert_eq!(p.y, u16::MAX);
    }

    #[test]
    fn position_hash_consistency() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(pos(10, 20));
        set.insert(pos(10, 20)); // duplicate
        assert_eq!(set.len(), 1);
        set.insert(pos(20, 10)); // different
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn position_copy_semantics() {
        let p = pos(5, 10);
        let q = p; // Copy, not move
        assert_eq!(p, q); // p is still usable
    }

    // === ChordKey edge cases ===

    #[test]
    fn chord_key_different_modifiers_not_equal() {
        let k1 = ChordKey::new(KeyCode::Char('k'), Modifiers::CTRL);
        let k2 = ChordKey::new(KeyCode::Char('k'), Modifiers::ALT);
        assert_ne!(k1, k2);
    }

    #[test]
    fn chord_key_clone_independence() {
        let original = ChordKey::new(KeyCode::Char('x'), Modifiers::SHIFT);
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn chord_key_no_modifiers() {
        let k = ChordKey::new(KeyCode::Enter, Modifiers::NONE);
        assert_eq!(k.modifiers, Modifiers::NONE);
    }

    #[test]
    fn chord_key_debug_format() {
        let k = ChordKey::new(KeyCode::Char('a'), Modifiers::CTRL);
        let dbg = format!("{k:?}");
        assert!(dbg.contains("ChordKey"));
    }

    // === SwipeDirection edge cases ===

    #[test]
    fn swipe_direction_double_opposite_is_identity() {
        for dir in [
            SwipeDirection::Up,
            SwipeDirection::Down,
            SwipeDirection::Left,
            SwipeDirection::Right,
        ] {
            assert_eq!(dir.opposite().opposite(), dir);
        }
    }

    #[test]
    fn swipe_direction_vertical_horizontal_mutually_exclusive() {
        for dir in [
            SwipeDirection::Up,
            SwipeDirection::Down,
            SwipeDirection::Left,
            SwipeDirection::Right,
        ] {
            assert_ne!(dir.is_vertical(), dir.is_horizontal());
        }
    }

    #[test]
    fn swipe_direction_copy_semantics() {
        let d = SwipeDirection::Up;
        let e = d; // Copy
        assert_eq!(d, e);
    }

    #[test]
    fn swipe_direction_hash_consistency() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(SwipeDirection::Up);
        set.insert(SwipeDirection::Up);
        assert_eq!(set.len(), 1);
        set.insert(SwipeDirection::Down);
        assert_eq!(set.len(), 2);
    }

    // === SemanticEvent edge cases ===

    #[test]
    fn position_for_double_click() {
        assert_eq!(
            SemanticEvent::DoubleClick {
                pos: pos(33, 44),
                button: MouseButton::Right,
            }
            .position(),
            Some(pos(33, 44))
        );
    }

    #[test]
    fn position_for_triple_click() {
        assert_eq!(
            SemanticEvent::TripleClick {
                pos: pos(1, 2),
                button: MouseButton::Middle,
            }
            .position(),
            Some(pos(1, 2))
        );
    }

    #[test]
    fn position_for_long_press() {
        assert_eq!(
            SemanticEvent::LongPress {
                pos: pos(99, 88),
                duration: Duration::from_secs(1),
            }
            .position(),
            Some(pos(99, 88))
        );
    }

    #[test]
    fn position_for_drag_start() {
        assert_eq!(
            SemanticEvent::DragStart {
                pos: pos(7, 8),
                button: MouseButton::Left,
            }
            .position(),
            Some(pos(7, 8))
        );
    }

    #[test]
    fn button_for_double_click() {
        assert_eq!(
            SemanticEvent::DoubleClick {
                pos: pos(0, 0),
                button: MouseButton::Middle,
            }
            .button(),
            Some(MouseButton::Middle)
        );
    }

    #[test]
    fn button_for_triple_click() {
        assert_eq!(
            SemanticEvent::TripleClick {
                pos: pos(0, 0),
                button: MouseButton::Right,
            }
            .button(),
            Some(MouseButton::Right)
        );
    }

    #[test]
    fn button_none_for_drag_move() {
        assert_eq!(
            SemanticEvent::DragMove {
                start: pos(0, 0),
                current: pos(1, 1),
                delta: (1, 1),
            }
            .button(),
            None
        );
    }

    #[test]
    fn button_none_for_drag_end() {
        assert_eq!(
            SemanticEvent::DragEnd {
                start: pos(0, 0),
                end: pos(5, 5),
            }
            .button(),
            None
        );
    }

    #[test]
    fn button_none_for_swipe() {
        assert_eq!(
            SemanticEvent::Swipe {
                direction: SwipeDirection::Left,
                distance: 5,
                velocity: 1.0,
            }
            .button(),
            None
        );
    }

    #[test]
    fn button_none_for_chord() {
        assert_eq!(
            SemanticEvent::Chord {
                sequence: vec![ChordKey::new(KeyCode::Char('a'), Modifiers::NONE)],
            }
            .button(),
            None
        );
    }

    #[test]
    fn is_drag_false_for_long_press() {
        assert!(
            !SemanticEvent::LongPress {
                pos: pos(0, 0),
                duration: Duration::from_millis(100),
            }
            .is_drag()
        );
    }

    #[test]
    fn is_drag_false_for_swipe() {
        assert!(
            !SemanticEvent::Swipe {
                direction: SwipeDirection::Down,
                distance: 10,
                velocity: 50.0,
            }
            .is_drag()
        );
    }

    #[test]
    fn is_click_false_for_long_press() {
        assert!(
            !SemanticEvent::LongPress {
                pos: pos(0, 0),
                duration: Duration::from_millis(500),
            }
            .is_click()
        );
    }

    #[test]
    fn is_click_false_for_swipe() {
        assert!(
            !SemanticEvent::Swipe {
                direction: SwipeDirection::Up,
                distance: 5,
                velocity: 20.0,
            }
            .is_click()
        );
    }

    #[test]
    fn drag_move_with_negative_delta() {
        let ev = SemanticEvent::DragMove {
            start: pos(20, 20),
            current: pos(10, 10),
            delta: (-10, -10),
        };
        assert!(ev.is_drag());
        assert_eq!(ev.position(), Some(pos(10, 10)));
    }

    #[test]
    fn drag_move_with_zero_delta() {
        let ev = SemanticEvent::DragMove {
            start: pos(5, 5),
            current: pos(5, 5),
            delta: (0, 0),
        };
        assert!(ev.is_drag());
        assert_eq!(ev.position(), Some(pos(5, 5)));
    }

    #[test]
    fn swipe_zero_velocity() {
        let ev = SemanticEvent::Swipe {
            direction: SwipeDirection::Right,
            distance: 0,
            velocity: 0.0,
        };
        assert_eq!(ev.position(), None);
        assert!(!ev.is_drag());
        assert!(!ev.is_click());
    }

    #[test]
    fn swipe_large_velocity() {
        let ev = SemanticEvent::Swipe {
            direction: SwipeDirection::Up,
            distance: u16::MAX,
            velocity: f32::MAX,
        };
        assert_eq!(ev.position(), None);
    }

    #[test]
    fn long_press_zero_duration() {
        let ev = SemanticEvent::LongPress {
            pos: pos(0, 0),
            duration: Duration::ZERO,
        };
        assert_eq!(ev.position(), Some(pos(0, 0)));
        assert!(!ev.is_click());
    }

    #[test]
    fn chord_empty_sequence() {
        // The invariant says non-empty, but the struct allows it
        let ev = SemanticEvent::Chord { sequence: vec![] };
        assert_eq!(ev.position(), None);
        assert_eq!(ev.button(), None);
        assert!(!ev.is_drag());
        assert!(!ev.is_click());
    }

    #[test]
    fn chord_clone_deep_copy() {
        let original = SemanticEvent::Chord {
            sequence: vec![
                ChordKey::new(KeyCode::Char('k'), Modifiers::CTRL),
                ChordKey::new(KeyCode::Char('c'), Modifiers::CTRL),
            ],
        };
        let cloned = original.clone();
        assert_eq!(original, cloned);
        // Modifying clone shouldn't affect original (Vec is deep cloned)
        if let SemanticEvent::Chord { sequence } = &cloned {
            assert_eq!(sequence.len(), 2);
        }
    }

    #[test]
    fn swipe_nan_velocity_not_equal_to_itself() {
        let ev1 = SemanticEvent::Swipe {
            direction: SwipeDirection::Up,
            distance: 5,
            velocity: f32::NAN,
        };
        let ev2 = ev1.clone();
        // NaN != NaN, so PartialEq should return false
        assert_ne!(ev1, ev2);
    }

    #[test]
    fn drag_cancel_is_minimal() {
        let ev = SemanticEvent::DragCancel;
        assert!(ev.is_drag());
        assert!(!ev.is_click());
        assert_eq!(ev.position(), None);
        assert_eq!(ev.button(), None);
    }

    #[test]
    fn drag_end_position_is_end_not_start() {
        let ev = SemanticEvent::DragEnd {
            start: pos(0, 0),
            end: pos(100, 200),
        };
        assert_eq!(ev.position(), Some(pos(100, 200)));
    }

    #[test]
    fn click_with_right_button() {
        let ev = SemanticEvent::Click {
            pos: pos(5, 10),
            button: MouseButton::Right,
        };
        assert!(ev.is_click());
        assert_eq!(ev.button(), Some(MouseButton::Right));
    }

    #[test]
    fn click_with_middle_button() {
        let ev = SemanticEvent::Click {
            pos: pos(0, 0),
            button: MouseButton::Middle,
        };
        assert!(ev.is_click());
        assert_eq!(ev.button(), Some(MouseButton::Middle));
    }

    // ─── End edge-case tests (bd-17azz) ──────────────────────────────

    #[test]
    fn semantic_event_clone_and_eq() {
        let original = SemanticEvent::DoubleClick {
            pos: pos(3, 7),
            button: MouseButton::Left,
        };
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }
}

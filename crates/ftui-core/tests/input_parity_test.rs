//! Input path parity tests.
//!
//! This module verifies that InputParser (raw byte parsing) and
//! Event::from_crossterm() (crossterm event mapping) produce
//! consistent Event outputs for equivalent inputs.
//!
//! # Design
//!
//! These tests ensure that applications get the same Event regardless
//! of which input path is used:
//!
//! 1. **Runtime path**: `crossterm::event::read()` → `Event::from_crossterm()`
//! 2. **InputParser path**: raw bytes → `InputParser::parse()`
//!
//! # Test Categories
//!
//! - Key codes: Enter, Escape, Backspace, Tab, BackTab, arrows, function keys
//! - Modifiers: Shift, Alt, Ctrl, Super combinations
//! - Mouse events: clicks, drag, scroll with modifiers
//! - Special: Null (Ctrl+Space), Kitty keyboard protocol

#![forbid(unsafe_code)]
#![cfg(all(not(target_arch = "wasm32"), feature = "crossterm"))]

use crossterm::event as cte;
use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers, MouseButton, MouseEventKind};
use ftui_core::input_parser::InputParser;

/// Helper to create a crossterm key event.
fn ct_key(code: cte::KeyCode, modifiers: cte::KeyModifiers, kind: cte::KeyEventKind) -> cte::Event {
    cte::Event::Key(cte::KeyEvent {
        code,
        modifiers,
        kind,
        state: cte::KeyEventState::NONE,
    })
}

/// Helper to create a crossterm mouse event.
fn ct_mouse(
    kind: cte::MouseEventKind,
    column: u16,
    row: u16,
    modifiers: cte::KeyModifiers,
) -> cte::Event {
    cte::Event::Mouse(cte::MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}

// ============================================================================
// Basic Key Code Parity
// ============================================================================

#[test]
fn parity_enter_key() {
    let mut parser = InputParser::new();

    // InputParser: CR (0x0D) maps to Enter
    let parser_events = parser.parse(b"\x0D");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Enter);
    assert_eq!(parser_key.modifiers, Modifiers::NONE);

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Enter,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::Enter);

    // Verify parity
    assert_eq!(
        parser_key.code, crossterm_key.code,
        "KeyCode mismatch for Enter"
    );
}

#[test]
fn parity_escape_key() {
    let mut parser = InputParser::new();

    // InputParser: ESC (0x1B) followed by nothing within timeout = Escape
    // In practice, standalone ESC is ambiguous. We test ESC followed by another ESC
    // which should give Alt+Escape, then test the crossterm mapping.
    let parser_events = parser.parse(b"\x1b\x1b");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Escape);
    assert_eq!(parser_key.modifiers, Modifiers::ALT);

    // Crossterm path for Escape
    let ct_event = ct_key(
        cte::KeyCode::Esc,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::Escape);
}

#[test]
fn parity_backspace_key() {
    let mut parser = InputParser::new();

    // InputParser: Both 0x7F (DEL) and 0x08 (BS) map to Backspace
    let parser_events_del = parser.parse(b"\x7F");
    let parser_events_bs = parser.parse(b"\x08");

    assert_eq!(parser_events_del.len(), 1);
    assert_eq!(parser_events_bs.len(), 1);

    let Event::Key(key_del) = &parser_events_del[0] else {
        panic!("Expected Key event");
    };
    let Event::Key(key_bs) = &parser_events_bs[0] else {
        panic!("Expected Key event");
    };

    assert_eq!(key_del.code, KeyCode::Backspace);
    assert_eq!(key_bs.code, KeyCode::Backspace);

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Backspace,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::Backspace);
}

#[test]
fn parity_tab_key() {
    let mut parser = InputParser::new();

    // InputParser: 0x09 (HT) maps to Tab
    let parser_events = parser.parse(b"\x09");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Tab);
    assert_eq!(parser_key.modifiers, Modifiers::NONE);

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Tab,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::Tab);
}

#[test]
fn parity_backtab_key() {
    let mut parser = InputParser::new();

    // InputParser: CSI Z (ESC [ Z) maps to BackTab
    let parser_events = parser.parse(b"\x1b[Z");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::BackTab);

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::BackTab,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::BackTab);

    // Verify parity
    assert_eq!(
        parser_key.code, crossterm_key.code,
        "KeyCode mismatch for BackTab"
    );
}

#[test]
fn parity_null_key() {
    let mut parser = InputParser::new();

    // InputParser: 0x00 (NUL) maps to Null
    let parser_events = parser.parse(b"\x00");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Null);
    assert_eq!(parser_key.modifiers, Modifiers::NONE);

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Null,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::Null);

    // Verify parity
    assert_eq!(
        parser_key.code, crossterm_key.code,
        "KeyCode mismatch for Null"
    );
}

// ============================================================================
// Arrow Keys
// ============================================================================

#[test]
fn parity_arrow_keys() {
    let mut parser = InputParser::new();

    let test_cases = [
        (b"\x1b[A".as_slice(), cte::KeyCode::Up, KeyCode::Up),
        (b"\x1b[B".as_slice(), cte::KeyCode::Down, KeyCode::Down),
        (b"\x1b[C".as_slice(), cte::KeyCode::Right, KeyCode::Right),
        (b"\x1b[D".as_slice(), cte::KeyCode::Left, KeyCode::Left),
    ];

    for (raw_bytes, ct_code, expected_code) in test_cases {
        // InputParser path
        let parser_events = parser.parse(raw_bytes);
        assert_eq!(
            parser_events.len(),
            1,
            "Expected 1 event for {:?}",
            raw_bytes
        );
        let Event::Key(parser_key) = &parser_events[0] else {
            panic!("Expected Key event for {:?}", raw_bytes);
        };
        assert_eq!(parser_key.code, expected_code);

        // Crossterm path
        let ct_event = ct_key(ct_code, cte::KeyModifiers::NONE, cte::KeyEventKind::Press);
        let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
        let Event::Key(crossterm_key) = crossterm_event else {
            panic!("Expected Key event");
        };
        assert_eq!(crossterm_key.code, expected_code);

        // Verify parity
        assert_eq!(parser_key.code, crossterm_key.code, "KeyCode mismatch");
    }
}

// ============================================================================
// Modifiers
// ============================================================================

#[test]
fn parity_ctrl_c() {
    let mut parser = InputParser::new();

    // InputParser: 0x03 (ETX) maps to Ctrl+C
    let parser_events = parser.parse(b"\x03");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Char('c'));
    assert!(parser_key.modifiers.contains(Modifiers::CTRL));

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Char('c'),
        cte::KeyModifiers::CONTROL,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::Char('c'));
    assert!(crossterm_key.modifiers.contains(Modifiers::CTRL));
}

#[test]
fn parity_arrow_with_shift() {
    let mut parser = InputParser::new();

    // InputParser: CSI 1;2 A = Shift+Up (modifier 2 = 1+1 = Shift)
    let parser_events = parser.parse(b"\x1b[1;2A");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Up);
    assert!(parser_key.modifiers.contains(Modifiers::SHIFT));

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Up,
        cte::KeyModifiers::SHIFT,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::Up);
    assert!(crossterm_key.modifiers.contains(Modifiers::SHIFT));
}

#[test]
fn parity_arrow_with_ctrl_alt() {
    let mut parser = InputParser::new();

    // InputParser: CSI 1;7 A = Ctrl+Alt+Up (modifier 7 = 1+2+4 = Shift+Alt+Ctrl, but 7-1=6=Alt+Ctrl)
    // Actually: xterm encoding is 1+bits, so 7 = 1+(1+2+4) = 1+7, meaning bits=6 = Alt(2)+Ctrl(4)
    let parser_events = parser.parse(b"\x1b[1;7A");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Up);
    assert!(parser_key.modifiers.contains(Modifiers::ALT));
    assert!(parser_key.modifiers.contains(Modifiers::CTRL));

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Up,
        cte::KeyModifiers::ALT | cte::KeyModifiers::CONTROL,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::Up);
    assert!(crossterm_key.modifiers.contains(Modifiers::ALT));
    assert!(crossterm_key.modifiers.contains(Modifiers::CTRL));
}

// ============================================================================
// Mouse Events
// ============================================================================

#[test]
fn parity_mouse_click() {
    let mut parser = InputParser::new();

    // InputParser: SGR mouse protocol CSI < 0 ; 10 ; 5 M = Left button down at (10,5)
    // Note: SGR coordinates are 1-indexed, InputParser converts to 0-indexed
    let parser_events = parser.parse(b"\x1b[<0;10;5M");
    assert_eq!(parser_events.len(), 1);
    let Event::Mouse(parser_mouse) = &parser_events[0] else {
        panic!("Expected Mouse event");
    };
    assert!(matches!(
        parser_mouse.kind,
        MouseEventKind::Down(MouseButton::Left)
    ));
    assert_eq!(parser_mouse.x, 9); // 0-indexed
    assert_eq!(parser_mouse.y, 4); // 0-indexed

    // Crossterm path (already 0-indexed)
    let ct_event = ct_mouse(
        cte::MouseEventKind::Down(cte::MouseButton::Left),
        9,
        4,
        cte::KeyModifiers::NONE,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Mouse(crossterm_mouse) = crossterm_event else {
        panic!("Expected Mouse event");
    };
    assert!(matches!(
        crossterm_mouse.kind,
        MouseEventKind::Down(MouseButton::Left)
    ));
    assert_eq!(crossterm_mouse.x, 9);
    assert_eq!(crossterm_mouse.y, 4);
}

#[test]
fn parity_mouse_scroll_up() {
    let mut parser = InputParser::new();

    // InputParser: SGR scroll up is button code 64 (bit 6 set, direction 0)
    let parser_events = parser.parse(b"\x1b[<64;1;1M");
    assert_eq!(parser_events.len(), 1);
    let Event::Mouse(parser_mouse) = &parser_events[0] else {
        panic!("Expected Mouse event");
    };
    assert!(matches!(parser_mouse.kind, MouseEventKind::ScrollUp));

    // Crossterm path
    let ct_event = ct_mouse(cte::MouseEventKind::ScrollUp, 0, 0, cte::KeyModifiers::NONE);
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Mouse(crossterm_mouse) = crossterm_event else {
        panic!("Expected Mouse event");
    };
    assert!(matches!(crossterm_mouse.kind, MouseEventKind::ScrollUp));
}

#[test]
fn parity_mouse_scroll_down() {
    let mut parser = InputParser::new();

    // InputParser: SGR scroll down is button code 65 (bit 6 set, direction 1)
    let parser_events = parser.parse(b"\x1b[<65;1;1M");
    assert_eq!(parser_events.len(), 1);
    let Event::Mouse(parser_mouse) = &parser_events[0] else {
        panic!("Expected Mouse event");
    };
    assert!(matches!(parser_mouse.kind, MouseEventKind::ScrollDown));

    // Crossterm path
    let ct_event = ct_mouse(
        cte::MouseEventKind::ScrollDown,
        0,
        0,
        cte::KeyModifiers::NONE,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Mouse(crossterm_mouse) = crossterm_event else {
        panic!("Expected Mouse event");
    };
    assert!(matches!(crossterm_mouse.kind, MouseEventKind::ScrollDown));
}

#[test]
fn parity_mouse_drag() {
    let mut parser = InputParser::new();

    // InputParser: SGR drag is button code with bit 5 set
    // Left button drag = 32 (bit 5) + 0 (left) = 32
    let parser_events = parser.parse(b"\x1b[<32;10;10M");
    assert_eq!(parser_events.len(), 1);
    let Event::Mouse(parser_mouse) = &parser_events[0] else {
        panic!("Expected Mouse event");
    };
    assert!(matches!(
        parser_mouse.kind,
        MouseEventKind::Drag(MouseButton::Left)
    ));

    // Crossterm path
    let ct_event = ct_mouse(
        cte::MouseEventKind::Drag(cte::MouseButton::Left),
        9,
        9,
        cte::KeyModifiers::NONE,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Mouse(crossterm_mouse) = crossterm_event else {
        panic!("Expected Mouse event");
    };
    assert!(matches!(
        crossterm_mouse.kind,
        MouseEventKind::Drag(MouseButton::Left)
    ));
}

#[test]
fn parity_mouse_with_shift() {
    let mut parser = InputParser::new();

    // InputParser: SGR with Shift modifier = bit 2 (value 4)
    // Left button down + Shift = 0 + 4 = 4
    let parser_events = parser.parse(b"\x1b[<4;1;1M");
    assert_eq!(parser_events.len(), 1);
    let Event::Mouse(parser_mouse) = &parser_events[0] else {
        panic!("Expected Mouse event");
    };
    assert!(matches!(
        parser_mouse.kind,
        MouseEventKind::Down(MouseButton::Left)
    ));
    assert!(parser_mouse.modifiers.contains(Modifiers::SHIFT));

    // Crossterm path
    let ct_event = ct_mouse(
        cte::MouseEventKind::Down(cte::MouseButton::Left),
        0,
        0,
        cte::KeyModifiers::SHIFT,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Mouse(crossterm_mouse) = crossterm_event else {
        panic!("Expected Mouse event");
    };
    assert!(crossterm_mouse.modifiers.contains(Modifiers::SHIFT));
}

// ============================================================================
// Kitty Keyboard Protocol
// ============================================================================

#[test]
fn parity_kitty_keyboard_basic() {
    let mut parser = InputParser::new();

    // InputParser: CSI 97 u = 'a' (unicode codepoint 97)
    let parser_events = parser.parse(b"\x1b[97u");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Char('a'));
    assert_eq!(parser_key.kind, KeyEventKind::Press);
}

#[test]
fn parity_kitty_keyboard_with_modifiers() {
    let mut parser = InputParser::new();

    // InputParser: CSI 97;5 u = Ctrl+a (modifier 5 = 1+4 = Ctrl)
    let parser_events = parser.parse(b"\x1b[97;5u");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Char('a'));
    assert!(parser_key.modifiers.contains(Modifiers::CTRL));
}

#[test]
fn parity_kitty_keyboard_release() {
    let mut parser = InputParser::new();

    // InputParser: CSI 97;1:3 u = 'a' release (event type 3)
    let parser_events = parser.parse(b"\x1b[97;1:3u");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Char('a'));
    assert_eq!(parser_key.kind, KeyEventKind::Release);

    // Crossterm path (when in kitty mode)
    let ct_event = ct_key(
        cte::KeyCode::Char('a'),
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Release,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.kind, KeyEventKind::Release);
}

#[test]
fn parity_kitty_keyboard_repeat() {
    let mut parser = InputParser::new();

    // InputParser: CSI 97;1:2 u = 'a' repeat (event type 2)
    let parser_events = parser.parse(b"\x1b[97;1:2u");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Char('a'));
    assert_eq!(parser_key.kind, KeyEventKind::Repeat);

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Char('a'),
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Repeat,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.kind, KeyEventKind::Repeat);
}

// ============================================================================
// Function Keys
// ============================================================================

#[test]
fn parity_function_keys() {
    let mut parser = InputParser::new();

    // F1-F4 use SS3 sequences
    let f1_events = parser.parse(b"\x1bOP");
    assert_eq!(f1_events.len(), 1);
    let Event::Key(f1_key) = &f1_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(f1_key.code, KeyCode::F(1));

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::F(1),
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::F(1));
}

#[test]
fn parity_function_keys_f5_f12() {
    let mut parser = InputParser::new();

    // F5+ use CSI ~ sequences
    let test_cases = [
        (b"\x1b[15~".as_slice(), 5),
        (b"\x1b[17~".as_slice(), 6),
        (b"\x1b[18~".as_slice(), 7),
        (b"\x1b[19~".as_slice(), 8),
        (b"\x1b[20~".as_slice(), 9),
        (b"\x1b[21~".as_slice(), 10),
        (b"\x1b[23~".as_slice(), 11),
        (b"\x1b[24~".as_slice(), 12),
    ];

    for (raw_bytes, expected_fn) in test_cases {
        let parser_events = parser.parse(raw_bytes);
        assert_eq!(
            parser_events.len(),
            1,
            "Expected 1 event for F{}",
            expected_fn
        );
        let Event::Key(parser_key) = &parser_events[0] else {
            panic!("Expected Key event for F{}", expected_fn);
        };
        assert_eq!(parser_key.code, KeyCode::F(expected_fn));

        // Crossterm path
        let ct_event = ct_key(
            cte::KeyCode::F(expected_fn),
            cte::KeyModifiers::NONE,
            cte::KeyEventKind::Press,
        );
        let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
        let Event::Key(crossterm_key) = crossterm_event else {
            panic!("Expected Key event");
        };
        assert_eq!(crossterm_key.code, KeyCode::F(expected_fn));
    }
}

// ============================================================================
// Focus Events
// ============================================================================

#[test]
fn parity_focus_events() {
    let mut parser = InputParser::new();

    // InputParser: CSI I = focus gained, CSI O = focus lost
    let focus_gained = parser.parse(b"\x1b[I");
    assert_eq!(focus_gained.len(), 1);
    assert!(matches!(focus_gained[0], Event::Focus(true)));

    let focus_lost = parser.parse(b"\x1b[O");
    assert_eq!(focus_lost.len(), 1);
    assert!(matches!(focus_lost[0], Event::Focus(false)));

    // Crossterm path
    let ct_gained = cte::Event::FocusGained;
    let ct_lost = cte::Event::FocusLost;

    assert!(matches!(
        Event::from_crossterm(ct_gained),
        Some(Event::Focus(true))
    ));
    assert!(matches!(
        Event::from_crossterm(ct_lost),
        Some(Event::Focus(false))
    ));
}

// ============================================================================
// Paste Events
// ============================================================================

#[test]
fn parity_paste_events() {
    let mut parser = InputParser::new();

    // InputParser: Bracketed paste CSI 200 ~ ... CSI 201 ~
    let paste_events = parser.parse(b"\x1b[200~hello world\x1b[201~");
    assert_eq!(paste_events.len(), 1);
    let Event::Paste(parser_paste) = &paste_events[0] else {
        panic!("Expected Paste event");
    };
    assert_eq!(parser_paste.text, "hello world");
    assert!(parser_paste.bracketed);

    // Crossterm path
    let ct_event = cte::Event::Paste("hello world".to_string());
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Paste(crossterm_paste) = crossterm_event else {
        panic!("Expected Paste event");
    };
    assert_eq!(crossterm_paste.text, "hello world");
}

// ============================================================================
// Edge Cases and Known Differences
// ============================================================================

// ============================================================================
// Navigation Keys (Home, End, Insert, Delete, Page Up/Down)
// ============================================================================

#[test]
fn parity_home_end_keys() {
    let mut parser = InputParser::new();

    // Home via CSI H
    let home_events = parser.parse(b"\x1b[H");
    assert_eq!(home_events.len(), 1);
    let Event::Key(home_key) = &home_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(home_key.code, KeyCode::Home);

    // End via CSI F
    let end_events = parser.parse(b"\x1b[F");
    assert_eq!(end_events.len(), 1);
    let Event::Key(end_key) = &end_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(end_key.code, KeyCode::End);

    // Also test CSI 1~ and CSI 4~ variants
    let home_tilde = parser.parse(b"\x1b[1~");
    assert_eq!(home_tilde.len(), 1);
    let Event::Key(ht_key) = &home_tilde[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(ht_key.code, KeyCode::Home);

    let end_tilde = parser.parse(b"\x1b[4~");
    assert_eq!(end_tilde.len(), 1);
    let Event::Key(et_key) = &end_tilde[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(et_key.code, KeyCode::End);

    // Crossterm path
    let ct_home = ct_key(
        cte::KeyCode::Home,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let ct_end = ct_key(
        cte::KeyCode::End,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );

    let crossterm_home = Event::from_crossterm(ct_home).expect("should map");
    let crossterm_end = Event::from_crossterm(ct_end).expect("should map");

    let Event::Key(ch_key) = crossterm_home else {
        panic!("Expected Key event");
    };
    let Event::Key(ce_key) = crossterm_end else {
        panic!("Expected Key event");
    };
    assert_eq!(ch_key.code, KeyCode::Home);
    assert_eq!(ce_key.code, KeyCode::End);
}

#[test]
fn parity_insert_delete_keys() {
    let mut parser = InputParser::new();

    // Insert via CSI 2~
    let insert_events = parser.parse(b"\x1b[2~");
    assert_eq!(insert_events.len(), 1);
    let Event::Key(insert_key) = &insert_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(insert_key.code, KeyCode::Insert);

    // Delete via CSI 3~
    let delete_events = parser.parse(b"\x1b[3~");
    assert_eq!(delete_events.len(), 1);
    let Event::Key(delete_key) = &delete_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(delete_key.code, KeyCode::Delete);

    // Crossterm path
    let ct_insert = ct_key(
        cte::KeyCode::Insert,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let ct_delete = ct_key(
        cte::KeyCode::Delete,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );

    let crossterm_insert = Event::from_crossterm(ct_insert).expect("should map");
    let crossterm_delete = Event::from_crossterm(ct_delete).expect("should map");

    let Event::Key(ci_key) = crossterm_insert else {
        panic!("Expected Key event");
    };
    let Event::Key(cd_key) = crossterm_delete else {
        panic!("Expected Key event");
    };
    assert_eq!(ci_key.code, KeyCode::Insert);
    assert_eq!(cd_key.code, KeyCode::Delete);
}

#[test]
fn parity_page_up_down_keys() {
    let mut parser = InputParser::new();

    // PageUp via CSI 5~
    let pageup_events = parser.parse(b"\x1b[5~");
    assert_eq!(pageup_events.len(), 1);
    let Event::Key(pageup_key) = &pageup_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(pageup_key.code, KeyCode::PageUp);

    // PageDown via CSI 6~
    let pagedown_events = parser.parse(b"\x1b[6~");
    assert_eq!(pagedown_events.len(), 1);
    let Event::Key(pagedown_key) = &pagedown_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(pagedown_key.code, KeyCode::PageDown);

    // Crossterm path
    let ct_pageup = ct_key(
        cte::KeyCode::PageUp,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );
    let ct_pagedown = ct_key(
        cte::KeyCode::PageDown,
        cte::KeyModifiers::NONE,
        cte::KeyEventKind::Press,
    );

    let crossterm_pageup = Event::from_crossterm(ct_pageup).expect("should map");
    let crossterm_pagedown = Event::from_crossterm(ct_pagedown).expect("should map");

    let Event::Key(cpu_key) = crossterm_pageup else {
        panic!("Expected Key event");
    };
    let Event::Key(cpd_key) = crossterm_pagedown else {
        panic!("Expected Key event");
    };
    assert_eq!(cpu_key.code, KeyCode::PageUp);
    assert_eq!(cpd_key.code, KeyCode::PageDown);
}

// ============================================================================
// Super/Meta/Hyper Modifier Mapping
// ============================================================================

#[test]
fn parity_super_modifier_variants() {
    // Crossterm maps SUPER, HYPER, and META all to ftui's SUPER
    let ct_super = ct_key(
        cte::KeyCode::Char('a'),
        cte::KeyModifiers::SUPER,
        cte::KeyEventKind::Press,
    );
    let ct_hyper = ct_key(
        cte::KeyCode::Char('a'),
        cte::KeyModifiers::HYPER,
        cte::KeyEventKind::Press,
    );
    let ct_meta = ct_key(
        cte::KeyCode::Char('a'),
        cte::KeyModifiers::META,
        cte::KeyEventKind::Press,
    );

    let super_event = Event::from_crossterm(ct_super).expect("should map");
    let hyper_event = Event::from_crossterm(ct_hyper).expect("should map");
    let meta_event = Event::from_crossterm(ct_meta).expect("should map");

    let Event::Key(super_key) = super_event else {
        panic!("Expected Key event");
    };
    let Event::Key(hyper_key) = hyper_event else {
        panic!("Expected Key event");
    };
    let Event::Key(meta_key) = meta_event else {
        panic!("Expected Key event");
    };

    // All three should map to SUPER
    assert!(super_key.modifiers.contains(Modifiers::SUPER));
    assert!(hyper_key.modifiers.contains(Modifiers::SUPER));
    assert!(meta_key.modifiers.contains(Modifiers::SUPER));
}

#[test]
fn parity_super_modifier_input_parser() {
    let mut parser = InputParser::new();

    // InputParser: xterm modifier encoding for Super is bit 3 (value 8)
    // CSI 1;9 A = Super+Up (modifier 9 = 1+8 = Super)
    let parser_events = parser.parse(b"\x1b[1;9A");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Up);
    assert!(parser_key.modifiers.contains(Modifiers::SUPER));

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Up,
        cte::KeyModifiers::SUPER,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert!(crossterm_key.modifiers.contains(Modifiers::SUPER));
}

// ============================================================================
// SS3 Sequences (Alternative Arrow Key Encoding)
// ============================================================================

#[test]
fn parity_ss3_arrow_keys() {
    let mut parser = InputParser::new();

    // Some terminals send SS3 sequences for arrow keys
    // ESC O A = Up, ESC O B = Down, ESC O C = Right, ESC O D = Left
    let up_events = parser.parse(b"\x1bOA");
    assert_eq!(up_events.len(), 1);
    let Event::Key(up_key) = &up_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(up_key.code, KeyCode::Up);

    let down_events = parser.parse(b"\x1bOB");
    let Event::Key(down_key) = &down_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(down_key.code, KeyCode::Down);

    let right_events = parser.parse(b"\x1bOC");
    let Event::Key(right_key) = &right_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(right_key.code, KeyCode::Right);

    let left_events = parser.parse(b"\x1bOD");
    let Event::Key(left_key) = &left_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(left_key.code, KeyCode::Left);
}

#[test]
fn parity_ss3_home_end() {
    let mut parser = InputParser::new();

    // ESC O H = Home, ESC O F = End
    let home_events = parser.parse(b"\x1bOH");
    assert_eq!(home_events.len(), 1);
    let Event::Key(home_key) = &home_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(home_key.code, KeyCode::Home);

    let end_events = parser.parse(b"\x1bOF");
    assert_eq!(end_events.len(), 1);
    let Event::Key(end_key) = &end_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(end_key.code, KeyCode::End);
}

// ============================================================================
// Edge Cases and Known Differences
// ============================================================================

#[test]
fn document_alt_letter_handling() {
    let mut parser = InputParser::new();

    // InputParser: ESC followed by letter = Alt+letter
    let parser_events = parser.parse(b"\x1bx");
    assert_eq!(parser_events.len(), 1);
    let Event::Key(parser_key) = &parser_events[0] else {
        panic!("Expected Key event");
    };
    assert_eq!(parser_key.code, KeyCode::Char('x'));
    assert!(parser_key.modifiers.contains(Modifiers::ALT));

    // Crossterm path
    let ct_event = ct_key(
        cte::KeyCode::Char('x'),
        cte::KeyModifiers::ALT,
        cte::KeyEventKind::Press,
    );
    let crossterm_event = Event::from_crossterm(ct_event).expect("should map");
    let Event::Key(crossterm_key) = crossterm_event else {
        panic!("Expected Key event");
    };
    assert_eq!(crossterm_key.code, KeyCode::Char('x'));
    assert!(crossterm_key.modifiers.contains(Modifiers::ALT));
}

//! Property-based invariant tests for the web input subsystem.
//!
//! Verifies:
//! 1.  JSON roundtrip: any InputEvent survives to_json_string → from_json_str
//! 2.  KeyCode roundtrip: to_code_string → from_code_string for known codes
//! 3.  MouseButton roundtrip: from_u8(to_u8(x)) == x for canonical buttons
//! 4.  Modifier bits never exceed the 4-bit mask
//! 5.  CompositionState: rewrite of non-composition events passes through when inactive
//! 6.  CompositionState: key events are dropped while composition is active
//! 7.  CompositionState: End/Cancel always leaves state inactive
//! 8.  VT encoder: Key-up events produce empty bytes in legacy mode
//! 9.  VT encoder: mouse events are empty when sgr_mouse is disabled
//! 10. VT encoder: focus events are empty when focus_events is disabled
//! 11. VT encoder: Ctrl+letter always produces exactly one byte (0x01..=0x1a)
//! 12. VT encoder: Kitty encoding always produces ESC[ prefix for known keys
//! 13. normalize_dom_key_code: Shift+Tab always produces BackTab
//! 14. encode_paste_text: bracketed paste wraps with ESC[200~ / ESC[201~
//! 15. Determinism: same event + features → same VT output
//! 16. CompositionState: double-start synthesizes cancel before second start

use frankenterm_web::input::{
    encode_paste_text, encode_vt_input_event, normalize_dom_key_code, CompositionInput,
    CompositionPhase, CompositionState, FocusInput, InputEvent, KeyCode, KeyInput, KeyPhase,
    Modifiers, MouseButton, MouseInput, MousePhase, TouchInput, TouchPhase, TouchPoint,
    VtInputEncoderFeatures, WheelInput,
};
use proptest::prelude::*;

// ── Strategy helpers ──────────────────────────────────────────────────

fn arb_modifiers() -> impl Strategy<Value = Modifiers> {
    (0u8..=15).prop_map(Modifiers::from_bits_truncate_u8)
}

fn arb_key_phase() -> impl Strategy<Value = KeyPhase> {
    prop_oneof![Just(KeyPhase::Down), Just(KeyPhase::Up),]
}

fn arb_mouse_phase() -> impl Strategy<Value = MousePhase> {
    prop_oneof![
        Just(MousePhase::Down),
        Just(MousePhase::Up),
        Just(MousePhase::Move),
        Just(MousePhase::Drag),
    ]
}

fn arb_mouse_button() -> impl Strategy<Value = Option<MouseButton>> {
    prop_oneof![
        Just(None),
        Just(Some(MouseButton::Left)),
        Just(Some(MouseButton::Middle)),
        Just(Some(MouseButton::Right)),
        (3u8..=255).prop_map(|n| Some(MouseButton::Other(n))),
    ]
}

fn arb_known_key_code() -> impl Strategy<Value = KeyCode> {
    prop_oneof![
        any::<char>()
            .prop_filter("printable", |c| !c.is_control())
            .prop_map(KeyCode::Char),
        Just(KeyCode::Enter),
        Just(KeyCode::Escape),
        Just(KeyCode::Backspace),
        Just(KeyCode::Tab),
        Just(KeyCode::BackTab),
        Just(KeyCode::Delete),
        Just(KeyCode::Insert),
        Just(KeyCode::Home),
        Just(KeyCode::End),
        Just(KeyCode::PageUp),
        Just(KeyCode::PageDown),
        Just(KeyCode::Up),
        Just(KeyCode::Down),
        Just(KeyCode::Left),
        Just(KeyCode::Right),
        (1u8..=24).prop_map(KeyCode::F),
    ]
}

fn arb_key_input() -> impl Strategy<Value = KeyInput> {
    (arb_key_phase(), arb_known_key_code(), arb_modifiers(), any::<bool>()).prop_map(
        |(phase, code, mods, repeat)| KeyInput {
            phase,
            code,
            mods,
            repeat,
        },
    )
}

fn arb_mouse_input() -> impl Strategy<Value = MouseInput> {
    (
        arb_mouse_phase(),
        arb_mouse_button(),
        0u16..=500,
        0u16..=500,
        arb_modifiers(),
    )
        .prop_map(|(phase, button, x, y, mods)| MouseInput {
            phase,
            button,
            x,
            y,
            mods,
        })
}

fn arb_wheel_input() -> impl Strategy<Value = WheelInput> {
    (0u16..=500, 0u16..=500, -16i16..=16, -16i16..=16, arb_modifiers()).prop_map(
        |(x, y, dx, dy, mods)| WheelInput { x, y, dx, dy, mods },
    )
}

fn arb_composition_phase() -> impl Strategy<Value = CompositionPhase> {
    prop_oneof![
        Just(CompositionPhase::Start),
        Just(CompositionPhase::Update),
        Just(CompositionPhase::End),
        Just(CompositionPhase::Cancel),
    ]
}

fn arb_input_event() -> impl Strategy<Value = InputEvent> {
    prop_oneof![
        arb_key_input().prop_map(InputEvent::Key),
        arb_mouse_input().prop_map(InputEvent::Mouse),
        arb_wheel_input().prop_map(InputEvent::Wheel),
        (arb_composition_phase(), proptest::option::of("[a-z]{0,5}")).prop_map(|(phase, data)| {
            InputEvent::Composition(CompositionInput {
                phase,
                data: data.map(Into::into),
            })
        }),
        any::<bool>().prop_map(|focused| InputEvent::Focus(FocusInput { focused })),
    ]
}

fn arb_features() -> impl Strategy<Value = VtInputEncoderFeatures> {
    (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
        |(sgr_mouse, bracketed_paste, focus_events, kitty_keyboard)| VtInputEncoderFeatures {
            sgr_mouse,
            bracketed_paste,
            focus_events,
            kitty_keyboard,
        },
    )
}

// ═════════════════════════════════════════════════════════════════════════
// 1. JSON roundtrip
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn json_roundtrip(event in arb_input_event()) {
        let json = event.to_json_string().expect("serialize");
        let back = InputEvent::from_json_str(&json).expect("deserialize");
        prop_assert_eq!(event, back, "roundtrip failed for JSON: {}", json);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. KeyCode roundtrip: to_code_string → from_code_string
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn key_code_roundtrip(code in arb_known_key_code()) {
        let s = code.to_code_string();
        let back = KeyCode::from_code_string(&s, None, None);
        prop_assert_eq!(code, back, "KeyCode roundtrip failed for string: {}", s);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. MouseButton roundtrip
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn mouse_button_canonical_roundtrip(n in 0u8..=2) {
        let button = MouseButton::from_u8(n);
        prop_assert_eq!(button.to_u8(), n);
        prop_assert_eq!(MouseButton::from_u8(button.to_u8()), button);
    }

    #[test]
    fn mouse_button_other_roundtrip(n in 3u8..=255) {
        let button = MouseButton::from_u8(n);
        prop_assert_eq!(button.to_u8(), n);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. Modifier bits never exceed 4-bit mask
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn modifier_bits_bounded(raw in any::<u8>()) {
        let mods = Modifiers::from_bits_truncate_u8(raw);
        prop_assert!(mods.bits() <= 0b1111, "modifier bits {} > 15", mods.bits());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. CompositionState: non-composition events pass through when inactive
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn non_composition_passthrough_when_inactive(
        key in arb_key_input(),
        mouse in arb_mouse_input(),
    ) {
        let mut state = CompositionState::default();
        prop_assert!(!state.is_active());

        let key_events: Vec<InputEvent> = state
            .rewrite(InputEvent::Key(key.clone()))
            .into_events()
            .collect();
        prop_assert_eq!(key_events, vec![InputEvent::Key(key)]);

        let mouse_events: Vec<InputEvent> = state
            .rewrite(InputEvent::Mouse(mouse))
            .into_events()
            .collect();
        prop_assert_eq!(mouse_events, vec![InputEvent::Mouse(mouse)]);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. CompositionState: key events dropped while active
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn keys_dropped_while_composing(key in arb_key_input()) {
        let mut state = CompositionState::default();
        // Start composition
        let _ = state.rewrite(InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Start,
            data: None,
        }));
        prop_assert!(state.is_active());

        let events: Vec<InputEvent> = state
            .rewrite(InputEvent::Key(key))
            .into_events()
            .collect();
        prop_assert!(events.is_empty(), "key events should be dropped during composition");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. CompositionState: End/Cancel always leaves state inactive
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn end_cancel_deactivates(was_active in any::<bool>()) {
        for phase in [CompositionPhase::End, CompositionPhase::Cancel] {
            let mut state = CompositionState::default();
            if was_active {
                let _ = state.rewrite(InputEvent::Composition(CompositionInput {
                    phase: CompositionPhase::Start,
                    data: None,
                }));
            }
            let _ = state.rewrite(InputEvent::Composition(CompositionInput {
                phase,
                data: None,
            }));
            prop_assert!(
                !state.is_active(),
                "state should be inactive after {:?}",
                phase
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. VT encoder: key-up produces empty in legacy mode
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn key_up_empty_in_legacy(code in arb_known_key_code(), mods in arb_modifiers()) {
        let event = InputEvent::Key(KeyInput {
            phase: KeyPhase::Up,
            code,
            mods,
            repeat: false,
        });
        let features = VtInputEncoderFeatures::default(); // legacy mode
        let encoded = encode_vt_input_event(&event, features);
        prop_assert!(
            encoded.is_empty(),
            "legacy key-up should produce empty bytes, got {} bytes",
            encoded.len()
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. VT encoder: mouse events empty when sgr_mouse disabled
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn mouse_empty_without_sgr(mouse in arb_mouse_input()) {
        let features = VtInputEncoderFeatures {
            sgr_mouse: false,
            ..VtInputEncoderFeatures::default()
        };
        let encoded = encode_vt_input_event(&InputEvent::Mouse(mouse), features);
        prop_assert!(encoded.is_empty(), "mouse should be empty without sgr_mouse");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. VT encoder: focus events empty when focus_events disabled
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn focus_empty_without_feature(focused in any::<bool>()) {
        let features = VtInputEncoderFeatures {
            focus_events: false,
            ..VtInputEncoderFeatures::default()
        };
        let event = InputEvent::Focus(FocusInput { focused });
        let encoded = encode_vt_input_event(&event, features);
        prop_assert!(encoded.is_empty(), "focus should be empty without feature");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. VT encoder: Ctrl+letter → single byte in range 0x01..=0x1a
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn ctrl_letter_produces_single_byte(idx in 0u8..26) {
        let ch = (b'a' + idx) as char;
        let event = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Char(ch),
            mods: Modifiers::CTRL,
            repeat: false,
        });
        let encoded = encode_vt_input_event(&event, VtInputEncoderFeatures::default());
        prop_assert_eq!(encoded.len(), 1, "Ctrl+{} should produce 1 byte", ch);
        let byte = encoded[0];
        prop_assert!(
            (0x01..=0x1a).contains(&byte),
            "Ctrl+{} produced byte {:#04x}, expected 0x01..=0x1a",
            ch,
            byte
        );
        // Verify exact mapping: 'a' → 1, 'b' → 2, ..., 'z' → 26
        let expected = (u32::from(ch) as u8) - b'a' + 1;
        prop_assert_eq!(byte, expected);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. VT encoder: Kitty encoding starts with ESC[ for known keys
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn kitty_starts_with_csi(key in arb_key_input()) {
        let features = VtInputEncoderFeatures {
            kitty_keyboard: true,
            ..VtInputEncoderFeatures::default()
        };
        let encoded = encode_vt_input_event(&InputEvent::Key(key.clone()), features);
        // Unidentified keys produce empty output; everything else should be CSI
        if !encoded.is_empty() {
            prop_assert!(
                encoded.len() >= 3
                    && encoded[0] == 0x1b
                    && encoded[1] == b'[',
                "kitty encoding for {:?} should start with ESC[, got {:?}",
                key.code,
                &encoded[..encoded.len().min(5)]
            );
            // Must end with 'u' for kitty protocol
            prop_assert_eq!(
                *encoded.last().unwrap(),
                b'u',
                "kitty encoding should end with 'u'"
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. normalize_dom_key_code: Shift+Tab → BackTab
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn shift_tab_always_backtab(dom_code in "Tab|Key[A-Z]|Digit[0-9]") {
        let mods = Modifiers::SHIFT;
        let result = normalize_dom_key_code("Tab", &dom_code, mods);
        prop_assert_eq!(result, KeyCode::BackTab, "Shift+Tab must yield BackTab");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. encode_paste_text: bracketed wraps correctly
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn bracketed_paste_wraps(text in ".{1,100}") {
        let encoded = encode_paste_text(&text, true);
        prop_assert!(
            encoded.starts_with(b"\x1b[200~"),
            "bracketed paste should start with ESC[200~"
        );
        prop_assert!(
            encoded.ends_with(b"\x1b[201~"),
            "bracketed paste should end with ESC[201~"
        );
        // Inner content should match original bytes
        let inner = &encoded[6..encoded.len() - 6];
        prop_assert_eq!(inner, text.as_bytes());
    }

    #[test]
    fn plain_paste_no_brackets(text in ".{1,100}") {
        let encoded = encode_paste_text(&text, false);
        prop_assert_eq!(encoded, text.as_bytes().to_vec());
    }

    #[test]
    fn empty_paste_always_empty(bracketed in any::<bool>()) {
        let encoded = encode_paste_text("", bracketed);
        prop_assert!(encoded.is_empty());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 15. Determinism: same event + features → same output
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn vt_encoding_deterministic(event in arb_input_event(), features in arb_features()) {
        let a = encode_vt_input_event(&event, features);
        let b = encode_vt_input_event(&event, features);
        prop_assert_eq!(a, b, "VT encoding must be deterministic");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 16. CompositionState: double-start synthesizes cancel
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn double_start_synthesizes_cancel(data in proptest::option::of("[a-z]{0,5}")) {
        let mut state = CompositionState::default();
        // First start
        let _ = state.rewrite(InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Start,
            data: None,
        }));
        prop_assert!(state.is_active());

        // Second start should synthesize cancel
        let events: Vec<InputEvent> = state
            .rewrite(InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Start,
                data: data.clone().map(Into::into),
            }))
            .into_events()
            .collect();

        prop_assert_eq!(events.len(), 2, "double-start should emit 2 events");
        // First event should be synthetic cancel
        prop_assert_eq!(
            &events[0],
            &InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Cancel,
                data: None,
            })
        );
        // Second event should be the new start
        prop_assert_eq!(
            &events[1],
            &InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Start,
                data: data.map(Into::into),
            })
        );
        prop_assert!(state.is_active());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 17. Touch events always produce empty VT output
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn touch_events_produce_no_vt(
        phase in prop_oneof![
            Just(TouchPhase::Start),
            Just(TouchPhase::Move),
            Just(TouchPhase::End),
            Just(TouchPhase::Cancel),
        ],
        features in arb_features(),
    ) {
        let event = InputEvent::Touch(TouchInput {
            phase,
            touches: vec![TouchPoint { id: 0, x: 10, y: 20 }],
            mods: Modifiers::empty(),
        });
        let encoded = encode_vt_input_event(&event, features);
        prop_assert!(encoded.is_empty(), "touch events should never produce VT bytes");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 18. SGR mouse encoding always starts with ESC[< when enabled
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn sgr_mouse_starts_with_csi_lt(mouse in arb_mouse_input()) {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let encoded = encode_vt_input_event(&InputEvent::Mouse(mouse), features);
        prop_assert!(!encoded.is_empty(), "mouse with sgr_mouse should produce output");
        prop_assert!(
            encoded.starts_with(b"\x1b[<"),
            "SGR mouse should start with ESC[<, got {:?}",
            &encoded[..encoded.len().min(5)]
        );
    }
}

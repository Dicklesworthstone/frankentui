#![forbid(unsafe_code)]

//! JSON input parser for converting frankenterm-web encoded inputs to
//! [`ftui_core::event::Event`] values.
//!
//! This module provides [`parse_encoded_input_to_event`], which accepts a JSON
//! string produced by `frankenterm-web`'s `InputEvent::to_json_string()` and
//! returns the corresponding terminal event. Events without a direct `Event`
//! mapping (e.g., accessibility, touch) return `Ok(None)`.
//!
//! # Design
//!
//! This parser lives in `ftui-web` (not `frankenterm-web`) so that the showcase
//! WASM runner can depend on it without pulling in web-sys/js-sys. It uses
//! `serde_json` for robustness and is feature-gated behind `input-parser`.

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
    PasteEvent,
};
use serde::Deserialize;

/// Errors from parsing encoded input JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputParseError {
    /// Malformed JSON.
    Json(String),
    /// Missing required field.
    MissingField(&'static str),
    /// Unknown key phase value.
    UnknownPhase(String),
}

impl core::fmt::Display for InputParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Json(msg) => write!(f, "JSON parse error: {msg}"),
            Self::MissingField(field) => write!(f, "missing required field: {field}"),
            Self::UnknownPhase(phase) => write!(f, "unknown phase: {phase}"),
        }
    }
}

impl std::error::Error for InputParseError {}

/// Internal deserialization target matching frankenterm-web's JSON schema.
#[derive(Debug, Deserialize)]
struct RawInput {
    kind: String,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    mods: Option<i32>,
    #[serde(default)]
    repeat: Option<bool>,
    #[serde(default)]
    button: Option<i32>,
    #[serde(default)]
    x: Option<i32>,
    #[serde(default)]
    y: Option<i32>,
    #[serde(default)]
    dx: Option<i32>,
    #[serde(default)]
    dy: Option<i32>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    focused: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)]
    raw_key: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    raw_code: Option<String>,
}

/// Parse a JSON-encoded input event (from `FrankenTermWeb.drainEncodedInputs()`)
/// into an [`Event`].
///
/// Returns `Ok(None)` for event kinds that have no `Event` equivalent
/// (accessibility, touch, composition with non-end phase).
///
/// Returns `Err` for malformed JSON or missing required fields.
pub fn parse_encoded_input_to_event(json: &str) -> Result<Option<Event>, InputParseError> {
    let raw: RawInput =
        serde_json::from_str(json).map_err(|e| InputParseError::Json(e.to_string()))?;

    match raw.kind.as_str() {
        "key" => parse_key_event(&raw).map(Some),
        "mouse" => parse_mouse_event(&raw).map(Some),
        "wheel" => parse_wheel_event(&raw),
        "paste" => parse_paste_event(&raw).map(Some),
        "focus" => parse_focus_event(&raw).map(Some),
        "composition" => parse_composition_event(&raw),
        // Touch, accessibility, and unknown kinds have no Event mapping.
        _ => Ok(None),
    }
}

fn parse_modifiers(mods: Option<i32>) -> Modifiers {
    let bits = mods.unwrap_or(0).clamp(0, i32::from(u8::MAX)) as u8;
    Modifiers::from_bits_truncate(bits)
}

fn parse_key_code_opt(code: &str) -> Option<KeyCode> {
    match code {
        "Enter" => Some(KeyCode::Enter),
        "NumpadEnter" => Some(KeyCode::Enter),
        "Escape" | "Esc" => Some(KeyCode::Escape),
        "Backspace" => Some(KeyCode::Backspace),
        "Tab" => Some(KeyCode::Tab),
        "BackTab" => Some(KeyCode::BackTab),
        "Delete" => Some(KeyCode::Delete),
        "Insert" => Some(KeyCode::Insert),
        "Home" => Some(KeyCode::Home),
        "End" => Some(KeyCode::End),
        "PageUp" => Some(KeyCode::PageUp),
        "PageDown" => Some(KeyCode::PageDown),
        "Up" | "ArrowUp" => Some(KeyCode::Up),
        "Down" | "ArrowDown" => Some(KeyCode::Down),
        "Left" | "ArrowLeft" => Some(KeyCode::Left),
        "Right" | "ArrowRight" => Some(KeyCode::Right),
        "Space" | "Spacebar" => Some(KeyCode::Char(' ')),
        other => {
            // DOM KeyboardEvent.code fallback: KeyA..KeyZ
            if let Some(tail) = other.strip_prefix("Key") {
                let mut chars = tail.chars();
                if let Some(c) = chars.next()
                    && chars.next().is_none()
                {
                    return Some(KeyCode::Char(c.to_ascii_lowercase()));
                }
            }
            // DOM KeyboardEvent.code fallback: Digit0..Digit9
            if let Some(tail) = other.strip_prefix("Digit") {
                let mut chars = tail.chars();
                if let Some(c) = chars.next()
                    && chars.next().is_none()
                    && c.is_ascii_digit()
                {
                    return Some(KeyCode::Char(c));
                }
            }
            // Check for function keys: F1..F24
            if let Some(n) = other
                .strip_prefix('F')
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|&n| (1..=24).contains(&n))
            {
                return Some(KeyCode::F(n));
            }
            // Single character
            let mut chars = other.chars();
            if let Some(c) = chars.next()
                && chars.next().is_none()
            {
                return Some(KeyCode::Char(c));
            }
            // Unknown multi-char key label.
            None
        }
    }
}

fn parse_key_code(code: &str) -> KeyCode {
    parse_key_code_opt(code).unwrap_or(KeyCode::Null)
}

fn parse_key_event(raw: &RawInput) -> Result<Event, InputParseError> {
    let phase = raw.phase.as_deref().unwrap_or("down");
    let kind = match phase {
        "down" => KeyEventKind::Press,
        "up" => KeyEventKind::Release,
        other => return Err(InputParseError::UnknownPhase(other.to_string())),
    };

    let key_str = raw
        .key
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(raw.raw_key.as_deref().filter(|s| !s.is_empty()));

    let code_str = raw
        .code
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(raw.raw_code.as_deref().filter(|s| !s.is_empty()));

    if key_str.is_none() && code_str.is_none() {
        return Err(InputParseError::MissingField("code"));
    }

    // Handle repeat as Press (ftui-core doesn't have a Repeat kind)
    let kind = if raw.repeat.unwrap_or(false) && kind == KeyEventKind::Press {
        KeyEventKind::Repeat
    } else {
        kind
    };

    // Prefer logical `key` when it maps cleanly (captures shifted punctuation
    // and mobile keyboard text). Fall back to physical `code` labels.
    let parsed_code = key_str
        .and_then(parse_key_code_opt)
        .or_else(|| code_str.and_then(parse_key_code_opt))
        .or_else(|| key_str.map(parse_key_code))
        .or_else(|| code_str.map(parse_key_code))
        .unwrap_or(KeyCode::Null);

    Ok(Event::Key(KeyEvent {
        code: parsed_code,
        modifiers: parse_modifiers(raw.mods),
        kind,
    }))
}

fn parse_mouse_button(button: Option<i32>) -> MouseButton {
    match button {
        Some(0) | None => MouseButton::Left,
        Some(1) => MouseButton::Middle,
        Some(2) => MouseButton::Right,
        _ => MouseButton::Left,
    }
}

#[inline]
fn clamp_coord_u16(coord: Option<i32>) -> u16 {
    let value = coord.unwrap_or(0);
    if value <= 0 {
        0
    } else {
        value.min(i32::from(u16::MAX)) as u16
    }
}

fn parse_mouse_event(raw: &RawInput) -> Result<Event, InputParseError> {
    let phase = raw.phase.as_deref().unwrap_or("down");
    let x = clamp_coord_u16(raw.x);
    let y = clamp_coord_u16(raw.y);
    let modifiers = parse_modifiers(raw.mods);
    let button = parse_mouse_button(raw.button);

    let kind = match phase {
        "down" => MouseEventKind::Down(button),
        "up" => MouseEventKind::Up(button),
        "move" => MouseEventKind::Moved,
        "drag" => MouseEventKind::Drag(button),
        other => return Err(InputParseError::UnknownPhase(other.to_string())),
    };

    Ok(Event::Mouse(MouseEvent {
        kind,
        x,
        y,
        modifiers,
    }))
}

fn parse_wheel_event(raw: &RawInput) -> Result<Option<Event>, InputParseError> {
    let x = clamp_coord_u16(raw.x);
    let y = clamp_coord_u16(raw.y);
    let dx = raw.dx.unwrap_or(0);
    let dy = raw.dy.unwrap_or(0);
    let modifiers = parse_modifiers(raw.mods);

    let kind = if dy < 0 {
        MouseEventKind::ScrollUp
    } else if dy > 0 {
        MouseEventKind::ScrollDown
    } else if dx < 0 {
        MouseEventKind::ScrollLeft
    } else if dx > 0 {
        MouseEventKind::ScrollRight
    } else {
        return Ok(None); // No scroll, skip.
    };

    Ok(Some(Event::Mouse(MouseEvent {
        kind,
        x,
        y,
        modifiers,
    })))
}

fn parse_paste_event(raw: &RawInput) -> Result<Event, InputParseError> {
    let data = raw
        .data
        .as_deref()
        .ok_or(InputParseError::MissingField("data"))?;
    Ok(Event::Paste(PasteEvent {
        text: data.to_string(),
        bracketed: true,
    }))
}

fn parse_focus_event(raw: &RawInput) -> Result<Event, InputParseError> {
    let focused = raw
        .focused
        .ok_or(InputParseError::MissingField("focused"))?;
    Ok(Event::Focus(focused))
}

fn parse_composition_event(raw: &RawInput) -> Result<Option<Event>, InputParseError> {
    let phase = raw.phase.as_deref().unwrap_or("");
    // Only "end" phase commits text; others are intermediate IME state.
    if phase != "end" {
        return Ok(None);
    }
    let data = raw.data.as_deref().unwrap_or("");
    if data.is_empty() {
        return Ok(None);
    }
    // Synthesize a Paste event for the committed composition text.
    Ok(Some(Event::Paste(PasteEvent {
        text: data.to_string(),
        bracketed: false,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn key_down_simple() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","code":"a","mods":0,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_up_enter() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"up","code":"Enter","mods":0,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Release,
            })
        );
    }

    #[test]
    fn key_with_modifiers() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","code":"c","mods":4,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: Modifiers::CTRL,
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_repeat() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","code":"a","mods":0,"repeat":true}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Repeat,
            })
        );
    }

    #[test]
    fn key_function_key() {
        let ev =
            parse_encoded_input_to_event(r#"{"kind":"key","phase":"down","code":"F5","mods":0}"#)
                .unwrap()
                .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::F(5),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_escape() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","code":"Escape","mods":0}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Escape,
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_dom_code_fallback_key_a_maps_to_char() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","code":"KeyA","mods":0,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_dom_code_fallback_digit_maps_to_char() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","code":"Digit7","mods":0,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Char('7'),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_dom_arrow_label_maps_to_arrow() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","code":"ArrowLeft","mods":0,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Left,
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_unknown_multichar_maps_to_null_not_escape() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","code":"Unidentified","mods":0,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Null,
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_uses_raw_key_fallback_when_code_missing() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","raw_key":"Enter","mods":0,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_field_preferred_for_shifted_symbol() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","key":"!","code":"Digit1","mods":1,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Char('!'),
                modifiers: Modifiers::SHIFT,
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn key_field_fallback_handles_punctuation_code_labels() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"key","phase":"down","key":"/","code":"Slash","mods":0,"repeat":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Char('/'),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn mouse_down() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"mouse","phase":"down","button":0,"x":10,"y":5,"mods":0}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                x: 10,
                y: 5,
                modifiers: Modifiers::empty(),
            })
        );
    }

    #[test]
    fn mouse_move() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"mouse","phase":"move","x":11,"y":5,"mods":0}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                x: 11,
                y: 5,
                modifiers: Modifiers::empty(),
            })
        );
    }

    #[test]
    fn mouse_right_button() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"mouse","phase":"down","button":2,"x":5,"y":3,"mods":1}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                x: 5,
                y: 3,
                modifiers: Modifiers::SHIFT,
            })
        );
    }

    #[test]
    fn mouse_negative_button_defaults_left_instead_of_parse_error() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"mouse","phase":"up","button":-1,"x":5,"y":3,"mods":0}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                x: 5,
                y: 3,
                modifiers: Modifiers::NONE,
            })
        );
    }

    #[test]
    fn modifiers_negative_values_are_clamped_to_zero() {
        let ev =
            parse_encoded_input_to_event(r#"{"kind":"key","phase":"down","code":"a","mods":-9}"#)
                .unwrap()
                .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: Modifiers::NONE,
                kind: KeyEventKind::Press,
            })
        );
    }

    #[test]
    fn mouse_negative_coords_are_clamped() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"mouse","phase":"move","x":-9,"y":-3,"mods":0}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                x: 0,
                y: 0,
                modifiers: Modifiers::empty(),
            })
        );
    }

    #[test]
    fn mouse_large_coords_are_clamped() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"mouse","phase":"down","button":0,"x":1000000,"y":999999,"mods":0}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                x: u16::MAX,
                y: u16::MAX,
                modifiers: Modifiers::empty(),
            })
        );
    }

    #[test]
    fn wheel_scroll_up() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"wheel","x":10,"y":5,"dx":0,"dy":-3,"mods":0}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                x: 10,
                y: 5,
                modifiers: Modifiers::empty(),
            })
        );
    }

    #[test]
    fn wheel_scroll_down() {
        let ev =
            parse_encoded_input_to_event(r#"{"kind":"wheel","x":0,"y":0,"dx":0,"dy":3,"mods":0}"#)
                .unwrap()
                .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                x: 0,
                y: 0,
                modifiers: Modifiers::empty(),
            })
        );
    }

    #[test]
    fn wheel_horizontal_scroll_right() {
        let ev =
            parse_encoded_input_to_event(r#"{"kind":"wheel","x":0,"y":0,"dx":5,"dy":0,"mods":0}"#)
                .unwrap()
                .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollRight,
                x: 0,
                y: 0,
                modifiers: Modifiers::empty(),
            })
        );
    }

    #[test]
    fn wheel_coords_are_clamped() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"wheel","x":-1,"y":70000,"dx":0,"dy":1,"mods":0}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                x: 0,
                y: u16::MAX,
                modifiers: Modifiers::empty(),
            })
        );
    }

    #[test]
    fn wheel_zero_both_returns_none() {
        let ev =
            parse_encoded_input_to_event(r#"{"kind":"wheel","x":0,"y":0,"dx":0,"dy":0,"mods":0}"#)
                .unwrap();
        assert!(ev.is_none());
    }

    #[test]
    fn paste_event() {
        let ev = parse_encoded_input_to_event(r#"{"kind":"paste","data":"hello world"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(
            ev,
            Event::Paste(PasteEvent {
                text: "hello world".to_string(),
                bracketed: true,
            })
        );
    }

    #[test]
    fn focus_gained() {
        let ev = parse_encoded_input_to_event(r#"{"kind":"focus","focused":true}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, Event::Focus(true));
    }

    #[test]
    fn focus_lost() {
        let ev = parse_encoded_input_to_event(r#"{"kind":"focus","focused":false}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev, Event::Focus(false));
    }

    #[test]
    fn composition_end_produces_paste() {
        let ev =
            parse_encoded_input_to_event(r#"{"kind":"composition","phase":"end","data":"你好"}"#)
                .unwrap()
                .unwrap();
        assert_eq!(
            ev,
            Event::Paste(PasteEvent {
                text: "你好".to_string(),
                bracketed: false,
            })
        );
    }

    #[test]
    fn composition_update_returns_none() {
        let ev =
            parse_encoded_input_to_event(r#"{"kind":"composition","phase":"update","data":"你"}"#)
                .unwrap();
        assert!(ev.is_none());
    }

    #[test]
    fn composition_end_empty_returns_none() {
        let ev = parse_encoded_input_to_event(r#"{"kind":"composition","phase":"end","data":""}"#)
            .unwrap();
        assert!(ev.is_none());
    }

    #[test]
    fn accessibility_returns_none() {
        let ev = parse_encoded_input_to_event(r#"{"kind":"accessibility","screen_reader":true}"#)
            .unwrap();
        assert!(ev.is_none());
    }

    #[test]
    fn touch_returns_none() {
        let ev = parse_encoded_input_to_event(
            r#"{"kind":"touch","phase":"start","touches":[{"id":1,"x":5,"y":3}],"mods":0}"#,
        )
        .unwrap();
        assert!(ev.is_none());
    }

    #[test]
    fn unknown_kind_returns_none() {
        let ev = parse_encoded_input_to_event(r#"{"kind":"unknown_future_kind","data":"test"}"#)
            .unwrap();
        assert!(ev.is_none());
    }

    #[test]
    fn malformed_json_returns_error() {
        let result = parse_encoded_input_to_event("not json");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InputParseError::Json(_)));
    }

    #[test]
    fn missing_kind_returns_error() {
        let result = parse_encoded_input_to_event(r#"{"phase":"down"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn key_missing_code_returns_error() {
        let result = parse_encoded_input_to_event(r#"{"kind":"key","phase":"down"}"#);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            InputParseError::MissingField("code")
        ));
    }

    #[test]
    fn paste_missing_data_returns_error() {
        let result = parse_encoded_input_to_event(r#"{"kind":"paste"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn focus_missing_focused_returns_error() {
        let result = parse_encoded_input_to_event(r#"{"kind":"focus"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn error_display() {
        let e1 = InputParseError::Json("bad".into());
        assert!(format!("{e1}").contains("JSON parse error"));

        let e2 = InputParseError::MissingField("code");
        assert!(format!("{e2}").contains("code"));

        let e3 = InputParseError::UnknownPhase("blink".into());
        assert!(format!("{e3}").contains("blink"));
    }

    #[test]
    fn all_arrow_keys() {
        for (code_str, expected) in [
            ("Up", KeyCode::Up),
            ("Down", KeyCode::Down),
            ("Left", KeyCode::Left),
            ("Right", KeyCode::Right),
        ] {
            let json = format!(r#"{{"kind":"key","phase":"down","code":"{code_str}","mods":0}}"#);
            let ev = parse_encoded_input_to_event(&json).unwrap().unwrap();
            assert_eq!(
                ev,
                Event::Key(KeyEvent {
                    code: expected,
                    modifiers: Modifiers::empty(),
                    kind: KeyEventKind::Press,
                })
            );
        }
    }

    #[test]
    fn all_special_keys() {
        for (code_str, expected) in [
            ("Home", KeyCode::Home),
            ("End", KeyCode::End),
            ("PageUp", KeyCode::PageUp),
            ("PageDown", KeyCode::PageDown),
            ("Insert", KeyCode::Insert),
            ("Delete", KeyCode::Delete),
            ("Backspace", KeyCode::Backspace),
            ("Tab", KeyCode::Tab),
            ("BackTab", KeyCode::BackTab),
        ] {
            let json = format!(r#"{{"kind":"key","phase":"down","code":"{code_str}","mods":0}}"#);
            let ev = parse_encoded_input_to_event(&json).unwrap().unwrap();
            assert_eq!(
                ev,
                Event::Key(KeyEvent {
                    code: expected,
                    modifiers: Modifiers::empty(),
                    kind: KeyEventKind::Press,
                })
            );
        }
    }

    #[test]
    fn modifier_combinations() {
        // SHIFT | CTRL = 0b0101 = 5
        let ev =
            parse_encoded_input_to_event(r#"{"kind":"key","phase":"down","code":"a","mods":5}"#)
                .unwrap()
                .unwrap();
        assert_eq!(
            ev,
            Event::Key(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: Modifiers::SHIFT | Modifiers::CTRL,
                kind: KeyEventKind::Press,
            })
        );
    }
}

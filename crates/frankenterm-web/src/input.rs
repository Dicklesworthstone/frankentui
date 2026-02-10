#![forbid(unsafe_code)]

//! Deterministic, JSON-friendly input schema for `frankenterm-web`.
//!
//! The web host (JS/TS) is expected to provide:
//! - cell coordinates for pointer/touch events, and
//! - quantized (`i16`) wheel deltas (already normalized for determinism).
//!
//! This module focuses on:
//! - stable key-code normalization (DOM `key`/`code` → [`KeyCode`]),
//! - a compact modifier bitset (`mods: u8`) for logs/traces, and
//! - JSON encoding suitable for record/replay.

use bitflags::bitflags;
use core::convert::Infallible;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

bitflags! {
    /// Modifier keys held during an input event.
    ///
    /// These flags are encoded as a compact `u8` bitset in JSON (`mods`).
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Modifiers: u8 {
        const SHIFT = 0b0001;
        const ALT   = 0b0010;
        const CTRL  = 0b0100;
        const SUPER = 0b1000;
    }
}

impl Modifiers {
    #[must_use]
    pub const fn from_bits_truncate_u8(bits: u8) -> Self {
        Self::from_bits_truncate(bits)
    }
}

/// Phase for key events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyPhase {
    Down,
    Up,
}

/// Phase for mouse events in cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MousePhase {
    Down,
    Up,
    Move,
    Drag,
}

/// Phase for IME composition events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionPhase {
    Start,
    Update,
    /// Final commit for the current composition session.
    ///
    /// The serialized form remains `"end"` to match DOM event naming.
    End,
    Cancel,
}

/// Phase for touch events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchPhase {
    Start,
    Move,
    End,
    Cancel,
}

/// Normalized key code for deterministic record/replay.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    BackTab,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8),
    Unidentified { key: Box<str>, code: Box<str> },
}

impl KeyCode {
    #[must_use]
    pub fn to_code_string(&self) -> String {
        match self {
            Self::Char(c) => c.to_string(),
            Self::Enter => "Enter".to_string(),
            Self::Escape => "Escape".to_string(),
            Self::Backspace => "Backspace".to_string(),
            Self::Tab => "Tab".to_string(),
            Self::BackTab => "BackTab".to_string(),
            Self::Delete => "Delete".to_string(),
            Self::Insert => "Insert".to_string(),
            Self::Home => "Home".to_string(),
            Self::End => "End".to_string(),
            Self::PageUp => "PageUp".to_string(),
            Self::PageDown => "PageDown".to_string(),
            Self::Up => "Up".to_string(),
            Self::Down => "Down".to_string(),
            Self::Left => "Left".to_string(),
            Self::Right => "Right".to_string(),
            Self::F(n) => format!("F{n}"),
            Self::Unidentified { .. } => "Unidentified".to_string(),
        }
    }

    #[must_use]
    pub fn from_code_string(code: &str, raw_key: Option<&str>, raw_code: Option<&str>) -> Self {
        match code {
            "Enter" => Self::Enter,
            "Escape" => Self::Escape,
            "Backspace" => Self::Backspace,
            "Tab" => Self::Tab,
            "BackTab" => Self::BackTab,
            "Delete" => Self::Delete,
            "Insert" => Self::Insert,
            "Home" => Self::Home,
            "End" => Self::End,
            "PageUp" => Self::PageUp,
            "PageDown" => Self::PageDown,
            "Up" => Self::Up,
            "Down" => Self::Down,
            "Left" => Self::Left,
            "Right" => Self::Right,
            "Unidentified" => Self::Unidentified {
                key: raw_key.unwrap_or("").into(),
                code: raw_code.unwrap_or("").into(),
            },
            _ => {
                if let Some(n) = parse_function_key(code) {
                    return Self::F(n);
                }

                let mut chars = code.chars();
                let Some(first) = chars.next() else {
                    return Self::Unidentified {
                        key: raw_key.unwrap_or("").into(),
                        code: raw_code.unwrap_or("").into(),
                    };
                };
                if chars.next().is_none() {
                    Self::Char(first)
                } else {
                    Self::Unidentified {
                        key: raw_key.unwrap_or(code).into(),
                        code: raw_code.unwrap_or("").into(),
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Other(u8),
}

impl MouseButton {
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::Other(n) => n,
        }
    }

    #[must_use]
    pub const fn from_u8(n: u8) -> Self {
        match n {
            0 => Self::Left,
            1 => Self::Middle,
            2 => Self::Right,
            other => Self::Other(other),
        }
    }
}

/// Normalized key input event.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyInput {
    pub phase: KeyPhase,
    pub code: KeyCode,
    pub mods: Modifiers,
    pub repeat: bool,
}

/// Normalized mouse input event in cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseInput {
    pub phase: MousePhase,
    pub button: Option<MouseButton>,
    pub x: u16,
    pub y: u16,
    pub mods: Modifiers,
}

/// Normalized wheel input event (deterministic integer deltas).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WheelInput {
    pub x: u16,
    pub y: u16,
    pub dx: i16,
    pub dy: i16,
    pub mods: Modifiers,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TouchPoint {
    pub id: u32,
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TouchInput {
    pub phase: TouchPhase,
    pub touches: Vec<TouchPoint>,
    pub mods: Modifiers,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompositionInput {
    pub phase: CompositionPhase,
    pub data: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PasteInput {
    pub data: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FocusInput {
    pub focused: bool,
}

/// Accessibility preference/control event from the web host.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccessibilityInput {
    pub screen_reader: Option<bool>,
    pub high_contrast: Option<bool>,
    pub reduced_motion: Option<bool>,
    pub announce: Option<Box<str>>,
}

impl AccessibilityInput {
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.screen_reader.is_none()
            && self.high_contrast.is_none()
            && self.reduced_motion.is_none()
            && self.announce.is_none()
    }
}

/// Normalized, deterministic web input event.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InputEvent {
    Key(KeyInput),
    Mouse(MouseInput),
    Wheel(WheelInput),
    Touch(TouchInput),
    Composition(CompositionInput),
    Paste(PasteInput),
    Focus(FocusInput),
    Accessibility(AccessibilityInput),
}

/// Rewrite result after applying composition-state normalization.
///
/// The normalizer may synthesize one extra composition event for malformed
/// host streams (for example, `update` without a prior `start`) and may also
/// drop key events while composition is active to prevent duplicate inserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionRewrite {
    pub synthetic: Option<InputEvent>,
    pub primary: Option<InputEvent>,
}

impl CompositionRewrite {
    pub fn into_events(self) -> impl Iterator<Item = InputEvent> {
        [self.synthetic, self.primary].into_iter().flatten()
    }
}

/// Tracks IME composition session state and normalizes event streams.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompositionState {
    active: bool,
}

impl CompositionState {
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Normalize one input event against current composition state.
    ///
    /// Guarantees:
    /// - No key events leak while composition is active.
    /// - `update`/`end` without an active composition synthesize a `start`.
    /// - Starting a new composition while active synthesizes a `cancel` first.
    /// - Focus loss during composition emits a synthetic `cancel`.
    #[must_use]
    pub fn rewrite(&mut self, event: InputEvent) -> CompositionRewrite {
        match event {
            InputEvent::Composition(comp) => self.rewrite_composition(comp),
            InputEvent::Focus(FocusInput { focused: false }) if self.active => {
                self.active = false;
                CompositionRewrite {
                    synthetic: Some(synthetic_composition_event(CompositionPhase::Cancel)),
                    primary: Some(InputEvent::Focus(FocusInput { focused: false })),
                }
            }
            InputEvent::Key(_) if self.active => CompositionRewrite {
                synthetic: None,
                primary: None,
            },
            other => CompositionRewrite {
                synthetic: None,
                primary: Some(other),
            },
        }
    }

    fn rewrite_composition(&mut self, comp: CompositionInput) -> CompositionRewrite {
        match comp.phase {
            CompositionPhase::Start => {
                let synthetic = if self.active {
                    Some(synthetic_composition_event(CompositionPhase::Cancel))
                } else {
                    None
                };
                self.active = true;
                CompositionRewrite {
                    synthetic,
                    primary: Some(InputEvent::Composition(comp)),
                }
            }
            CompositionPhase::Update => {
                let synthetic = if self.active {
                    None
                } else {
                    self.active = true;
                    Some(synthetic_composition_event(CompositionPhase::Start))
                };
                CompositionRewrite {
                    synthetic,
                    primary: Some(InputEvent::Composition(comp)),
                }
            }
            CompositionPhase::End => {
                let synthetic = if self.active {
                    None
                } else {
                    Some(synthetic_composition_event(CompositionPhase::Start))
                };
                self.active = false;
                CompositionRewrite {
                    synthetic,
                    primary: Some(InputEvent::Composition(comp)),
                }
            }
            CompositionPhase::Cancel => {
                self.active = false;
                CompositionRewrite {
                    synthetic: None,
                    primary: Some(InputEvent::Composition(comp)),
                }
            }
        }
    }
}

fn synthetic_composition_event(phase: CompositionPhase) -> InputEvent {
    InputEvent::Composition(CompositionInput { phase, data: None })
}

/// Minimal modifier tracker used to guarantee "no stuck modifiers" after focus loss.
#[derive(Debug, Default, Clone)]
pub struct ModifierTracker {
    current: Modifiers,
}

impl ModifierTracker {
    #[must_use]
    pub const fn current(&self) -> Modifiers {
        self.current
    }

    pub fn handle_focus(&mut self, focused: bool) {
        if !focused {
            self.current = Modifiers::empty();
        }
    }

    pub fn reconcile(&mut self, seen: Modifiers) {
        self.current = seen;
    }
}

/// Deterministic normalization of DOM key/code strings into a [`KeyCode`].
#[must_use]
pub fn normalize_dom_key_code(dom_key: &str, dom_code: &str, mods: Modifiers) -> KeyCode {
    // Shift+Tab should be represented explicitly.
    if dom_key == "Tab" && mods.contains(Modifiers::SHIFT) {
        return KeyCode::BackTab;
    }

    // Prefer the logical `key` for printable characters (already includes shift).
    let mut chars = dom_key.chars();
    if let Some(first) = chars.next()
        && chars.next().is_none()
    {
        return KeyCode::Char(first);
    }

    match dom_key {
        "Enter" => KeyCode::Enter,
        "Escape" | "Esc" => KeyCode::Escape,
        "Backspace" => KeyCode::Backspace,
        "Tab" => KeyCode::Tab,
        "Delete" => KeyCode::Delete,
        "Insert" => KeyCode::Insert,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        "Spacebar" => KeyCode::Char(' '),
        _ => {
            if let Some(n) = parse_function_key(dom_key) {
                return KeyCode::F(n);
            }

            // Fallback to DOM `code` for non-printable keys.
            if let Some(code) = key_code_from_dom_code(dom_code, mods) {
                return code;
            }

            KeyCode::Unidentified {
                key: dom_key.into(),
                code: dom_code.into(),
            }
        }
    }
}

fn parse_function_key(s: &str) -> Option<u8> {
    let rest = s.strip_prefix('F')?;
    rest.parse::<u8>().ok().filter(|n| (1..=24).contains(n))
}

fn key_code_from_dom_code(dom_code: &str, mods: Modifiers) -> Option<KeyCode> {
    // Support the `code` form for BackTab as well (some wrappers may pass it).
    if dom_code == "Tab" && mods.contains(Modifiers::SHIFT) {
        return Some(KeyCode::BackTab);
    }

    Some(match dom_code {
        "Enter" | "NumpadEnter" => KeyCode::Enter,
        "Escape" => KeyCode::Escape,
        "Backspace" => KeyCode::Backspace,
        "Tab" => KeyCode::Tab,
        "Delete" => KeyCode::Delete,
        "Insert" => KeyCode::Insert,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        _ => {
            return None;
        }
    })
}

/// JSON encoding used by `ftui-web` and golden traces.
///
/// This is intentionally small and stable: a `kind` tag plus the minimum
/// semantic fields needed for replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEventJson {
    Key {
        phase: KeyPhase,
        code: String,
        mods: u8,
        repeat: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_code: Option<String>,
    },
    Mouse {
        phase: MousePhase,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<u8>,
        x: u16,
        y: u16,
        mods: u8,
    },
    Wheel {
        x: u16,
        y: u16,
        dx: i16,
        dy: i16,
        mods: u8,
    },
    Touch {
        phase: TouchPhase,
        touches: Vec<TouchPoint>,
        mods: u8,
    },
    Composition {
        phase: CompositionPhase,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
    Paste {
        data: String,
    },
    Focus {
        focused: bool,
    },
    Accessibility {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen_reader: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        high_contrast: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reduced_motion: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        announce: Option<String>,
    },
}

impl InputEvent {
    /// Encode this event as a stable JSON string.
    pub fn to_json_string(&self) -> Result<String, Infallible> {
        Ok(input_event_json_to_string(&InputEventJson::from(self)))
    }

    /// Decode a previously encoded event JSON string.
    ///
    /// Errors occur if the JSON does not match the expected schema.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_json_str(s: &str) -> Result<Self, serde_json::Error> {
        let json: InputEventJson = serde_json::from_str(s)?;
        Ok(Self::from(json))
    }
}

fn input_event_json_to_string(value: &InputEventJson) -> String {
    let mut out = String::with_capacity(160);
    out.push('{');
    let mut first = true;

    match value {
        InputEventJson::Key {
            phase,
            code,
            mods,
            repeat,
            raw_key,
            raw_code,
        } => {
            push_json_str_field(&mut out, &mut first, "kind", "key");
            push_json_str_field(&mut out, &mut first, "phase", key_phase_str(*phase));
            push_json_str_field(&mut out, &mut first, "code", code);
            push_json_u64_field(&mut out, &mut first, "mods", u64::from(*mods));
            push_json_bool_field(&mut out, &mut first, "repeat", *repeat);
            if let Some(raw_key) = raw_key.as_deref() {
                push_json_str_field(&mut out, &mut first, "raw_key", raw_key);
            }
            if let Some(raw_code) = raw_code.as_deref() {
                push_json_str_field(&mut out, &mut first, "raw_code", raw_code);
            }
        }
        InputEventJson::Mouse {
            phase,
            button,
            x,
            y,
            mods,
        } => {
            push_json_str_field(&mut out, &mut first, "kind", "mouse");
            push_json_str_field(&mut out, &mut first, "phase", mouse_phase_str(*phase));
            if let Some(button) = button {
                push_json_u64_field(&mut out, &mut first, "button", u64::from(*button));
            }
            push_json_u64_field(&mut out, &mut first, "x", u64::from(*x));
            push_json_u64_field(&mut out, &mut first, "y", u64::from(*y));
            push_json_u64_field(&mut out, &mut first, "mods", u64::from(*mods));
        }
        InputEventJson::Wheel { x, y, dx, dy, mods } => {
            push_json_str_field(&mut out, &mut first, "kind", "wheel");
            push_json_u64_field(&mut out, &mut first, "x", u64::from(*x));
            push_json_u64_field(&mut out, &mut first, "y", u64::from(*y));
            push_json_i64_field(&mut out, &mut first, "dx", i64::from(*dx));
            push_json_i64_field(&mut out, &mut first, "dy", i64::from(*dy));
            push_json_u64_field(&mut out, &mut first, "mods", u64::from(*mods));
        }
        InputEventJson::Touch {
            phase,
            touches,
            mods,
        } => {
            push_json_str_field(&mut out, &mut first, "kind", "touch");
            push_json_str_field(&mut out, &mut first, "phase", touch_phase_str(*phase));
            push_json_key(&mut out, &mut first, "touches");
            out.push('[');
            for (idx, touch) in touches.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                out.push('{');
                let mut touch_first = true;
                push_json_u64_field(&mut out, &mut touch_first, "id", u64::from(touch.id));
                push_json_u64_field(&mut out, &mut touch_first, "x", u64::from(touch.x));
                push_json_u64_field(&mut out, &mut touch_first, "y", u64::from(touch.y));
                out.push('}');
            }
            out.push(']');
            push_json_u64_field(&mut out, &mut first, "mods", u64::from(*mods));
        }
        InputEventJson::Composition { phase, data } => {
            push_json_str_field(&mut out, &mut first, "kind", "composition");
            push_json_str_field(&mut out, &mut first, "phase", composition_phase_str(*phase));
            if let Some(data) = data.as_deref() {
                push_json_str_field(&mut out, &mut first, "data", data);
            }
        }
        InputEventJson::Paste { data } => {
            push_json_str_field(&mut out, &mut first, "kind", "paste");
            push_json_str_field(&mut out, &mut first, "data", data);
        }
        InputEventJson::Focus { focused } => {
            push_json_str_field(&mut out, &mut first, "kind", "focus");
            push_json_bool_field(&mut out, &mut first, "focused", *focused);
        }
        InputEventJson::Accessibility {
            screen_reader,
            high_contrast,
            reduced_motion,
            announce,
        } => {
            push_json_str_field(&mut out, &mut first, "kind", "accessibility");
            if let Some(v) = screen_reader {
                push_json_bool_field(&mut out, &mut first, "screen_reader", *v);
            }
            if let Some(v) = high_contrast {
                push_json_bool_field(&mut out, &mut first, "high_contrast", *v);
            }
            if let Some(v) = reduced_motion {
                push_json_bool_field(&mut out, &mut first, "reduced_motion", *v);
            }
            if let Some(announce) = announce.as_deref() {
                push_json_str_field(&mut out, &mut first, "announce", announce);
            }
        }
    }
    out.push('}');
    out
}

const fn key_phase_str(phase: KeyPhase) -> &'static str {
    match phase {
        KeyPhase::Down => "down",
        KeyPhase::Up => "up",
    }
}

const fn mouse_phase_str(phase: MousePhase) -> &'static str {
    match phase {
        MousePhase::Down => "down",
        MousePhase::Up => "up",
        MousePhase::Move => "move",
        MousePhase::Drag => "drag",
    }
}

const fn touch_phase_str(phase: TouchPhase) -> &'static str {
    match phase {
        TouchPhase::Start => "start",
        TouchPhase::Move => "move",
        TouchPhase::End => "end",
        TouchPhase::Cancel => "cancel",
    }
}

const fn composition_phase_str(phase: CompositionPhase) -> &'static str {
    match phase {
        CompositionPhase::Start => "start",
        CompositionPhase::Update => "update",
        CompositionPhase::End => "end",
        CompositionPhase::Cancel => "cancel",
    }
}

fn push_json_key(out: &mut String, first: &mut bool, key: &str) {
    if !*first {
        out.push(',');
    }
    *first = false;
    push_json_string(out, key);
    out.push(':');
}

fn push_json_str_field(out: &mut String, first: &mut bool, key: &str, value: &str) {
    push_json_key(out, first, key);
    push_json_string(out, value);
}

fn push_json_bool_field(out: &mut String, first: &mut bool, key: &str, value: bool) {
    push_json_key(out, first, key);
    if value {
        out.push_str("true");
    } else {
        out.push_str("false");
    }
}

fn push_json_u64_field(out: &mut String, first: &mut bool, key: &str, value: u64) {
    push_json_key(out, first, key);
    let _ = write!(out, "{value}");
}

fn push_json_i64_field(out: &mut String, first: &mut bool, key: &str, value: i64) {
    push_json_key(out, first, key);
    let _ = write!(out, "{value}");
}

fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl From<&InputEvent> for InputEventJson {
    fn from(value: &InputEvent) -> Self {
        match value {
            InputEvent::Key(key) => {
                let (code, raw_key, raw_code) = match &key.code {
                    KeyCode::Unidentified { key, code } => (
                        "Unidentified".to_string(),
                        Some(key.to_string()),
                        Some(code.to_string()),
                    ),
                    other => (other.to_code_string(), None, None),
                };
                Self::Key {
                    phase: key.phase,
                    code,
                    mods: key.mods.bits(),
                    repeat: key.repeat,
                    raw_key,
                    raw_code,
                }
            }
            InputEvent::Mouse(mouse) => Self::Mouse {
                phase: mouse.phase,
                button: mouse.button.map(MouseButton::to_u8),
                x: mouse.x,
                y: mouse.y,
                mods: mouse.mods.bits(),
            },
            InputEvent::Wheel(wheel) => Self::Wheel {
                x: wheel.x,
                y: wheel.y,
                dx: wheel.dx,
                dy: wheel.dy,
                mods: wheel.mods.bits(),
            },
            InputEvent::Touch(touch) => Self::Touch {
                phase: touch.phase,
                touches: touch.touches.clone(),
                mods: touch.mods.bits(),
            },
            InputEvent::Composition(comp) => Self::Composition {
                phase: comp.phase,
                data: comp.data.as_deref().map(str::to_string),
            },
            InputEvent::Paste(paste) => Self::Paste {
                data: paste.data.to_string(),
            },
            InputEvent::Focus(f) => Self::Focus { focused: f.focused },
            InputEvent::Accessibility(a11y) => Self::Accessibility {
                screen_reader: a11y.screen_reader,
                high_contrast: a11y.high_contrast,
                reduced_motion: a11y.reduced_motion,
                announce: a11y.announce.as_deref().map(str::to_string),
            },
        }
    }
}

impl From<InputEventJson> for InputEvent {
    fn from(value: InputEventJson) -> Self {
        match value {
            InputEventJson::Key {
                phase,
                code,
                mods,
                repeat,
                raw_key,
                raw_code,
            } => Self::Key(KeyInput {
                phase,
                code: KeyCode::from_code_string(&code, raw_key.as_deref(), raw_code.as_deref()),
                mods: Modifiers::from_bits_truncate_u8(mods),
                repeat,
            }),
            InputEventJson::Mouse {
                phase,
                button,
                x,
                y,
                mods,
            } => Self::Mouse(MouseInput {
                phase,
                button: button.map(MouseButton::from_u8),
                x,
                y,
                mods: Modifiers::from_bits_truncate_u8(mods),
            }),
            InputEventJson::Wheel { x, y, dx, dy, mods } => Self::Wheel(WheelInput {
                x,
                y,
                dx,
                dy,
                mods: Modifiers::from_bits_truncate_u8(mods),
            }),
            InputEventJson::Touch {
                phase,
                touches,
                mods,
            } => Self::Touch(TouchInput {
                phase,
                touches,
                mods: Modifiers::from_bits_truncate_u8(mods),
            }),
            InputEventJson::Composition { phase, data } => Self::Composition(CompositionInput {
                phase,
                data: data.map(Into::into),
            }),
            InputEventJson::Paste { data } => Self::Paste(PasteInput { data: data.into() }),
            InputEventJson::Focus { focused } => Self::Focus(FocusInput { focused }),
            InputEventJson::Accessibility {
                screen_reader,
                high_contrast,
                reduced_motion,
                announce,
            } => Self::Accessibility(AccessibilityInput {
                screen_reader,
                high_contrast,
                reduced_motion,
                announce: announce.map(Into::into),
            }),
        }
    }
}

/// Feature toggles controlling VT input encoding behavior.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VtInputEncoderFeatures {
    /// Emit SGR mouse protocol bytes for mouse/wheel events.
    pub sgr_mouse: bool,
    /// Wrap pasted text with bracketed paste delimiters.
    pub bracketed_paste: bool,
    /// Emit focus-in/focus-out CSI sequences.
    pub focus_events: bool,
    /// Prefer Kitty keyboard protocol for key events.
    pub kitty_keyboard: bool,
}

/// Encode one normalized input event into a VT-compatible byte sequence.
///
/// Returns an empty vector for events that are intentionally not encoded under
/// the provided feature toggles.
#[must_use]
pub fn encode_vt_input_event(event: &InputEvent, features: VtInputEncoderFeatures) -> Vec<u8> {
    match event {
        InputEvent::Key(key) => encode_key_input(key, features),
        InputEvent::Mouse(mouse) => encode_mouse_input(mouse, features),
        InputEvent::Wheel(wheel) => encode_wheel_input(wheel, features),
        InputEvent::Touch(_) => Vec::new(),
        InputEvent::Composition(comp) => encode_composition_input(comp, features),
        InputEvent::Paste(paste) => {
            encode_paste_text(paste.data.as_ref(), features.bracketed_paste)
        }
        InputEvent::Focus(focus) => encode_focus_input(*focus, features),
        InputEvent::Accessibility(_) => Vec::new(),
    }
}

/// Encode text as plain bytes or bracketed-paste bytes.
#[must_use]
pub fn encode_paste_text(text: &str, bracketed_paste: bool) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(text.len() + 16);
    if bracketed_paste {
        out.extend_from_slice(b"\x1b[200~");
    }
    out.extend_from_slice(text.as_bytes());
    if bracketed_paste {
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

fn encode_key_input(key: &KeyInput, features: VtInputEncoderFeatures) -> Vec<u8> {
    if features.kitty_keyboard {
        return encode_key_kitty(key);
    }
    // Legacy terminal streams do not represent key-up; only encode key-down/repeat.
    if key.phase == KeyPhase::Up {
        return Vec::new();
    }
    encode_key_legacy(key)
}

fn encode_key_legacy(key: &KeyInput) -> Vec<u8> {
    match key.code {
        KeyCode::Char(ch) => encode_legacy_char(ch, key.mods),
        KeyCode::Enter => maybe_alt_prefix(key.mods, b"\r"),
        KeyCode::Escape => maybe_alt_prefix(key.mods, b"\x1b"),
        KeyCode::Backspace => maybe_alt_prefix(key.mods, &[0x7f]),
        KeyCode::Tab => maybe_alt_prefix(key.mods, b"\t"),
        KeyCode::BackTab => csi_with_mod_or_plain('Z', key.mods),
        KeyCode::Up => csi_with_mod_or_plain('A', key.mods),
        KeyCode::Down => csi_with_mod_or_plain('B', key.mods),
        KeyCode::Right => csi_with_mod_or_plain('C', key.mods),
        KeyCode::Left => csi_with_mod_or_plain('D', key.mods),
        KeyCode::Home => csi_with_mod_or_plain('H', key.mods),
        KeyCode::End => csi_with_mod_or_plain('F', key.mods),
        KeyCode::Insert => csi_tilde_with_mod(2, key.mods),
        KeyCode::Delete => csi_tilde_with_mod(3, key.mods),
        KeyCode::PageUp => csi_tilde_with_mod(5, key.mods),
        KeyCode::PageDown => csi_tilde_with_mod(6, key.mods),
        KeyCode::F(n) => encode_legacy_function_key(n, key.mods),
        KeyCode::Unidentified { .. } => Vec::new(),
    }
}

fn encode_legacy_char(ch: char, mods: Modifiers) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    if mods.contains(Modifiers::ALT) {
        out.push(0x1b);
    }

    if mods.contains(Modifiers::CTRL)
        && let Some(ctrl) = ctrl_char_to_byte(ch)
    {
        out.push(ctrl);
        return out;
    }

    let mut buf = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    out
}

fn encode_legacy_function_key(n: u8, mods: Modifiers) -> Vec<u8> {
    match n {
        1..=4 => {
            if !mods.is_empty() {
                return Vec::new();
            }
            let ss3 = match n {
                1 => b'P',
                2 => b'Q',
                3 => b'R',
                _ => b'S',
            };
            vec![0x1b, b'O', ss3]
        }
        5 => csi_tilde_with_mod(15, mods),
        6 => csi_tilde_with_mod(17, mods),
        7 => csi_tilde_with_mod(18, mods),
        8 => csi_tilde_with_mod(19, mods),
        9 => csi_tilde_with_mod(20, mods),
        10 => csi_tilde_with_mod(21, mods),
        11 => csi_tilde_with_mod(23, mods),
        12 => csi_tilde_with_mod(24, mods),
        _ => Vec::new(),
    }
}

fn encode_key_kitty(key: &KeyInput) -> Vec<u8> {
    let Some(codepoint) = kitty_codepoint_for_keycode(&key.code) else {
        return Vec::new();
    };

    let mod_value = xterm_modifier_value(key.mods);
    let kind_value = match (key.phase, key.repeat) {
        (KeyPhase::Up, _) => 3,
        (_, true) => 2,
        _ => 1,
    };

    let seq = if kind_value == 1 {
        format!("\x1b[{codepoint};{mod_value}u")
    } else {
        format!("\x1b[{codepoint};{mod_value}:{kind_value}u")
    };
    seq.into_bytes()
}

fn kitty_codepoint_for_keycode(code: &KeyCode) -> Option<u32> {
    match code {
        KeyCode::Char(ch) => Some(u32::from(*ch)),
        KeyCode::Enter => Some(57_345),
        KeyCode::Escape => Some(57_344),
        KeyCode::Backspace => Some(57_347),
        KeyCode::Tab | KeyCode::BackTab => Some(57_346),
        KeyCode::Delete => Some(57_349),
        KeyCode::Insert => Some(57_348),
        KeyCode::Home => Some(57_356),
        KeyCode::End => Some(57_357),
        KeyCode::PageUp => Some(57_354),
        KeyCode::PageDown => Some(57_355),
        KeyCode::Up => Some(57_352),
        KeyCode::Down => Some(57_353),
        KeyCode::Left => Some(57_350),
        KeyCode::Right => Some(57_351),
        KeyCode::F(n @ 1..=24) => Some(57_364 + (u32::from(*n) - 1)),
        KeyCode::F(_) | KeyCode::Unidentified { .. } => None,
    }
}

fn maybe_alt_prefix(mods: Modifiers, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if mods.contains(Modifiers::ALT) {
        out.push(0x1b);
    }
    out.extend_from_slice(bytes);
    out
}

fn csi_with_mod_or_plain(final_byte: char, mods: Modifiers) -> Vec<u8> {
    if mods.is_empty() {
        format!("\x1b[{final_byte}").into_bytes()
    } else {
        let mod_value = xterm_modifier_value(mods);
        format!("\x1b[1;{mod_value}{final_byte}").into_bytes()
    }
}

fn csi_tilde_with_mod(code: u16, mods: Modifiers) -> Vec<u8> {
    if mods.is_empty() {
        format!("\x1b[{code}~").into_bytes()
    } else {
        let mod_value = xterm_modifier_value(mods);
        format!("\x1b[{code};{mod_value}~").into_bytes()
    }
}

fn xterm_modifier_value(mods: Modifiers) -> u8 {
    // xterm encoding is `1 + bits`, with bits matching our bitflag layout.
    1 + mods.bits()
}

fn ctrl_char_to_byte(ch: char) -> Option<u8> {
    match ch {
        '@' | ' ' => Some(0x00),
        'a'..='z' => Some((u32::from(ch) as u8) - b'a' + 1),
        'A'..='Z' => Some((u32::from(ch) as u8) - b'A' + 1),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

fn encode_mouse_input(mouse: &MouseInput, features: VtInputEncoderFeatures) -> Vec<u8> {
    if !features.sgr_mouse {
        return Vec::new();
    }

    let mut code = u16::from(mouse_mod_bits(mouse.mods));
    let final_byte = match mouse.phase {
        MousePhase::Down => {
            code |= u16::from(mouse_button_code(mouse.button));
            'M'
        }
        MousePhase::Up => {
            code |= u16::from(mouse_button_code(mouse.button));
            'm'
        }
        MousePhase::Move => {
            code |= 32 + 3;
            'M'
        }
        MousePhase::Drag => {
            code |= 32;
            code |= u16::from(mouse_button_code(mouse.button));
            'M'
        }
    };

    let x = mouse.x.saturating_add(1);
    let y = mouse.y.saturating_add(1);
    format!("\x1b[<{code};{x};{y}{final_byte}").into_bytes()
}

fn encode_wheel_input(wheel: &WheelInput, features: VtInputEncoderFeatures) -> Vec<u8> {
    if !features.sgr_mouse {
        return Vec::new();
    }

    let mut out = Vec::new();
    let steps = i16::max(wheel.dx.abs(), wheel.dy.abs()).clamp(0, 16);
    if steps == 0 {
        return out;
    }

    let base_code = if wheel.dy != 0 {
        if wheel.dy > 0 { 65u16 } else { 64u16 }
    } else if wheel.dx > 0 {
        67u16
    } else {
        66u16
    };
    let code = base_code | u16::from(mouse_mod_bits(wheel.mods));
    let x = wheel.x.saturating_add(1);
    let y = wheel.y.saturating_add(1);
    for _ in 0..steps {
        out.extend_from_slice(format!("\x1b[<{code};{x};{y}M").as_bytes());
    }
    out
}

fn mouse_button_code(button: Option<MouseButton>) -> u8 {
    match button {
        Some(MouseButton::Left) => 0,
        Some(MouseButton::Middle) => 1,
        Some(MouseButton::Right) => 2,
        Some(MouseButton::Other(n)) => n & 0b11,
        None => 0,
    }
}

fn mouse_mod_bits(mods: Modifiers) -> u8 {
    let mut bits = 0u8;
    if mods.contains(Modifiers::SHIFT) {
        bits |= 4;
    }
    if mods.contains(Modifiers::ALT) {
        bits |= 8;
    }
    if mods.contains(Modifiers::CTRL) {
        bits |= 16;
    }
    bits
}

fn encode_composition_input(
    composition: &CompositionInput,
    features: VtInputEncoderFeatures,
) -> Vec<u8> {
    if composition.phase != CompositionPhase::End {
        return Vec::new();
    }
    let Some(data) = composition.data.as_deref() else {
        return Vec::new();
    };
    encode_paste_text(data, features.bracketed_paste)
}

fn encode_focus_input(focus: FocusInput, features: VtInputEncoderFeatures) -> Vec<u8> {
    if !features.focus_events {
        return Vec::new();
    }
    if focus.focused {
        b"\x1b[I".to_vec()
    } else {
        b"\x1b[O".to_vec()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn map_dom_key_specials() {
        let mods = Modifiers::empty();
        assert_eq!(
            normalize_dom_key_code("Enter", "Enter", mods),
            KeyCode::Enter
        );
        assert_eq!(
            normalize_dom_key_code("ArrowLeft", "ArrowLeft", mods),
            KeyCode::Left
        );
        assert_eq!(normalize_dom_key_code("F12", "F12", mods), KeyCode::F(12));
    }

    #[test]
    fn shift_tab_is_backtab() {
        let mods = Modifiers::SHIFT;
        assert_eq!(normalize_dom_key_code("Tab", "Tab", mods), KeyCode::BackTab);
    }

    #[test]
    fn key_event_json_roundtrip_is_stable() {
        let ev = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Char('a'),
            mods: Modifiers::empty(),
            repeat: false,
        });
        let j1 = ev.to_json_string().expect("serialize");
        let j2 = ev.to_json_string().expect("serialize");
        assert_eq!(j1, j2);
        let back = InputEvent::from_json_str(&j1).expect("deserialize");
        assert_eq!(ev, back);
    }

    #[test]
    fn composition_update_without_start_synthesizes_start() {
        let mut state = CompositionState::default();
        let update = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Update,
            data: Some("に".into()),
        });

        let out: Vec<InputEvent> = state.rewrite(update.clone()).into_events().collect();
        assert_eq!(
            out,
            vec![
                InputEvent::Composition(CompositionInput {
                    phase: CompositionPhase::Start,
                    data: None,
                }),
                update,
            ]
        );
        assert!(state.is_active());
    }

    #[test]
    fn composition_drops_key_events_until_end() {
        let mut state = CompositionState::default();
        let start = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Start,
            data: None,
        });
        let _ = state.rewrite(start);

        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Char('a'),
            mods: Modifiers::empty(),
            repeat: false,
        });

        let dropped: Vec<InputEvent> = state.rewrite(key.clone()).into_events().collect();
        assert!(dropped.is_empty());

        let end = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::End,
            data: Some("あ".into()),
        });
        let end_out: Vec<InputEvent> = state.rewrite(end).into_events().collect();
        assert_eq!(end_out.len(), 1);
        assert!(!state.is_active());

        let pass_through: Vec<InputEvent> = state.rewrite(key.clone()).into_events().collect();
        assert_eq!(pass_through, vec![key]);
    }

    #[test]
    fn composition_focus_loss_emits_cancel_before_focus_event() {
        let mut state = CompositionState::default();
        let _ = state.rewrite(InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Start,
            data: None,
        }));
        assert!(state.is_active());

        let out: Vec<InputEvent> = state
            .rewrite(InputEvent::Focus(FocusInput { focused: false }))
            .into_events()
            .collect();
        assert_eq!(
            out,
            vec![
                InputEvent::Composition(CompositionInput {
                    phase: CompositionPhase::Cancel,
                    data: None,
                }),
                InputEvent::Focus(FocusInput { focused: false }),
            ]
        );
        assert!(!state.is_active());
    }

    #[test]
    fn vt_encoder_plain_and_ctrl_keys() {
        let plain = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Char('x'),
            mods: Modifiers::empty(),
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&plain, VtInputEncoderFeatures::default()),
            b"x".to_vec()
        );

        let ctrl_c = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Char('c'),
            mods: Modifiers::CTRL,
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&ctrl_c, VtInputEncoderFeatures::default()),
            vec![0x03]
        );
    }

    #[test]
    fn vt_encoder_arrow_with_modifier_uses_csi_modifier_form() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Up,
            mods: Modifiers::CTRL,
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&key, VtInputEncoderFeatures::default()),
            b"\x1b[1;5A".to_vec()
        );
    }

    #[test]
    fn vt_encoder_mouse_and_focus_respect_toggles() {
        let mouse = InputEvent::Mouse(MouseInput {
            phase: MousePhase::Down,
            button: Some(MouseButton::Left),
            x: 9,
            y: 19,
            mods: Modifiers::empty(),
        });
        assert!(
            encode_vt_input_event(&mouse, VtInputEncoderFeatures::default()).is_empty(),
            "mouse must be gated by sgr_mouse feature"
        );
        assert_eq!(
            encode_vt_input_event(
                &mouse,
                VtInputEncoderFeatures {
                    sgr_mouse: true,
                    ..VtInputEncoderFeatures::default()
                }
            ),
            b"\x1b[<0;10;20M".to_vec()
        );

        let focus = InputEvent::Focus(FocusInput { focused: true });
        assert!(
            encode_vt_input_event(&focus, VtInputEncoderFeatures::default()).is_empty(),
            "focus events must be gated by focus_events feature"
        );
        assert_eq!(
            encode_vt_input_event(
                &focus,
                VtInputEncoderFeatures {
                    focus_events: true,
                    ..VtInputEncoderFeatures::default()
                }
            ),
            b"\x1b[I".to_vec()
        );
    }

    #[test]
    fn vt_encoder_composition_commit_uses_paste_mode_when_enabled() {
        let commit = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::End,
            data: Some("你好".into()),
        });
        let encoded = encode_vt_input_event(
            &commit,
            VtInputEncoderFeatures {
                bracketed_paste: true,
                ..VtInputEncoderFeatures::default()
            },
        );
        assert_eq!(
            encoded,
            b"\x1b[200~\xe4\xbd\xa0\xe5\xa5\xbd\x1b[201~".to_vec()
        );
    }

    #[test]
    fn vt_encoder_paste_event_uses_bracketed_mode_when_enabled() {
        let paste = InputEvent::Paste(PasteInput {
            data: "hello\nworld".into(),
        });
        let encoded = encode_vt_input_event(
            &paste,
            VtInputEncoderFeatures {
                bracketed_paste: true,
                ..VtInputEncoderFeatures::default()
            },
        );
        assert_eq!(encoded, b"\x1b[200~hello\nworld\x1b[201~".to_vec());
    }

    #[test]
    fn paste_event_json_roundtrip_is_stable() {
        let ev = InputEvent::Paste(PasteInput {
            data: "clip me".into(),
        });
        let json = ev.to_json_string().expect("serialize");
        let roundtrip = InputEvent::from_json_str(&json).expect("deserialize");
        assert_eq!(ev, roundtrip);
    }

    #[test]
    fn vt_encoder_kitty_encodes_release_kind() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Up,
            code: KeyCode::F(1),
            mods: Modifiers::CTRL,
            repeat: false,
        });
        let encoded = encode_vt_input_event(
            &key,
            VtInputEncoderFeatures {
                kitty_keyboard: true,
                ..VtInputEncoderFeatures::default()
            },
        );
        assert_eq!(encoded, b"\x1b[57364;5:3u".to_vec());
    }

    #[test]
    fn accessibility_event_json_roundtrip_is_stable() {
        let ev = InputEvent::Accessibility(AccessibilityInput {
            screen_reader: Some(true),
            high_contrast: Some(false),
            reduced_motion: Some(true),
            announce: Some("ready".into()),
        });
        let json = ev.to_json_string().expect("serialize");
        let roundtrip = InputEvent::from_json_str(&json).expect("deserialize");
        assert_eq!(ev, roundtrip);
    }

    #[test]
    fn accessibility_input_is_noop_only_when_all_fields_are_absent() {
        let none = AccessibilityInput {
            screen_reader: None,
            high_contrast: None,
            reduced_motion: None,
            announce: None,
        };
        assert!(none.is_noop());

        let with_announce = AccessibilityInput {
            announce: Some("hello".into()),
            ..none.clone()
        };
        assert!(!with_announce.is_noop());

        let with_toggle = AccessibilityInput {
            reduced_motion: Some(true),
            ..none
        };
        assert!(!with_toggle.is_noop());
    }

    #[test]
    fn vt_encoder_ignores_accessibility_event() {
        let ev = InputEvent::Accessibility(AccessibilityInput {
            screen_reader: Some(true),
            high_contrast: Some(true),
            reduced_motion: Some(true),
            announce: None,
        });
        assert!(
            encode_vt_input_event(&ev, VtInputEncoderFeatures::default()).is_empty(),
            "accessibility control events are host-side only and must not emit VT bytes"
        );
    }

    // ---- encode_wheel_input ----

    #[test]
    fn wheel_scroll_up_negative_dy_emits_sgr_code_64() {
        let wheel = InputEvent::Wheel(WheelInput {
            x: 5,
            y: 10,
            dx: 0,
            dy: -3,
            mods: Modifiers::empty(),
        });
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let encoded = encode_vt_input_event(&wheel, features);
        // dy < 0 → scroll up → base_code 64 (button 4), repeated 3 times
        let single = b"\x1b[<64;6;11M";
        assert_eq!(encoded.len(), single.len() * 3);
        assert_eq!(&encoded[..single.len()], single.as_slice());
    }

    #[test]
    fn wheel_scroll_down_positive_dy_emits_sgr_code_65() {
        let wheel = InputEvent::Wheel(WheelInput {
            x: 0,
            y: 0,
            dx: 0,
            dy: 1,
            mods: Modifiers::empty(),
        });
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let encoded = encode_vt_input_event(&wheel, features);
        // dy > 0 → scroll down → base_code 65 (button 5)
        assert_eq!(encoded, b"\x1b[<65;1;1M".to_vec());
    }

    #[test]
    fn wheel_horizontal_scroll_right_code_67_left_code_66() {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let right = InputEvent::Wheel(WheelInput {
            x: 0,
            y: 0,
            dx: 1,
            dy: 0,
            mods: Modifiers::empty(),
        });
        assert_eq!(
            encode_vt_input_event(&right, features),
            b"\x1b[<67;1;1M".to_vec()
        );
        let left = InputEvent::Wheel(WheelInput {
            x: 0,
            y: 0,
            dx: -1,
            dy: 0,
            mods: Modifiers::empty(),
        });
        assert_eq!(
            encode_vt_input_event(&left, features),
            b"\x1b[<66;1;1M".to_vec()
        );
    }

    #[test]
    fn wheel_zero_delta_produces_empty() {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let wheel = InputEvent::Wheel(WheelInput {
            x: 5,
            y: 5,
            dx: 0,
            dy: 0,
            mods: Modifiers::empty(),
        });
        assert!(encode_vt_input_event(&wheel, features).is_empty());
    }

    #[test]
    fn wheel_steps_clamped_to_16() {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let wheel = InputEvent::Wheel(WheelInput {
            x: 0,
            y: 0,
            dx: 100,
            dy: 0,
            mods: Modifiers::empty(),
        });
        let encoded = encode_vt_input_event(&wheel, features);
        let single = b"\x1b[<67;1;1M";
        // steps should clamp to 16
        assert_eq!(encoded.len(), single.len() * 16);
    }

    #[test]
    fn wheel_with_modifiers_ored_into_code() {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let wheel = InputEvent::Wheel(WheelInput {
            x: 0,
            y: 0,
            dx: 0,
            dy: 1,
            mods: Modifiers::CTRL,
        });
        // base 65 | ctrl(16) = 81
        assert_eq!(
            encode_vt_input_event(&wheel, features),
            b"\x1b[<81;1;1M".to_vec()
        );
    }

    #[test]
    fn wheel_disabled_when_sgr_mouse_off() {
        let wheel = InputEvent::Wheel(WheelInput {
            x: 0,
            y: 0,
            dx: 0,
            dy: 5,
            mods: Modifiers::empty(),
        });
        assert!(encode_vt_input_event(&wheel, VtInputEncoderFeatures::default()).is_empty());
    }

    // ---- encode_mouse_input phases ----

    #[test]
    fn mouse_up_emits_lowercase_m() {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let mouse = InputEvent::Mouse(MouseInput {
            phase: MousePhase::Up,
            button: Some(MouseButton::Left),
            x: 0,
            y: 0,
            mods: Modifiers::empty(),
        });
        assert_eq!(
            encode_vt_input_event(&mouse, features),
            b"\x1b[<0;1;1m".to_vec()
        );
    }

    #[test]
    fn mouse_move_emits_code_35() {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let mouse = InputEvent::Mouse(MouseInput {
            phase: MousePhase::Move,
            button: None,
            x: 3,
            y: 7,
            mods: Modifiers::empty(),
        });
        // 32 + 3 = 35
        assert_eq!(
            encode_vt_input_event(&mouse, features),
            b"\x1b[<35;4;8M".to_vec()
        );
    }

    #[test]
    fn mouse_drag_adds_32_to_button_code() {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let mouse = InputEvent::Mouse(MouseInput {
            phase: MousePhase::Drag,
            button: Some(MouseButton::Right),
            x: 0,
            y: 0,
            mods: Modifiers::empty(),
        });
        // 32 | 2 = 34
        assert_eq!(
            encode_vt_input_event(&mouse, features),
            b"\x1b[<34;1;1M".to_vec()
        );
    }

    #[test]
    fn mouse_all_button_codes() {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        for (button, expected_code) in [
            (Some(MouseButton::Left), 0),
            (Some(MouseButton::Middle), 1),
            (Some(MouseButton::Right), 2),
            (Some(MouseButton::Other(5)), 1), // 5 & 0b11 = 1
            (None, 0),
        ] {
            let mouse = InputEvent::Mouse(MouseInput {
                phase: MousePhase::Down,
                button,
                x: 0,
                y: 0,
                mods: Modifiers::empty(),
            });
            let expected = format!("\x1b[<{expected_code};1;1M");
            assert_eq!(
                encode_vt_input_event(&mouse, features),
                expected.into_bytes(),
                "button={button:?}"
            );
        }
    }

    #[test]
    fn mouse_modifier_bits_shift4_alt8_ctrl16() {
        let features = VtInputEncoderFeatures {
            sgr_mouse: true,
            ..VtInputEncoderFeatures::default()
        };
        let mouse = InputEvent::Mouse(MouseInput {
            phase: MousePhase::Down,
            button: Some(MouseButton::Left),
            x: 0,
            y: 0,
            mods: Modifiers::SHIFT | Modifiers::ALT | Modifiers::CTRL,
        });
        // 0 (left) | 4 (shift) | 8 (alt) | 16 (ctrl) = 28
        assert_eq!(
            encode_vt_input_event(&mouse, features),
            b"\x1b[<28;1;1M".to_vec()
        );
    }

    // ---- composition state edge cases ----

    #[test]
    fn composition_start_while_active_emits_cancel_then_start() {
        let mut state = CompositionState::default();
        let start = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Start,
            data: None,
        });
        // First start - no synthetic
        let out: Vec<InputEvent> = state.rewrite(start.clone()).into_events().collect();
        assert_eq!(out, vec![start.clone()]);
        assert!(state.is_active());

        // Second start while active - synthetic Cancel emitted
        let out: Vec<InputEvent> = state.rewrite(start.clone()).into_events().collect();
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Cancel,
                data: None,
            })
        );
        assert_eq!(out[1], start);
        assert!(state.is_active());
    }

    #[test]
    fn composition_end_without_start_synthesizes_start() {
        let mut state = CompositionState::default();
        assert!(!state.is_active());
        let end = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::End,
            data: Some("日本語".into()),
        });
        let out: Vec<InputEvent> = state.rewrite(end.clone()).into_events().collect();
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Start,
                data: None,
            })
        );
        assert_eq!(out[1], end);
        assert!(!state.is_active());
    }

    #[test]
    fn composition_cancel_without_active_passes_through() {
        let mut state = CompositionState::default();
        let cancel = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Cancel,
            data: None,
        });
        let out: Vec<InputEvent> = state.rewrite(cancel.clone()).into_events().collect();
        assert_eq!(out, vec![cancel]);
        assert!(!state.is_active());
    }

    #[test]
    fn composition_non_key_events_pass_through_while_active() {
        let mut state = CompositionState::default();
        let _ = state.rewrite(InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Start,
            data: None,
        }));
        assert!(state.is_active());

        // Mouse passes through
        let mouse = InputEvent::Mouse(MouseInput {
            phase: MousePhase::Down,
            button: Some(MouseButton::Left),
            x: 0,
            y: 0,
            mods: Modifiers::empty(),
        });
        let out: Vec<InputEvent> = state.rewrite(mouse.clone()).into_events().collect();
        assert_eq!(out, vec![mouse]);

        // Focus(true) passes through
        let focus = InputEvent::Focus(FocusInput { focused: true });
        let out: Vec<InputEvent> = state.rewrite(focus.clone()).into_events().collect();
        assert_eq!(out, vec![focus]);
        assert!(state.is_active());
    }

    #[test]
    fn composition_multiple_updates_synthesize_start_only_once() {
        let mut state = CompositionState::default();
        let update1 = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Update,
            data: Some("に".into()),
        });
        let out: Vec<InputEvent> = state.rewrite(update1.clone()).into_events().collect();
        assert_eq!(out.len(), 2); // synthetic Start + Update

        let update2 = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::Update,
            data: Some("にほ".into()),
        });
        let out: Vec<InputEvent> = state.rewrite(update2.clone()).into_events().collect();
        assert_eq!(out.len(), 1); // only Update, no Start
        assert_eq!(out[0], update2);
    }

    // ---- JSON roundtrips for Mouse/Wheel/Touch ----

    #[test]
    fn mouse_event_json_roundtrip() {
        let ev = InputEvent::Mouse(MouseInput {
            phase: MousePhase::Drag,
            button: Some(MouseButton::Middle),
            x: 42,
            y: 99,
            mods: Modifiers::CTRL | Modifiers::SHIFT,
        });
        let json = ev.to_json_string().expect("serialize");
        let back = InputEvent::from_json_str(&json).expect("deserialize");
        assert_eq!(ev, back);
    }

    #[test]
    fn wheel_event_json_roundtrip() {
        let ev = InputEvent::Wheel(WheelInput {
            x: 10,
            y: 20,
            dx: -5,
            dy: 3,
            mods: Modifiers::ALT,
        });
        let json = ev.to_json_string().expect("serialize");
        let back = InputEvent::from_json_str(&json).expect("deserialize");
        assert_eq!(ev, back);
    }

    #[test]
    fn touch_event_json_roundtrip() {
        let ev = InputEvent::Touch(TouchInput {
            phase: TouchPhase::Move,
            touches: vec![
                TouchPoint { id: 0, x: 1, y: 2 },
                TouchPoint {
                    id: 1,
                    x: 50,
                    y: 60,
                },
                TouchPoint {
                    id: 2,
                    x: 100,
                    y: 200,
                },
            ],
            mods: Modifiers::SHIFT | Modifiers::ALT,
        });
        let json = ev.to_json_string().expect("serialize");
        let back = InputEvent::from_json_str(&json).expect("deserialize");
        assert_eq!(ev, back);
    }

    #[test]
    fn focus_event_json_roundtrip() {
        for focused in [true, false] {
            let ev = InputEvent::Focus(FocusInput { focused });
            let json = ev.to_json_string().expect("serialize");
            let back = InputEvent::from_json_str(&json).expect("deserialize");
            assert_eq!(ev, back);
        }
    }

    #[test]
    fn unidentified_key_json_roundtrip_preserves_raw() {
        let ev = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Unidentified {
                key: "Zenkaku".into(),
                code: "IntlRo".into(),
            },
            mods: Modifiers::empty(),
            repeat: false,
        });
        let json = ev.to_json_string().expect("serialize");
        assert!(json.contains("Unidentified"));
        assert!(json.contains("Zenkaku"));
        assert!(json.contains("IntlRo"));
        let back = InputEvent::from_json_str(&json).expect("deserialize");
        assert_eq!(ev, back);
    }

    // ---- push_json_string escaping ----

    #[test]
    fn json_string_escapes_special_characters() {
        let ev = InputEvent::Paste(PasteInput {
            data: "he said \"hello\"\npath\\to\\file\ttab\r\n".into(),
        });
        let json = ev.to_json_string().expect("serialize");
        assert!(json.contains(r#"\"hello\""#));
        assert!(json.contains(r"\\to\\"));
        assert!(json.contains(r"\t"));
        assert!(json.contains(r"\r"));
        assert!(json.contains(r"\n"));
        // roundtrip
        let back = InputEvent::from_json_str(&json).expect("deserialize");
        assert_eq!(ev, back);
    }

    #[test]
    fn json_string_escapes_control_characters() {
        let data = "null\x00bell\x07bs\x08ff\x0c";
        let ev = InputEvent::Paste(PasteInput { data: data.into() });
        let json = ev.to_json_string().expect("serialize");
        assert!(json.contains(r"\u0000"));
        assert!(json.contains(r"\u0007"));
        assert!(json.contains(r"\b"));
        assert!(json.contains(r"\f"));
        let back = InputEvent::from_json_str(&json).expect("deserialize");
        assert_eq!(ev, back);
    }

    #[test]
    fn json_string_passes_unicode_through() {
        let ev = InputEvent::Paste(PasteInput {
            data: "日本語 emoji 🎉 café".into(),
        });
        let json = ev.to_json_string().expect("serialize");
        assert!(json.contains("日本語"));
        assert!(json.contains("🎉"));
        let back = InputEvent::from_json_str(&json).expect("deserialize");
        assert_eq!(ev, back);
    }

    // ---- encode_key_legacy: special keys with modifiers ----

    #[test]
    fn alt_enter_emits_esc_cr() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Enter,
            mods: Modifiers::ALT,
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&key, VtInputEncoderFeatures::default()),
            b"\x1b\r".to_vec()
        );
    }

    #[test]
    fn alt_backspace_emits_esc_del() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Backspace,
            mods: Modifiers::ALT,
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&key, VtInputEncoderFeatures::default()),
            b"\x1b\x7f".to_vec()
        );
    }

    #[test]
    fn delete_with_ctrl_emits_csi_modifier_tilde() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Delete,
            mods: Modifiers::CTRL,
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&key, VtInputEncoderFeatures::default()),
            b"\x1b[3;5~".to_vec()
        );
    }

    #[test]
    fn page_up_with_shift_emits_csi_modifier_tilde() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::PageUp,
            mods: Modifiers::SHIFT,
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&key, VtInputEncoderFeatures::default()),
            b"\x1b[5;2~".to_vec()
        );
    }

    #[test]
    fn home_with_shift_emits_csi_modifier_form() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Home,
            mods: Modifiers::SHIFT,
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&key, VtInputEncoderFeatures::default()),
            b"\x1b[1;2H".to_vec()
        );
    }

    // ---- legacy function keys ----

    #[test]
    fn f1_through_f4_emit_ss3_without_modifiers() {
        let features = VtInputEncoderFeatures::default();
        for (n, byte) in [(1, b'P'), (2, b'Q'), (3, b'R'), (4, b'S')] {
            let key = InputEvent::Key(KeyInput {
                phase: KeyPhase::Down,
                code: KeyCode::F(n),
                mods: Modifiers::empty(),
                repeat: false,
            });
            assert_eq!(
                encode_vt_input_event(&key, features),
                vec![0x1b, b'O', byte],
                "F{n}"
            );
        }
    }

    #[test]
    fn f1_with_modifier_returns_empty() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::F(1),
            mods: Modifiers::SHIFT,
            repeat: false,
        });
        assert!(encode_vt_input_event(&key, VtInputEncoderFeatures::default()).is_empty());
    }

    #[test]
    fn f5_through_f12_emit_csi_tilde_codes() {
        let features = VtInputEncoderFeatures::default();
        let expected_codes = [
            (5, 15),
            (6, 17),
            (7, 18),
            (8, 19),
            (9, 20),
            (10, 21),
            (11, 23),
            (12, 24),
        ];
        for (n, csi_code) in expected_codes {
            let key = InputEvent::Key(KeyInput {
                phase: KeyPhase::Down,
                code: KeyCode::F(n),
                mods: Modifiers::empty(),
                repeat: false,
            });
            let expected = format!("\x1b[{csi_code}~");
            assert_eq!(
                encode_vt_input_event(&key, features),
                expected.into_bytes(),
                "F{n}"
            );
        }
    }

    #[test]
    fn f13_and_above_return_empty_in_legacy() {
        let features = VtInputEncoderFeatures::default();
        for n in [13, 24] {
            let key = InputEvent::Key(KeyInput {
                phase: KeyPhase::Down,
                code: KeyCode::F(n),
                mods: Modifiers::empty(),
                repeat: false,
            });
            assert!(
                encode_vt_input_event(&key, features).is_empty(),
                "F{n} should be empty in legacy encoding"
            );
        }
    }

    // ---- kitty keyboard protocol ----

    #[test]
    fn kitty_char_normal_press_no_colon() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Char('a'),
            mods: Modifiers::empty(),
            repeat: false,
        });
        let features = VtInputEncoderFeatures {
            kitty_keyboard: true,
            ..VtInputEncoderFeatures::default()
        };
        // 'a' = 97, mods=1 (empty+1), kind=1 → no colon
        assert_eq!(
            encode_vt_input_event(&key, features),
            b"\x1b[97;1u".to_vec()
        );
    }

    #[test]
    fn kitty_repeat_key_uses_colon_kind_2() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Char('x'),
            mods: Modifiers::empty(),
            repeat: true,
        });
        let features = VtInputEncoderFeatures {
            kitty_keyboard: true,
            ..VtInputEncoderFeatures::default()
        };
        // 'x' = 120, mods=1, kind=2
        assert_eq!(
            encode_vt_input_event(&key, features),
            b"\x1b[120;1:2u".to_vec()
        );
    }

    #[test]
    fn kitty_modifier_value_shift2_alt3_ctrl5_super9() {
        let features = VtInputEncoderFeatures {
            kitty_keyboard: true,
            ..VtInputEncoderFeatures::default()
        };
        for (mods, mod_value) in [
            (Modifiers::SHIFT, 2),
            (Modifiers::ALT, 3),
            (Modifiers::CTRL, 5),
            (Modifiers::SUPER, 9),
            (Modifiers::SHIFT | Modifiers::CTRL, 6),
        ] {
            let key = InputEvent::Key(KeyInput {
                phase: KeyPhase::Down,
                code: KeyCode::Char('a'),
                mods,
                repeat: false,
            });
            let expected = format!("\x1b[97;{mod_value}u");
            assert_eq!(
                encode_vt_input_event(&key, features),
                expected.into_bytes(),
                "mods={mods:?}"
            );
        }
    }

    #[test]
    fn kitty_unidentified_key_returns_empty() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Unidentified {
                key: "Unknown".into(),
                code: "".into(),
            },
            mods: Modifiers::empty(),
            repeat: false,
        });
        let features = VtInputEncoderFeatures {
            kitty_keyboard: true,
            ..VtInputEncoderFeatures::default()
        };
        assert!(encode_vt_input_event(&key, features).is_empty());
    }

    // ---- key-up ignored in legacy mode ----

    #[test]
    fn legacy_key_up_returns_empty() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Up,
            code: KeyCode::Char('a'),
            mods: Modifiers::empty(),
            repeat: false,
        });
        assert!(encode_vt_input_event(&key, VtInputEncoderFeatures::default()).is_empty());
    }

    // ---- ctrl char encoding ----

    #[test]
    fn ctrl_a_through_z_produces_correct_bytes() {
        let features = VtInputEncoderFeatures::default();
        for (ch, expected_byte) in [('a', 1u8), ('c', 3), ('z', 26)] {
            let key = InputEvent::Key(KeyInput {
                phase: KeyPhase::Down,
                code: KeyCode::Char(ch),
                mods: Modifiers::CTRL,
                repeat: false,
            });
            assert_eq!(
                encode_vt_input_event(&key, features),
                vec![expected_byte],
                "Ctrl+{ch}"
            );
        }
    }

    #[test]
    fn ctrl_alt_char_emits_esc_prefix_then_ctrl_byte() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Char('c'),
            mods: Modifiers::CTRL | Modifiers::ALT,
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&key, VtInputEncoderFeatures::default()),
            vec![0x1b, 0x03]
        );
    }

    #[test]
    fn alt_char_emits_esc_prefix() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::Char('a'),
            mods: Modifiers::ALT,
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&key, VtInputEncoderFeatures::default()),
            b"\x1ba".to_vec()
        );
    }

    // ---- DOM key normalization ----

    #[test]
    fn normalize_dom_key_esc_alias() {
        assert_eq!(
            normalize_dom_key_code("Esc", "Escape", Modifiers::empty()),
            KeyCode::Escape
        );
    }

    #[test]
    fn normalize_dom_key_spacebar() {
        assert_eq!(
            normalize_dom_key_code("Spacebar", "Space", Modifiers::empty()),
            KeyCode::Char(' ')
        );
    }

    #[test]
    fn normalize_dom_key_printable_unicode() {
        assert_eq!(
            normalize_dom_key_code("é", "KeyE", Modifiers::empty()),
            KeyCode::Char('é')
        );
    }

    #[test]
    fn normalize_dom_key_multichar_is_unidentified() {
        match normalize_dom_key_code("Compose", "Compose", Modifiers::empty()) {
            KeyCode::Unidentified { .. } => {}
            other => panic!("expected Unidentified, got {other:?}"),
        }
    }

    #[test]
    fn normalize_dom_key_numpad_enter_via_code_fallback() {
        // dom_key is multi-char "NumpadEnter" → not a special match, not single-char
        // falls through to key_code_from_dom_code which handles "NumpadEnter"
        assert_eq!(
            normalize_dom_key_code("NumpadEnter", "NumpadEnter", Modifiers::empty()),
            KeyCode::Enter
        );
    }

    // ---- parse_function_key edge cases ----

    #[test]
    fn parse_function_key_f0_and_f25_invalid() {
        assert_eq!(
            normalize_dom_key_code("F0", "F0", Modifiers::empty()),
            KeyCode::Unidentified {
                key: "F0".into(),
                code: "F0".into(),
            }
        );
        assert_eq!(
            normalize_dom_key_code("F25", "F25", Modifiers::empty()),
            KeyCode::Unidentified {
                key: "F25".into(),
                code: "F25".into(),
            }
        );
    }

    // ---- KeyCode roundtrip ----

    #[test]
    fn keycode_to_from_code_string_roundtrip() {
        let variants = vec![
            KeyCode::Char('x'),
            KeyCode::Enter,
            KeyCode::Escape,
            KeyCode::Backspace,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::F(1),
            KeyCode::F(12),
            KeyCode::F(24),
        ];
        for code in variants {
            let s = code.to_code_string();
            let back = KeyCode::from_code_string(&s, None, None);
            assert_eq!(code, back, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn keycode_from_empty_string_is_unidentified() {
        match KeyCode::from_code_string("", None, None) {
            KeyCode::Unidentified { .. } => {}
            other => panic!("expected Unidentified, got {other:?}"),
        }
    }

    // ---- MouseButton conversions ----

    #[test]
    fn mouse_button_u8_roundtrip() {
        for n in 0..=5u8 {
            let btn = MouseButton::from_u8(n);
            assert_eq!(btn.to_u8(), n);
        }
    }

    // ---- composition encoding ----

    #[test]
    fn composition_non_end_phases_emit_nothing() {
        let features = VtInputEncoderFeatures::default();
        for phase in [
            CompositionPhase::Start,
            CompositionPhase::Update,
            CompositionPhase::Cancel,
        ] {
            let ev = InputEvent::Composition(CompositionInput {
                phase,
                data: Some("test".into()),
            });
            assert!(
                encode_vt_input_event(&ev, features).is_empty(),
                "phase {phase:?} should emit nothing"
            );
        }
    }

    #[test]
    fn composition_end_without_data_emits_nothing() {
        let ev = InputEvent::Composition(CompositionInput {
            phase: CompositionPhase::End,
            data: None,
        });
        assert!(encode_vt_input_event(&ev, VtInputEncoderFeatures::default()).is_empty());
    }

    // ---- focus encoding ----

    #[test]
    fn focus_out_emits_csi_o() {
        let ev = InputEvent::Focus(FocusInput { focused: false });
        let features = VtInputEncoderFeatures {
            focus_events: true,
            ..VtInputEncoderFeatures::default()
        };
        assert_eq!(encode_vt_input_event(&ev, features), b"\x1b[O".to_vec());
    }

    // ---- BackTab encoding ----

    #[test]
    fn backtab_plain_emits_csi_z() {
        let key = InputEvent::Key(KeyInput {
            phase: KeyPhase::Down,
            code: KeyCode::BackTab,
            mods: Modifiers::empty(),
            repeat: false,
        });
        assert_eq!(
            encode_vt_input_event(&key, VtInputEncoderFeatures::default()),
            b"\x1b[Z".to_vec()
        );
    }

    proptest! {
        #[test]
        fn modifier_tracker_focus_loss_is_idempotent(events in prop::collection::vec(any::<u8>(), 1..200)) {
            let mut tracker = ModifierTracker::default();
            for mods in events {
                tracker.reconcile(Modifiers::from_bits_truncate_u8(mods));
            }
            tracker.handle_focus(false);
            tracker.handle_focus(false);
            prop_assert_eq!(tracker.current(), Modifiers::empty());
        }
    }
}

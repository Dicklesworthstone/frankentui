#![forbid(unsafe_code)]

use crate::input::{
    CompositionInput, CompositionPhase, CompositionState, FocusInput, InputEvent, KeyInput,
    KeyPhase, ModifierTracker, Modifiers, MouseButton, MouseInput, MousePhase, TouchInput,
    TouchPhase, TouchPoint, WheelInput, normalize_dom_key_code,
};
use js_sys::{Array, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

/// Web/WASM terminal surface.
///
/// This is the minimal JS-facing API surface. Implementation will evolve to:
/// - own a WebGPU renderer (glyph atlas + instancing),
/// - own web input capture + IME/clipboard,
/// - accept either VT/ANSI byte streams (`feed`) or direct cell diffs
///   (`applyPatch`) for ftui-web mode.
#[wasm_bindgen]
pub struct FrankenTermWeb {
    cols: u16,
    rows: u16,
    initialized: bool,
    canvas: Option<HtmlCanvasElement>,
    mods: ModifierTracker,
    composition: CompositionState,
    encoded_inputs: Vec<String>,
}

impl Default for FrankenTermWeb {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl FrankenTermWeb {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            cols: 0,
            rows: 0,
            initialized: false,
            canvas: None,
            mods: ModifierTracker::default(),
            composition: CompositionState::default(),
            encoded_inputs: Vec::new(),
        }
    }

    /// Initialize the terminal surface with an existing `<canvas>`.
    ///
    /// Exported as an async JS function returning a Promise, matching the
    /// `frankenterm-web` architecture spec.
    pub async fn init(
        &mut self,
        canvas: HtmlCanvasElement,
        _options: Option<JsValue>,
    ) -> Result<(), JsValue> {
        self.canvas = Some(canvas);
        self.initialized = true;
        Ok(())
    }

    /// Resize the terminal in logical grid coordinates (cols/rows).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }

    /// Accepts DOM-derived keyboard/mouse/touch events.
    ///
    /// This method expects an `InputEvent`-shaped JS object (not a raw DOM event),
    /// with a `kind` discriminator and normalized cell coordinates where relevant.
    ///
    /// The event is normalized to a stable JSON encoding suitable for record/replay,
    /// then queued for downstream consumption (e.g. feeding `ftui-web`).
    pub fn input(&mut self, event: JsValue) -> Result<(), JsValue> {
        let ev = parse_input_event(&event)?;
        let rewrite = self.composition.rewrite(ev);

        for ev in rewrite.into_events() {
            // Guarantee no "stuck modifiers" after focus loss by treating focus
            // loss as an explicit modifier reset point.
            if let InputEvent::Focus(focus) = &ev {
                self.mods.handle_focus(focus.focused);
            } else {
                self.mods.reconcile(event_mods(&ev));
            }

            let json = ev
                .to_json_string()
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
            self.encoded_inputs.push(json);
        }
        Ok(())
    }

    /// Drain queued, normalized input events as JSON strings.
    #[wasm_bindgen(js_name = drainEncodedInputs)]
    pub fn drain_encoded_inputs(&mut self) -> Array {
        let arr = Array::new();
        for s in self.encoded_inputs.drain(..) {
            arr.push(&JsValue::from_str(&s));
        }
        arr
    }

    /// Feed a VT/ANSI byte stream (remote mode).
    pub fn feed(&mut self, _data: &[u8]) {}

    /// Apply a cell patch (ftui-web mode).
    #[wasm_bindgen(js_name = applyPatch)]
    pub fn apply_patch(&mut self, _patch: JsValue) {}

    /// Request a frame render. In the full implementation this will schedule a
    /// WebGPU pass and present atomically.
    pub fn render(&mut self) {
        // Stub: real rendering will be implemented after WebGPU init work lands.
    }

    /// Explicit teardown for JS callers. Clears internal references so the
    /// canvas and any GPU resources can be reclaimed.
    pub fn destroy(&mut self) {
        self.initialized = false;
        self.canvas = None;
    }
}

fn event_mods(ev: &InputEvent) -> Modifiers {
    match ev {
        InputEvent::Key(k) => k.mods,
        InputEvent::Mouse(m) => m.mods,
        InputEvent::Wheel(w) => w.mods,
        InputEvent::Touch(t) => t.mods,
        InputEvent::Composition(_) | InputEvent::Focus(_) => Modifiers::empty(),
    }
}

fn parse_input_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let kind = get_string(event, "kind")?;
    match kind.as_str() {
        "key" => parse_key_event(event),
        "mouse" => parse_mouse_event(event),
        "wheel" => parse_wheel_event(event),
        "touch" => parse_touch_event(event),
        "composition" => parse_composition_event(event),
        "focus" => parse_focus_event(event),
        other => Err(JsValue::from_str(&format!("unknown input kind: {other}"))),
    }
}

fn parse_key_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let phase = parse_key_phase(event)?;
    let dom_key = get_string(event, "key")?;
    let dom_code = get_string(event, "code")?;
    let repeat = get_bool(event, "repeat")?.unwrap_or(false);
    let mods = parse_mods(event)?;
    let code = normalize_dom_key_code(&dom_key, &dom_code, mods);

    Ok(InputEvent::Key(KeyInput {
        phase,
        code,
        mods,
        repeat,
    }))
}

fn parse_mouse_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let phase = parse_mouse_phase(event)?;
    let x = get_u16(event, "x")?;
    let y = get_u16(event, "y")?;
    let mods = parse_mods(event)?;
    let button = get_u8_opt(event, "button")?.map(MouseButton::from_u8);

    Ok(InputEvent::Mouse(MouseInput {
        phase,
        button,
        x,
        y,
        mods,
    }))
}

fn parse_wheel_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let x = get_u16(event, "x")?;
    let y = get_u16(event, "y")?;
    let dx = get_i16(event, "dx")?;
    let dy = get_i16(event, "dy")?;
    let mods = parse_mods(event)?;

    Ok(InputEvent::Wheel(WheelInput { x, y, dx, dy, mods }))
}

fn parse_touch_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let phase = parse_touch_phase(event)?;
    let mods = parse_mods(event)?;

    let touches_val = Reflect::get(event, &JsValue::from_str("touches"))?;
    if touches_val.is_null() || touches_val.is_undefined() {
        return Err(JsValue::from_str("touch event missing touches[]"));
    }

    let touches_arr = Array::from(&touches_val);
    let mut touches = Vec::with_capacity(touches_arr.length() as usize);
    for t in touches_arr.iter() {
        let id = get_u32(&t, "id")?;
        let x = get_u16(&t, "x")?;
        let y = get_u16(&t, "y")?;
        touches.push(TouchPoint { id, x, y });
    }

    Ok(InputEvent::Touch(TouchInput {
        phase,
        touches,
        mods,
    }))
}

fn parse_composition_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let phase = parse_composition_phase(event)?;
    let data = get_string_opt(event, "data")?.map(Into::into);
    Ok(InputEvent::Composition(CompositionInput { phase, data }))
}

fn parse_focus_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let focused = get_bool(event, "focused")?
        .ok_or_else(|| JsValue::from_str("focus event missing focused:boolean"))?;
    Ok(InputEvent::Focus(FocusInput { focused }))
}

fn parse_key_phase(event: &JsValue) -> Result<KeyPhase, JsValue> {
    let phase = get_string(event, "phase")?;
    match phase.as_str() {
        "down" | "keydown" => Ok(KeyPhase::Down),
        "up" | "keyup" => Ok(KeyPhase::Up),
        other => Err(JsValue::from_str(&format!("invalid key phase: {other}"))),
    }
}

fn parse_mouse_phase(event: &JsValue) -> Result<MousePhase, JsValue> {
    let phase = get_string(event, "phase")?;
    match phase.as_str() {
        "down" => Ok(MousePhase::Down),
        "up" => Ok(MousePhase::Up),
        "move" => Ok(MousePhase::Move),
        "drag" => Ok(MousePhase::Drag),
        other => Err(JsValue::from_str(&format!("invalid mouse phase: {other}"))),
    }
}

fn parse_touch_phase(event: &JsValue) -> Result<TouchPhase, JsValue> {
    let phase = get_string(event, "phase")?;
    match phase.as_str() {
        "start" => Ok(TouchPhase::Start),
        "move" => Ok(TouchPhase::Move),
        "end" => Ok(TouchPhase::End),
        "cancel" => Ok(TouchPhase::Cancel),
        other => Err(JsValue::from_str(&format!("invalid touch phase: {other}"))),
    }
}

fn parse_composition_phase(event: &JsValue) -> Result<CompositionPhase, JsValue> {
    let phase = get_string(event, "phase")?;
    match phase.as_str() {
        "start" | "compositionstart" => Ok(CompositionPhase::Start),
        "update" | "compositionupdate" => Ok(CompositionPhase::Update),
        "end" | "commit" | "compositionend" => Ok(CompositionPhase::End),
        "cancel" | "compositioncancel" => Ok(CompositionPhase::Cancel),
        other => Err(JsValue::from_str(&format!(
            "invalid composition phase: {other}"
        ))),
    }
}

fn parse_mods(event: &JsValue) -> Result<Modifiers, JsValue> {
    // Preferred compact encoding: `mods: number` bitset.
    if let Ok(v) = Reflect::get(event, &JsValue::from_str("mods"))
        && let Some(n) = v.as_f64()
    {
        let bits_i64 = number_to_i64_exact(n, "mods")?;
        let bits = u8::try_from(bits_i64)
            .map_err(|_| JsValue::from_str("mods out of range (expected 0..=255)"))?;
        return Ok(Modifiers::from_bits_truncate_u8(bits));
    }

    // Alternate encoding: `mods: { shift, ctrl, alt, super/meta }`.
    if let Ok(v) = Reflect::get(event, &JsValue::from_str("mods"))
        && v.is_object()
    {
        return mods_from_flags(&v);
    }

    // Fallback: top-level boolean flags (supports DOM-like names too).
    mods_from_flags(event)
}

fn mods_from_flags(obj: &JsValue) -> Result<Modifiers, JsValue> {
    let shift = get_bool_any(obj, &["shift", "shiftKey"])?;
    let ctrl = get_bool_any(obj, &["ctrl", "ctrlKey"])?;
    let alt = get_bool_any(obj, &["alt", "altKey"])?;
    let sup = get_bool_any(obj, &["super", "meta", "metaKey", "superKey"])?;

    let mut mods = Modifiers::empty();
    if shift {
        mods |= Modifiers::SHIFT;
    }
    if ctrl {
        mods |= Modifiers::CTRL;
    }
    if alt {
        mods |= Modifiers::ALT;
    }
    if sup {
        mods |= Modifiers::SUPER;
    }
    Ok(mods)
}

fn get_string(obj: &JsValue, key: &str) -> Result<String, JsValue> {
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    if v.is_null() || v.is_undefined() {
        return Err(JsValue::from_str(&format!(
            "missing required string field: {key}"
        )));
    }
    v.as_string()
        .ok_or_else(|| JsValue::from_str(&format!("field {key} must be a string")))
}

fn get_string_opt(obj: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    v.as_string()
        .map(Some)
        .ok_or_else(|| JsValue::from_str(&format!("field {key} must be a string")))
}

fn get_bool(obj: &JsValue, key: &str) -> Result<Option<bool>, JsValue> {
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    Ok(Some(v.as_bool().ok_or_else(|| {
        JsValue::from_str(&format!("field {key} must be a boolean"))
    })?))
}

fn get_bool_any(obj: &JsValue, keys: &[&str]) -> Result<bool, JsValue> {
    for key in keys {
        if let Some(v) = get_bool(obj, key)? {
            return Ok(v);
        }
    }
    Ok(false)
}

fn get_u16(obj: &JsValue, key: &str) -> Result<u16, JsValue> {
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    let Some(n) = v.as_f64() else {
        return Err(JsValue::from_str(&format!("field {key} must be a number")));
    };
    let n_i64 = number_to_i64_exact(n, key)?;
    u16::try_from(n_i64).map_err(|_| JsValue::from_str(&format!("field {key} out of range")))
}

fn get_u32(obj: &JsValue, key: &str) -> Result<u32, JsValue> {
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    let Some(n) = v.as_f64() else {
        return Err(JsValue::from_str(&format!("field {key} must be a number")));
    };
    let n_i64 = number_to_i64_exact(n, key)?;
    u32::try_from(n_i64).map_err(|_| JsValue::from_str(&format!("field {key} out of range")))
}

fn get_u8_opt(obj: &JsValue, key: &str) -> Result<Option<u8>, JsValue> {
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    let Some(n) = v.as_f64() else {
        return Err(JsValue::from_str(&format!("field {key} must be a number")));
    };
    let n_i64 = number_to_i64_exact(n, key)?;
    let val =
        u8::try_from(n_i64).map_err(|_| JsValue::from_str(&format!("field {key} out of range")))?;
    Ok(Some(val))
}

fn get_i16(obj: &JsValue, key: &str) -> Result<i16, JsValue> {
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    let Some(n) = v.as_f64() else {
        return Err(JsValue::from_str(&format!("field {key} must be a number")));
    };
    let n_i64 = number_to_i64_exact(n, key)?;
    i16::try_from(n_i64).map_err(|_| JsValue::from_str(&format!("field {key} out of range")))
}

fn number_to_i64_exact(n: f64, key: &str) -> Result<i64, JsValue> {
    if !n.is_finite() {
        return Err(JsValue::from_str(&format!("field {key} must be finite")));
    }
    if n.fract() != 0.0 {
        return Err(JsValue::from_str(&format!(
            "field {key} must be an integer"
        )));
    }
    if n < (i64::MIN as f64) || n > (i64::MAX as f64) {
        return Err(JsValue::from_str(&format!("field {key} out of range")));
    }
    // After the integral check, `as i64` is safe and deterministic for our expected ranges.
    Ok(n as i64)
}

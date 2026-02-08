#![forbid(unsafe_code)]

use crate::input::{
    CompositionInput, CompositionPhase, CompositionState, FocusInput, InputEvent, KeyInput,
    KeyPhase, ModifierTracker, Modifiers, MouseButton, MouseInput, MousePhase, TouchInput,
    TouchPhase, TouchPoint, VtInputEncoderFeatures, WheelInput, encode_vt_input_event,
    normalize_dom_key_code,
};
use crate::renderer::{CellData, CellPatch, RendererConfig, WebGpuRenderer};
use js_sys::{Array, Reflect, Uint8Array};
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
    encoder_features: VtInputEncoderFeatures,
    encoded_inputs: Vec<String>,
    encoded_input_bytes: Vec<Vec<u8>>,
    renderer: Option<WebGpuRenderer>,
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
            encoder_features: VtInputEncoderFeatures::default(),
            encoded_inputs: Vec::new(),
            encoded_input_bytes: Vec::new(),
            renderer: None,
        }
    }

    /// Initialize the terminal surface with an existing `<canvas>`.
    ///
    /// Creates the WebGPU renderer, performing adapter/device negotiation.
    /// Exported as an async JS function returning a Promise.
    pub async fn init(
        &mut self,
        canvas: HtmlCanvasElement,
        options: Option<JsValue>,
    ) -> Result<(), JsValue> {
        let cols = parse_init_u16(&options, "cols").unwrap_or(80);
        let rows = parse_init_u16(&options, "rows").unwrap_or(24);
        let cell_width = parse_init_u16(&options, "cellWidth").unwrap_or(8);
        let cell_height = parse_init_u16(&options, "cellHeight").unwrap_or(16);
        let dpr = options
            .as_ref()
            .and_then(|o| Reflect::get(o, &JsValue::from_str("dpr")).ok())
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0) as f32;

        let config = RendererConfig {
            cell_width,
            cell_height,
            dpr,
        };

        let renderer = WebGpuRenderer::init(canvas.clone(), cols, rows, &config)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        self.cols = cols;
        self.rows = rows;
        self.canvas = Some(canvas);
        self.renderer = Some(renderer);
        self.encoder_features = parse_encoder_features(&options);
        self.initialized = true;
        Ok(())
    }

    /// Resize the terminal in logical grid coordinates (cols/rows).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        if let Some(r) = self.renderer.as_mut() {
            r.resize(cols, rows);
        }
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

            let vt = encode_vt_input_event(&ev, self.encoder_features);
            if !vt.is_empty() {
                self.encoded_input_bytes.push(vt);
            }
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

    /// Drain queued VT-compatible input byte chunks for remote PTY forwarding.
    #[wasm_bindgen(js_name = drainEncodedInputBytes)]
    pub fn drain_encoded_input_bytes(&mut self) -> Array {
        let arr = Array::new();
        for bytes in self.encoded_input_bytes.drain(..) {
            let chunk = Uint8Array::from(bytes.as_slice());
            arr.push(&chunk.into());
        }
        arr
    }

    /// Feed a VT/ANSI byte stream (remote mode).
    pub fn feed(&mut self, _data: &[u8]) {}

    /// Apply a cell patch (ftui-web mode).
    ///
    /// Accepts a JS object: `{ offset: number, cells: [{bg, fg, glyph, attrs}] }`.
    /// Only the patched cells are uploaded to the GPU.
    #[wasm_bindgen(js_name = applyPatch)]
    pub fn apply_patch(&mut self, patch: JsValue) -> Result<(), JsValue> {
        let Some(renderer) = self.renderer.as_mut() else {
            return Err(JsValue::from_str("renderer not initialized"));
        };

        let offset = get_u32(&patch, "offset")?;
        let cells_val = Reflect::get(&patch, &JsValue::from_str("cells"))?;
        if cells_val.is_null() || cells_val.is_undefined() {
            return Err(JsValue::from_str("patch missing cells[]"));
        }

        let cells_arr = Array::from(&cells_val);
        let mut cells = Vec::with_capacity(cells_arr.length() as usize);
        for c in cells_arr.iter() {
            let bg = get_u32(&c, "bg").unwrap_or(0x000000FF);
            let fg = get_u32(&c, "fg").unwrap_or(0xFFFFFFFF);
            let glyph = get_u32(&c, "glyph").unwrap_or(0);
            let attrs = get_u32(&c, "attrs").unwrap_or(0);
            cells.push(CellData {
                bg_rgba: bg,
                fg_rgba: fg,
                glyph_id: glyph,
                attrs,
            });
        }

        renderer.apply_patches(&[CellPatch { offset, cells }]);
        Ok(())
    }

    /// Request a frame render. Encodes and submits a WebGPU draw pass.
    pub fn render(&mut self) -> Result<(), JsValue> {
        let Some(renderer) = self.renderer.as_ref() else {
            return Ok(());
        };
        renderer
            .render_frame()
            .map(|_| ())
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Explicit teardown for JS callers. Drops GPU resources and clears
    /// internal references so the canvas can be reclaimed.
    pub fn destroy(&mut self) {
        self.renderer = None;
        self.initialized = false;
        self.canvas = None;
        self.encoded_inputs.clear();
        self.encoded_input_bytes.clear();
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

fn parse_init_u16(options: &Option<JsValue>, key: &str) -> Option<u16> {
    let obj = options.as_ref()?;
    let v = Reflect::get(obj, &JsValue::from_str(key)).ok()?;
    let n = v.as_f64()?;
    u16::try_from(n as i64).ok()
}

fn parse_init_bool(options: &Option<JsValue>, key: &str) -> Option<bool> {
    let obj = options.as_ref()?;
    let v = Reflect::get(obj, &JsValue::from_str(key)).ok()?;
    if v.is_null() || v.is_undefined() {
        return None;
    }
    v.as_bool()
}

fn parse_encoder_features(options: &Option<JsValue>) -> VtInputEncoderFeatures {
    let sgr_mouse = parse_init_bool(options, "sgrMouse").or(parse_init_bool(options, "sgr_mouse"));
    let bracketed_paste =
        parse_init_bool(options, "bracketedPaste").or(parse_init_bool(options, "bracketed_paste"));
    let focus_events =
        parse_init_bool(options, "focusEvents").or(parse_init_bool(options, "focus_events"));
    let kitty_keyboard =
        parse_init_bool(options, "kittyKeyboard").or(parse_init_bool(options, "kitty_keyboard"));

    VtInputEncoderFeatures {
        sgr_mouse: sgr_mouse.unwrap_or(false),
        bracketed_paste: bracketed_paste.unwrap_or(false),
        focus_events: focus_events.unwrap_or(false),
        kitty_keyboard: kitty_keyboard.unwrap_or(false),
    }
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

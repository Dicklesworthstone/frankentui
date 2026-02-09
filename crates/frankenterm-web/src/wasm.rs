#![forbid(unsafe_code)]

use crate::frame_harness::{GeometrySnapshot, resize_storm_frame_jsonl};
use crate::input::{
    AccessibilityInput, CompositionInput, CompositionPhase, CompositionState, FocusInput,
    InputEvent, KeyInput, KeyPhase, ModifierTracker, Modifiers, MouseButton, MouseInput,
    MousePhase, TouchInput, TouchPhase, TouchPoint, VtInputEncoderFeatures, WheelInput,
    encode_vt_input_event, normalize_dom_key_code,
};
use crate::renderer::{
    CellData, CellPatch, CursorStyle, GridGeometry, RendererConfig, WebGpuRenderer,
    cell_attr_link_id,
};
use js_sys::{Array, Object, Reflect, Uint8Array};
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
    link_clicks: Vec<LinkClickEvent>,
    hovered_link_id: u32,
    cursor_offset: Option<u32>,
    cursor_style: CursorStyle,
    selection_range: Option<(u32, u32)>,
    screen_reader_enabled: bool,
    high_contrast_enabled: bool,
    reduced_motion_enabled: bool,
    live_announcements: Vec<String>,
    shadow_cells: Vec<CellData>,
    renderer: Option<WebGpuRenderer>,
}

#[derive(Debug, Clone, Copy)]
struct LinkClickEvent {
    x: u16,
    y: u16,
    button: Option<MouseButton>,
    link_id: u32,
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
            link_clicks: Vec::new(),
            hovered_link_id: 0,
            cursor_offset: None,
            cursor_style: CursorStyle::None,
            selection_range: None,
            screen_reader_enabled: false,
            high_contrast_enabled: false,
            reduced_motion_enabled: false,
            live_announcements: Vec::new(),
            shadow_cells: Vec::new(),
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
        let zoom = parse_init_f32(&options, "zoom").unwrap_or(1.0);

        let config = RendererConfig {
            cell_width,
            cell_height,
            dpr,
            zoom,
        };

        let renderer = WebGpuRenderer::init(canvas.clone(), cols, rows, &config)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        self.cols = cols;
        self.rows = rows;
        self.shadow_cells = vec![CellData::EMPTY; usize::from(cols) * usize::from(rows)];
        self.canvas = Some(canvas);
        self.renderer = Some(renderer);
        self.encoder_features = parse_encoder_features(&options);
        self.screen_reader_enabled = parse_init_bool(&options, "screenReader")
            .or(parse_init_bool(&options, "screen_reader"))
            .unwrap_or(false);
        self.high_contrast_enabled = parse_init_bool(&options, "highContrast")
            .or(parse_init_bool(&options, "high_contrast"))
            .unwrap_or(false);
        self.reduced_motion_enabled = parse_init_bool(&options, "reducedMotion")
            .or(parse_init_bool(&options, "reduced_motion"))
            .unwrap_or(false);
        self.initialized = true;
        Ok(())
    }

    /// Resize the terminal in logical grid coordinates (cols/rows).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.shadow_cells
            .resize(usize::from(cols) * usize::from(rows), CellData::EMPTY);
        if let Some(r) = self.renderer.as_mut() {
            r.resize(cols, rows);
        }
        self.sync_renderer_interaction_state();
    }

    /// Update DPR + zoom scaling while preserving current grid size.
    ///
    /// Returns deterministic geometry snapshot:
    /// `{ cols, rows, pixelWidth, pixelHeight, cellWidthPx, cellHeightPx, dpr, zoom }`.
    #[wasm_bindgen(js_name = setScale)]
    pub fn set_scale(&mut self, dpr: f32, zoom: f32) -> Result<JsValue, JsValue> {
        let Some(renderer) = self.renderer.as_mut() else {
            return Err(JsValue::from_str("renderer not initialized"));
        };
        renderer.set_scale(dpr, zoom);
        let geometry = renderer.current_geometry();
        Ok(geometry_to_js(geometry))
    }

    /// Convenience wrapper for user-controlled zoom updates.
    #[wasm_bindgen(js_name = setZoom)]
    pub fn set_zoom(&mut self, zoom: f32) -> Result<JsValue, JsValue> {
        let Some(renderer) = self.renderer.as_mut() else {
            return Err(JsValue::from_str("renderer not initialized"));
        };
        let dpr = renderer.dpr();
        renderer.set_scale(dpr, zoom);
        let geometry = renderer.current_geometry();
        Ok(geometry_to_js(geometry))
    }

    /// Fit the grid to a CSS-pixel container using current font metrics.
    ///
    /// `container_width_css` and `container_height_css` are CSS pixels.
    /// `dpr` lets callers pass the latest `window.devicePixelRatio`.
    #[wasm_bindgen(js_name = fitToContainer)]
    pub fn fit_to_container(
        &mut self,
        container_width_css: u32,
        container_height_css: u32,
        dpr: f32,
    ) -> Result<JsValue, JsValue> {
        let Some(renderer) = self.renderer.as_mut() else {
            return Err(JsValue::from_str("renderer not initialized"));
        };

        let zoom = renderer.zoom();
        renderer.set_scale(dpr, zoom);
        let geometry = renderer.fit_to_container(container_width_css, container_height_css);
        self.cols = geometry.cols;
        self.rows = geometry.rows;
        self.shadow_cells.resize(
            usize::from(geometry.cols) * usize::from(geometry.rows),
            CellData::EMPTY,
        );
        Ok(geometry_to_js(geometry))
    }

    /// Emit one JSONL `frame` trace record for browser resize-storm E2E logs.
    ///
    /// The line includes both a deterministic frame hash and the current
    /// geometry snapshot so test runners can diagnose resize/zoom/DPR mismatches.
    #[wasm_bindgen(js_name = snapshotResizeStormFrameJsonl)]
    pub fn snapshot_resize_storm_frame_jsonl(
        &self,
        run_id: &str,
        seed: u32,
        timestamp: &str,
        frame_idx: u32,
    ) -> Result<String, JsValue> {
        if run_id.is_empty() {
            return Err(JsValue::from_str("run_id must not be empty"));
        }
        if timestamp.is_empty() {
            return Err(JsValue::from_str("timestamp must not be empty"));
        }

        let Some(renderer) = self.renderer.as_ref() else {
            return Err(JsValue::from_str("renderer not initialized"));
        };

        let geometry = GeometrySnapshot::from(renderer.current_geometry());
        Ok(resize_storm_frame_jsonl(
            run_id,
            u64::from(seed),
            timestamp,
            u64::from(frame_idx),
            geometry,
            &self.shadow_cells,
        ))
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

            if let InputEvent::Accessibility(a11y) = &ev {
                self.apply_accessibility_input(a11y);
            }
            self.handle_interaction_event(&ev);

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

        let max = usize::from(self.cols) * usize::from(self.rows);
        self.shadow_cells.resize(max, CellData::EMPTY);
        let start = usize::try_from(offset).unwrap_or(max).min(max);
        let count = cells.len().min(max.saturating_sub(start));
        for (i, cell) in cells.iter().take(count).enumerate() {
            self.shadow_cells[start + i] = *cell;
        }

        renderer.apply_patches(&[CellPatch { offset, cells }]);
        Ok(())
    }

    /// Configure cursor overlay.
    ///
    /// - `offset`: linear cell offset (`row * cols + col`), or `< 0` to clear.
    /// - `style`: `0=none`, `1=block`, `2=bar`, `3=underline`.
    #[wasm_bindgen(js_name = setCursor)]
    pub fn set_cursor(&mut self, offset: i32, style: u32) -> Result<(), JsValue> {
        self.cursor_offset = if offset < 0 {
            None
        } else {
            let value = u32::try_from(offset).map_err(|_| JsValue::from_str("invalid cursor"))?;
            self.clamp_offset(value)
        };
        self.cursor_style = if self.cursor_offset.is_some() {
            CursorStyle::from_u32(style)
        } else {
            CursorStyle::None
        };
        self.sync_renderer_interaction_state();
        Ok(())
    }

    /// Configure selection overlay using a `[start, end)` cell-offset range.
    ///
    /// Pass negative values to clear selection.
    #[wasm_bindgen(js_name = setSelectionRange)]
    pub fn set_selection_range(&mut self, start: i32, end: i32) -> Result<(), JsValue> {
        self.selection_range = if start < 0 || end < 0 {
            None
        } else {
            let start_u32 = u32::try_from(start).map_err(|_| JsValue::from_str("invalid start"))?;
            let end_u32 = u32::try_from(end).map_err(|_| JsValue::from_str("invalid end"))?;
            self.normalize_selection_range((start_u32, end_u32))
        };
        self.sync_renderer_interaction_state();
        Ok(())
    }

    #[wasm_bindgen(js_name = clearSelection)]
    pub fn clear_selection(&mut self) {
        self.selection_range = None;
        self.sync_renderer_interaction_state();
    }

    #[wasm_bindgen(js_name = setHoveredLinkId)]
    pub fn set_hovered_link_id(&mut self, link_id: u32) {
        self.hovered_link_id = link_id;
        self.sync_renderer_interaction_state();
    }

    /// Return hyperlink ID at a given grid cell (0 if none / out of bounds).
    #[wasm_bindgen(js_name = linkAt)]
    pub fn link_at(&self, x: u16, y: u16) -> u32 {
        self.link_id_at_xy(x, y)
    }

    /// Drain queued hyperlink click events detected from normalized mouse input.
    ///
    /// Each entry has `{x, y, button, linkId}`.
    #[wasm_bindgen(js_name = drainLinkClicks)]
    pub fn drain_link_clicks(&mut self) -> Array {
        let arr = Array::new();
        for click in self.link_clicks.drain(..) {
            let obj = Object::new();
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("x"),
                &JsValue::from_f64(f64::from(click.x)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("y"),
                &JsValue::from_f64(f64::from(click.y)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("button"),
                &click.button.map_or(JsValue::NULL, |button| {
                    JsValue::from_f64(f64::from(button.to_u8()))
                }),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("linkId"),
                &JsValue::from_f64(f64::from(click.link_id)),
            );
            arr.push(&obj);
        }
        arr
    }

    /// Extract selected text from current shadow cells (for copy workflows).
    #[wasm_bindgen(js_name = extractSelectionText)]
    pub fn extract_selection_text(&self) -> String {
        let Some((start, end)) = self.selection_range else {
            return String::new();
        };
        let cols = usize::from(self.cols.max(1));
        let total = self.shadow_cells.len() as u32;
        let mut out = String::new();
        let start = start.min(total);
        let end = end.min(total);
        for offset in start..end {
            let idx = usize::try_from(offset).unwrap_or(usize::MAX);
            if idx >= self.shadow_cells.len() {
                break;
            }
            if offset > start && idx % cols == 0 {
                out.push('\n');
            }
            let glyph_id = self.shadow_cells[idx].glyph_id;
            let ch = if glyph_id == 0 {
                ' '
            } else {
                char::from_u32(glyph_id).unwrap_or('□')
            };
            out.push(ch);
        }
        out
    }

    /// Update accessibility preferences from a JS object.
    ///
    /// Supported keys:
    /// - `screenReader` / `screen_reader`: boolean
    /// - `highContrast` / `high_contrast`: boolean
    /// - `reducedMotion` / `reduced_motion`: boolean
    /// - `announce`: string (optional live-region message)
    #[wasm_bindgen(js_name = setAccessibility)]
    pub fn set_accessibility(&mut self, options: JsValue) -> Result<(), JsValue> {
        let input = parse_accessibility_input(&options)?;
        self.apply_accessibility_input(&input);
        Ok(())
    }

    /// Return current accessibility preferences.
    ///
    /// Shape:
    /// `{ screenReader, highContrast, reducedMotion, pendingAnnouncements }`
    #[wasm_bindgen(js_name = accessibilityState)]
    pub fn accessibility_state(&self) -> JsValue {
        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("screenReader"),
            &JsValue::from_bool(self.screen_reader_enabled),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("highContrast"),
            &JsValue::from_bool(self.high_contrast_enabled),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("reducedMotion"),
            &JsValue::from_bool(self.reduced_motion_enabled),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("pendingAnnouncements"),
            &JsValue::from_f64(self.live_announcements.len() as f64),
        );
        obj.into()
    }

    /// Drain queued live-region announcements for host-side screen-reader wiring.
    #[wasm_bindgen(js_name = drainAccessibilityAnnouncements)]
    pub fn drain_accessibility_announcements(&mut self) -> Array {
        let out = Array::new();
        for entry in self.live_announcements.drain(..) {
            out.push(&JsValue::from_str(&entry));
        }
        out
    }

    /// Build plain-text viewport mirror for screen readers.
    #[wasm_bindgen(js_name = screenReaderMirrorText)]
    pub fn screen_reader_mirror_text(&self) -> String {
        if !self.screen_reader_enabled {
            return String::new();
        }
        self.build_screen_reader_mirror_text()
    }

    /// Request a frame render. Encodes and submits a WebGPU draw pass.
    pub fn render(&mut self) -> Result<(), JsValue> {
        let Some(renderer) = self.renderer.as_mut() else {
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
        self.link_clicks.clear();
        self.hovered_link_id = 0;
        self.cursor_offset = None;
        self.cursor_style = CursorStyle::None;
        self.selection_range = None;
        self.screen_reader_enabled = false;
        self.high_contrast_enabled = false;
        self.reduced_motion_enabled = false;
        self.live_announcements.clear();
        self.shadow_cells.clear();
    }
}

impl FrankenTermWeb {
    fn grid_capacity(&self) -> u32 {
        u32::from(self.cols).saturating_mul(u32::from(self.rows))
    }

    fn clamp_offset(&self, offset: u32) -> Option<u32> {
        (offset < self.grid_capacity()).then_some(offset)
    }

    fn normalize_selection_range(&self, range: (u32, u32)) -> Option<(u32, u32)> {
        let max = self.grid_capacity();
        let start = range.0.min(max);
        let end = range.1.min(max);
        if start == end {
            return None;
        }
        Some((start.min(end), start.max(end)))
    }

    fn sync_renderer_interaction_state(&mut self) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_hovered_link_id(self.hovered_link_id);
            renderer.set_cursor(self.cursor_offset, self.cursor_style);
            renderer.set_selection_range(self.selection_range);
        }
    }

    fn cell_offset_at_xy(&self, x: u16, y: u16) -> Option<usize> {
        if x >= self.cols || y >= self.rows {
            return None;
        }
        Some(usize::from(y) * usize::from(self.cols) + usize::from(x))
    }

    fn link_id_at_xy(&self, x: u16, y: u16) -> u32 {
        let Some(offset) = self.cell_offset_at_xy(x, y) else {
            return 0;
        };
        self.shadow_cells
            .get(offset)
            .map_or(0, |cell| cell_attr_link_id(cell.attrs))
    }

    fn set_hover_from_xy(&mut self, x: u16, y: u16) {
        let link_id = self.link_id_at_xy(x, y);
        if self.hovered_link_id != link_id {
            self.hovered_link_id = link_id;
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.set_hovered_link_id(link_id);
            }
        }
    }

    fn apply_accessibility_input(&mut self, input: &AccessibilityInput) {
        if let Some(v) = input.screen_reader {
            if self.screen_reader_enabled != v {
                let state = if v { "enabled" } else { "disabled" };
                self.push_live_announcement(&format!("Screen reader mode {state}."));
            }
            self.screen_reader_enabled = v;
        }
        if let Some(v) = input.high_contrast {
            if self.high_contrast_enabled != v {
                let state = if v { "enabled" } else { "disabled" };
                self.push_live_announcement(&format!("High contrast mode {state}."));
            }
            self.high_contrast_enabled = v;
        }
        if let Some(v) = input.reduced_motion {
            if self.reduced_motion_enabled != v {
                let state = if v { "enabled" } else { "disabled" };
                self.push_live_announcement(&format!("Reduced motion {state}."));
            }
            self.reduced_motion_enabled = v;
        }
        if let Some(text) = input.announce.as_deref() {
            self.push_live_announcement(text);
        }
    }

    fn push_live_announcement(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        // Keep the queue bounded so host-side consumers can poll lazily.
        let limit = 64;
        if self.live_announcements.len() >= limit {
            let overflow = self.live_announcements.len() - limit + 1;
            self.live_announcements.drain(..overflow);
        }
        self.live_announcements.push(trimmed.to_string());
    }

    fn build_screen_reader_mirror_text(&self) -> String {
        let cols = usize::from(self.cols.max(1));
        let rows = usize::from(self.rows);
        let mut out = String::new();
        for y in 0..rows {
            if y > 0 {
                out.push('\n');
            }
            let row_start = y.saturating_mul(cols);
            let row_end = row_start.saturating_add(cols).min(self.shadow_cells.len());
            let mut line = String::new();
            for idx in row_start..row_end {
                let glyph_id = self.shadow_cells[idx].glyph_id;
                let ch = if glyph_id == 0 {
                    ' '
                } else {
                    char::from_u32(glyph_id).unwrap_or('□')
                };
                line.push(ch);
            }
            out.push_str(line.trim_end_matches(' '));
        }
        out
    }

    fn handle_interaction_event(&mut self, ev: &InputEvent) {
        let InputEvent::Mouse(mouse) = ev else {
            return;
        };

        match mouse.phase {
            MousePhase::Move | MousePhase::Drag | MousePhase::Down => {
                self.set_hover_from_xy(mouse.x, mouse.y);
            }
            MousePhase::Up => {}
        }

        if mouse.phase == MousePhase::Down {
            let link_id = self.link_id_at_xy(mouse.x, mouse.y);
            if link_id != 0 {
                self.link_clicks.push(LinkClickEvent {
                    x: mouse.x,
                    y: mouse.y,
                    button: mouse.button,
                    link_id,
                });
            }
        }
    }
}

fn event_mods(ev: &InputEvent) -> Modifiers {
    match ev {
        InputEvent::Key(k) => k.mods,
        InputEvent::Mouse(m) => m.mods,
        InputEvent::Wheel(w) => w.mods,
        InputEvent::Touch(t) => t.mods,
        InputEvent::Composition(_) | InputEvent::Focus(_) | InputEvent::Accessibility(_) => {
            Modifiers::empty()
        }
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
        "accessibility" => parse_accessibility_event(event),
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

fn parse_accessibility_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let input = parse_accessibility_input(event)?;
    if input.is_noop() {
        return Err(JsValue::from_str(
            "accessibility event requires at least one of screenReader/highContrast/reducedMotion/announce",
        ));
    }
    Ok(InputEvent::Accessibility(input))
}

fn parse_accessibility_input(event: &JsValue) -> Result<AccessibilityInput, JsValue> {
    let screen_reader = parse_bool_alias(event, "screenReader", "screen_reader")?;
    let high_contrast = parse_bool_alias(event, "highContrast", "high_contrast")?;
    let reduced_motion = parse_bool_alias(event, "reducedMotion", "reduced_motion")?;
    let announce = get_string_opt(event, "announce")?.map(Into::into);
    Ok(AccessibilityInput {
        screen_reader,
        high_contrast,
        reduced_motion,
        announce,
    })
}

fn parse_bool_alias(event: &JsValue, camel: &str, snake: &str) -> Result<Option<bool>, JsValue> {
    if let Some(value) = get_bool(event, camel)? {
        return Ok(Some(value));
    }
    get_bool(event, snake)
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

fn parse_init_f32(options: &Option<JsValue>, key: &str) -> Option<f32> {
    let obj = options.as_ref()?;
    let v = Reflect::get(obj, &JsValue::from_str(key)).ok()?;
    let n = v.as_f64()? as f32;
    if n.is_finite() { Some(n) } else { None }
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

fn geometry_to_js(geometry: GridGeometry) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("cols"),
        &JsValue::from_f64(f64::from(geometry.cols)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("rows"),
        &JsValue::from_f64(f64::from(geometry.rows)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("pixelWidth"),
        &JsValue::from_f64(f64::from(geometry.pixel_width)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("pixelHeight"),
        &JsValue::from_f64(f64::from(geometry.pixel_height)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("cellWidthPx"),
        &JsValue::from_f64(f64::from(geometry.cell_width_px)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("cellHeightPx"),
        &JsValue::from_f64(f64::from(geometry.cell_height_px)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("dpr"),
        &JsValue::from_f64(f64::from(geometry.dpr)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("zoom"),
        &JsValue::from_f64(f64::from(geometry.zoom)),
    );
    obj.into()
}

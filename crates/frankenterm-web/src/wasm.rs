#![forbid(unsafe_code)]

use crate::frame_harness::{
    GeometrySnapshot, InteractionSnapshot, LinkClickSnapshot, link_click_jsonl,
    resize_storm_frame_jsonl_with_interaction,
};
use crate::input::{
    AccessibilityInput, CompositionInput, CompositionPhase, CompositionState, FocusInput,
    InputEvent, KeyInput, KeyPhase, ModifierTracker, Modifiers, MouseButton, MouseInput,
    MousePhase, PasteInput, TouchInput, TouchPhase, TouchPoint, VtInputEncoderFeatures, WheelInput,
    encode_vt_input_event, normalize_dom_key_code,
};
use crate::renderer::{
    CellData, CellPatch, CursorStyle, GridGeometry, RendererConfig, WebGpuRenderer,
    cell_attr_link_id, cell_patches_from_flat_u32,
};
use crate::scroll::{SearchConfig, SearchIndex};
use js_sys::{Array, Object, Reflect, Uint8Array, Uint32Array};
use std::collections::HashMap;
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

/// Synthetic link-id range reserved for auto-detected plaintext URLs.
const AUTO_LINK_ID_BASE: u32 = 0x00F0_0001;
const AUTO_LINK_ID_MAX: u32 = 0x00FF_FFFE;
/// Max decoded clipboard paste payload (matches websocket-protocol limits).
const MAX_PASTE_BYTES: usize = 768 * 1024;

fn empty_search_index(config: SearchConfig) -> SearchIndex {
    SearchIndex::build(std::iter::empty::<&str>(), "", config)
}

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
    auto_link_ids: Vec<u32>,
    auto_link_urls: HashMap<u32, String>,
    link_open_policy: LinkOpenPolicy,
    text_shaping: TextShapingConfig,
    hovered_link_id: u32,
    cursor_offset: Option<u32>,
    cursor_style: CursorStyle,
    selection_range: Option<(u32, u32)>,
    search_query: String,
    search_config: SearchConfig,
    search_index: SearchIndex,
    search_active_match: Option<usize>,
    search_highlight_range: Option<(u32, u32)>,
    screen_reader_enabled: bool,
    high_contrast_enabled: bool,
    reduced_motion_enabled: bool,
    focused: bool,
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

#[derive(Debug, Clone)]
struct ResolvedLinkClick {
    click: LinkClickEvent,
    source: &'static str,
    url: Option<String>,
    open_decision: LinkOpenDecision,
}

#[derive(Debug, Clone, Copy)]
struct LinkOpenDecision {
    allowed: bool,
    reason: Option<&'static str>,
}

impl LinkOpenDecision {
    const fn allow() -> Self {
        Self {
            allowed: true,
            reason: None,
        }
    }

    const fn deny(reason: &'static str) -> Self {
        Self {
            allowed: false,
            reason: Some(reason),
        }
    }
}

#[derive(Debug, Clone)]
struct LinkOpenPolicy {
    allow_http: bool,
    allow_https: bool,
    allowed_hosts: Vec<String>,
    blocked_hosts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TextShapingEngine {
    #[default]
    None,
    Harfbuzz,
}

impl TextShapingEngine {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Harfbuzz => "harfbuzz",
        }
    }

    const fn as_u32(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Harfbuzz => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct TextShapingConfig {
    enabled: bool,
    engine: TextShapingEngine,
}

impl Default for LinkOpenPolicy {
    fn default() -> Self {
        Self {
            allow_http: true,
            allow_https: true,
            allowed_hosts: Vec::new(),
            blocked_hosts: Vec::new(),
        }
    }
}

impl LinkOpenPolicy {
    fn evaluate(&self, url: Option<&str>) -> LinkOpenDecision {
        let Some(url) = url else {
            return LinkOpenDecision::deny("url_unavailable");
        };

        let Some((scheme, host)) = parse_http_url_scheme_and_host(url) else {
            return LinkOpenDecision::deny("invalid_url");
        };

        match scheme {
            "http" if !self.allow_http => return LinkOpenDecision::deny("scheme_blocked"),
            "https" if !self.allow_https => return LinkOpenDecision::deny("scheme_blocked"),
            "http" | "https" => {}
            _ => return LinkOpenDecision::deny("scheme_blocked"),
        }

        if self.blocked_hosts.iter().any(|blocked| blocked == &host) {
            return LinkOpenDecision::deny("host_blocked");
        }

        if !self.allowed_hosts.is_empty()
            && !self.allowed_hosts.iter().any(|allowed| allowed == &host)
        {
            return LinkOpenDecision::deny("host_not_allowlisted");
        }

        LinkOpenDecision::allow()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AccessibilityDomSnapshot {
    role: &'static str,
    aria_multiline: bool,
    aria_live: &'static str,
    aria_atomic: bool,
    tab_index: i32,
    focused: bool,
    focus_visible: bool,
    screen_reader: bool,
    high_contrast: bool,
    reduced_motion: bool,
    value: String,
    cursor_offset: Option<u32>,
    selection_start: Option<u32>,
    selection_end: Option<u32>,
}

impl AccessibilityDomSnapshot {
    fn validate(&self) -> Result<(), &'static str> {
        if self.role != "textbox" {
            return Err("role must be textbox");
        }
        if self.tab_index < 0 {
            return Err("tab_index must be non-negative");
        }
        if !self.aria_multiline {
            return Err("aria_multiline must be true");
        }
        if self.aria_live != "off" && self.aria_live != "polite" {
            return Err("aria_live must be off|polite");
        }
        if self.focus_visible && !self.focused {
            return Err("focus_visible requires focused");
        }
        if self.selection_start.is_some() != self.selection_end.is_some() {
            return Err("selection bounds must be paired");
        }
        if let (Some(start), Some(end)) = (self.selection_start, self.selection_end)
            && start > end
        {
            return Err("selection_start must be <= selection_end");
        }
        if !self.screen_reader && !self.value.is_empty() {
            return Err("value must be empty when screen_reader is disabled");
        }
        Ok(())
    }
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
            auto_link_ids: Vec::new(),
            auto_link_urls: HashMap::new(),
            link_open_policy: LinkOpenPolicy::default(),
            text_shaping: TextShapingConfig::default(),
            hovered_link_id: 0,
            cursor_offset: None,
            cursor_style: CursorStyle::None,
            selection_range: None,
            search_query: String::new(),
            search_config: SearchConfig::default(),
            search_index: empty_search_index(SearchConfig::default()),
            search_active_match: None,
            search_highlight_range: None,
            screen_reader_enabled: false,
            high_contrast_enabled: false,
            reduced_motion_enabled: false,
            focused: false,
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
        self.auto_link_ids = vec![0; usize::from(cols) * usize::from(rows)];
        self.auto_link_urls.clear();
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
        self.focused = parse_init_bool(&options, "focused").unwrap_or(false);
        self.link_open_policy = parse_link_open_policy(options.as_ref())?;
        self.text_shaping =
            parse_text_shaping_config(options.as_ref(), TextShapingConfig::default())?;
        self.initialized = true;
        Ok(())
    }

    /// Resize the terminal in logical grid coordinates (cols/rows).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.shadow_cells
            .resize(usize::from(cols) * usize::from(rows), CellData::EMPTY);
        self.auto_link_ids
            .resize(usize::from(cols) * usize::from(rows), 0);
        self.auto_link_urls.clear();
        self.refresh_search_after_buffer_change();
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
        self.auto_link_ids
            .resize(usize::from(geometry.cols) * usize::from(geometry.rows), 0);
        self.auto_link_urls.clear();
        self.refresh_search_after_buffer_change();
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
        Ok(resize_storm_frame_jsonl_with_interaction(
            run_id,
            u64::from(seed),
            timestamp,
            u64::from(frame_idx),
            geometry,
            &self.shadow_cells,
            self.resize_storm_interaction_snapshot(),
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
            self.queue_input_event(ev)?;
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

    /// Queue pasted text as terminal input bytes.
    ///
    /// Browser clipboard APIs require trusted user gestures; hosts should read
    /// clipboard content in JS and pass the text here for deterministic VT encoding.
    #[wasm_bindgen(js_name = pasteText)]
    pub fn paste_text(&mut self, text: &str) -> Result<(), JsValue> {
        if text.is_empty() {
            return Ok(());
        }
        if text.len() > MAX_PASTE_BYTES {
            return Err(JsValue::from_str(
                "paste payload too large (max 786432 UTF-8 bytes)",
            ));
        }
        self.queue_input_event(InputEvent::Paste(PasteInput { data: text.into() }))
    }

    /// Feed a VT/ANSI byte stream (remote mode).
    pub fn feed(&mut self, _data: &[u8]) {}

    /// Apply a cell patch (ftui-web mode).
    ///
    /// Accepts a JS object: `{ offset: number, cells: [{bg, fg, glyph, attrs}] }`.
    /// When a renderer is initialized, only the patched cells are uploaded to
    /// the GPU. Without a renderer, patches still update the in-memory shadow
    /// state so host-side logic (search/link lookup/evidence) remains usable.
    #[wasm_bindgen(js_name = applyPatch)]
    pub fn apply_patch(&mut self, patch: JsValue) -> Result<(), JsValue> {
        let patch = parse_cell_patch(&patch)?;
        self.apply_cell_patches(std::slice::from_ref(&patch));
        Ok(())
    }

    /// Apply multiple cell patches (ftui-web mode).
    ///
    /// Accepts a JS array:
    /// `[{ offset: number, cells: [{bg, fg, glyph, attrs}] }, ...]`.
    ///
    /// This is optimized for `ftui-web` patch runs so hosts can forward a
    /// complete present step with one JS→WASM call.
    #[wasm_bindgen(js_name = applyPatchBatch)]
    pub fn apply_patch_batch(&mut self, patches: JsValue) -> Result<(), JsValue> {
        if patches.is_null() || patches.is_undefined() {
            return Err(JsValue::from_str("patch batch must be an array"));
        }
        if !Array::is_array(&patches) {
            return Err(JsValue::from_str("patch batch must be an array"));
        }

        let patches_arr = Array::from(&patches);
        let mut parsed = Vec::with_capacity(patches_arr.length() as usize);
        for patch in patches_arr.iter() {
            parsed.push(parse_cell_patch(&patch)?);
        }
        self.apply_cell_patches(&parsed);
        Ok(())
    }

    /// Apply multiple cell patches from flat payload arrays (ftui-web fast path).
    ///
    /// - `spans`: `Uint32Array` in `[offset, len, offset, len, ...]` order
    /// - `cells`: `Uint32Array` in `[bg, fg, glyph, attrs, ...]` order
    ///
    /// `len` is measured in cells (not `u32` words).
    #[wasm_bindgen(js_name = applyPatchBatchFlat)]
    pub fn apply_patch_batch_flat(
        &mut self,
        spans: Uint32Array,
        cells: Uint32Array,
    ) -> Result<(), JsValue> {
        let spans = spans.to_vec();
        let cells = cells.to_vec();
        let parsed = cell_patches_from_flat_u32(&spans, &cells).map_err(JsValue::from_str)?;
        self.apply_cell_patches(&parsed);
        Ok(())
    }

    fn apply_cell_patches(&mut self, patches: &[CellPatch]) {
        let max = usize::from(self.cols) * usize::from(self.rows);
        self.shadow_cells.resize(max, CellData::EMPTY);
        self.auto_link_ids.resize(max, 0);

        for patch in patches {
            let start = usize::try_from(patch.offset).unwrap_or(max).min(max);
            let count = patch.cells.len().min(max.saturating_sub(start));
            for (i, cell) in patch.cells.iter().take(count).enumerate() {
                self.shadow_cells[start + i] = *cell;
            }
        }

        self.recompute_auto_links();
        self.refresh_search_after_buffer_change();
        if self.hovered_link_id != 0 && !self.link_id_present(self.hovered_link_id) {
            self.hovered_link_id = 0;
            self.sync_renderer_interaction_state();
        }

        if let Some(renderer) = self.renderer.as_mut() {
            renderer.apply_patches(patches);
        }
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

    /// Build or refresh search results over the current shadow grid.
    ///
    /// `options` keys:
    /// - `caseSensitive` / `case_sensitive`: boolean (default false)
    /// - `normalizeUnicode` / `normalize_unicode`: boolean (default true)
    ///
    /// Returns current search state:
    /// `{query, normalizedQuery, caseSensitive, normalizeUnicode, matchCount,
    ///   activeMatchIndex, activeLine, activeStart, activeEnd}`
    #[wasm_bindgen(js_name = setSearchQuery)]
    pub fn set_search_query(
        &mut self,
        query: &str,
        options: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        self.search_query.clear();
        self.search_query.push_str(query);
        self.search_config = parse_search_config(options.as_ref())?;
        self.refresh_search_after_buffer_change();
        Ok(self.search_state())
    }

    /// Jump to the next search match (wrap at end) and update highlight overlay.
    ///
    /// Returns current search state.
    #[wasm_bindgen(js_name = searchNext)]
    pub fn search_next(&mut self) -> JsValue {
        self.search_active_match = self.search_index.next_index(self.search_active_match);
        self.search_highlight_range = self.search_highlight_for_active_match();
        self.sync_renderer_interaction_state();
        self.search_state()
    }

    /// Jump to the previous search match (wrap at beginning) and update highlight overlay.
    ///
    /// Returns current search state.
    #[wasm_bindgen(js_name = searchPrev)]
    pub fn search_prev(&mut self) -> JsValue {
        self.search_active_match = self.search_index.prev_index(self.search_active_match);
        self.search_highlight_range = self.search_highlight_for_active_match();
        self.sync_renderer_interaction_state();
        self.search_state()
    }

    /// Clear search query/results and remove search highlight.
    #[wasm_bindgen(js_name = clearSearch)]
    pub fn clear_search(&mut self) {
        self.search_query.clear();
        self.search_index = empty_search_index(self.search_config);
        self.search_active_match = None;
        self.search_highlight_range = None;
        self.sync_renderer_interaction_state();
    }

    /// Return search state snapshot as a JS object.
    ///
    /// Shape:
    /// `{ query, normalizedQuery, caseSensitive, normalizeUnicode, matchCount,
    ///    activeMatchIndex, activeLine, activeStart, activeEnd }`
    #[wasm_bindgen(js_name = searchState)]
    pub fn search_state(&self) -> JsValue {
        search_state_to_js(
            &self.search_query,
            self.search_config,
            &self.search_index,
            self.search_active_match,
        )
    }

    /// Return hyperlink ID at a given grid cell (0 if none / out of bounds).
    #[wasm_bindgen(js_name = linkAt)]
    pub fn link_at(&self, x: u16, y: u16) -> u32 {
        self.link_id_at_xy(x, y)
    }

    /// Return plaintext auto-detected URL at a given grid cell, if present.
    #[wasm_bindgen(js_name = linkUrlAt)]
    pub fn link_url_at(&self, x: u16, y: u16) -> Option<String> {
        let offset = self.cell_offset_at_xy(x, y)?;
        let id = self.auto_link_ids.get(offset).copied().unwrap_or(0);
        self.auto_link_urls.get(&id).cloned()
    }

    /// Drain queued hyperlink click events detected from normalized mouse input.
    ///
    /// Each entry has:
    /// `{x, y, button, linkId, source, url, openAllowed, openReason}`.
    #[wasm_bindgen(js_name = drainLinkClicks)]
    pub fn drain_link_clicks(&mut self) -> Array {
        let arr = Array::new();
        for resolved in self.drain_resolved_link_clicks() {
            let click = resolved.click;
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
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("source"),
                &JsValue::from_str(resolved.source),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("url"),
                &resolved
                    .url
                    .as_ref()
                    .map_or(JsValue::NULL, |url| JsValue::from_str(url)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("openAllowed"),
                &JsValue::from_bool(resolved.open_decision.allowed),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("openReason"),
                &resolved
                    .open_decision
                    .reason
                    .map_or(JsValue::NULL, JsValue::from_str),
            );
            arr.push(&obj);
        }
        arr
    }

    /// Drain queued link clicks into JSONL lines for deterministic E2E logs.
    ///
    /// Host code can persist the returned lines directly into an E2E JSONL log.
    #[wasm_bindgen(js_name = drainLinkClicksJsonl)]
    pub fn drain_link_clicks_jsonl(
        &mut self,
        run_id: String,
        seed: u64,
        timestamp: String,
    ) -> Array {
        let out = Array::new();
        for (event_idx, resolved) in self.drain_resolved_link_clicks().into_iter().enumerate() {
            let click = &resolved.click;
            let snapshot = LinkClickSnapshot {
                x: click.x,
                y: click.y,
                button: click.button.map(MouseButton::to_u8),
                link_id: click.link_id,
                url: resolved.url,
                open_allowed: resolved.open_decision.allowed,
                open_reason: resolved.open_decision.reason.map(str::to_string),
            };
            let line = link_click_jsonl(&run_id, seed, &timestamp, event_idx as u64, &snapshot);
            out.push(&JsValue::from_str(&line));
        }
        out
    }

    /// Configure host-side link open policy.
    ///
    /// Supported keys:
    /// - `allowHttp` / `allow_http`: bool
    /// - `allowHttps` / `allow_https`: bool
    /// - `allowedHosts` / `allowed_hosts`: string[]
    /// - `blockedHosts` / `blocked_hosts`: string[]
    #[wasm_bindgen(js_name = setLinkOpenPolicy)]
    pub fn set_link_open_policy(&mut self, options: JsValue) -> Result<(), JsValue> {
        self.link_open_policy = parse_link_open_policy(Some(&options))?;
        Ok(())
    }

    /// Return current link open policy snapshot.
    #[wasm_bindgen(js_name = linkOpenPolicy)]
    pub fn link_open_policy_snapshot(&self) -> JsValue {
        link_open_policy_to_js(&self.link_open_policy)
    }

    /// Configure text shaping / ligature behavior.
    ///
    /// Supported keys:
    /// - `enabled`: bool
    /// - `shapingEnabled` / `shaping_enabled`: bool
    /// - `textShaping` / `text_shaping`: bool
    ///
    /// Default behavior is disabled to preserve baseline perf characteristics.
    #[wasm_bindgen(js_name = setTextShaping)]
    pub fn set_text_shaping(&mut self, options: JsValue) -> Result<(), JsValue> {
        self.text_shaping = parse_text_shaping_config(Some(&options), self.text_shaping)?;
        Ok(())
    }

    /// Return current text shaping configuration.
    ///
    /// Shape: `{ enabled, engine, fallback }`
    #[wasm_bindgen(js_name = textShapingState)]
    pub fn text_shaping_state(&self) -> JsValue {
        text_shaping_config_to_js(self.text_shaping)
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

    /// Return selected text for host-managed clipboard writes.
    ///
    /// Returns `None` when there is no active non-empty selection.
    #[wasm_bindgen(js_name = copySelection)]
    pub fn copy_selection(&self) -> Option<String> {
        let text = self.extract_selection_text();
        if text.is_empty() { None } else { Some(text) }
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
    /// `{ screenReader, highContrast, reducedMotion, focused, pendingAnnouncements }`
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
            &JsValue::from_str("focused"),
            &JsValue::from_bool(self.focused),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("pendingAnnouncements"),
            &JsValue::from_f64(self.live_announcements.len() as f64),
        );
        obj.into()
    }

    /// Expose a host-friendly DOM mirror snapshot for ARIA wiring.
    ///
    /// Shape:
    /// `{ role, ariaMultiline, ariaLive, ariaAtomic, tabIndex, focused, focusVisible,
    ///    screenReader, highContrast, reducedMotion, value, cursorOffset,
    ///    selectionStart, selectionEnd }`
    #[wasm_bindgen(js_name = accessibilityDomSnapshot)]
    pub fn accessibility_dom_snapshot(&self) -> JsValue {
        let snapshot = self.build_accessibility_dom_snapshot();
        debug_assert!(snapshot.validate().is_ok());
        accessibility_dom_snapshot_to_js(&snapshot)
    }

    /// Suggested host-side CSS classes for accessibility modes.
    #[wasm_bindgen(js_name = accessibilityClassNames)]
    pub fn accessibility_class_names(&self) -> Array {
        let out = Array::new();
        if self.screen_reader_enabled {
            out.push(&JsValue::from_str("ftui-a11y-screen-reader"));
        }
        if self.high_contrast_enabled {
            out.push(&JsValue::from_str("ftui-a11y-high-contrast"));
        }
        if self.reduced_motion_enabled {
            out.push(&JsValue::from_str("ftui-a11y-reduced-motion"));
        }
        if self.focused {
            out.push(&JsValue::from_str("ftui-a11y-focused"));
        }
        out
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
        self.auto_link_ids.clear();
        self.auto_link_urls.clear();
        self.text_shaping = TextShapingConfig::default();
        self.hovered_link_id = 0;
        self.cursor_offset = None;
        self.cursor_style = CursorStyle::None;
        self.selection_range = None;
        self.search_query.clear();
        self.search_index = empty_search_index(self.search_config);
        self.search_active_match = None;
        self.search_highlight_range = None;
        self.screen_reader_enabled = false;
        self.high_contrast_enabled = false;
        self.reduced_motion_enabled = false;
        self.focused = false;
        self.live_announcements.clear();
        self.shadow_cells.clear();
    }
}

impl FrankenTermWeb {
    fn queue_input_event(&mut self, ev: InputEvent) -> Result<(), JsValue> {
        // Guarantee no "stuck modifiers" after focus loss by treating focus
        // loss as an explicit modifier reset point.
        if let InputEvent::Focus(focus) = &ev {
            self.set_focus_internal(focus.focused);
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
        Ok(())
    }

    fn set_focus_internal(&mut self, focused: bool) {
        self.focused = focused;
        self.mods.handle_focus(focused);
        if !focused {
            self.hovered_link_id = 0;
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.set_hovered_link_id(0);
            }
        }
    }

    fn build_accessibility_dom_snapshot(&self) -> AccessibilityDomSnapshot {
        let (selection_start, selection_end) = self
            .selection_range
            .map(|(start, end)| (Some(start), Some(end)))
            .unwrap_or((None, None));
        AccessibilityDomSnapshot {
            role: "textbox",
            aria_multiline: true,
            aria_live: if self.live_announcements.is_empty() {
                "off"
            } else {
                "polite"
            },
            aria_atomic: false,
            tab_index: 0,
            focused: self.focused,
            focus_visible: self.focused,
            screen_reader: self.screen_reader_enabled,
            high_contrast: self.high_contrast_enabled,
            reduced_motion: self.reduced_motion_enabled,
            value: self.screen_reader_mirror_text(),
            cursor_offset: self.cursor_offset,
            selection_start,
            selection_end,
        }
    }

    fn resize_storm_interaction_snapshot(&self) -> Option<InteractionSnapshot> {
        let has_shaping_state = self.text_shaping != TextShapingConfig::default();
        let has_a11y_state = self.screen_reader_enabled
            || self.high_contrast_enabled
            || self.reduced_motion_enabled
            || self.focused;
        let has_overlay = self.hovered_link_id != 0
            || self.cursor_offset.is_some()
            || self.active_selection_range().is_some()
            || has_shaping_state
            || has_a11y_state;
        if !has_overlay {
            return None;
        }
        let (selection_active, selection_start, selection_end) = self
            .active_selection_range()
            .map_or((false, 0, 0), |(start, end)| (true, start, end));
        Some(InteractionSnapshot {
            hovered_link_id: self.hovered_link_id,
            cursor_offset: self.cursor_offset.unwrap_or(0),
            cursor_style: self.cursor_style.as_u32(),
            selection_active,
            selection_start,
            selection_end,
            text_shaping_enabled: self.text_shaping.enabled,
            text_shaping_engine: self.text_shaping.engine.as_u32(),
            screen_reader_enabled: self.screen_reader_enabled,
            high_contrast_enabled: self.high_contrast_enabled,
            reduced_motion_enabled: self.reduced_motion_enabled,
            focused: self.focused,
        })
    }

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

    fn active_selection_range(&self) -> Option<(u32, u32)> {
        self.selection_range.or(self.search_highlight_range)
    }

    fn sync_renderer_interaction_state(&mut self) {
        let selection = self.active_selection_range();
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_hovered_link_id(self.hovered_link_id);
            renderer.set_cursor(self.cursor_offset, self.cursor_style);
            renderer.set_selection_range(selection);
        }
    }

    fn build_search_lines(&self) -> Vec<String> {
        let cols = usize::from(self.cols.max(1));
        let rows = usize::from(self.rows);
        let mut lines = Vec::with_capacity(rows);

        for y in 0..rows {
            let row_start = y.saturating_mul(cols);
            let row_end = row_start.saturating_add(cols).min(self.shadow_cells.len());
            let mut line = String::with_capacity(cols);
            let mut char_count = 0usize;
            for idx in row_start..row_end {
                let glyph_id = self.shadow_cells[idx].glyph_id;
                let ch = if glyph_id == 0 {
                    ' '
                } else {
                    char::from_u32(glyph_id).unwrap_or('□')
                };
                line.push(ch);
                char_count = char_count.saturating_add(1);
            }
            while char_count < cols {
                line.push(' ');
                char_count = char_count.saturating_add(1);
            }
            lines.push(line);
        }

        lines
    }

    fn search_highlight_for_active_match(&self) -> Option<(u32, u32)> {
        let idx = self.search_active_match?;
        let search_match = *self.search_index.matches().get(idx)?;
        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        if cols == 0 || rows == 0 || search_match.line_idx >= rows {
            return None;
        }

        let start_col = search_match.start_char.min(cols);
        let end_col = search_match.end_char.min(cols);
        if end_col <= start_col {
            return None;
        }

        let line_base = search_match.line_idx.saturating_mul(cols);
        let start = line_base.saturating_add(start_col) as u32;
        let end = line_base.saturating_add(end_col) as u32;
        self.normalize_selection_range((start, end))
    }

    fn refresh_search_after_buffer_change(&mut self) {
        if self.search_query.is_empty() {
            self.search_index = empty_search_index(self.search_config);
            self.search_active_match = None;
            self.search_highlight_range = None;
            self.sync_renderer_interaction_state();
            return;
        }

        let prev_active = self.search_active_match;
        let lines = self.build_search_lines();
        self.search_index = SearchIndex::build(
            lines.iter().map(String::as_str),
            &self.search_query,
            self.search_config,
        );

        self.search_active_match = if self.search_index.is_empty() {
            None
        } else {
            prev_active
                .filter(|idx| *idx < self.search_index.len())
                .or_else(|| self.search_index.next_index(None))
        };

        self.search_highlight_range = self.search_highlight_for_active_match();
        self.sync_renderer_interaction_state();
    }

    fn cell_offset_at_xy(&self, x: u16, y: u16) -> Option<usize> {
        if x >= self.cols || y >= self.rows {
            return None;
        }
        Some(usize::from(y) * usize::from(self.cols) + usize::from(x))
    }

    fn drain_resolved_link_clicks(&mut self) -> Vec<ResolvedLinkClick> {
        let clicks: Vec<LinkClickEvent> = self.link_clicks.drain(..).collect();
        clicks
            .into_iter()
            .map(|click| self.resolve_link_click(click))
            .collect()
    }

    fn resolve_link_click(&self, click: LinkClickEvent) -> ResolvedLinkClick {
        let url = self.auto_link_urls.get(&click.link_id).cloned();
        let source = if url.is_some() { "auto" } else { "osc8" };
        let open_decision = self.link_open_policy.evaluate(url.as_deref());
        ResolvedLinkClick {
            click,
            source,
            url,
            open_decision,
        }
    }

    fn link_id_at_xy(&self, x: u16, y: u16) -> u32 {
        let Some(offset) = self.cell_offset_at_xy(x, y) else {
            return 0;
        };
        let explicit = self
            .shadow_cells
            .get(offset)
            .map_or(0, |cell| cell_attr_link_id(cell.attrs));
        if explicit != 0 {
            return explicit;
        }
        self.auto_link_ids.get(offset).copied().unwrap_or(0)
    }

    fn link_id_present(&self, link_id: u32) -> bool {
        if link_id == 0 {
            return false;
        }
        if self.auto_link_urls.contains_key(&link_id) {
            return true;
        }
        self.shadow_cells
            .iter()
            .any(|cell| cell_attr_link_id(cell.attrs) == link_id)
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

    fn recompute_auto_links(&mut self) {
        let max = usize::from(self.cols) * usize::from(self.rows);
        self.auto_link_ids.resize(max, 0);
        self.auto_link_ids.fill(0);
        self.auto_link_urls.clear();

        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        let mut next_id = AUTO_LINK_ID_BASE;

        for row in 0..rows {
            let row_start = row.saturating_mul(cols);
            let row_end = row_start.saturating_add(cols).min(self.shadow_cells.len());
            if row_start >= row_end {
                break;
            }

            let mut row_chars = Vec::with_capacity(row_end - row_start);
            for idx in row_start..row_end {
                let glyph_id = self.shadow_cells[idx].glyph_id;
                let ch = if glyph_id == 0 {
                    ' '
                } else {
                    char::from_u32(glyph_id).unwrap_or(' ')
                };
                row_chars.push(ch);
            }

            for detected in detect_auto_urls_in_row(&row_chars) {
                if next_id > AUTO_LINK_ID_MAX {
                    return;
                }
                let link_id = next_id;
                next_id = next_id.saturating_add(1);
                self.auto_link_urls.insert(link_id, detected.url);

                for col in detected.start_col..detected.end_col {
                    let idx = row_start + col;
                    if idx >= row_end {
                        break;
                    }
                    if cell_attr_link_id(self.shadow_cells[idx].attrs) == 0 {
                        self.auto_link_ids[idx] = link_id;
                    }
                }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct AutoUrlMatch {
    start_col: usize,
    end_col: usize,
    url: String,
}

fn detect_auto_urls_in_row(row: &[char]) -> Vec<AutoUrlMatch> {
    let mut matches = Vec::new();
    let mut idx = 0usize;
    while idx < row.len() {
        if let Some(url_match) = detect_auto_url_at(row, idx) {
            idx = url_match.end_col;
            matches.push(url_match);
        } else {
            idx = idx.saturating_add(1);
        }
    }
    matches
}

fn detect_auto_url_at(row: &[char], start: usize) -> Option<AutoUrlMatch> {
    const HTTP: &[char] = &['h', 't', 't', 'p', ':', '/', '/'];
    const HTTPS: &[char] = &['h', 't', 't', 'p', 's', ':', '/', '/'];

    let has_http = row.get(start..start + HTTP.len()) == Some(HTTP);
    let has_https = row.get(start..start + HTTPS.len()) == Some(HTTPS);
    let prefix_len = if has_https {
        HTTPS.len()
    } else if has_http {
        HTTP.len()
    } else {
        return None;
    };

    if start > 0 {
        let prev = row[start - 1];
        if prev.is_ascii_alphanumeric() || prev == '_' {
            return None;
        }
    }

    let mut end = start;
    while end < row.len() && is_url_char(row[end]) {
        end += 1;
    }
    if end <= start + prefix_len {
        return None;
    }
    while end > start && is_url_trailing_punctuation(row[end - 1]) {
        end -= 1;
    }
    if end <= start + prefix_len {
        return None;
    }

    let candidate: String = row[start..end].iter().collect();
    let url = sanitize_auto_url(&candidate)?;
    Some(AutoUrlMatch {
        start_col: start,
        end_col: end,
        url,
    })
}

fn is_url_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '-' | '_'
                | '.'
                | '~'
                | '/'
                | ':'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
        )
}

fn is_url_trailing_punctuation(ch: char) -> bool {
    matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
}

fn sanitize_auto_url(candidate: &str) -> Option<String> {
    if candidate.is_empty() || candidate.len() > 2048 {
        return None;
    }
    if candidate.chars().any(char::is_control) {
        return None;
    }
    let lower = candidate.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Some(candidate.to_owned())
    } else {
        None
    }
}

fn event_mods(ev: &InputEvent) -> Modifiers {
    match ev {
        InputEvent::Key(k) => k.mods,
        InputEvent::Mouse(m) => m.mods,
        InputEvent::Wheel(w) => w.mods,
        InputEvent::Touch(t) => t.mods,
        InputEvent::Composition(_)
        | InputEvent::Paste(_)
        | InputEvent::Focus(_)
        | InputEvent::Accessibility(_) => Modifiers::empty(),
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
        "paste" => parse_paste_event(event),
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

fn parse_paste_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let data = get_string(event, "data")?;
    if data.len() > MAX_PASTE_BYTES {
        return Err(JsValue::from_str(
            "paste payload too large (max 786432 UTF-8 bytes)",
        ));
    }
    Ok(InputEvent::Paste(PasteInput { data: data.into() }))
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

fn parse_cell_patch(patch: &JsValue) -> Result<CellPatch, JsValue> {
    let offset = get_u32(patch, "offset")?;
    let cells_val = Reflect::get(patch, &JsValue::from_str("cells"))?;
    if cells_val.is_null() || cells_val.is_undefined() {
        return Err(JsValue::from_str("patch missing cells[]"));
    }

    let cells_arr = Array::from(&cells_val);
    let mut cells = Vec::with_capacity(cells_arr.length() as usize);
    for cell in cells_arr.iter() {
        let bg = get_u32(&cell, "bg").unwrap_or(0x000000FF);
        let fg = get_u32(&cell, "fg").unwrap_or(0xFFFFFFFF);
        let glyph = get_u32(&cell, "glyph").unwrap_or(0);
        let attrs = get_u32(&cell, "attrs").unwrap_or(0);
        cells.push(CellData {
            bg_rgba: bg,
            fg_rgba: fg,
            glyph_id: glyph,
            attrs,
        });
    }

    Ok(CellPatch { offset, cells })
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

fn parse_search_config(options: Option<&JsValue>) -> Result<SearchConfig, JsValue> {
    let mut config = SearchConfig::default();

    let Some(options) = options else {
        return Ok(config);
    };

    if let Some(v) = get_bool(options, "caseSensitive")?.or(get_bool(options, "case_sensitive")?) {
        config.case_sensitive = v;
    }
    if let Some(v) =
        get_bool(options, "normalizeUnicode")?.or(get_bool(options, "normalize_unicode")?)
    {
        config.normalize_unicode = v;
    }

    Ok(config)
}

fn parse_link_open_policy(options: Option<&JsValue>) -> Result<LinkOpenPolicy, JsValue> {
    let mut policy = LinkOpenPolicy::default();
    let Some(options) = options else {
        return Ok(policy);
    };

    if let Some(v) = get_bool(options, "allowHttp")?.or(get_bool(options, "allow_http")?) {
        policy.allow_http = v;
    }
    if let Some(v) = get_bool(options, "allowHttps")?.or(get_bool(options, "allow_https")?) {
        policy.allow_https = v;
    }
    if let Some(v) = get_host_list(options, &["allowedHosts", "allowed_hosts"])? {
        policy.allowed_hosts = v;
    }
    if let Some(v) = get_host_list(options, &["blockedHosts", "blocked_hosts"])? {
        policy.blocked_hosts = v;
    }

    Ok(policy)
}

fn parse_text_shaping_config(
    options: Option<&JsValue>,
    mut config: TextShapingConfig,
) -> Result<TextShapingConfig, JsValue> {
    let Some(options) = options else {
        return Ok(config);
    };

    if let Some(v) = get_bool(options, "enabled")?
        .or(get_bool(options, "shapingEnabled")?)
        .or(get_bool(options, "shaping_enabled")?)
        .or(get_bool(options, "textShaping")?)
        .or(get_bool(options, "text_shaping")?)
    {
        config.enabled = v;
    }

    if let Some(v) = get_string_opt(options, "engine")?
        .or(get_string_opt(options, "shapingEngine")?)
        .or(get_string_opt(options, "shaping_engine")?)
    {
        let engine_key = v.trim().to_ascii_lowercase();
        config.engine = match engine_key.as_str() {
            "none" => TextShapingEngine::None,
            "harfbuzz" => TextShapingEngine::Harfbuzz,
            _ => {
                return Err(JsValue::from_str(
                    "field engine must be one of: none, harfbuzz",
                ));
            }
        };
    }

    Ok(config)
}

fn get_host_list(obj: &JsValue, keys: &[&str]) -> Result<Option<Vec<String>>, JsValue> {
    for key in keys {
        let v = Reflect::get(obj, &JsValue::from_str(key))?;
        if v.is_null() || v.is_undefined() {
            continue;
        }
        if !Array::is_array(&v) {
            return Err(JsValue::from_str(&format!(
                "field {key} must be an array of strings"
            )));
        }
        let arr = Array::from(&v);
        let mut out = Vec::with_capacity(arr.length() as usize);
        for entry in arr.iter() {
            let Some(raw) = entry.as_string() else {
                return Err(JsValue::from_str(&format!(
                    "field {key} must contain only strings"
                )));
            };
            let Some(host) = canonicalize_host(raw.trim()) else {
                return Err(JsValue::from_str(&format!(
                    "field {key} contains an invalid host: {raw}"
                )));
            };
            if !out.iter().any(|existing| existing == &host) {
                out.push(host);
            }
        }
        return Ok(Some(out));
    }
    Ok(None)
}

fn parse_http_url_scheme_and_host(url: &str) -> Option<(&'static str, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let normalized_scheme = if scheme.eq_ignore_ascii_case("http") {
        "http"
    } else if scheme.eq_ignore_ascii_case("https") {
        "https"
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = canonicalize_host(authority)?;
    Some((normalized_scheme, host))
}

fn canonicalize_host(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }

    let without_user = trimmed.rsplit('@').next().unwrap_or(trimmed).trim();
    if without_user.is_empty() {
        return None;
    }

    let host = if let Some(rest) = without_user.strip_prefix('[') {
        let end = rest.find(']')?;
        &rest[..end]
    } else {
        without_user.split(':').next().unwrap_or(without_user)
    };

    let host = host.trim().trim_end_matches('.');
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn link_open_policy_to_js(policy: &LinkOpenPolicy) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("allowHttp"),
        &JsValue::from_bool(policy.allow_http),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("allowHttps"),
        &JsValue::from_bool(policy.allow_https),
    );

    let allowed_hosts = Array::new();
    for host in &policy.allowed_hosts {
        allowed_hosts.push(&JsValue::from_str(host));
    }
    let _ = Reflect::set(&obj, &JsValue::from_str("allowedHosts"), &allowed_hosts);

    let blocked_hosts = Array::new();
    for host in &policy.blocked_hosts {
        blocked_hosts.push(&JsValue::from_str(host));
    }
    let _ = Reflect::set(&obj, &JsValue::from_str("blockedHosts"), &blocked_hosts);

    obj.into()
}

fn text_shaping_config_to_js(config: TextShapingConfig) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("enabled"),
        &JsValue::from_bool(config.enabled),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("engine"),
        &JsValue::from_str(config.engine.as_str()),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("fallback"),
        &JsValue::from_str("cell_scalar"),
    );
    obj.into()
}

fn search_state_to_js(
    query: &str,
    config: SearchConfig,
    index: &SearchIndex,
    active_match: Option<usize>,
) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(&obj, &JsValue::from_str("query"), &JsValue::from_str(query));
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("normalizedQuery"),
        &JsValue::from_str(index.normalized_query()),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("caseSensitive"),
        &JsValue::from_bool(config.case_sensitive),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("normalizeUnicode"),
        &JsValue::from_bool(config.normalize_unicode),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("matchCount"),
        &JsValue::from_f64(index.len() as f64),
    );

    if let Some(idx) = active_match {
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("activeMatchIndex"),
            &JsValue::from_f64(idx as f64),
        );
        if let Some(m) = index.matches().get(idx) {
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("activeLine"),
                &JsValue::from_f64(m.line_idx as f64),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("activeStart"),
                &JsValue::from_f64(m.start_char as f64),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("activeEnd"),
                &JsValue::from_f64(m.end_char as f64),
            );
        } else {
            let _ = Reflect::set(&obj, &JsValue::from_str("activeLine"), &JsValue::NULL);
            let _ = Reflect::set(&obj, &JsValue::from_str("activeStart"), &JsValue::NULL);
            let _ = Reflect::set(&obj, &JsValue::from_str("activeEnd"), &JsValue::NULL);
        }
    } else {
        let _ = Reflect::set(&obj, &JsValue::from_str("activeMatchIndex"), &JsValue::NULL);
        let _ = Reflect::set(&obj, &JsValue::from_str("activeLine"), &JsValue::NULL);
        let _ = Reflect::set(&obj, &JsValue::from_str("activeStart"), &JsValue::NULL);
        let _ = Reflect::set(&obj, &JsValue::from_str("activeEnd"), &JsValue::NULL);
    }

    obj.into()
}

fn accessibility_dom_snapshot_to_js(snapshot: &AccessibilityDomSnapshot) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("role"),
        &JsValue::from_str(snapshot.role),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("ariaMultiline"),
        &JsValue::from_bool(snapshot.aria_multiline),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("ariaLive"),
        &JsValue::from_str(snapshot.aria_live),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("ariaAtomic"),
        &JsValue::from_bool(snapshot.aria_atomic),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("tabIndex"),
        &JsValue::from_f64(f64::from(snapshot.tab_index)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("focused"),
        &JsValue::from_bool(snapshot.focused),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("focusVisible"),
        &JsValue::from_bool(snapshot.focus_visible),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("screenReader"),
        &JsValue::from_bool(snapshot.screen_reader),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("highContrast"),
        &JsValue::from_bool(snapshot.high_contrast),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("reducedMotion"),
        &JsValue::from_bool(snapshot.reduced_motion),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("value"),
        &JsValue::from_str(&snapshot.value),
    );
    if let Some(offset) = snapshot.cursor_offset {
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("cursorOffset"),
            &JsValue::from_f64(f64::from(offset)),
        );
    } else {
        let _ = Reflect::set(&obj, &JsValue::from_str("cursorOffset"), &JsValue::NULL);
    }
    if let Some(start) = snapshot.selection_start {
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("selectionStart"),
            &JsValue::from_f64(f64::from(start)),
        );
    } else {
        let _ = Reflect::set(&obj, &JsValue::from_str("selectionStart"), &JsValue::NULL);
    }
    if let Some(end) = snapshot.selection_end {
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("selectionEnd"),
            &JsValue::from_f64(f64::from(end)),
        );
    } else {
        let _ = Reflect::set(&obj, &JsValue::from_str("selectionEnd"), &JsValue::NULL);
    }
    obj.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessibility_toggle_announcements_emit_only_on_change() {
        let mut term = FrankenTermWeb::new();
        term.apply_accessibility_input(&AccessibilityInput {
            screen_reader: Some(true),
            high_contrast: Some(false),
            reduced_motion: Some(true),
            announce: None,
        });
        term.apply_accessibility_input(&AccessibilityInput {
            screen_reader: Some(true),
            high_contrast: Some(false),
            reduced_motion: Some(true),
            announce: None,
        });
        assert_eq!(
            term.live_announcements,
            vec![
                "Screen reader mode enabled.".to_string(),
                "Reduced motion enabled.".to_string()
            ]
        );
    }

    #[test]
    fn accessibility_announcement_queue_stays_bounded() {
        let mut term = FrankenTermWeb::new();
        for idx in 0..70 {
            term.push_live_announcement(&format!("msg-{idx}"));
        }
        assert_eq!(term.live_announcements.len(), 64);
        assert_eq!(
            term.live_announcements.first().map(String::as_str),
            Some("msg-6")
        );
        assert_eq!(
            term.live_announcements.last().map(String::as_str),
            Some("msg-69")
        );
    }

    #[test]
    fn blur_clears_hover_state_and_focus_flag() {
        let mut term = FrankenTermWeb::new();
        term.hovered_link_id = 42;
        term.set_focus_internal(true);
        assert!(term.focused);

        term.set_focus_internal(false);
        assert!(!term.focused);
        assert_eq!(term.hovered_link_id, 0);
    }

    #[test]
    fn accessibility_dom_snapshot_invariants_hold_for_valid_state() {
        let mut term = FrankenTermWeb::new();
        term.cols = 4;
        term.rows = 1;
        let mut cell = CellData::EMPTY;
        cell.glyph_id = u32::from('A');
        term.shadow_cells = vec![cell, CellData::EMPTY, CellData::EMPTY, CellData::EMPTY];
        term.screen_reader_enabled = true;
        term.high_contrast_enabled = true;
        term.reduced_motion_enabled = false;
        term.focused = true;
        term.cursor_offset = Some(1);
        term.selection_range = Some((1, 3));
        term.live_announcements.push("ready".to_string());

        let snapshot = term.build_accessibility_dom_snapshot();
        assert!(snapshot.validate().is_ok());
        assert_eq!(snapshot.role, "textbox");
        assert_eq!(snapshot.aria_live, "polite");
        assert_eq!(snapshot.selection_start, Some(1));
        assert_eq!(snapshot.selection_end, Some(3));
        assert!(!snapshot.value.is_empty());
    }

    #[test]
    fn accessibility_dom_snapshot_hides_value_when_screen_reader_is_disabled() {
        let mut term = FrankenTermWeb::new();
        term.cols = 1;
        term.rows = 1;
        let mut cell = CellData::EMPTY;
        cell.glyph_id = u32::from('Z');
        term.shadow_cells = vec![cell];
        term.screen_reader_enabled = false;

        let snapshot = term.build_accessibility_dom_snapshot();
        assert!(snapshot.validate().is_ok());
        assert!(snapshot.value.is_empty());
        assert_eq!(snapshot.aria_live, "off");
    }

    #[test]
    fn resize_storm_interaction_snapshot_is_none_when_no_overlays() {
        let term = FrankenTermWeb::new();
        assert_eq!(term.resize_storm_interaction_snapshot(), None);
    }

    #[test]
    fn resize_storm_interaction_snapshot_maps_overlay_state() {
        let mut term = FrankenTermWeb::new();
        term.hovered_link_id = 7;
        term.cursor_offset = Some(5);
        term.cursor_style = CursorStyle::Underline;
        term.selection_range = Some((2, 9));

        assert_eq!(
            term.resize_storm_interaction_snapshot(),
            Some(InteractionSnapshot {
                hovered_link_id: 7,
                cursor_offset: 5,
                cursor_style: CursorStyle::Underline.as_u32(),
                selection_active: true,
                selection_start: 2,
                selection_end: 9,
                text_shaping_enabled: false,
                text_shaping_engine: 0,
                screen_reader_enabled: false,
                high_contrast_enabled: false,
                reduced_motion_enabled: false,
                focused: false,
            })
        );
    }

    #[test]
    fn resize_storm_interaction_snapshot_keeps_defaults_for_missing_ranges() {
        let mut term = FrankenTermWeb::new();
        term.hovered_link_id = 11;
        term.cursor_offset = None;
        term.cursor_style = CursorStyle::None;
        term.selection_range = None;

        assert_eq!(
            term.resize_storm_interaction_snapshot(),
            Some(InteractionSnapshot {
                hovered_link_id: 11,
                cursor_offset: 0,
                cursor_style: CursorStyle::None.as_u32(),
                selection_active: false,
                selection_start: 0,
                selection_end: 0,
                text_shaping_enabled: false,
                text_shaping_engine: 0,
                screen_reader_enabled: false,
                high_contrast_enabled: false,
                reduced_motion_enabled: false,
                focused: false,
            })
        );
    }

    #[test]
    fn resize_storm_interaction_snapshot_includes_shaping_state_without_other_overlays() {
        let mut term = FrankenTermWeb::new();
        term.text_shaping = TextShapingConfig {
            enabled: true,
            engine: TextShapingEngine::Harfbuzz,
        };

        assert_eq!(
            term.resize_storm_interaction_snapshot(),
            Some(InteractionSnapshot {
                hovered_link_id: 0,
                cursor_offset: 0,
                cursor_style: CursorStyle::None.as_u32(),
                selection_active: false,
                selection_start: 0,
                selection_end: 0,
                text_shaping_enabled: true,
                text_shaping_engine: 1,
                screen_reader_enabled: false,
                high_contrast_enabled: false,
                reduced_motion_enabled: false,
                focused: false,
            })
        );
    }

    #[test]
    fn resize_storm_interaction_snapshot_includes_accessibility_state_without_other_overlays() {
        let mut term = FrankenTermWeb::new();
        term.screen_reader_enabled = true;
        term.high_contrast_enabled = true;
        term.reduced_motion_enabled = true;
        term.focused = true;

        assert_eq!(
            term.resize_storm_interaction_snapshot(),
            Some(InteractionSnapshot {
                hovered_link_id: 0,
                cursor_offset: 0,
                cursor_style: CursorStyle::None.as_u32(),
                selection_active: false,
                selection_start: 0,
                selection_end: 0,
                text_shaping_enabled: false,
                text_shaping_engine: 0,
                screen_reader_enabled: true,
                high_contrast_enabled: true,
                reduced_motion_enabled: true,
                focused: true,
            })
        );
    }

    fn text_row_cells(text: &str) -> Vec<CellData> {
        text.chars()
            .map(|ch| CellData {
                glyph_id: u32::from(ch),
                ..CellData::EMPTY
            })
            .collect()
    }

    fn patch_value(offset: u32, cells: &[CellData]) -> JsValue {
        let patch = Object::new();
        let _ = Reflect::set(
            &patch,
            &JsValue::from_str("offset"),
            &JsValue::from_f64(f64::from(offset)),
        );
        let arr = Array::new();
        for cell in cells {
            let obj = Object::new();
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("bg"),
                &JsValue::from_f64(f64::from(cell.bg_rgba)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("fg"),
                &JsValue::from_f64(f64::from(cell.fg_rgba)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("glyph"),
                &JsValue::from_f64(f64::from(cell.glyph_id)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("attrs"),
                &JsValue::from_f64(f64::from(cell.attrs)),
            );
            arr.push(&obj);
        }
        let _ = Reflect::set(&patch, &JsValue::from_str("cells"), &arr);
        patch.into()
    }

    fn patch_batch_value(patches: &[(u32, &[CellData])]) -> JsValue {
        let arr = Array::new();
        for (offset, cells) in patches {
            arr.push(&patch_value(*offset, cells));
        }
        arr.into()
    }

    fn patch_batch_flat_arrays(patches: &[(u32, &[CellData])]) -> (Uint32Array, Uint32Array) {
        let mut spans = Vec::with_capacity(patches.len() * 2);
        let total_cells = patches.iter().map(|(_, cells)| cells.len()).sum::<usize>();
        let mut flat_cells = Vec::with_capacity(total_cells * 4);

        for (offset, cells) in patches {
            spans.push(*offset);
            let len = cells.len().min(u32::MAX as usize) as u32;
            spans.push(len);
            for cell in *cells {
                flat_cells.push(cell.bg_rgba);
                flat_cells.push(cell.fg_rgba);
                flat_cells.push(cell.glyph_id);
                flat_cells.push(cell.attrs);
            }
        }

        (
            Uint32Array::from(spans.as_slice()),
            Uint32Array::from(flat_cells.as_slice()),
        )
    }

    #[test]
    fn set_selection_range_normalizes_reverse_and_out_of_bounds() {
        let mut term = FrankenTermWeb::new();
        term.cols = 4;
        term.rows = 2; // capacity = 8

        assert!(term.set_selection_range(6, 2).is_ok());
        assert_eq!(term.selection_range, Some((2, 6)));

        assert!(term.set_selection_range(6, 99).is_ok());
        assert_eq!(term.selection_range, Some((6, 8)));

        // Both clamp to the same bound, so range is cleared.
        assert!(term.set_selection_range(99, 99).is_ok());
        assert_eq!(term.selection_range, None);
    }

    #[test]
    fn set_search_query_builds_index_and_highlight() {
        let mut term = FrankenTermWeb::new();
        term.cols = 5;
        term.rows = 2;
        term.shadow_cells = text_row_cells("abcdeabcde");

        assert!(term.set_search_query("bc", None).is_ok());
        assert_eq!(term.search_index.len(), 2);
        assert_eq!(term.search_active_match, Some(0));
        assert_eq!(term.search_highlight_range, Some((1, 3)));
        assert_eq!(term.active_selection_range(), Some((1, 3)));
    }

    #[test]
    fn search_next_prev_wrap_and_follow_match_ranges() {
        let mut term = FrankenTermWeb::new();
        term.cols = 5;
        term.rows = 2;
        term.shadow_cells = text_row_cells("abcdeabcde");
        assert!(term.set_search_query("bc", None).is_ok());

        let _ = term.search_next();
        assert_eq!(term.search_active_match, Some(1));
        assert_eq!(term.search_highlight_range, Some((6, 8)));

        let _ = term.search_next();
        assert_eq!(term.search_active_match, Some(0));
        assert_eq!(term.search_highlight_range, Some((1, 3)));

        let _ = term.search_prev();
        assert_eq!(term.search_active_match, Some(1));
        assert_eq!(term.search_highlight_range, Some((6, 8)));
    }

    #[test]
    fn explicit_selection_overrides_search_highlight_until_cleared() {
        let mut term = FrankenTermWeb::new();
        term.cols = 5;
        term.rows = 2;
        term.shadow_cells = text_row_cells("abcdeabcde");
        assert!(term.set_search_query("bc", None).is_ok());
        assert_eq!(term.active_selection_range(), Some((1, 3)));

        assert!(term.set_selection_range(8, 10).is_ok());
        assert_eq!(term.selection_range, Some((8, 10)));
        assert_eq!(term.active_selection_range(), Some((8, 10)));

        let _ = term.search_next();
        assert_eq!(term.search_highlight_range, Some((6, 8)));
        assert_eq!(term.active_selection_range(), Some((8, 10)));

        term.clear_selection();
        assert_eq!(term.selection_range, None);
        assert_eq!(term.active_selection_range(), Some((6, 8)));
    }

    #[test]
    fn apply_patch_without_renderer_accepts_unicode_row_and_populates_autolinks() {
        let text = "界e\u{301} 👩\u{200d}💻 https://example.test";
        let cells = text_row_cells(text);
        let mut term = FrankenTermWeb::new();
        term.cols = text.chars().count() as u16;
        term.rows = 1;

        assert!(term.apply_patch(patch_value(0, &cells)).is_ok());
        assert_eq!(term.shadow_cells, cells);

        let url_byte = text
            .find("https://")
            .expect("fixture should contain https:// URL marker");
        let url_col = text[..url_byte].chars().count() as u16;
        let link_id = term.link_at(url_col, 0);
        assert!(link_id >= AUTO_LINK_ID_BASE);
        assert_eq!(
            term.link_url_at(url_col, 0),
            Some("https://example.test".to_string())
        );
    }

    #[test]
    fn apply_patch_without_renderer_keeps_unicode_autolink_mapping_deterministic() {
        let text = "αβγ https://deterministic.test/path";
        let cells = text_row_cells(text);
        let mut term = FrankenTermWeb::new();
        term.cols = text.chars().count() as u16;
        term.rows = 1;

        assert!(term.apply_patch(patch_value(0, &cells)).is_ok());
        let first_ids = term.auto_link_ids.clone();
        let first_urls = term.auto_link_urls.clone();

        assert!(term.apply_patch(patch_value(0, &cells)).is_ok());
        assert_eq!(term.auto_link_ids, first_ids);
        assert_eq!(term.auto_link_urls, first_urls);
    }

    #[test]
    fn apply_patch_without_renderer_respects_offset_for_unicode_cells() {
        let mut term = FrankenTermWeb::new();
        term.cols = 6;
        term.rows = 2;
        term.shadow_cells = vec![CellData::EMPTY; 12];

        let cells = text_row_cells("界🙂");
        assert!(term.apply_patch(patch_value(7, &cells)).is_ok());

        assert_eq!(term.shadow_cells[7].glyph_id, u32::from('界'));
        assert_eq!(term.shadow_cells[8].glyph_id, u32::from('🙂'));
        assert_eq!(term.shadow_cells[6], CellData::EMPTY);
        assert_eq!(term.shadow_cells[9], CellData::EMPTY);
    }

    #[test]
    fn apply_patch_batch_without_renderer_respects_multiple_offsets() {
        let mut term = FrankenTermWeb::new();
        term.cols = 7;
        term.rows = 2;
        term.shadow_cells = vec![CellData::EMPTY; 14];

        let alpha = text_row_cells("αβ");
        let wide = text_row_cells("界🙂");
        let patches = patch_batch_value(&[(0, &alpha), (9, &wide)]);
        assert!(term.apply_patch_batch(patches).is_ok());

        assert_eq!(term.shadow_cells[0].glyph_id, u32::from('α'));
        assert_eq!(term.shadow_cells[1].glyph_id, u32::from('β'));
        assert_eq!(term.shadow_cells[9].glyph_id, u32::from('界'));
        assert_eq!(term.shadow_cells[10].glyph_id, u32::from('🙂'));
        assert_eq!(term.shadow_cells[8], CellData::EMPTY);
        assert_eq!(term.shadow_cells[11], CellData::EMPTY);
    }

    #[test]
    fn apply_patch_batch_matches_sequential_patch_side_effects() {
        let left = text_row_cells("α https://one.test ");
        let right = text_row_cells("β https://two.test");
        let right_offset = 1 + left.len() as u32;

        let mut sequential = FrankenTermWeb::new();
        sequential.cols = 40;
        sequential.rows = 1;
        assert!(sequential.set_search_query("https", None).is_ok());
        assert!(sequential.apply_patch(patch_value(1, &left)).is_ok());
        assert!(
            sequential
                .apply_patch(patch_value(right_offset, &right))
                .is_ok()
        );

        let mut batched = FrankenTermWeb::new();
        batched.cols = 40;
        batched.rows = 1;
        assert!(batched.set_search_query("https", None).is_ok());
        let patches = patch_batch_value(&[(1, &left), (right_offset, &right)]);
        assert!(batched.apply_patch_batch(patches).is_ok());

        assert_eq!(batched.shadow_cells, sequential.shadow_cells);
        assert_eq!(batched.auto_link_ids, sequential.auto_link_ids);
        assert_eq!(batched.auto_link_urls, sequential.auto_link_urls);
        assert_eq!(batched.search_index.len(), sequential.search_index.len());
        assert_eq!(batched.search_active_match, sequential.search_active_match);
        assert_eq!(
            batched.search_highlight_range,
            sequential.search_highlight_range
        );
    }

    #[test]
    fn apply_patch_batch_flat_without_renderer_respects_multiple_offsets() {
        let mut term = FrankenTermWeb::new();
        term.cols = 7;
        term.rows = 2;
        term.shadow_cells = vec![CellData::EMPTY; 14];

        let alpha = text_row_cells("αβ");
        let wide = text_row_cells("界🙂");
        let (spans, cells) = patch_batch_flat_arrays(&[(0, &alpha), (9, &wide)]);
        assert!(term.apply_patch_batch_flat(spans, cells).is_ok());

        assert_eq!(term.shadow_cells[0].glyph_id, u32::from('α'));
        assert_eq!(term.shadow_cells[1].glyph_id, u32::from('β'));
        assert_eq!(term.shadow_cells[9].glyph_id, u32::from('界'));
        assert_eq!(term.shadow_cells[10].glyph_id, u32::from('🙂'));
        assert_eq!(term.shadow_cells[8], CellData::EMPTY);
        assert_eq!(term.shadow_cells[11], CellData::EMPTY);
    }

    #[test]
    fn apply_patch_batch_flat_matches_object_batch_side_effects() {
        let left = text_row_cells("α https://one.test ");
        let right = text_row_cells("β https://two.test");
        let right_offset = 1 + left.len() as u32;

        let mut object_path = FrankenTermWeb::new();
        object_path.cols = 40;
        object_path.rows = 1;
        assert!(object_path.set_search_query("https", None).is_ok());
        let patches = patch_batch_value(&[(1, &left), (right_offset, &right)]);
        assert!(object_path.apply_patch_batch(patches).is_ok());

        let mut flat_path = FrankenTermWeb::new();
        flat_path.cols = 40;
        flat_path.rows = 1;
        assert!(flat_path.set_search_query("https", None).is_ok());
        let (spans, cells) = patch_batch_flat_arrays(&[(1, &left), (right_offset, &right)]);
        assert!(flat_path.apply_patch_batch_flat(spans, cells).is_ok());

        assert_eq!(flat_path.shadow_cells, object_path.shadow_cells);
        assert_eq!(flat_path.auto_link_ids, object_path.auto_link_ids);
        assert_eq!(flat_path.auto_link_urls, object_path.auto_link_urls);
        assert_eq!(flat_path.search_index.len(), object_path.search_index.len());
        assert_eq!(
            flat_path.search_active_match,
            object_path.search_active_match
        );
        assert_eq!(
            flat_path.search_highlight_range,
            object_path.search_highlight_range
        );
    }

    #[test]
    fn apply_patch_batch_flat_rejects_invalid_payload_without_mutation() {
        let mut term = FrankenTermWeb::new();
        term.cols = 4;
        term.rows = 1;
        term.shadow_cells = text_row_cells("base");
        term.auto_link_ids = vec![7, 7, 7, 7];
        let baseline_cells = term.shadow_cells.clone();
        let baseline_link_ids = term.auto_link_ids.clone();
        let baseline_urls = term.auto_link_urls.clone();

        let spans = Uint32Array::from([0, 2].as_slice());
        let cells = Uint32Array::from([1, 2, 3, 4].as_slice());
        assert!(term.apply_patch_batch_flat(spans, cells).is_err());

        assert_eq!(term.shadow_cells, baseline_cells);
        assert_eq!(term.auto_link_ids, baseline_link_ids);
        assert_eq!(term.auto_link_urls, baseline_urls);
    }

    #[test]
    fn apply_patch_batch_rejects_invalid_patch_without_mutation() {
        let mut term = FrankenTermWeb::new();
        term.cols = 4;
        term.rows = 1;
        term.shadow_cells = text_row_cells("base");
        term.auto_link_ids = vec![7, 7, 7, 7];
        let baseline_cells = term.shadow_cells.clone();
        let baseline_link_ids = term.auto_link_ids.clone();
        let baseline_urls = term.auto_link_urls.clone();

        let valid_cells = text_row_cells("zz");
        let valid = patch_value(0, &valid_cells);
        let invalid = Object::new();
        let _ = Reflect::set(
            &invalid,
            &JsValue::from_str("offset"),
            &JsValue::from_f64(2.0),
        );

        let batch = Array::new();
        batch.push(&valid);
        batch.push(&invalid);

        assert!(term.apply_patch_batch(batch.into()).is_err());
        assert_eq!(term.shadow_cells, baseline_cells);
        assert_eq!(term.auto_link_ids, baseline_link_ids);
        assert_eq!(term.auto_link_urls, baseline_urls);
    }

    #[test]
    fn buffer_change_rebuilds_search_index_for_active_query() {
        let mut term = FrankenTermWeb::new();
        term.cols = 4;
        term.rows = 1;
        term.shadow_cells = text_row_cells("aaaa");

        assert!(term.set_search_query("z", None).is_ok());
        assert!(term.search_index.is_empty());
        assert_eq!(term.search_active_match, None);

        term.shadow_cells = text_row_cells("zzzz");
        term.refresh_search_after_buffer_change();

        assert_eq!(term.search_index.len(), 4);
        assert_eq!(term.search_active_match, Some(0));
        assert_eq!(term.search_highlight_range, Some((0, 1)));
    }

    #[test]
    fn extract_and_copy_selection_insert_row_breaks_at_grid_boundaries() {
        let mut term = FrankenTermWeb::new();
        term.cols = 4;
        term.rows = 2;
        term.shadow_cells = text_row_cells("ABCDEFGH");
        term.selection_range = Some((1, 7));

        assert_eq!(term.extract_selection_text(), "BCD\nEFG");
        assert_eq!(term.copy_selection(), Some("BCD\nEFG".to_string()));
    }

    #[test]
    fn mouse_link_click_queue_drains_in_order() {
        let mut term = FrankenTermWeb::new();
        term.cols = 2;
        term.rows = 1;
        term.shadow_cells = vec![CellData::EMPTY, CellData::EMPTY];

        // Simulate an OSC8 link id in cell (1, 0).
        term.shadow_cells[1].attrs = (55u32 << 8) | 0x1;

        // Non-link cell down should not enqueue.
        assert!(
            term.queue_input_event(InputEvent::Mouse(MouseInput {
                phase: MousePhase::Down,
                button: Some(MouseButton::Left),
                x: 0,
                y: 0,
                mods: Modifiers::default(),
            }))
            .is_ok()
        );
        assert!(term.link_clicks.is_empty());

        // Hover-only move should update hover but not enqueue.
        assert!(
            term.queue_input_event(InputEvent::Mouse(MouseInput {
                phase: MousePhase::Move,
                button: None,
                x: 1,
                y: 0,
                mods: Modifiers::default(),
            }))
            .is_ok()
        );
        assert_eq!(term.hovered_link_id, 55);
        assert!(term.link_clicks.is_empty());

        // Down on linked cell enqueues; Up does not.
        assert!(
            term.queue_input_event(InputEvent::Mouse(MouseInput {
                phase: MousePhase::Down,
                button: Some(MouseButton::Left),
                x: 1,
                y: 0,
                mods: Modifiers::default(),
            }))
            .is_ok()
        );
        assert!(
            term.queue_input_event(InputEvent::Mouse(MouseInput {
                phase: MousePhase::Up,
                button: Some(MouseButton::Left),
                x: 1,
                y: 0,
                mods: Modifiers::default(),
            }))
            .is_ok()
        );

        assert_eq!(term.link_clicks.len(), 1);
        assert_eq!(term.link_clicks[0].x, 1);
        assert_eq!(term.link_clicks[0].y, 0);
        assert_eq!(term.link_clicks[0].button, Some(MouseButton::Left));
        assert_eq!(term.link_clicks[0].link_id, 55);

        let drained = term.drain_link_clicks();
        assert_eq!(drained.length(), 1);
        assert!(term.link_clicks.is_empty());
        assert_eq!(term.drain_link_clicks().length(), 0);
    }

    #[test]
    fn detect_auto_urls_in_row_finds_http_and_https() {
        let row: Vec<char> = "visit http://a.test and https://b.test/path"
            .chars()
            .collect();
        let found = detect_auto_urls_in_row(&row);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].url, "http://a.test");
        assert_eq!(found[1].url, "https://b.test/path");
    }

    #[test]
    fn detect_auto_urls_in_row_trims_trailing_punctuation() {
        let row: Vec<char> = "open https://example.test/docs, now".chars().collect();
        let found = detect_auto_urls_in_row(&row);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "https://example.test/docs");
    }

    #[test]
    fn detect_auto_urls_requires_token_boundary() {
        let row: Vec<char> = "foohttps://example.test should-not-link".chars().collect();
        let found = detect_auto_urls_in_row(&row);
        assert!(found.is_empty());
    }

    #[test]
    fn recompute_auto_links_populates_link_at_and_url_lookup() {
        let text = "go to https://example.test/path now";
        let mut term = FrankenTermWeb::new();
        term.cols = text.chars().count() as u16;
        term.rows = 1;
        term.shadow_cells = text_row_cells(text);
        term.auto_link_ids = vec![0; term.shadow_cells.len()];
        term.recompute_auto_links();

        let link_x = text
            .find("https://")
            .expect("fixture should contain https:// URL marker") as u16;
        let link_id = term.link_at(link_x, 0);
        assert!(link_id >= AUTO_LINK_ID_BASE);
        assert_eq!(
            term.link_url_at(link_x, 0),
            Some("https://example.test/path".to_string())
        );
    }

    #[test]
    fn explicit_osc8_link_takes_precedence_over_auto_detected_link() {
        let text = "https://example.test";
        let mut term = FrankenTermWeb::new();
        term.cols = text.chars().count() as u16;
        term.rows = 1;
        term.shadow_cells = text_row_cells(text);
        term.auto_link_ids = vec![0; term.shadow_cells.len()];

        // Simulate an OSC8-provided link id in the first URL cell.
        term.shadow_cells[0].attrs = (77u32 << 8) | 0x1;
        term.recompute_auto_links();
        assert_eq!(term.link_at(0, 0), 77);
    }

    #[test]
    fn parse_http_url_scheme_and_host_normalizes_case_and_port() {
        let (scheme, host) = parse_http_url_scheme_and_host("HTTPS://Example.Test:443/path?q=1")
            .expect("valid HTTPS URL should parse into normalized scheme and host");
        assert_eq!(scheme, "https");
        assert_eq!(host, "example.test");
    }

    #[test]
    fn link_open_policy_blocks_http_when_disabled() {
        let policy = LinkOpenPolicy {
            allow_http: false,
            allow_https: true,
            allowed_hosts: Vec::new(),
            blocked_hosts: Vec::new(),
        };

        let denied = policy.evaluate(Some("http://example.test/path"));
        assert!(!denied.allowed);
        assert_eq!(denied.reason, Some("scheme_blocked"));

        let allowed = policy.evaluate(Some("https://example.test/path"));
        assert!(allowed.allowed);
        assert_eq!(allowed.reason, None);
    }

    #[test]
    fn link_open_policy_enforces_allow_and_block_lists() {
        let policy = LinkOpenPolicy {
            allow_http: true,
            allow_https: true,
            allowed_hosts: vec!["allowed.test".to_string()],
            blocked_hosts: vec!["blocked.test".to_string()],
        };

        let denied_missing = policy.evaluate(Some("https://other.test"));
        assert!(!denied_missing.allowed);
        assert_eq!(denied_missing.reason, Some("host_not_allowlisted"));

        let denied_blocked = policy.evaluate(Some("https://blocked.test"));
        assert!(!denied_blocked.allowed);
        assert_eq!(denied_blocked.reason, Some("host_blocked"));

        let allowed = policy.evaluate(Some("https://allowed.test/docs"));
        assert!(allowed.allowed);
        assert_eq!(allowed.reason, None);
    }

    #[test]
    fn text_shaping_is_disabled_by_default() {
        let term = FrankenTermWeb::new();
        assert_eq!(term.text_shaping, TextShapingConfig::default());

        let state = term.text_shaping_state();
        assert_eq!(
            Reflect::get(&state, &JsValue::from_str("enabled"))
                .expect("text_shaping_state should contain enabled key")
                .as_bool(),
            Some(false)
        );
        assert_eq!(
            Reflect::get(&state, &JsValue::from_str("engine"))
                .expect("text_shaping_state should contain engine key")
                .as_string()
                .as_deref(),
            Some("none")
        );
    }

    #[test]
    fn set_text_shaping_accepts_aliases_and_toggles_state() {
        let mut term = FrankenTermWeb::new();

        let enable = Object::new();
        let _ = Reflect::set(
            &enable,
            &JsValue::from_str("shapingEnabled"),
            &JsValue::from_bool(true),
        );
        assert!(term.set_text_shaping(enable.into()).is_ok());
        assert_eq!(
            term.text_shaping,
            TextShapingConfig {
                enabled: true,
                engine: TextShapingEngine::None
            }
        );

        let disable = Object::new();
        let _ = Reflect::set(
            &disable,
            &JsValue::from_str("enabled"),
            &JsValue::from_bool(false),
        );
        assert!(term.set_text_shaping(disable.into()).is_ok());
        assert_eq!(term.text_shaping, TextShapingConfig::default());
    }

    #[test]
    fn set_text_shaping_rejects_non_boolean_values() {
        let mut term = FrankenTermWeb::new();

        let invalid = Object::new();
        let _ = Reflect::set(
            &invalid,
            &JsValue::from_str("enabled"),
            &JsValue::from_str("yes"),
        );

        assert!(term.set_text_shaping(invalid.into()).is_err());
        assert_eq!(term.text_shaping, TextShapingConfig::default());
    }

    #[test]
    fn set_text_shaping_parses_engine_and_rejects_unknown_values() {
        let mut term = FrankenTermWeb::new();

        let cfg = Object::new();
        let _ = Reflect::set(
            &cfg,
            &JsValue::from_str("enabled"),
            &JsValue::from_bool(true),
        );
        let _ = Reflect::set(
            &cfg,
            &JsValue::from_str("engine"),
            &JsValue::from_str("harfbuzz"),
        );
        assert!(term.set_text_shaping(cfg.into()).is_ok());
        assert_eq!(
            term.text_shaping,
            TextShapingConfig {
                enabled: true,
                engine: TextShapingEngine::Harfbuzz
            }
        );
        let state = term.text_shaping_state();
        assert_eq!(
            Reflect::get(&state, &JsValue::from_str("engine"))
                .expect("text_shaping_state should contain engine key")
                .as_string()
                .as_deref(),
            Some("harfbuzz")
        );

        let invalid = Object::new();
        let _ = Reflect::set(
            &invalid,
            &JsValue::from_str("engine"),
            &JsValue::from_str("icu"),
        );
        assert!(term.set_text_shaping(invalid.into()).is_err());
    }

    #[test]
    fn destroy_restores_text_shaping_default_state() {
        let mut term = FrankenTermWeb::new();

        let enable = Object::new();
        let _ = Reflect::set(
            &enable,
            &JsValue::from_str("enabled"),
            &JsValue::from_bool(true),
        );
        assert!(term.set_text_shaping(enable.into()).is_ok());
        assert_eq!(
            term.text_shaping,
            TextShapingConfig {
                enabled: true,
                engine: TextShapingEngine::None
            }
        );

        term.destroy();
        assert_eq!(term.text_shaping, TextShapingConfig::default());
    }

    #[test]
    fn text_shaping_toggle_keeps_patch_projection_deterministic() {
        let text = "ffi αβ https://shape.test/path";
        let cells = text_row_cells(text);
        let mut term = FrankenTermWeb::new();
        term.cols = text.chars().count() as u16;
        term.rows = 1;

        assert!(term.apply_patch(patch_value(0, &cells)).is_ok());
        let baseline_shadow = term.shadow_cells.clone();
        let baseline_ids = term.auto_link_ids.clone();
        let baseline_urls = term.auto_link_urls.clone();

        let enable = Object::new();
        let _ = Reflect::set(
            &enable,
            &JsValue::from_str("enabled"),
            &JsValue::from_bool(true),
        );
        assert!(term.set_text_shaping(enable.into()).is_ok());
        assert!(term.apply_patch(patch_value(0, &cells)).is_ok());
        assert_eq!(term.shadow_cells, baseline_shadow);
        assert_eq!(term.auto_link_ids, baseline_ids);
        assert_eq!(term.auto_link_urls, baseline_urls);

        let disable = Object::new();
        let _ = Reflect::set(
            &disable,
            &JsValue::from_str("enabled"),
            &JsValue::from_bool(false),
        );
        assert!(term.set_text_shaping(disable.into()).is_ok());
        assert!(term.apply_patch(patch_value(0, &cells)).is_ok());
        assert_eq!(term.shadow_cells, baseline_shadow);
        assert_eq!(term.auto_link_ids, baseline_ids);
        assert_eq!(term.auto_link_urls, baseline_urls);
    }

    #[test]
    fn drain_link_clicks_reports_policy_decision() {
        let text = "http://example.test docs";
        let mut term = FrankenTermWeb::new();
        term.cols = text.chars().count() as u16;
        term.rows = 1;
        term.shadow_cells = text_row_cells(text);
        term.auto_link_ids = vec![0; term.shadow_cells.len()];
        term.recompute_auto_links();
        term.link_open_policy.allow_http = false;

        let url_x = text
            .find("http://")
            .expect("fixture should contain http:// URL marker") as u16;
        assert!(
            term.queue_input_event(InputEvent::Mouse(MouseInput {
                phase: MousePhase::Down,
                button: Some(MouseButton::Left),
                x: url_x,
                y: 0,
                mods: Modifiers::default(),
            }))
            .is_ok()
        );

        let events = term.drain_link_clicks();
        assert_eq!(events.length(), 1);
        let event = events.get(0);

        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("source"))
                .expect("link click event should expose source")
                .as_string()
                .as_deref(),
            Some("auto")
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("url"))
                .expect("link click event should expose url")
                .as_string()
                .as_deref(),
            Some("http://example.test")
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("openAllowed"))
                .expect("link click event should expose openAllowed")
                .as_bool(),
            Some(false)
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("openReason"))
                .expect("link click event should expose openReason")
                .as_string()
                .as_deref(),
            Some("scheme_blocked")
        );
    }

    #[test]
    fn drain_link_clicks_jsonl_emits_e2e_records() {
        let text = "https://example.test docs";
        let mut term = FrankenTermWeb::new();
        term.cols = text.chars().count() as u16;
        term.rows = 1;
        term.shadow_cells = text_row_cells(text);
        term.auto_link_ids = vec![0; term.shadow_cells.len()];
        term.recompute_auto_links();

        let url_x = text
            .find("https://")
            .expect("fixture should contain https:// URL marker") as u16;
        assert!(
            term.queue_input_event(InputEvent::Mouse(MouseInput {
                phase: MousePhase::Down,
                button: Some(MouseButton::Left),
                x: url_x,
                y: 0,
                mods: Modifiers::default(),
            }))
            .is_ok()
        );

        let lines = term.drain_link_clicks_jsonl("run-link".to_string(), 5, "T000120".to_string());
        assert_eq!(lines.length(), 1);

        let line = lines
            .get(0)
            .as_string()
            .expect("drain_link_clicks_jsonl should emit string JSONL line");
        let parsed: serde_json::Value = serde_json::from_str(&line)
            .expect("drain_link_clicks_jsonl output should be parseable JSON");
        assert_eq!(parsed["type"], "link_click");
        assert_eq!(parsed["run_id"], "run-link");
        assert_eq!(parsed["seed"], 5);
        assert_eq!(parsed["event_idx"], 0);
        assert_eq!(parsed["open_allowed"], true);
        assert_eq!(parsed["url"], "https://example.test");
    }
}

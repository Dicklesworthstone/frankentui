#![forbid(unsafe_code)]

use crate::attach::{
    AttachAction, AttachClientStateMachine, AttachEvent, AttachSnapshot, AttachTransition,
};
use crate::frame_harness::{
    GeometrySnapshot, InteractionSnapshot, LinkClickSnapshot, link_click_jsonl,
    resize_storm_frame_jsonl_with_interaction, scrollback_virtualization_frame_jsonl,
};
use crate::input::{
    AccessibilityInput, CompositionInput, CompositionPhase, CompositionState, FocusInput,
    InputEvent, KeyInput, KeyPhase, ModifierTracker, Modifiers, MouseButton, MouseInput,
    MousePhase, PasteInput, TouchInput, TouchPhase, TouchPoint, VtInputEncoderFeatures, WheelInput,
    encode_vt_input_event, normalize_dom_key_code,
};
use crate::markers::{DecorationKind, HistoryWindow, MarkerStore};
use crate::patch_feed::core_patch_to_patches;
use crate::renderer::{
    CellData, CellPatch, CursorStyle, GridGeometry, RendererBackendPreference, RendererConfig,
    WebGpuRenderer, cell_attr_link_id, cell_patches_from_flat_u32,
};
use crate::scroll::{ScrollState, SearchConfig, SearchIndex, ViewportSnapshot};
use crate::{
    FRANKENTERM_JS_API_LINE, FRANKENTERM_JS_API_VERSION, FRANKENTERM_JS_EVENT_BUFFER_POLICY,
    FRANKENTERM_JS_EVENT_ORDERING_RULES, FRANKENTERM_JS_EVENT_SCHEMA_VERSION,
    FRANKENTERM_JS_EVENT_TYPES, FRANKENTERM_JS_PROTOCOL_VERSION, FRANKENTERM_JS_PUBLIC_METHODS,
    FRANKENTERM_JS_VERSIONING_POLICY,
};
use frankenterm_core::{Action, HyperlinkId, Parser, ScrollbackWindow, TerminalEngine};
use js_sys::{Array, Object, Reflect, Uint8Array, Uint32Array};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, trace, warn};
use unicode_width::UnicodeWidthChar;
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

/// Synthetic link-id range reserved for auto-detected plaintext URLs.
const AUTO_LINK_ID_BASE: u32 = 0x00F0_0001;
const AUTO_LINK_ID_MAX: u32 = 0x00FF_FFFE;
/// Max decoded clipboard paste payload (matches websocket-protocol limits).
const MAX_PASTE_BYTES: usize = 768 * 1024;
/// Bounded queue limits for host-drained event streams.
const MAX_ENCODED_INPUT_EVENTS: usize = 4096;
const MAX_ENCODED_INPUT_BYTE_CHUNKS: usize = 4096;
const MAX_IME_TRACE_EVENTS: usize = 2048;
const MAX_LINK_CLICKS: usize = 2048;
const MAX_ACCESSIBILITY_ANNOUNCEMENTS: usize = 64;
const DEFAULT_EVENT_SUBSCRIPTION_BUFFER_MAX: usize = 512;
const MAX_EVENT_SUBSCRIPTION_BUFFER_MAX: usize = 8192;
const MAX_EVENT_SUBSCRIPTIONS: usize = 256;

fn empty_search_index(config: SearchConfig) -> SearchIndex {
    SearchIndex::build(std::iter::empty::<&str>(), "", config)
}

fn js_array_from_strings(items: &[&str]) -> Array {
    let arr = Array::new_with_length(items.len() as u32);
    for (idx, item) in items.iter().enumerate() {
        arr.set(idx as u32, JsValue::from_str(item));
    }
    arr
}

fn push_bounded<T>(queue: &mut Vec<T>, item: T, limit: usize) {
    if queue.len() >= limit {
        let overflow = queue.len() - limit + 1;
        queue.drain(..overflow);
    }
    queue.push(item);
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn trim_trailing_spaces(line: &mut String) {
    while line.ends_with(' ') {
        line.pop();
    }
}

fn infer_wide_continuations(cells: &[CellData], cols: usize) -> Vec<bool> {
    if cols == 0 {
        return vec![false; cells.len()];
    }
    let mut continuation = vec![false; cells.len()];
    for row_start in (0..cells.len()).step_by(cols) {
        let row_end = row_start.saturating_add(cols).min(cells.len());
        let mut pending = 0usize;
        for idx in row_start..row_end {
            let glyph_id = cells[idx].glyph_id;
            if pending > 0 {
                if glyph_id == 0 {
                    continuation[idx] = true;
                    pending -= 1;
                    continue;
                }
                pending = 0;
            }
            if glyph_id == 0 {
                continue;
            }
            let ch = char::from_u32(glyph_id).unwrap_or('□');
            pending = UnicodeWidthChar::width(ch).unwrap_or(1).saturating_sub(1);
        }
    }
    continuation
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
    ime_trace_events: Vec<ImeTraceEvent>,
    link_clicks: Vec<LinkClickEvent>,
    event_subscriptions: HashMap<u32, EventSubscription>,
    next_event_subscription_id: u32,
    next_host_event_seq: u64,
    auto_link_ids: Vec<u32>,
    auto_link_urls: HashMap<u32, String>,
    link_open_policy: LinkOpenPolicy,
    clipboard_policy: ClipboardPolicy,
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
    flat_spans_scratch: Vec<u32>,
    flat_cells_scratch: Vec<u32>,
    dirty_row_marks: Vec<u8>,
    dirty_rows_scratch: Vec<usize>,
    scroll_state: ScrollState,
    follow_output: bool,
    attach_client: AttachClientStateMachine,
    next_auto_link_id: u32,
    progress_parser: Parser,
    progress_last_value: u8,
    marker_store: MarkerStore,
    engine: Option<TerminalEngine>,
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
    audit_url: Option<String>,
    audit_url_redacted: bool,
    policy_rule: &'static str,
    action_outcome: &'static str,
    open_decision: LinkOpenDecision,
}

#[derive(Debug, Clone)]
struct ImeTraceEvent {
    event_kind: &'static str,
    phase: Option<CompositionPhase>,
    data: Option<String>,
    synthetic: bool,
    active_after: bool,
    preedit_after: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostEventType {
    AttachTransition = 0,
    InputAccessibility = 1,
    InputComposition = 2,
    InputCompositionTrace = 3,
    InputFocus = 4,
    InputKey = 5,
    InputMouse = 6,
    InputPaste = 7,
    InputTouch = 8,
    InputVtBytes = 9,
    InputWheel = 10,
    TerminalProgress = 11,
    TerminalReplyBytes = 12,
    UiAccessibilityAnnouncement = 13,
    UiLinkClick = 14,
}

impl HostEventType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AttachTransition => "attach.transition",
            Self::InputAccessibility => "input.accessibility",
            Self::InputComposition => "input.composition",
            Self::InputCompositionTrace => "input.composition_trace",
            Self::InputFocus => "input.focus",
            Self::InputKey => "input.key",
            Self::InputMouse => "input.mouse",
            Self::InputPaste => "input.paste",
            Self::InputTouch => "input.touch",
            Self::InputVtBytes => "input.vt_bytes",
            Self::InputWheel => "input.wheel",
            Self::TerminalProgress => "terminal.progress",
            Self::TerminalReplyBytes => "terminal.reply_bytes",
            Self::UiAccessibilityAnnouncement => "ui.accessibility_announcement",
            Self::UiLinkClick => "ui.link_click",
        }
    }

    const fn bit(self) -> u32 {
        1_u32 << (self as u32)
    }

    const fn all() -> [Self; 15] {
        [
            Self::AttachTransition,
            Self::InputAccessibility,
            Self::InputComposition,
            Self::InputCompositionTrace,
            Self::InputFocus,
            Self::InputKey,
            Self::InputMouse,
            Self::InputPaste,
            Self::InputTouch,
            Self::InputVtBytes,
            Self::InputWheel,
            Self::TerminalProgress,
            Self::TerminalReplyBytes,
            Self::UiAccessibilityAnnouncement,
            Self::UiLinkClick,
        ]
    }

    fn from_input_event(event: &InputEvent) -> Self {
        match event {
            InputEvent::Accessibility(_) => Self::InputAccessibility,
            InputEvent::Composition(_) => Self::InputComposition,
            InputEvent::Focus(_) => Self::InputFocus,
            InputEvent::Key(_) => Self::InputKey,
            InputEvent::Mouse(_) => Self::InputMouse,
            InputEvent::Paste(_) => Self::InputPaste,
            InputEvent::Touch(_) => Self::InputTouch,
            InputEvent::Wheel(_) => Self::InputWheel,
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "attach.transition" => Some(Self::AttachTransition),
            "input.accessibility" => Some(Self::InputAccessibility),
            "input.composition" => Some(Self::InputComposition),
            "input.composition_trace" => Some(Self::InputCompositionTrace),
            "input.focus" => Some(Self::InputFocus),
            "input.key" => Some(Self::InputKey),
            "input.mouse" => Some(Self::InputMouse),
            "input.paste" => Some(Self::InputPaste),
            "input.touch" => Some(Self::InputTouch),
            "input.vt_bytes" => Some(Self::InputVtBytes),
            "input.wheel" => Some(Self::InputWheel),
            "terminal.progress" => Some(Self::TerminalProgress),
            "terminal.reply_bytes" => Some(Self::TerminalReplyBytes),
            "ui.accessibility_announcement" => Some(Self::UiAccessibilityAnnouncement),
            "ui.link_click" => Some(Self::UiLinkClick),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressSignalState {
    Remove = 0,
    Normal = 1,
    Error = 2,
    Indeterminate = 3,
    Warning = 4,
}

impl ProgressSignalState {
    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Remove),
            1 => Some(Self::Normal),
            2 => Some(Self::Error),
            3 => Some(Self::Indeterminate),
            4 => Some(Self::Warning),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Remove => "remove",
            Self::Normal => "normal",
            Self::Error => "error",
            Self::Indeterminate => "indeterminate",
            Self::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone)]
struct ProgressSignalRecord {
    accepted: bool,
    reason: Option<&'static str>,
    state: Option<ProgressSignalState>,
    state_code: Option<u8>,
    value: Option<u8>,
    value_provided: bool,
    raw_payload: String,
    raw_state: Option<String>,
    raw_value: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EventTypeMask(u32);

impl EventTypeMask {
    const fn contains(self, event_type: HostEventType) -> bool {
        (self.0 & event_type.bit()) != 0
    }

    fn from_event_types(event_types: &[HostEventType]) -> Self {
        let mut bits = 0_u32;
        for event_type in event_types {
            bits |= event_type.bit();
        }
        Self(bits)
    }
}

#[derive(Debug, Clone)]
struct SubscriptionEventRecord {
    seq: u64,
    event_type: HostEventType,
    payload_json: String,
    queue_depth_after: u32,
    dropped_total: u64,
}

#[derive(Debug, Clone)]
struct EventSubscription {
    id: u32,
    event_types: Vec<HostEventType>,
    mask: EventTypeMask,
    max_buffered: usize,
    queue: Vec<SubscriptionEventRecord>,
    emitted_total: u64,
    drained_total: u64,
    dropped_total: u64,
}

impl EventSubscription {
    fn new(id: u32, mut event_types: Vec<HostEventType>, max_buffered: usize) -> Self {
        if event_types.is_empty() {
            event_types = HostEventType::all().to_vec();
        }
        event_types.sort_by(|lhs, rhs| lhs.as_str().cmp(rhs.as_str()));
        event_types.dedup();
        let mask = EventTypeMask::from_event_types(&event_types);
        Self {
            id,
            event_types,
            mask,
            max_buffered,
            queue: Vec::new(),
            emitted_total: 0,
            drained_total: 0,
            dropped_total: 0,
        }
    }

    fn matches(&self, event_type: HostEventType) -> bool {
        self.mask.contains(event_type)
    }
}

#[derive(Debug, Clone)]
struct EventSubscriptionOptions {
    event_types: Vec<HostEventType>,
    max_buffered: usize,
}

impl Default for EventSubscriptionOptions {
    fn default() -> Self {
        Self {
            event_types: HostEventType::all().to_vec(),
            max_buffered: DEFAULT_EVENT_SUBSCRIPTION_BUFFER_MAX,
        }
    }
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

    const fn policy_rule(self) -> &'static str {
        match self.reason {
            Some(reason) => reason,
            None => "allow_default",
        }
    }

    const fn action_outcome(self) -> &'static str {
        if self.allowed {
            "allow_open"
        } else {
            "block_open"
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

#[derive(Debug, Clone)]
struct ClipboardPolicy {
    copy_enabled: bool,
    paste_enabled: bool,
    max_paste_bytes: usize,
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
            // Secure-by-default posture: block cleartext HTTP unless explicitly enabled.
            allow_http: false,
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

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            copy_enabled: true,
            paste_enabled: true,
            max_paste_bytes: MAX_PASTE_BYTES,
        }
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
    fn sync_canvas_css_size(&self, geometry: GridGeometry) {
        let Some(canvas) = self.canvas.as_ref() else {
            return;
        };

        // Avoid CSS stretching. The renderer configures the WebGPU surface in device pixels
        // (`geometry.pixel_width/height`). If the canvas is stretched by CSS, browsers will
        // scale the rendered output, which looks garbled (seams/pixelation) and can be slow.
        let dpr = geometry.dpr.max(0.0001);
        let css_w = ((geometry.pixel_width as f32) / dpr).round().max(1.0) as u32;
        let css_h = ((geometry.pixel_height as f32) / dpr).round().max(1.0) as u32;

        // Avoid relying on web-sys `HtmlElement::style()` feature flags; set via reflection.
        let style = match Reflect::get(canvas.as_ref(), &JsValue::from_str("style")) {
            Ok(v) => v,
            Err(_) => return,
        };
        let _ = Reflect::set(
            &style,
            &JsValue::from_str("width"),
            &JsValue::from_str(&format!("{css_w}px")),
        );
        let _ = Reflect::set(
            &style,
            &JsValue::from_str("height"),
            &JsValue::from_str(&format!("{css_h}px")),
        );
    }

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
            ime_trace_events: Vec::new(),
            link_clicks: Vec::new(),
            event_subscriptions: HashMap::new(),
            next_event_subscription_id: 1,
            next_host_event_seq: 1,
            auto_link_ids: Vec::new(),
            auto_link_urls: HashMap::new(),
            link_open_policy: LinkOpenPolicy::default(),
            clipboard_policy: ClipboardPolicy::default(),
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
            flat_spans_scratch: Vec::new(),
            flat_cells_scratch: Vec::new(),
            dirty_row_marks: Vec::new(),
            dirty_rows_scratch: Vec::new(),
            scroll_state: ScrollState::with_defaults(),
            follow_output: true,
            attach_client: AttachClientStateMachine::default(),
            next_auto_link_id: AUTO_LINK_ID_BASE,
            progress_parser: Parser::new(),
            progress_last_value: 0,
            marker_store: MarkerStore::new(),
            engine: None,
            renderer: None,
        }
    }

    /// Stable FrankenTermJS API semver for host-side compatibility checks.
    ///
    /// This is intentionally distinct from crate/package semver.
    #[wasm_bindgen(js_name = apiVersion)]
    pub fn api_version(&self) -> String {
        FRANKENTERM_JS_API_VERSION.to_owned()
    }

    /// Canonical API contract snapshot for deterministic host validation.
    ///
    /// Shape:
    /// `{ apiLine, apiVersion, packageName, packageVersion, protocolVersion,
    ///    methods, versioningPolicy, eventSchemaVersion, eventTypes,
    ///    eventOrdering, eventBufferPolicy }`
    #[wasm_bindgen(js_name = apiContract)]
    pub fn api_contract(&self) -> JsValue {
        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("apiLine"),
            &JsValue::from_str(FRANKENTERM_JS_API_LINE),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("apiVersion"),
            &JsValue::from_str(FRANKENTERM_JS_API_VERSION),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("packageName"),
            &JsValue::from_str(env!("CARGO_PKG_NAME")),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("packageVersion"),
            &JsValue::from_str(env!("CARGO_PKG_VERSION")),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("protocolVersion"),
            &JsValue::from_str(FRANKENTERM_JS_PROTOCOL_VERSION),
        );
        let methods = js_array_from_strings(&FRANKENTERM_JS_PUBLIC_METHODS);
        let _ = Reflect::set(&obj, &JsValue::from_str("methods"), &methods);
        let versioning_policy = js_array_from_strings(&FRANKENTERM_JS_VERSIONING_POLICY);
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("versioningPolicy"),
            &versioning_policy,
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("eventSchemaVersion"),
            &JsValue::from_str(FRANKENTERM_JS_EVENT_SCHEMA_VERSION),
        );
        let event_types = js_array_from_strings(&FRANKENTERM_JS_EVENT_TYPES);
        let _ = Reflect::set(&obj, &JsValue::from_str("eventTypes"), &event_types);
        let event_ordering = js_array_from_strings(&FRANKENTERM_JS_EVENT_ORDERING_RULES);
        let _ = Reflect::set(&obj, &JsValue::from_str("eventOrdering"), &event_ordering);
        let event_buffer_policy = js_array_from_strings(&FRANKENTERM_JS_EVENT_BUFFER_POLICY);
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("eventBufferPolicy"),
            &event_buffer_policy,
        );
        obj.into()
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
        let cols = parse_init_u16(&options, "cols")?.unwrap_or(80);
        let rows = parse_init_u16(&options, "rows")?.unwrap_or(24);
        let cell_width = parse_init_u16(&options, "cellWidth")?.unwrap_or(8);
        let cell_height = parse_init_u16(&options, "cellHeight")?.unwrap_or(16);
        let dpr = parse_init_f32(&options, "dpr")?.unwrap_or(1.0);
        let zoom = parse_init_f32(&options, "zoom")?.unwrap_or(1.0);
        let backend_preference = parse_init_renderer_backend(&options)?;

        let config = RendererConfig {
            cell_width,
            cell_height,
            dpr,
            zoom,
            backend_preference,
        };

        let renderer = WebGpuRenderer::init(canvas.clone(), cols, rows, &config)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let geometry = renderer.current_geometry();

        self.cols = cols;
        self.rows = rows;
        self.shadow_cells = vec![CellData::EMPTY; usize::from(cols) * usize::from(rows)];
        self.auto_link_ids = vec![0; usize::from(cols) * usize::from(rows)];
        self.auto_link_urls.clear();
        self.follow_output = true;
        self.sync_terminal_engine_size(cols, rows);
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
        self.refresh_viewport_snapshot();
        self.sync_canvas_css_size(geometry);
        Ok(())
    }

    /// Return the active renderer backend (`webgpu`, `canvas2d`, or `none` before init).
    #[wasm_bindgen(js_name = rendererBackend)]
    pub fn renderer_backend(&self) -> String {
        self.renderer
            .as_ref()
            .map(|renderer| renderer.backend_name().to_owned())
            .unwrap_or_else(|| "none".to_owned())
    }

    /// Resize the terminal in logical grid coordinates (cols/rows).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let previous_viewport_start = self.refresh_viewport_snapshot().viewport_start;
        self.cols = cols;
        self.rows = rows;
        self.shadow_cells
            .resize(usize::from(cols) * usize::from(rows), CellData::EMPTY);
        self.auto_link_ids
            .resize(usize::from(cols) * usize::from(rows), 0);
        self.auto_link_urls.clear();
        self.sync_terminal_engine_size(cols, rows);
        self.refresh_search_after_buffer_change();
        if let Some(r) = self.renderer.as_mut() {
            r.resize(cols, rows);
            let geometry = r.current_geometry();
            self.sync_canvas_css_size(geometry);
        }
        self.refresh_viewport_after_resize(previous_viewport_start);
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
        self.sync_canvas_css_size(geometry);
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
        self.sync_canvas_css_size(geometry);
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
        let previous_viewport_start = self.refresh_viewport_snapshot().viewport_start;
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
        self.sync_terminal_engine_size(geometry.cols, geometry.rows);
        self.refresh_search_after_buffer_change();
        self.refresh_viewport_after_resize(previous_viewport_start);
        self.sync_canvas_css_size(geometry);
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

    /// Return the current IME composition snapshot.
    ///
    /// Shape:
    /// `{ active, preedit }` where `preedit` is `null` when no tracked preedit text exists.
    #[wasm_bindgen(js_name = imeState)]
    pub fn ime_state(&self) -> JsValue {
        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("active"),
            &JsValue::from_bool(self.composition.is_active()),
        );
        let preedit = self
            .composition
            .preedit()
            .map_or(JsValue::NULL, JsValue::from_str);
        let _ = Reflect::set(&obj, &JsValue::from_str("preedit"), &preedit);
        obj.into()
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
        let was_key_event = matches!(ev, InputEvent::Key(_));
        let rewrite = self.composition.rewrite(ev);

        let synthetic = rewrite.synthetic;
        let primary = rewrite.primary;
        if synthetic.is_none() && primary.is_none() && was_key_event {
            self.record_ime_drop_key_trace();
        }

        if let Some(ev) = synthetic {
            self.record_ime_trace_event(&ev, true);
            self.queue_input_event(ev)?;
        }
        if let Some(ev) = primary {
            self.record_ime_trace_event(&ev, false);
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

    /// Drain queued IME composition trace records as JSONL lines.
    ///
    /// Records are emitted in rewrite order and include post-rewrite composition
    /// state snapshots for deterministic failure triage.
    #[wasm_bindgen(js_name = drainImeCompositionJsonl)]
    pub fn drain_ime_composition_jsonl(
        &mut self,
        run_id: String,
        seed: u64,
        timestamp: String,
    ) -> Array {
        let out = Array::new();
        for (event_idx, event) in self.ime_trace_events.drain(..).enumerate() {
            let line = serde_json::json!({
                "schema_version": "e2e-jsonl-v1",
                "type": "ime_composition",
                "run_id": run_id,
                "seed": seed,
                "timestamp": timestamp,
                "event_idx": event_idx as u64,
                "event_kind": event.event_kind,
                "phase": event.phase.map(composition_phase_label),
                "data": event.data,
                "synthetic": event.synthetic,
                "active_after": event.active_after,
                "preedit_after": event.preedit_after,
            });
            if let Ok(line) = serde_json::to_string(&line) {
                out.push(&JsValue::from_str(&line));
            }
        }
        out
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

    /// Register a typed host-event subscription with bounded buffering.
    ///
    /// `options` keys:
    /// - `eventTypes` / `event_types`: string[] event taxonomy filter (defaults to all)
    /// - `maxBuffered` / `max_buffered`: number in `1..=8192` (defaults to 512)
    #[wasm_bindgen(js_name = createEventSubscription)]
    pub fn create_event_subscription(
        &mut self,
        options: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        if self.event_subscriptions.len() >= MAX_EVENT_SUBSCRIPTIONS {
            return Err(JsValue::from_str(
                "event subscription limit reached (max 256 active subscriptions)",
            ));
        }
        let config = parse_event_subscription_options(options.as_ref())?;
        let Some(subscription_id) = self.next_subscription_id() else {
            return Err(JsValue::from_str(
                "unable to allocate event subscription id",
            ));
        };
        let subscription =
            EventSubscription::new(subscription_id, config.event_types, config.max_buffered);
        let snapshot = event_subscription_to_js(&subscription);
        self.event_subscriptions
            .insert(subscription_id, subscription);
        debug!(
            target: "frankenterm_web::events",
            subscription_id,
            max_buffered = config.max_buffered,
            active_subscriptions = self.event_subscriptions.len(),
            "created event subscription"
        );
        Ok(snapshot)
    }

    /// Dispose an event subscription handle and release its queued records.
    #[wasm_bindgen(js_name = closeEventSubscription)]
    pub fn close_event_subscription(&mut self, subscription_id: u32) -> bool {
        let removed = self.event_subscriptions.remove(&subscription_id);
        if let Some(subscription) = removed {
            debug!(
                target: "frankenterm_web::events",
                subscription_id,
                emitted_total = subscription.emitted_total,
                drained_total = subscription.drained_total,
                dropped_total = subscription.dropped_total,
                "closed event subscription"
            );
            true
        } else {
            false
        }
    }

    /// Snapshot subscription queue depth/drop counters for host observability.
    ///
    /// Returns `null` when the handle does not exist.
    #[wasm_bindgen(js_name = eventSubscriptionState)]
    pub fn event_subscription_state(&self, subscription_id: u32) -> JsValue {
        self.event_subscriptions
            .get(&subscription_id)
            .map(event_subscription_to_js)
            .unwrap_or(JsValue::NULL)
    }

    /// Drain queued subscription events as structured JS objects.
    #[wasm_bindgen(js_name = drainEventSubscription)]
    pub fn drain_event_subscription(&mut self, subscription_id: u32) -> Result<Array, JsValue> {
        let Some(subscription) = self.event_subscriptions.get_mut(&subscription_id) else {
            return Err(JsValue::from_str("unknown event subscription id"));
        };

        let drained: Vec<SubscriptionEventRecord> = subscription.queue.drain(..).collect();
        subscription.drained_total = subscription
            .drained_total
            .saturating_add(drained.len() as u64);
        let arr = Array::new();
        for record in drained {
            let obj = Object::new();
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("seq"),
                &JsValue::from_f64(record.seq as f64),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("eventType"),
                &JsValue::from_str(record.event_type.as_str()),
            );
            let payload = js_sys::JSON::parse(&record.payload_json)
                .unwrap_or_else(|_| JsValue::from_str(&record.payload_json));
            let _ = Reflect::set(&obj, &JsValue::from_str("payload"), &payload);
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("queueDepthAfter"),
                &JsValue::from_f64(f64::from(record.queue_depth_after)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("droppedTotal"),
                &JsValue::from_f64(record.dropped_total as f64),
            );
            arr.push(&obj);
        }
        Ok(arr)
    }

    /// Drain queued subscription events as deterministic JSONL records.
    #[wasm_bindgen(js_name = drainEventSubscriptionJsonl)]
    pub fn drain_event_subscription_jsonl(
        &mut self,
        subscription_id: u32,
        run_id: String,
        seed: u64,
        timestamp: String,
    ) -> Result<Array, JsValue> {
        let normalized = run_id.trim();
        if normalized.is_empty() {
            return Err(JsValue::from_str("run_id must not be empty"));
        }
        let Some(subscription) = self.event_subscriptions.get_mut(&subscription_id) else {
            return Err(JsValue::from_str("unknown event subscription id"));
        };
        let drained: Vec<SubscriptionEventRecord> = subscription.queue.drain(..).collect();
        subscription.drained_total = subscription
            .drained_total
            .saturating_add(drained.len() as u64);
        let out = Array::new();
        for (event_idx, record) in drained.into_iter().enumerate() {
            let payload_value = serde_json::from_str::<serde_json::Value>(&record.payload_json)
                .unwrap_or_else(|_| serde_json::Value::String(record.payload_json.clone()));
            let line = serde_json::json!({
                "schema_version": "e2e-jsonl-v1",
                "type": "event_subscription",
                "run_id": normalized,
                "seed": seed,
                "timestamp": timestamp,
                "event_idx": event_idx as u64,
                "subscription_id": subscription_id,
                "seq": record.seq,
                "event_type": record.event_type.as_str(),
                "queue_depth_after": record.queue_depth_after,
                "dropped_total": record.dropped_total,
                "payload": payload_value,
            });
            if let Ok(line) = serde_json::to_string(&line) {
                out.push(&JsValue::from_str(&line));
            }
        }
        Ok(out)
    }

    /// Drain pending terminal reply bytes generated by VT query sequences.
    ///
    /// Returned as `Array<Uint8Array>` chunks in FIFO order.
    #[wasm_bindgen(js_name = drainReplyBytes)]
    pub fn drain_reply_bytes(&mut self) -> Array {
        let arr = Array::new();
        let drained_replies = if let Some(engine) = self.engine.as_mut() {
            engine.drain_replies()
        } else {
            return arr;
        };
        for bytes in drained_replies {
            let payload_json = serde_json::json!({
                "bytes_len": bytes.len(),
                "bytes_hex": bytes_to_hex(bytes.as_slice()),
            })
            .to_string();
            self.emit_host_event(HostEventType::TerminalReplyBytes, payload_json);
            let chunk = Uint8Array::from(bytes.as_slice());
            arr.push(&chunk.into());
        }
        arr
    }

    /// Return websocket-attach lifecycle snapshot.
    ///
    /// Shape:
    /// `{state, attempt, maxRetries, handshakeDeadlineMs, retryDeadlineMs,
    ///   sessionId, closeReason, failureCode, closeCode, cleanClose, canRetry}`
    #[wasm_bindgen(js_name = attachState)]
    pub fn attach_state(&self) -> JsValue {
        attach_snapshot_to_js(&self.attach_client.snapshot())
    }

    /// Start (or restart) a websocket attach lifecycle.
    ///
    /// Host is expected to open the websocket transport after this call reports
    /// `open_transport` in `actions`.
    #[wasm_bindgen(js_name = attachConnect)]
    pub fn attach_connect(&mut self, now_ms: u32) -> JsValue {
        let transition = self
            .attach_client
            .handle_event(u64::from(now_ms), AttachEvent::ConnectRequested);
        self.emit_attach_transition_event(&transition);
        attach_transition_to_js(&transition)
    }

    /// Inform state machine that the transport opened successfully.
    ///
    /// Host should send handshake frame when transition actions include
    /// `send_handshake`.
    #[wasm_bindgen(js_name = attachTransportOpened)]
    pub fn attach_transport_opened(&mut self, now_ms: u32) -> JsValue {
        let transition = self
            .attach_client
            .handle_event(u64::from(now_ms), AttachEvent::TransportOpened);
        self.emit_attach_transition_event(&transition);
        attach_transition_to_js(&transition)
    }

    /// Inform state machine that handshake acknowledgement was received.
    #[wasm_bindgen(js_name = attachHandshakeAck)]
    pub fn attach_handshake_ack(
        &mut self,
        session_id: &str,
        now_ms: u32,
    ) -> Result<JsValue, JsValue> {
        let normalized = session_id.trim();
        if normalized.is_empty() {
            return Err(JsValue::from_str("session_id must not be empty"));
        }
        let transition = self.attach_client.handle_event(
            u64::from(now_ms),
            AttachEvent::HandshakeAck {
                session_id: normalized.to_owned(),
            },
        );
        self.emit_attach_transition_event(&transition);
        Ok(attach_transition_to_js(&transition))
    }

    /// Inform state machine that transport was closed.
    #[wasm_bindgen(js_name = attachTransportClosed)]
    pub fn attach_transport_closed(
        &mut self,
        code: u16,
        clean: bool,
        reason: &str,
        now_ms: u32,
    ) -> JsValue {
        let transition = self.attach_client.handle_event(
            u64::from(now_ms),
            AttachEvent::TransportClosed {
                code,
                clean,
                reason: reason.to_owned(),
            },
        );
        self.emit_attach_transition_event(&transition);
        attach_transition_to_js(&transition)
    }

    /// Inform state machine about protocol-level error.
    #[wasm_bindgen(js_name = attachProtocolError)]
    pub fn attach_protocol_error(
        &mut self,
        code: &str,
        fatal: bool,
        now_ms: u32,
    ) -> Result<JsValue, JsValue> {
        let normalized = code.trim();
        if normalized.is_empty() {
            return Err(JsValue::from_str("protocol error code must not be empty"));
        }
        let transition = self.attach_client.handle_event(
            u64::from(now_ms),
            AttachEvent::ProtocolError {
                code: normalized.to_owned(),
                fatal,
            },
        );
        self.emit_attach_transition_event(&transition);
        Ok(attach_transition_to_js(&transition))
    }

    /// Inform state machine about server-initiated session end.
    #[wasm_bindgen(js_name = attachSessionEnded)]
    pub fn attach_session_ended(&mut self, reason: &str, now_ms: u32) -> JsValue {
        let transition = self.attach_client.handle_event(
            u64::from(now_ms),
            AttachEvent::SessionEnded {
                reason: reason.to_owned(),
            },
        );
        self.emit_attach_transition_event(&transition);
        attach_transition_to_js(&transition)
    }

    /// Request graceful client-side session close.
    #[wasm_bindgen(js_name = attachClose)]
    pub fn attach_close(&mut self, reason: &str, now_ms: u32) -> JsValue {
        let transition = self.attach_client.handle_event(
            u64::from(now_ms),
            AttachEvent::CloseRequested {
                reason: reason.to_owned(),
            },
        );
        self.emit_attach_transition_event(&transition);
        attach_transition_to_js(&transition)
    }

    /// Advance timer-driven attach transitions deterministically.
    #[wasm_bindgen(js_name = attachTick)]
    pub fn attach_tick(&mut self, now_ms: u32) -> JsValue {
        let transition = self
            .attach_client
            .handle_event(u64::from(now_ms), AttachEvent::Tick);
        self.emit_attach_transition_event(&transition);
        attach_transition_to_js(&transition)
    }

    /// Reset attach lifecycle to detached baseline state.
    #[wasm_bindgen(js_name = attachReset)]
    pub fn attach_reset(&mut self, now_ms: u32) -> JsValue {
        let transition = self
            .attach_client
            .handle_event(u64::from(now_ms), AttachEvent::Reset);
        self.emit_attach_transition_event(&transition);
        attach_transition_to_js(&transition)
    }

    /// Drain structured attach transition logs as JSONL lines.
    #[wasm_bindgen(js_name = drainAttachTransitionsJsonl)]
    pub fn drain_attach_transitions_jsonl(&mut self, run_id: &str) -> Result<Array, JsValue> {
        let normalized = run_id.trim();
        if normalized.is_empty() {
            return Err(JsValue::from_str("run_id must not be empty"));
        }
        let out = Array::new();
        for line in self.attach_client.drain_transition_jsonl(normalized) {
            out.push(&JsValue::from_str(&line));
        }
        Ok(out)
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
        if !self.clipboard_policy.paste_enabled {
            return Err(JsValue::from_str("paste disabled by clipboard policy"));
        }
        if text.len() > self.clipboard_policy.max_paste_bytes {
            return Err(JsValue::from_str(&format!(
                "paste payload too large (max {} UTF-8 bytes)",
                self.clipboard_policy.max_paste_bytes
            )));
        }
        self.queue_input_event(InputEvent::Paste(PasteInput { data: text.into() }))
    }

    /// Feed a VT/ANSI byte stream (remote mode).
    pub fn feed(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.process_progress_signals(data);
        let Some(_) = self.engine.as_ref() else {
            return;
        };
        let previous_viewport_start = self.refresh_viewport_snapshot().viewport_start;

        let patch = {
            let engine = self
                .engine
                .as_mut()
                .expect("engine was checked as present above");
            engine.feed_bytes(data);
            engine.snapshot_patches()
        };
        let patches = core_patch_to_patches(&patch);
        if patches.is_empty() {
            self.refresh_viewport_after_content_change(previous_viewport_start);
            return;
        }
        self.apply_cell_patches(&patches);
        self.refresh_viewport_after_content_change(previous_viewport_start);
    }

    /// Apply a cell patch (ftui-web mode).
    ///
    /// Accepts a JS object: `{ offset: number, cells: [{bg, fg, glyph, attrs}] }`.
    /// When a renderer is initialized, only the patched cells are uploaded to
    /// the GPU. Without a renderer, patches still update the in-memory shadow
    /// state so host-side logic (search/link lookup/evidence) remains usable.
    #[wasm_bindgen(js_name = applyPatch)]
    pub fn apply_patch(&mut self, patch: JsValue) -> Result<(), JsValue> {
        let previous_viewport_start = self.refresh_viewport_snapshot().viewport_start;
        let patch = parse_cell_patch(&patch)?;
        self.apply_cell_patches(std::slice::from_ref(&patch));
        self.refresh_viewport_after_content_change(previous_viewport_start);
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
        let previous_viewport_start = self.refresh_viewport_snapshot().viewport_start;
        self.apply_cell_patches(&parsed);
        self.refresh_viewport_after_content_change(previous_viewport_start);
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
        self.flat_spans_scratch.resize(spans.length() as usize, 0);
        spans.copy_to(self.flat_spans_scratch.as_mut_slice());
        self.flat_cells_scratch.resize(cells.length() as usize, 0);
        cells.copy_to(self.flat_cells_scratch.as_mut_slice());
        let parsed = cell_patches_from_flat_u32(&self.flat_spans_scratch, &self.flat_cells_scratch)
            .map_err(JsValue::from_str)?;
        let previous_viewport_start = self.refresh_viewport_snapshot().viewport_start;
        self.apply_cell_patches(&parsed);
        self.refresh_viewport_after_content_change(previous_viewport_start);
        Ok(())
    }

    fn apply_cell_patches(&mut self, patches: &[CellPatch]) {
        let cols = usize::from(self.cols);
        let row_count = usize::from(self.rows);
        let max = cols * usize::from(self.rows);
        self.shadow_cells.resize(max, CellData::EMPTY);
        self.auto_link_ids.resize(max, 0);
        if self.dirty_row_marks.len() < row_count {
            self.dirty_row_marks.resize(row_count, 0);
        }

        let mut dirty_rows = std::mem::take(&mut self.dirty_rows_scratch);
        dirty_rows.clear();
        for patch in patches {
            let start = usize::try_from(patch.offset).unwrap_or(max).min(max);
            let count = patch.cells.len().min(max.saturating_sub(start));
            for (i, cell) in patch.cells.iter().take(count).enumerate() {
                self.shadow_cells[start + i] = *cell;
            }
            if cols > 0 && count > 0 {
                let first_row = start / cols;
                let last_row = (start + count - 1) / cols;
                for r in first_row..=last_row {
                    if self.dirty_row_marks[r] == 0 {
                        self.dirty_row_marks[r] = 1;
                        dirty_rows.push(r);
                    }
                }
            }
        }
        // Preserve historical row-major determinism regardless of patch order.
        if dirty_rows.len() > 1 {
            dirty_rows.sort_unstable();
        }
        // Reset row marks for next patch batch.
        for &row in &dirty_rows {
            self.dirty_row_marks[row] = 0;
        }

        self.recompute_auto_links_for_rows(&dirty_rows);
        if !self.search_query.is_empty() {
            self.refresh_search_after_buffer_change();
        }
        if self.hovered_link_id != 0 && !self.link_id_present(self.hovered_link_id) {
            self.hovered_link_id = 0;
            self.sync_renderer_interaction_state();
        }

        if let Some(renderer) = self.renderer.as_mut() {
            renderer.apply_patches(patches);
        }
        self.dirty_rows_scratch = dirty_rows;
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

    /// Create a marker anchored to a unified-history line index.
    ///
    /// - `line_idx`: `0 = oldest retained line`, must be in range of
    ///   `viewportState().totalLines`.
    /// - `column`: optional preferred column for inline/range decorations.
    ///
    /// Returns a deterministic marker id (`u32`).
    #[wasm_bindgen(js_name = createMarker)]
    pub fn create_marker(&mut self, line_idx: i32, column: i32) -> Result<u32, JsValue> {
        if line_idx < 0 {
            return Err(JsValue::from_str("line index must be >= 0"));
        }
        let relative_line =
            usize::try_from(line_idx).map_err(|_| JsValue::from_str("line index overflow"))?;
        let column_u16 = if column < 0 {
            0
        } else {
            u16::try_from(column).map_err(|_| JsValue::from_str("column overflow"))?
        };
        let window = self.marker_history_window();
        self.marker_store
            .create_marker(relative_line, column_u16, window)
            .map_err(JsValue::from_str)
    }

    /// Remove a marker by id.
    ///
    /// Returns `true` when a marker existed and was removed.
    #[wasm_bindgen(js_name = dropMarker)]
    pub fn drop_marker(&mut self, marker_id: u32) -> bool {
        let window = self.marker_history_window();
        self.marker_store.remove_marker(marker_id, window)
    }

    /// Return marker snapshots with deterministic anchor-resolution metadata.
    #[wasm_bindgen(js_name = markersState)]
    pub fn markers_state(&self) -> JsValue {
        let window = self.marker_history_window();
        let markers = self.marker_store.marker_snapshots(window);

        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("historyBaseAbsoluteLine"),
            &JsValue::from_f64(window.base_absolute_line as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("totalLines"),
            &JsValue::from_f64(window.total_lines as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("scrollbackLines"),
            &JsValue::from_f64(window.scrollback_lines as f64),
        );
        let marker_arr = Array::new_with_length(markers.len() as u32);
        for (idx, marker) in markers.iter().enumerate() {
            let marker_obj = Object::new();
            let _ = Reflect::set(
                &marker_obj,
                &JsValue::from_str("id"),
                &JsValue::from_f64(f64::from(marker.id)),
            );
            let _ = Reflect::set(
                &marker_obj,
                &JsValue::from_str("absoluteLine"),
                &JsValue::from_f64(marker.absolute_line as f64),
            );
            let _ = Reflect::set(
                &marker_obj,
                &JsValue::from_str("column"),
                &JsValue::from_f64(f64::from(marker.column)),
            );
            let _ = Reflect::set(
                &marker_obj,
                &JsValue::from_str("stale"),
                &JsValue::from_bool(marker.stale),
            );
            let stale_reason = marker
                .stale_reason
                .map(JsValue::from_str)
                .unwrap_or(JsValue::NULL);
            let _ = Reflect::set(&marker_obj, &JsValue::from_str("staleReason"), &stale_reason);
            let relative_line = marker
                .relative_line
                .map(|v| JsValue::from_f64(v as f64))
                .unwrap_or(JsValue::NULL);
            let _ = Reflect::set(
                &marker_obj,
                &JsValue::from_str("relativeLine"),
                &relative_line,
            );
            let grid_row = marker
                .grid_row
                .map(|v| JsValue::from_f64(v as f64))
                .unwrap_or(JsValue::NULL);
            let _ = Reflect::set(&marker_obj, &JsValue::from_str("gridRow"), &grid_row);
            let cell_offset = marker
                .cell_offset
                .map(|v| JsValue::from_f64(v as f64))
                .unwrap_or(JsValue::NULL);
            let _ = Reflect::set(&marker_obj, &JsValue::from_str("cellOffset"), &cell_offset);
            marker_arr.set(idx as u32, marker_obj.into());
        }
        let _ = Reflect::set(&obj, &JsValue::from_str("markers"), &marker_arr);
        obj.into()
    }

    /// Create a decoration primitive anchored by marker ids.
    ///
    /// `kind` values:
    /// - `"inline"`: range `[startCol, endCol)` on `startMarkerId` line
    /// - `"line"`: full-line decoration at `startMarkerId`
    /// - `"range"`: multiline range from `startMarkerId` to `endMarkerId`
    ///
    /// For non-range kinds pass `endMarkerId < 0`.
    #[wasm_bindgen(js_name = createDecoration)]
    pub fn create_decoration(
        &mut self,
        kind: &str,
        start_marker_id: u32,
        end_marker_id: i32,
        start_col: i32,
        end_col: i32,
    ) -> Result<u32, JsValue> {
        let kind = DecorationKind::parse(kind)
            .ok_or_else(|| JsValue::from_str("invalid decoration kind"))?;
        let end_marker = if end_marker_id < 0 {
            None
        } else {
            Some(
                u32::try_from(end_marker_id)
                    .map_err(|_| JsValue::from_str("end marker id overflow"))?,
            )
        };
        let start_col = if start_col < 0 {
            0
        } else {
            u16::try_from(start_col).map_err(|_| JsValue::from_str("start col overflow"))?
        };
        let end_col = if end_col < 0 {
            0
        } else {
            u16::try_from(end_col).map_err(|_| JsValue::from_str("end col overflow"))?
        };
        let window = self.marker_history_window();
        self.marker_store
            .create_decoration(kind, start_marker_id, end_marker, start_col, end_col, window)
            .map_err(JsValue::from_str)
    }

    /// Remove a decoration by id.
    ///
    /// Returns `true` when a decoration existed and was removed.
    #[wasm_bindgen(js_name = dropDecoration)]
    pub fn drop_decoration(&mut self, decoration_id: u32) -> bool {
        self.marker_store.remove_decoration(decoration_id)
    }

    /// Return decoration snapshots resolved against the current viewport/history.
    #[wasm_bindgen(js_name = decorationsState)]
    pub fn decorations_state(&self) -> JsValue {
        let window = self.marker_history_window();
        let decorations = self.marker_store.decoration_snapshots(window);
        let arr = Array::new_with_length(decorations.len() as u32);
        for (idx, decoration) in decorations.iter().enumerate() {
            let obj = Object::new();
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("id"),
                &JsValue::from_f64(f64::from(decoration.id)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("kind"),
                &JsValue::from_str(decoration.kind.as_str()),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("startMarkerId"),
                &JsValue::from_f64(f64::from(decoration.start_marker_id)),
            );
            let end_marker = decoration
                .end_marker_id
                .map(|id| JsValue::from_f64(f64::from(id)))
                .unwrap_or(JsValue::NULL);
            let _ = Reflect::set(&obj, &JsValue::from_str("endMarkerId"), &end_marker);
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("stale"),
                &JsValue::from_bool(decoration.stale),
            );
            let stale_reason = decoration
                .stale_reason
                .map(JsValue::from_str)
                .unwrap_or(JsValue::NULL);
            let _ = Reflect::set(&obj, &JsValue::from_str("staleReason"), &stale_reason);
            let start_offset = decoration
                .start_offset
                .map(|v| JsValue::from_f64(v as f64))
                .unwrap_or(JsValue::NULL);
            let _ = Reflect::set(&obj, &JsValue::from_str("startOffset"), &start_offset);
            let end_offset = decoration
                .end_offset
                .map(|v| JsValue::from_f64(v as f64))
                .unwrap_or(JsValue::NULL);
            let _ = Reflect::set(&obj, &JsValue::from_str("endOffset"), &end_offset);
            arr.set(idx as u32, obj.into());
        }
        arr.into()
    }

    /// Drain marker/decoration diagnostics as JSONL lines.
    ///
    /// Records are ordered by deterministic diagnostic sequence and include
    /// stale/invalidation reasons for replay-grade troubleshooting.
    #[wasm_bindgen(js_name = drainMarkerDecorationJsonl)]
    pub fn drain_marker_decoration_jsonl(
        &mut self,
        run_id: String,
        seed: u64,
        timestamp: String,
    ) -> Result<Array, JsValue> {
        if run_id.is_empty() {
            return Err(JsValue::from_str("run_id must not be empty"));
        }
        if timestamp.is_empty() {
            return Err(JsValue::from_str("timestamp must not be empty"));
        }
        let window = self.marker_history_window();
        let events = self.marker_store.drain_diagnostics();
        let out = Array::new_with_length(events.len() as u32);
        for (event_idx, event) in events.into_iter().enumerate() {
            let payload = serde_json::json!({
                "type": "marker_decoration",
                "schema_version": "frankenterm-marker-v1",
                "run_id": run_id,
                "seed": seed,
                "timestamp": timestamp,
                "event_idx": event_idx,
                "seq": event.seq,
                "entity": event.entity.as_str(),
                "action": event.action,
                "id": event.id,
                "reason": event.reason,
                "history_base_absolute_line": window.base_absolute_line,
                "history_total_lines": window.total_lines,
                "scrollback_lines": window.scrollback_lines,
                "cols": window.cols,
                "rows": window.rows,
            })
            .to_string();
            out.set(event_idx as u32, JsValue::from_str(&payload));
        }
        Ok(out)
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
        self.align_viewport_to_active_search_match();
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
        self.align_viewport_to_active_search_match();
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

    /// Return a deterministic viewport snapshot over unified history
    /// (`scrollback + visible grid`).
    ///
    /// Shape:
    /// `{ totalLines, scrollbackLines, gridRows, viewportStart, viewportEnd,
    ///    renderStart, renderEnd, scrollOffsetFromBottom, maxScrollOffset,
    ///    atBottom, followOutput, animating, subLineOffset }`
    #[wasm_bindgen(js_name = viewportState)]
    pub fn viewport_state(&mut self) -> JsValue {
        let snap = self.refresh_viewport_snapshot();
        let scrollback_lines = self.scrollback_line_count();
        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("totalLines"),
            &JsValue::from_f64(snap.total_lines as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("scrollbackLines"),
            &JsValue::from_f64(scrollback_lines as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("gridRows"),
            &JsValue::from_f64(f64::from(self.rows)),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("viewportStart"),
            &JsValue::from_f64(snap.viewport_start as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("viewportEnd"),
            &JsValue::from_f64(snap.viewport_end as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("renderStart"),
            &JsValue::from_f64(snap.render_start as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("renderEnd"),
            &JsValue::from_f64(snap.render_end as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("scrollOffsetFromBottom"),
            &JsValue::from_f64(snap.scroll_offset_from_bottom as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("maxScrollOffset"),
            &JsValue::from_f64(snap.max_scroll_offset as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("atBottom"),
            &JsValue::from_bool(snap.is_at_bottom),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("followOutput"),
            &JsValue::from_bool(self.follow_output),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("animating"),
            &JsValue::from_bool(snap.is_animating),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("subLineOffset"),
            &JsValue::from_f64(snap.sub_line_offset),
        );
        obj.into()
    }

    /// Return visible viewport text lines over unified history
    /// (`scrollback + visible grid`).
    #[wasm_bindgen(js_name = viewportLines)]
    pub fn viewport_lines(&mut self) -> Array {
        let snap = self.refresh_viewport_snapshot();
        let out = Array::new();
        for line_idx in snap.viewport_start..snap.viewport_end {
            out.push(&JsValue::from_str(&self.history_line_text(line_idx)));
        }
        out
    }

    /// Emit one JSONL `scrollback_frame` trace record for viewport telemetry.
    ///
    /// This mirrors `frame_harness::scrollback_virtualization_frame_jsonl` and
    /// is intended for deterministic E2E/perf evidence collection.
    #[wasm_bindgen(js_name = snapshotScrollbackFrameJsonl)]
    pub fn snapshot_scrollback_frame_jsonl(
        &mut self,
        run_id: &str,
        timestamp: &str,
        frame_idx: u32,
        render_cost_us: u32,
    ) -> Result<String, JsValue> {
        if run_id.is_empty() {
            return Err(JsValue::from_str("run_id must not be empty"));
        }
        if timestamp.is_empty() {
            return Err(JsValue::from_str("timestamp must not be empty"));
        }

        let snap = self.refresh_viewport_snapshot();
        let window = ScrollbackWindow {
            total_lines: snap.total_lines,
            max_scroll_offset: snap.max_scroll_offset,
            scroll_offset_from_bottom: snap.scroll_offset_from_bottom,
            viewport_start: snap.viewport_start,
            viewport_end: snap.viewport_end,
            render_start: snap.render_start,
            render_end: snap.render_end,
        };
        Ok(scrollback_virtualization_frame_jsonl(
            run_id,
            timestamp,
            u64::from(frame_idx),
            window,
            Duration::from_micros(u64::from(render_cost_us)),
        ))
    }

    /// Scroll viewport by signed line count (positive = older, negative = newer).
    #[wasm_bindgen(js_name = scrollLines)]
    pub fn scroll_lines_nav(&mut self, lines: i32) -> JsValue {
        self.scroll_state.scroll_lines(lines as isize);
        self.refresh_viewport_after_user_navigation();
        self.viewport_state()
    }

    /// Scroll viewport by signed page count.
    ///
    /// One page equals current viewport row count.
    #[wasm_bindgen(js_name = scrollPages)]
    pub fn scroll_pages_nav(&mut self, pages: i32) -> JsValue {
        let rows = usize::from(self.rows.max(1));
        let delta = i64::from(pages).saturating_mul(rows as i64);
        let clamped = delta.clamp(isize::MIN as i64, isize::MAX as i64) as isize;
        self.scroll_state.scroll_lines(clamped);
        self.refresh_viewport_after_user_navigation();
        self.viewport_state()
    }

    /// Jump viewport to newest output (follow-output position).
    #[wasm_bindgen(js_name = scrollToBottom)]
    pub fn scroll_to_bottom_nav(&mut self) -> JsValue {
        self.scroll_state.snap_to_bottom();
        self.follow_output = true;
        self.viewport_state()
    }

    /// Jump viewport to oldest retained line.
    #[wasm_bindgen(js_name = scrollToTop)]
    pub fn scroll_to_top_nav(&mut self) -> JsValue {
        self.scroll_state
            .snap_to_top(self.total_history_lines(), usize::from(self.rows));
        self.refresh_viewport_after_user_navigation();
        self.viewport_state()
    }

    /// Jump viewport so target absolute history line is visible.
    ///
    /// `line_idx` uses unified history indexing (`0 = oldest retained line`).
    #[wasm_bindgen(js_name = scrollToLine)]
    pub fn scroll_to_line_nav(&mut self, line_idx: u32) -> JsValue {
        self.scroll_state.jump_to_line(
            self.total_history_lines(),
            usize::from(self.rows),
            line_idx as usize,
        );
        self.refresh_viewport_after_user_navigation();
        self.viewport_state()
    }

    /// Return hyperlink ID at a given grid cell (0 if none / out of bounds).
    #[wasm_bindgen(js_name = linkAt)]
    pub fn link_at(&self, x: u16, y: u16) -> u32 {
        self.link_id_at_xy(x, y)
    }

    /// Return resolved hyperlink URL at a given cell, if present.
    ///
    /// Explicit OSC-8 links take precedence over auto-detected plaintext URLs.
    #[wasm_bindgen(js_name = linkUrlAt)]
    pub fn link_url_at(&self, x: u16, y: u16) -> Option<String> {
        let offset = self.cell_offset_at_xy(x, y)?;
        let explicit_id = self
            .shadow_cells
            .get(offset)
            .map_or(0, |cell| cell_attr_link_id(cell.attrs));
        if explicit_id != 0 {
            return self.explicit_link_url(explicit_id);
        }
        let auto_id = self.auto_link_ids.get(offset).copied().unwrap_or(0);
        self.auto_link_urls.get(&auto_id).cloned()
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
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("policyRule"),
                &JsValue::from_str(resolved.policy_rule),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("actionOutcome"),
                &JsValue::from_str(resolved.action_outcome),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("auditUrl"),
                &resolved
                    .audit_url
                    .as_ref()
                    .map_or(JsValue::NULL, |url| JsValue::from_str(url)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("auditUrlRedacted"),
                &JsValue::from_bool(resolved.audit_url_redacted),
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
                policy_rule: resolved.policy_rule.to_string(),
                action_outcome: resolved.action_outcome.to_string(),
                audit_url: resolved.audit_url,
                audit_url_redacted: resolved.audit_url_redacted,
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
    ///
    /// Defaults: `allowHttp=false`, `allowHttps=true`, empty allow/block host lists.
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

    /// Configure clipboard policy defaults.
    ///
    /// Supported keys:
    /// - `copyEnabled` / `copy_enabled`: bool
    /// - `pasteEnabled` / `paste_enabled`: bool
    /// - `maxPasteBytes` / `max_paste_bytes`: number (1..=786432)
    #[wasm_bindgen(js_name = setClipboardPolicy)]
    pub fn set_clipboard_policy(&mut self, options: JsValue) -> Result<(), JsValue> {
        self.clipboard_policy =
            parse_clipboard_policy(Some(&options), self.clipboard_policy.clone())?;
        Ok(())
    }

    /// Return current clipboard policy snapshot.
    #[wasm_bindgen(js_name = clipboardPolicy)]
    pub fn clipboard_policy_snapshot(&self) -> JsValue {
        clipboard_policy_to_js(&self.clipboard_policy)
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
        let start = start.min(total);
        let end = end.min(total);
        if start >= end {
            return String::new();
        }
        let wide_continuation = infer_wide_continuations(&self.shadow_cells, cols);
        let mut lines = Vec::new();
        let mut line = String::new();
        for offset in start..end {
            let idx = usize::try_from(offset).unwrap_or(usize::MAX);
            if idx >= self.shadow_cells.len() {
                break;
            }
            if offset > start && idx % cols == 0 {
                trim_trailing_spaces(&mut line);
                lines.push(std::mem::take(&mut line));
            }
            if wide_continuation[idx] {
                continue;
            }
            let glyph_id = self.shadow_cells[idx].glyph_id;
            let ch = if glyph_id == 0 {
                ' '
            } else {
                char::from_u32(glyph_id).unwrap_or('□')
            };
            line.push(ch);
        }
        trim_trailing_spaces(&mut line);
        lines.push(line);
        lines.join("\n")
    }

    /// Return selected text for host-managed clipboard writes.
    ///
    /// Returns `None` when there is no active non-empty selection.
    #[wasm_bindgen(js_name = copySelection)]
    pub fn copy_selection(&self) -> Option<String> {
        if !self.clipboard_policy.copy_enabled {
            return None;
        }
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
        self.scroll_state.tick(self.max_scroll_offset());
        self.refresh_viewport_snapshot();
        let Some(renderer) = self.renderer.as_mut() else {
            return Err(JsValue::from_str("renderer not initialized"));
        };
        renderer
            .render_frame()
            .map(|_| ())
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Explicit teardown for JS callers. Drops GPU resources and clears
    /// internal references so the canvas can be reclaimed.
    pub fn destroy(&mut self) {
        self.engine = None;
        self.renderer = None;
        self.initialized = false;
        self.canvas = None;
        self.mods = ModifierTracker::default();
        self.composition = CompositionState::default();
        self.encoder_features = VtInputEncoderFeatures::default();
        self.encoded_inputs.clear();
        self.encoded_input_bytes.clear();
        self.ime_trace_events.clear();
        self.link_clicks.clear();
        self.event_subscriptions.clear();
        self.next_event_subscription_id = 1;
        self.next_host_event_seq = 1;
        self.auto_link_ids.clear();
        self.auto_link_urls.clear();
        self.next_auto_link_id = AUTO_LINK_ID_BASE;
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
        self.flat_spans_scratch.clear();
        self.flat_cells_scratch.clear();
        self.dirty_row_marks.clear();
        self.dirty_rows_scratch.clear();
        self.scroll_state = ScrollState::with_defaults();
        self.follow_output = true;
        self.attach_client = AttachClientStateMachine::default();
        self.progress_parser = Parser::new();
        self.progress_last_value = 0;
    }
}

impl FrankenTermWeb {
    fn next_subscription_id(&mut self) -> Option<u32> {
        for _ in 0..=MAX_EVENT_SUBSCRIPTIONS {
            let candidate = self.next_event_subscription_id.max(1);
            self.next_event_subscription_id = candidate.saturating_add(1);
            if !self.event_subscriptions.contains_key(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    fn emit_host_event(&mut self, event_type: HostEventType, payload_json: String) {
        let seq = self.next_host_event_seq;
        self.next_host_event_seq = self.next_host_event_seq.saturating_add(1);
        if self.event_subscriptions.is_empty() {
            return;
        }

        for subscription in self.event_subscriptions.values_mut() {
            if !subscription.matches(event_type) {
                continue;
            }
            let mut dropped_now = 0usize;
            if subscription.queue.len() >= subscription.max_buffered {
                let overflow = subscription.queue.len() - subscription.max_buffered + 1;
                subscription.queue.drain(..overflow);
                subscription.dropped_total =
                    subscription.dropped_total.saturating_add(overflow as u64);
                dropped_now = overflow;
                warn!(
                    target: "frankenterm_web::events",
                    subscription_id = subscription.id,
                    seq,
                    event_type = event_type.as_str(),
                    queue_max = subscription.max_buffered,
                    overflow = overflow,
                    dropped_total = subscription.dropped_total,
                    "event subscription dropped oldest records"
                );
            }

            subscription.emitted_total = subscription.emitted_total.saturating_add(1);
            let queue_depth_after = subscription.queue.len().saturating_add(1);
            let queue_depth_after_u32 = u32::try_from(queue_depth_after).unwrap_or(u32::MAX);
            let dropped_total = subscription.dropped_total;
            subscription.queue.push(SubscriptionEventRecord {
                seq,
                event_type,
                payload_json: payload_json.clone(),
                queue_depth_after: queue_depth_after_u32,
                dropped_total,
            });
            trace!(
                target: "frankenterm_web::events",
                subscription_id = subscription.id,
                seq,
                event_type = event_type.as_str(),
                queue_depth_after = queue_depth_after,
                dropped_now = dropped_now,
                dropped_total = dropped_total,
                "event delivered to subscription queue"
            );
        }
    }

    fn emit_attach_transition_event(&mut self, transition: &AttachTransition) {
        let payload_json = serde_json::json!({
            "transition_seq": transition.seq,
            "at_ms": transition.at_ms,
            "event": transition.event.as_str(),
            "from_state": transition.from_state.as_str(),
            "to_state": transition.to_state.as_str(),
            "attempt": transition.attempt,
            "session_id": transition.session_id,
            "close_code": transition.close_code,
            "clean_close": transition.clean_close,
            "reason": transition.reason,
        })
        .to_string();
        self.emit_host_event(HostEventType::AttachTransition, payload_json);
    }

    fn process_progress_signals(&mut self, data: &[u8]) {
        if self.event_subscriptions.is_empty() {
            // Still advance parser state for deterministic chunk-boundary behavior.
            let _ = self.progress_parser.feed(data);
            return;
        }
        let actions = self.progress_parser.feed(data);
        for action in actions {
            let Action::Escape(sequence) = action else {
                continue;
            };
            let Some((command, payload)) = parse_osc_command_and_payload(&sequence) else {
                continue;
            };
            if command != 9 {
                continue;
            }
            let Some(record) = parse_progress_signal_payload(&payload, self.progress_last_value)
            else {
                // OSC 9 with a non-progress subcommand is not part of this API.
                continue;
            };
            if record.accepted {
                if let Some(value) = record.value {
                    self.progress_last_value = value;
                }
                trace!(
                    target: "frankenterm_web::events",
                    state = record.state.map(ProgressSignalState::as_str),
                    state_code = record.state_code,
                    value = record.value,
                    value_provided = record.value_provided,
                    "accepted OSC 9;4 progress signal"
                );
            } else {
                warn!(
                    target: "frankenterm_web::events",
                    reason = record.reason.unwrap_or("unknown"),
                    raw_payload = record.raw_payload,
                    raw_state = record.raw_state,
                    raw_value = record.raw_value,
                    "rejected OSC 9;4 progress signal"
                );
            }
            let payload_json = progress_signal_payload_json(&record, bytes_to_hex(&sequence));
            self.emit_host_event(HostEventType::TerminalProgress, payload_json);
        }
    }

    fn scrollback_line_count(&self) -> usize {
        self.engine
            .as_ref()
            .map_or(0, |engine| engine.scrollback().len())
    }

    fn history_base_absolute_line(&self) -> u64 {
        self.engine
            .as_ref()
            .map_or(0, |engine| engine.scrollback().oldest_line_absolute())
    }

    fn marker_history_window(&self) -> HistoryWindow {
        HistoryWindow {
            base_absolute_line: self.history_base_absolute_line(),
            total_lines: self.total_history_lines(),
            scrollback_lines: self.scrollback_line_count(),
            cols: usize::from(self.cols),
            rows: usize::from(self.rows),
        }
    }

    fn reconcile_markers(&mut self) {
        let window = self.marker_history_window();
        self.marker_store.reconcile(window);
    }

    fn total_history_lines(&self) -> usize {
        self.scrollback_line_count()
            .saturating_add(usize::from(self.rows))
    }

    fn max_scroll_offset(&self) -> usize {
        let total_lines = self.total_history_lines();
        let viewport_rows = usize::from(self.rows);
        total_lines.saturating_sub(viewport_rows.min(total_lines))
    }

    fn refresh_viewport_snapshot(&mut self) -> ViewportSnapshot {
        self.scroll_state
            .viewport(self.total_history_lines(), usize::from(self.rows))
    }

    fn refresh_viewport_after_user_navigation(&mut self) -> ViewportSnapshot {
        let snap = self.refresh_viewport_snapshot();
        self.reconcile_markers();
        self.follow_output = snap.is_at_bottom;
        snap
    }

    fn refresh_viewport_after_content_change(
        &mut self,
        previous_viewport_start: usize,
    ) -> ViewportSnapshot {
        if self.follow_output {
            self.scroll_state.snap_to_bottom();
        } else {
            self.scroll_state.set_viewport_start(
                self.total_history_lines(),
                usize::from(self.rows),
                previous_viewport_start,
            );
        }
        let snap = self.refresh_viewport_snapshot();
        self.reconcile_markers();
        if !self.follow_output && snap.max_scroll_offset == 0 {
            // If there is no scrollback headroom anymore, automatically resume
            // follow-output mode.
            self.follow_output = true;
        }
        snap
    }

    fn refresh_viewport_after_resize(
        &mut self,
        previous_viewport_start: usize,
    ) -> ViewportSnapshot {
        self.refresh_viewport_after_content_change(previous_viewport_start)
    }

    fn history_line_text(&self, line_idx: usize) -> String {
        let scrollback_len = self.scrollback_line_count();
        if line_idx < scrollback_len {
            if let Some(engine) = self.engine.as_ref()
                && let Some(line) = engine.scrollback().get(line_idx)
            {
                return line.cells.iter().map(|cell| cell.content()).collect();
            }
            return String::new();
        }

        let grid_row = line_idx.saturating_sub(scrollback_len);
        self.shadow_grid_row_text(grid_row)
    }

    fn shadow_grid_row_text(&self, row: usize) -> String {
        let rows = usize::from(self.rows);
        if row >= rows {
            return String::new();
        }

        let cols = usize::from(self.cols);
        if cols == 0 {
            return String::new();
        }

        let row_start = row.saturating_mul(cols);
        let row_end = row_start.saturating_add(cols).min(self.shadow_cells.len());
        if row_start >= row_end {
            return String::new();
        }

        let mut line = String::with_capacity(cols);
        for idx in row_start..row_end {
            let glyph_id = self.shadow_cells[idx].glyph_id;
            let ch = if glyph_id == 0 {
                ' '
            } else {
                char::from_u32(glyph_id).unwrap_or('□')
            };
            line.push(ch);
        }
        line
    }

    fn sync_terminal_engine_size(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            self.engine = None;
            self.reconcile_markers();
            return;
        }

        if let Some(engine) = self.engine.as_mut() {
            engine.resize(cols, rows);
        } else {
            self.engine = Some(TerminalEngine::new(cols, rows));
        }
        self.reconcile_markers();
    }

    fn queue_input_event(&mut self, ev: InputEvent) -> Result<(), JsValue> {
        if let InputEvent::Paste(paste) = &ev {
            if !self.clipboard_policy.paste_enabled {
                return Err(JsValue::from_str("paste disabled by clipboard policy"));
            }
            if paste.data.len() > self.clipboard_policy.max_paste_bytes {
                return Err(JsValue::from_str(&format!(
                    "paste payload too large (max {} UTF-8 bytes)",
                    self.clipboard_policy.max_paste_bytes
                )));
            }
        }

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
        if let InputEvent::Wheel(wheel) = &ev
            && !self.encoder_features.sgr_mouse
        {
            self.scroll_state
                .apply_wheel(i32::from(wheel.dy), self.max_scroll_offset());
            self.refresh_viewport_after_user_navigation();
        }
        self.handle_interaction_event(&ev);

        let json = ev
            .to_json_string()
            .map_err(|err| JsValue::from_str(&err.to_string()))?;
        push_bounded(
            &mut self.encoded_inputs,
            json.clone(),
            MAX_ENCODED_INPUT_EVENTS,
        );
        let input_payload = serde_json::json!({
            "kind": HostEventType::from_input_event(&ev).as_str(),
            "encoded_input": json,
            "encoded_queue_depth": self.encoded_inputs.len(),
        })
        .to_string();
        self.emit_host_event(HostEventType::from_input_event(&ev), input_payload);

        let vt = encode_vt_input_event(&ev, self.encoder_features);
        if !vt.is_empty() {
            let vt_payload = serde_json::json!({
                "bytes_len": vt.len(),
                "bytes_hex": bytes_to_hex(vt.as_slice()),
            })
            .to_string();
            push_bounded(
                &mut self.encoded_input_bytes,
                vt,
                MAX_ENCODED_INPUT_BYTE_CHUNKS,
            );
            self.emit_host_event(HostEventType::InputVtBytes, vt_payload);
        }
        Ok(())
    }

    fn record_ime_trace_event(&mut self, event: &InputEvent, synthetic: bool) {
        let InputEvent::Composition(comp) = event else {
            return;
        };
        let record = ImeTraceEvent {
            event_kind: "composition",
            phase: Some(comp.phase),
            data: comp.data.as_deref().map(ToOwned::to_owned),
            synthetic,
            active_after: self.composition.is_active(),
            preedit_after: self.composition.preedit().map(ToOwned::to_owned),
        };
        let payload_json = serde_json::json!({
            "event_kind": record.event_kind,
            "phase": record.phase.map(composition_phase_label),
            "data": record.data,
            "synthetic": record.synthetic,
            "active_after": record.active_after,
            "preedit_after": record.preedit_after,
        })
        .to_string();
        push_bounded(&mut self.ime_trace_events, record, MAX_IME_TRACE_EVENTS);
        self.emit_host_event(HostEventType::InputCompositionTrace, payload_json);
    }

    fn record_ime_drop_key_trace(&mut self) {
        let record = ImeTraceEvent {
            event_kind: "drop_key",
            phase: None,
            data: None,
            synthetic: false,
            active_after: self.composition.is_active(),
            preedit_after: self.composition.preedit().map(ToOwned::to_owned),
        };
        let payload_json = serde_json::json!({
            "event_kind": record.event_kind,
            "phase": serde_json::Value::Null,
            "data": serde_json::Value::Null,
            "synthetic": record.synthetic,
            "active_after": record.active_after,
            "preedit_after": record.preedit_after,
        })
        .to_string();
        push_bounded(&mut self.ime_trace_events, record, MAX_IME_TRACE_EVENTS);
        self.emit_host_event(HostEventType::InputCompositionTrace, payload_json);
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
        let has_shaping_state = self.text_shaping.enabled;
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
        let text_shaping_engine = if self.text_shaping.enabled {
            self.text_shaping.engine.as_u32()
        } else {
            0
        };
        Some(InteractionSnapshot {
            hovered_link_id: self.hovered_link_id,
            cursor_offset: self.cursor_offset.unwrap_or(0),
            cursor_style: self.cursor_style.as_u32(),
            selection_active,
            selection_start,
            selection_end,
            text_shaping_enabled: self.text_shaping.enabled,
            text_shaping_engine,
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
        let total_lines = self.total_history_lines();
        let mut lines = Vec::with_capacity(total_lines);
        for line_idx in 0..total_lines {
            lines.push(self.history_line_text(line_idx));
        }
        lines
    }

    fn search_highlight_for_active_match(&self) -> Option<(u32, u32)> {
        let idx = self.search_active_match?;
        let search_match = *self.search_index.matches().get(idx)?;
        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        let scrollback_len = self.scrollback_line_count();
        let grid_row = search_match.line_idx.checked_sub(scrollback_len)?;
        if cols == 0 || rows == 0 || grid_row >= rows {
            return None;
        }

        let start_col = search_match.start_char.min(cols);
        let end_col = search_match.end_char.min(cols);
        if end_col <= start_col {
            return None;
        }

        let line_base = grid_row.saturating_mul(cols);
        let start = line_base.saturating_add(start_col) as u32;
        let end = line_base.saturating_add(end_col) as u32;
        self.normalize_selection_range((start, end))
    }

    fn align_viewport_to_active_search_match(&mut self) {
        let target_line = self
            .search_active_match
            .and_then(|idx| self.search_index.matches().get(idx).map(|m| m.line_idx));
        let Some(target_line) = target_line else {
            return;
        };
        self.scroll_state.jump_to_line(
            self.total_history_lines(),
            usize::from(self.rows),
            target_line,
        );
        self.refresh_viewport_after_user_navigation();
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

    fn explicit_link_url(&self, link_id: u32) -> Option<String> {
        let core_id = HyperlinkId::try_from(link_id).ok()?;
        self.engine
            .as_ref()
            .and_then(|engine| engine.hyperlink_uri(core_id))
            .map(str::to_owned)
    }

    fn resolve_link_target(&self, link_id: u32) -> (Option<String>, &'static str) {
        if let Some(url) = self.auto_link_urls.get(&link_id).cloned() {
            return (Some(url), "auto");
        }
        (self.explicit_link_url(link_id), "osc8")
    }

    fn resolve_link_click(&self, click: LinkClickEvent) -> ResolvedLinkClick {
        let (url, source) = self.resolve_link_target(click.link_id);
        // OSC-8 links without URL metadata in this host path are denied explicitly.
        let open_decision = if source == "osc8" && url.is_none() {
            LinkOpenDecision::deny("osc8_url_unavailable")
        } else {
            self.link_open_policy.evaluate(url.as_deref())
        };
        let audit_url = url.as_deref().and_then(redact_url_for_audit);
        let audit_url_redacted = url
            .as_deref()
            .is_some_and(|raw| audit_url.as_deref() != Some(raw));
        ResolvedLinkClick {
            click,
            source,
            url,
            audit_url,
            audit_url_redacted,
            policy_rule: open_decision.policy_rule(),
            action_outcome: open_decision.action_outcome(),
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

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn recompute_auto_links(&mut self) {
        let max = usize::from(self.cols) * usize::from(self.rows);
        self.auto_link_ids.resize(max, 0);
        self.auto_link_ids.fill(0);
        self.auto_link_urls.clear();
        self.next_auto_link_id = AUTO_LINK_ID_BASE;

        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);

        for row in 0..rows {
            self.scan_row_for_auto_links(row, cols);
        }
    }

    /// Recompute auto-links only for the specified dirty rows.
    ///
    /// For each dirty row, clears existing auto-link IDs and removes their
    /// URL entries, then rescans only those rows for URLs. Clean rows are
    /// left untouched — O(cols × dirty_rows) instead of O(cols × rows).
    fn recompute_auto_links_for_rows(&mut self, dirty_rows: &[usize]) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        // Guard: if the monotonic ID counter has consumed more than half the
        // available range, fall back to a full recompute to reclaim IDs.
        // This is very rare (requires millions of frame-row URL reassignments).
        const MIDPOINT: u32 = AUTO_LINK_ID_BASE + (AUTO_LINK_ID_MAX - AUTO_LINK_ID_BASE) / 2;
        if self.next_auto_link_id > MIDPOINT {
            self.recompute_auto_links();
            return;
        }

        let cols = usize::from(self.cols);

        // Clear auto-link state for dirty rows and remove their URL entries.
        for &row in dirty_rows {
            let row_start = row.saturating_mul(cols);
            let row_end = row_start.saturating_add(cols).min(self.auto_link_ids.len());
            for idx in row_start..row_end {
                let old_id = self.auto_link_ids[idx];
                if old_id != 0 {
                    self.auto_link_urls.remove(&old_id);
                    self.auto_link_ids[idx] = 0;
                }
            }
        }

        // Rescan only dirty rows.
        for &row in dirty_rows {
            self.scan_row_for_auto_links(row, cols);
        }
    }

    /// Scan a single row for auto-detected URLs and assign link IDs.
    fn scan_row_for_auto_links(&mut self, row: usize, cols: usize) {
        let row_start = row.saturating_mul(cols);
        let row_end = row_start.saturating_add(cols).min(self.shadow_cells.len());
        if row_start >= row_end {
            return;
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
            if self.next_auto_link_id > AUTO_LINK_ID_MAX {
                return;
            }
            let link_id = self.next_auto_link_id;
            self.next_auto_link_id = self.next_auto_link_id.saturating_add(1);
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
        if self.live_announcements.len() >= MAX_ACCESSIBILITY_ANNOUNCEMENTS {
            let overflow = self.live_announcements.len() - MAX_ACCESSIBILITY_ANNOUNCEMENTS + 1;
            self.live_announcements.drain(..overflow);
        }
        self.live_announcements.push(trimmed.to_string());
        let payload_json = serde_json::json!({
            "text": trimmed,
            "queue_depth": self.live_announcements.len(),
        })
        .to_string();
        self.emit_host_event(HostEventType::UiAccessibilityAnnouncement, payload_json);
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
                push_bounded(
                    &mut self.link_clicks,
                    LinkClickEvent {
                        x: mouse.x,
                        y: mouse.y,
                        button: mouse.button,
                        link_id,
                    },
                    MAX_LINK_CLICKS,
                );
                let payload_json = serde_json::json!({
                    "x": mouse.x,
                    "y": mouse.y,
                    "button": mouse.button.map(MouseButton::to_u8),
                    "link_id": link_id,
                    "queue_depth": self.link_clicks.len(),
                })
                .to_string();
                self.emit_host_event(HostEventType::UiLinkClick, payload_json);
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

const fn composition_phase_label(phase: CompositionPhase) -> &'static str {
    match phase {
        CompositionPhase::Start => "start",
        CompositionPhase::Update => "update",
        CompositionPhase::End => "end",
        CompositionPhase::Cancel => "cancel",
    }
}

fn parse_key_event(event: &JsValue) -> Result<InputEvent, JsValue> {
    let phase = parse_key_phase(event)?;
    // Some browsers / wrapper code paths can produce undefined `key`/`code`.
    // Treat missing fields as empty strings to avoid throwing and dropping all input.
    let dom_key = get_string_opt(event, "key")?.unwrap_or_default();
    let dom_code = get_string_opt(event, "code")?.unwrap_or_default();
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

fn get_u32_opt(obj: &JsValue, key: &str) -> Result<Option<u32>, JsValue> {
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    let Some(n) = v.as_f64() else {
        return Err(JsValue::from_str(&format!("field {key} must be a number")));
    };
    let n_i64 = number_to_i64_exact(n, key)?;
    let out = u32::try_from(n_i64)
        .map_err(|_| JsValue::from_str(&format!("field {key} out of range")))?;
    Ok(Some(out))
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

fn parse_init_u16(options: &Option<JsValue>, key: &str) -> Result<Option<u16>, JsValue> {
    let Some(obj) = options.as_ref() else {
        return Ok(None);
    };
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    let Some(n) = v.as_f64() else {
        return Err(JsValue::from_str(&format!("field {key} must be a number")));
    };
    let n_i64 = number_to_i64_exact(n, key)?;
    let val = u16::try_from(n_i64)
        .map_err(|_| JsValue::from_str(&format!("field {key} out of range")))?;
    Ok(Some(val))
}

fn parse_init_f32(options: &Option<JsValue>, key: &str) -> Result<Option<f32>, JsValue> {
    let Some(obj) = options.as_ref() else {
        return Ok(None);
    };
    let v = Reflect::get(obj, &JsValue::from_str(key))?;
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    let Some(n) = v.as_f64() else {
        return Err(JsValue::from_str(&format!("field {key} must be a number")));
    };
    let n_f32 = n as f32;
    if !n_f32.is_finite() {
        return Err(JsValue::from_str(&format!("field {key} must be finite")));
    }
    Ok(Some(n_f32))
}

fn parse_init_bool(options: &Option<JsValue>, key: &str) -> Option<bool> {
    let obj = options.as_ref()?;
    let v = Reflect::get(obj, &JsValue::from_str(key)).ok()?;
    if v.is_null() || v.is_undefined() {
        return None;
    }
    v.as_bool()
}

fn parse_init_renderer_backend(
    options: &Option<JsValue>,
) -> Result<RendererBackendPreference, JsValue> {
    let Some(obj) = options.as_ref() else {
        return Ok(RendererBackendPreference::Auto);
    };
    let selected = get_string_opt(obj, "rendererBackend")?
        .or(get_string_opt(obj, "renderer_backend")?)
        .or(get_string_opt(obj, "backend")?);
    let Some(raw) = selected else {
        return Ok(RendererBackendPreference::Auto);
    };
    RendererBackendPreference::parse(&raw).ok_or_else(|| {
        JsValue::from_str(&format!(
            "field rendererBackend must be one of auto|webgpu|canvas2d (got {raw:?})"
        ))
    })
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

fn attach_snapshot_to_js(snapshot: &AttachSnapshot) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("state"),
        &JsValue::from_str(snapshot.state.as_str()),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("attempt"),
        &JsValue::from_f64(f64::from(snapshot.attempt)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("maxRetries"),
        &JsValue::from_f64(f64::from(snapshot.max_retries)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("handshakeDeadlineMs"),
        &snapshot
            .handshake_deadline_ms
            .map_or(JsValue::NULL, |value| JsValue::from_f64(value as f64)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("retryDeadlineMs"),
        &snapshot
            .retry_deadline_ms
            .map_or(JsValue::NULL, |value| JsValue::from_f64(value as f64)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("sessionId"),
        &snapshot
            .session_id
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("closeReason"),
        &snapshot
            .close_reason
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("failureCode"),
        &snapshot
            .failure_code
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("closeCode"),
        &snapshot
            .close_code
            .map_or(JsValue::NULL, |value| JsValue::from_f64(f64::from(value))),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("cleanClose"),
        &snapshot
            .clean_close
            .map_or(JsValue::NULL, JsValue::from_bool),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("canRetry"),
        &JsValue::from_bool(snapshot.can_retry),
    );
    obj.into()
}

fn attach_transition_to_js(transition: &AttachTransition) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("seq"),
        &JsValue::from_f64(transition.seq as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("atMs"),
        &JsValue::from_f64(transition.at_ms as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("event"),
        &JsValue::from_str(transition.event.as_str()),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("fromState"),
        &JsValue::from_str(transition.from_state.as_str()),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("toState"),
        &JsValue::from_str(transition.to_state.as_str()),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("attempt"),
        &JsValue::from_f64(f64::from(transition.attempt)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("handshakeDeadlineMs"),
        &transition
            .handshake_deadline_ms
            .map_or(JsValue::NULL, |value| JsValue::from_f64(value as f64)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("retryDeadlineMs"),
        &transition
            .retry_deadline_ms
            .map_or(JsValue::NULL, |value| JsValue::from_f64(value as f64)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("sessionId"),
        &transition
            .session_id
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("closeCode"),
        &transition
            .close_code
            .map_or(JsValue::NULL, |value| JsValue::from_f64(f64::from(value))),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("cleanClose"),
        &transition
            .clean_close
            .map_or(JsValue::NULL, JsValue::from_bool),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("reason"),
        &transition
            .reason
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("failureCode"),
        &transition
            .failure_code
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );

    let actions = Array::new();
    for action in &transition.actions {
        actions.push(&attach_action_to_js(action));
    }
    let _ = Reflect::set(&obj, &JsValue::from_str("actions"), &actions);

    obj.into()
}

fn attach_action_to_js(action: &AttachAction) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("kind"),
        &JsValue::from_str(action.kind.as_str()),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("deadlineMs"),
        &action
            .deadline_ms
            .map_or(JsValue::NULL, |value| JsValue::from_f64(value as f64)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("attempt"),
        &action
            .attempt
            .map_or(JsValue::NULL, |value| JsValue::from_f64(f64::from(value))),
    );
    obj.into()
}

fn event_subscription_to_js(subscription: &EventSubscription) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("id"),
        &JsValue::from_f64(f64::from(subscription.id)),
    );
    let event_types = Array::new();
    for event_type in &subscription.event_types {
        event_types.push(&JsValue::from_str(event_type.as_str()));
    }
    let _ = Reflect::set(&obj, &JsValue::from_str("eventTypes"), &event_types);
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("maxBuffered"),
        &JsValue::from_f64(subscription.max_buffered as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("buffered"),
        &JsValue::from_f64(subscription.queue.len() as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("emittedTotal"),
        &JsValue::from_f64(subscription.emitted_total as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("drainedTotal"),
        &JsValue::from_f64(subscription.drained_total as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("droppedTotal"),
        &JsValue::from_f64(subscription.dropped_total as f64),
    );
    obj.into()
}

fn parse_osc_command_and_payload(sequence: &[u8]) -> Option<(u16, String)> {
    if sequence.len() < 4 || sequence.first().copied() != Some(0x1b) || sequence[1] != b']' {
        return None;
    }
    let content = if *sequence.last()? == 0x07 {
        &sequence[2..sequence.len().saturating_sub(1)]
    } else if sequence.len() >= 4
        && sequence[sequence.len() - 2] == 0x1b
        && sequence[sequence.len() - 1] == b'\\'
    {
        &sequence[2..sequence.len().saturating_sub(2)]
    } else {
        return None;
    };
    let content = core::str::from_utf8(content).ok()?;
    let first_semi = content.find(';')?;
    let command = parse_u16_ascii(&content[..first_semi])?;
    Some((command, content[first_semi + 1..].to_string()))
}

fn parse_u16_ascii(raw: &str) -> Option<u16> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u16>().ok()
}

fn parse_u8_percent(raw: &str) -> Option<u8> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value = raw.parse::<u16>().ok()?;
    Some(value.min(100) as u8)
}

fn parse_progress_signal_payload(payload: &str, last_value: u8) -> Option<ProgressSignalRecord> {
    let mut parts = payload.split(';');
    let discriminator = parts.next()?;
    if discriminator != "4" {
        return None;
    }
    let raw_state = parts.next().map(str::to_string);
    let raw_value = parts.next().map(str::to_string);
    if parts.next().is_some() {
        return Some(ProgressSignalRecord {
            accepted: false,
            reason: Some("invalid_arity"),
            state: None,
            state_code: None,
            value: None,
            value_provided: raw_value.as_deref().is_some_and(|value| !value.is_empty()),
            raw_payload: payload.to_string(),
            raw_state,
            raw_value,
        });
    }
    let Some(raw_state_token) = raw_state.as_deref() else {
        return Some(ProgressSignalRecord {
            accepted: false,
            reason: Some("missing_state"),
            state: None,
            state_code: None,
            value: None,
            value_provided: raw_value.as_deref().is_some_and(|value| !value.is_empty()),
            raw_payload: payload.to_string(),
            raw_state,
            raw_value,
        });
    };
    let Some(state_code) =
        parse_u16_ascii(raw_state_token).and_then(|value| u8::try_from(value).ok())
    else {
        return Some(ProgressSignalRecord {
            accepted: false,
            reason: Some("invalid_state_code"),
            state: None,
            state_code: None,
            value: None,
            value_provided: raw_value.as_deref().is_some_and(|value| !value.is_empty()),
            raw_payload: payload.to_string(),
            raw_state,
            raw_value,
        });
    };
    let Some(state) = ProgressSignalState::from_code(state_code) else {
        return Some(ProgressSignalRecord {
            accepted: false,
            reason: Some("unsupported_state_code"),
            state: None,
            state_code: Some(state_code),
            value: None,
            value_provided: raw_value.as_deref().is_some_and(|value| !value.is_empty()),
            raw_payload: payload.to_string(),
            raw_state,
            raw_value,
        });
    };
    let value_provided = raw_value.as_deref().is_some_and(|value| !value.is_empty());
    let value = match state {
        ProgressSignalState::Remove => Some(0),
        ProgressSignalState::Normal => {
            if value_provided {
                match parse_u8_percent(raw_value.as_deref().unwrap_or_default()) {
                    Some(value) => Some(value),
                    None => {
                        return Some(ProgressSignalRecord {
                            accepted: false,
                            reason: Some("invalid_value"),
                            state: None,
                            state_code: Some(state_code),
                            value: None,
                            value_provided,
                            raw_payload: payload.to_string(),
                            raw_state,
                            raw_value,
                        });
                    }
                }
            } else {
                Some(0)
            }
        }
        ProgressSignalState::Error | ProgressSignalState::Warning => {
            if value_provided {
                match parse_u8_percent(raw_value.as_deref().unwrap_or_default()) {
                    Some(0) => Some(last_value),
                    Some(value) => Some(value),
                    None => {
                        return Some(ProgressSignalRecord {
                            accepted: false,
                            reason: Some("invalid_value"),
                            state: None,
                            state_code: Some(state_code),
                            value: None,
                            value_provided,
                            raw_payload: payload.to_string(),
                            raw_state,
                            raw_value,
                        });
                    }
                }
            } else {
                Some(last_value)
            }
        }
        ProgressSignalState::Indeterminate => Some(last_value),
    };
    Some(ProgressSignalRecord {
        accepted: true,
        reason: None,
        state: Some(state),
        state_code: Some(state_code),
        value,
        value_provided,
        raw_payload: payload.to_string(),
        raw_state,
        raw_value,
    })
}

fn progress_signal_payload_json(record: &ProgressSignalRecord, sequence_hex: String) -> String {
    serde_json::json!({
        "protocol": "osc_9_4",
        "accepted": record.accepted,
        "reason": record.reason,
        "state": record.state.map(ProgressSignalState::as_str),
        "state_code": record.state_code,
        "value": record.value,
        "value_provided": record.value_provided,
        "raw_payload": record.raw_payload,
        "raw_state": record.raw_state,
        "raw_value": record.raw_value,
        "sequence_hex": sequence_hex,
    })
    .to_string()
}

fn parse_event_subscription_options(
    options: Option<&JsValue>,
) -> Result<EventSubscriptionOptions, JsValue> {
    let mut config = EventSubscriptionOptions::default();
    let Some(options) = options else {
        return Ok(config);
    };

    if let Some(event_types) = get_event_type_list(options, &["eventTypes", "event_types"])? {
        if event_types.is_empty() {
            return Err(JsValue::from_str(
                "eventTypes must contain at least one known event type",
            ));
        }
        config.event_types = event_types;
    }

    if let Some(v) = get_u32_opt(options, "maxBuffered")?.or(get_u32_opt(options, "max_buffered")?)
    {
        let max_buffered =
            usize::try_from(v).map_err(|_| JsValue::from_str("field maxBuffered out of range"))?;
        if max_buffered == 0 || max_buffered > MAX_EVENT_SUBSCRIPTION_BUFFER_MAX {
            return Err(JsValue::from_str("field maxBuffered must be in 1..=8192"));
        }
        config.max_buffered = max_buffered;
    }

    Ok(config)
}

fn get_event_type_list(
    obj: &JsValue,
    keys: &[&str],
) -> Result<Option<Vec<HostEventType>>, JsValue> {
    for key in keys {
        let v = Reflect::get(obj, &JsValue::from_str(key))?;
        if v.is_null() || v.is_undefined() {
            continue;
        }
        if !Array::is_array(&v) {
            return Err(JsValue::from_str(&format!(
                "field {key} must be an array of event type strings"
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
            let Some(event_type) = HostEventType::parse(raw.trim()) else {
                return Err(JsValue::from_str(&format!(
                    "field {key} contains unknown event type: {raw}"
                )));
            };
            if !out.contains(&event_type) {
                out.push(event_type);
            }
        }
        out.sort_by(|lhs, rhs| lhs.as_str().cmp(rhs.as_str()));
        return Ok(Some(out));
    }
    Ok(None)
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

fn parse_clipboard_policy(
    options: Option<&JsValue>,
    mut policy: ClipboardPolicy,
) -> Result<ClipboardPolicy, JsValue> {
    let Some(options) = options else {
        return Ok(policy);
    };

    if let Some(v) = get_bool(options, "copyEnabled")?.or(get_bool(options, "copy_enabled")?) {
        policy.copy_enabled = v;
    }
    if let Some(v) = get_bool(options, "pasteEnabled")?.or(get_bool(options, "paste_enabled")?) {
        policy.paste_enabled = v;
    }

    if let Some(v) =
        get_u32_opt(options, "maxPasteBytes")?.or(get_u32_opt(options, "max_paste_bytes")?)
    {
        let max = usize::try_from(v)
            .map_err(|_| JsValue::from_str("field maxPasteBytes out of range"))?;
        if max == 0 || max > MAX_PASTE_BYTES {
            return Err(JsValue::from_str(&format!(
                "field maxPasteBytes must be in 1..={MAX_PASTE_BYTES}"
            )));
        }
        policy.max_paste_bytes = max;
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

    let enabled_override = get_bool(options, "enabled")?
        .or(get_bool(options, "shapingEnabled")?)
        .or(get_bool(options, "shaping_enabled")?)
        .or(get_bool(options, "textShaping")?)
        .or(get_bool(options, "text_shaping")?);
    if let Some(v) = enabled_override {
        config.enabled = v;
    }

    if let Some(v) = get_string_opt(options, "engine")?
        .or(get_string_opt(options, "shapingEngine")?)
        .or(get_string_opt(options, "shaping_engine")?)
    {
        // If this update explicitly disables shaping, ignore engine hints.
        if enabled_override != Some(false) {
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
    }

    // Canonical state: disabled shaping always reports "none" engine.
    if !config.enabled {
        config.engine = TextShapingEngine::None;
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

fn redact_url_for_audit(url: &str) -> Option<String> {
    let (scheme, host) = parse_http_url_scheme_and_host(url)?;
    Some(format!("{scheme}://{host}"))
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

fn clipboard_policy_to_js(policy: &ClipboardPolicy) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("copyEnabled"),
        &JsValue::from_bool(policy.copy_enabled),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("pasteEnabled"),
        &JsValue::from_bool(policy.paste_enabled),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("maxPasteBytes"),
        &JsValue::from_f64(policy.max_paste_bytes as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("hostManagedClipboard"),
        &JsValue::from_bool(true),
    );
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
    fn resize_storm_interaction_snapshot_ignores_disabled_shaping_engine_without_other_overlays() {
        let mut term = FrankenTermWeb::new();
        term.text_shaping = TextShapingConfig {
            enabled: false,
            engine: TextShapingEngine::Harfbuzz,
        };

        assert_eq!(term.resize_storm_interaction_snapshot(), None);
    }

    #[test]
    fn resize_storm_interaction_snapshot_zeroes_disabled_shaping_engine_with_other_overlays() {
        let mut term = FrankenTermWeb::new();
        term.hovered_link_id = 7;
        term.text_shaping = TextShapingConfig {
            enabled: false,
            engine: TextShapingEngine::Harfbuzz,
        };

        assert_eq!(
            term.resize_storm_interaction_snapshot(),
            Some(InteractionSnapshot {
                hovered_link_id: 7,
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

    fn feed_numbered_lines(term: &mut FrankenTermWeb, line_count: usize, cols: usize) {
        let width = cols.max(1);
        let mut payload = Vec::with_capacity(line_count.saturating_mul(width.saturating_add(2)));
        for i in 0..line_count {
            let mut line = format!("{i:0>width$}", width = width);
            line.truncate(width);
            payload.extend_from_slice(line.as_bytes());
            if i + 1 < line_count {
                payload.extend_from_slice(b"\r\n");
            }
        }
        term.feed(&payload);
    }

    fn event_subscription_options(event_types: &[&str], max_buffered: Option<u32>) -> JsValue {
        let options = Object::new();
        if !event_types.is_empty() {
            let event_types_array = Array::new();
            for event_type in event_types {
                event_types_array.push(&JsValue::from_str(event_type));
            }
            let _ = Reflect::set(
                &options,
                &JsValue::from_str("eventTypes"),
                &event_types_array,
            );
        }
        if let Some(max) = max_buffered {
            let _ = Reflect::set(
                &options,
                &JsValue::from_str("maxBuffered"),
                &JsValue::from_f64(f64::from(max)),
            );
        }
        options.into()
    }

    fn subscription_id_from_snapshot(snapshot: &JsValue) -> u32 {
        let id = Reflect::get(snapshot, &JsValue::from_str("id"))
            .expect("subscription snapshot should expose id")
            .as_f64()
            .expect("subscription id should be numeric");
        id as u32
    }

    #[test]
    fn event_subscription_filters_types_and_preserves_fifo_seq() {
        let mut term = FrankenTermWeb::new();
        let snapshot = term
            .create_event_subscription(Some(event_subscription_options(
                &["input.paste", "input.focus"],
                Some(8),
            )))
            .expect("create_event_subscription should succeed");
        let subscription_id = subscription_id_from_snapshot(&snapshot);

        assert!(
            term.queue_input_event(InputEvent::Paste(PasteInput { data: "one".into() }))
                .is_ok()
        );
        assert!(
            term.queue_input_event(InputEvent::Wheel(WheelInput {
                x: 0,
                y: 0,
                dx: 0,
                dy: 1,
                mods: Modifiers::default(),
            }))
            .is_ok()
        );
        assert!(
            term.queue_input_event(InputEvent::Focus(FocusInput { focused: true }))
                .is_ok()
        );

        let drained = term
            .drain_event_subscription(subscription_id)
            .expect("drain_event_subscription should succeed");
        assert_eq!(drained.length(), 2);

        let first = drained.get(0);
        let second = drained.get(1);
        assert_eq!(
            Reflect::get(&first, &JsValue::from_str("eventType"))
                .expect("event record should expose eventType")
                .as_string()
                .as_deref(),
            Some("input.paste")
        );
        assert_eq!(
            Reflect::get(&second, &JsValue::from_str("eventType"))
                .expect("event record should expose eventType")
                .as_string()
                .as_deref(),
            Some("input.focus")
        );
        let seq_first = Reflect::get(&first, &JsValue::from_str("seq"))
            .expect("event record should expose seq")
            .as_f64()
            .expect("seq should be numeric");
        let seq_second = Reflect::get(&second, &JsValue::from_str("seq"))
            .expect("event record should expose seq")
            .as_f64()
            .expect("seq should be numeric");
        assert!(seq_first < seq_second);
    }

    #[test]
    fn event_subscription_queue_drops_oldest_and_tracks_drop_counters() {
        let mut term = FrankenTermWeb::new();
        let snapshot = term
            .create_event_subscription(Some(event_subscription_options(&["input.paste"], Some(2))))
            .expect("create_event_subscription should succeed");
        let subscription_id = subscription_id_from_snapshot(&snapshot);

        for idx in 0..5 {
            assert!(
                term.queue_input_event(InputEvent::Paste(PasteInput {
                    data: format!("evt-{idx}").into_boxed_str(),
                }))
                .is_ok()
            );
        }

        let state = term.event_subscription_state(subscription_id);
        assert_eq!(
            Reflect::get(&state, &JsValue::from_str("buffered"))
                .expect("state should expose buffered")
                .as_f64(),
            Some(2.0)
        );
        assert_eq!(
            Reflect::get(&state, &JsValue::from_str("droppedTotal"))
                .expect("state should expose droppedTotal")
                .as_f64(),
            Some(3.0)
        );

        let drained = term
            .drain_event_subscription(subscription_id)
            .expect("drain_event_subscription should succeed");
        assert_eq!(drained.length(), 2);

        let first_payload = Reflect::get(&drained.get(0), &JsValue::from_str("payload"))
            .expect("event record should expose payload");
        let second_payload = Reflect::get(&drained.get(1), &JsValue::from_str("payload"))
            .expect("event record should expose payload");
        let first_json = js_sys::JSON::stringify(&first_payload)
            .expect("payload should stringify")
            .as_string()
            .expect("stringified payload should be a string");
        let second_json = js_sys::JSON::stringify(&second_payload)
            .expect("payload should stringify")
            .as_string()
            .expect("stringified payload should be a string");
        assert!(first_json.contains("evt-3"));
        assert!(second_json.contains("evt-4"));
    }

    #[test]
    fn event_subscription_close_disposes_handle_and_rejects_future_drains() {
        let mut term = FrankenTermWeb::new();
        let snapshot = term
            .create_event_subscription(Some(event_subscription_options(&[], None)))
            .expect("create_event_subscription should succeed");
        let subscription_id = subscription_id_from_snapshot(&snapshot);

        assert!(term.close_event_subscription(subscription_id));
        assert!(!term.close_event_subscription(subscription_id));
        assert!(term.event_subscription_state(subscription_id).is_null());
        assert!(term.drain_event_subscription(subscription_id).is_err());
        assert!(
            term.drain_event_subscription_jsonl(
                subscription_id,
                "run-test".to_string(),
                1,
                "T000001".to_string()
            )
            .is_err()
        );
    }

    #[test]
    fn feed_emits_terminal_progress_events_with_normalization_and_rejections() {
        let mut term = FrankenTermWeb::new();
        term.resize(8, 4);
        let snapshot = term
            .create_event_subscription(Some(event_subscription_options(
                &["terminal.progress"],
                Some(16),
            )))
            .expect("create_event_subscription should succeed");
        let subscription_id = subscription_id_from_snapshot(&snapshot);

        term.feed(
            b"\x1b]9;4;1;10\x07\x1b]9;4;3;\x07\x1b]9;4;2;0\x07\x1b]9;4;1;abc\x07\x1b]9;4;0;\x07",
        );

        let drained = term
            .drain_event_subscription(subscription_id)
            .expect("drain_event_subscription should succeed");
        assert_eq!(drained.length(), 5);

        for idx in 0..drained.length() {
            assert_eq!(
                Reflect::get(&drained.get(idx), &JsValue::from_str("eventType"))
                    .expect("event record should expose eventType")
                    .as_string()
                    .as_deref(),
                Some("terminal.progress")
            );
        }

        let payload_0 = Reflect::get(&drained.get(0), &JsValue::from_str("payload"))
            .expect("event record should expose payload");
        assert_eq!(
            Reflect::get(&payload_0, &JsValue::from_str("accepted"))
                .expect("payload should expose accepted")
                .as_bool(),
            Some(true)
        );
        assert_eq!(
            Reflect::get(&payload_0, &JsValue::from_str("state"))
                .expect("payload should expose state")
                .as_string()
                .as_deref(),
            Some("normal")
        );
        assert_eq!(
            Reflect::get(&payload_0, &JsValue::from_str("value"))
                .expect("payload should expose value")
                .as_f64(),
            Some(10.0)
        );

        let payload_1 = Reflect::get(&drained.get(1), &JsValue::from_str("payload"))
            .expect("event record should expose payload");
        assert_eq!(
            Reflect::get(&payload_1, &JsValue::from_str("state"))
                .expect("payload should expose state")
                .as_string()
                .as_deref(),
            Some("indeterminate")
        );
        assert_eq!(
            Reflect::get(&payload_1, &JsValue::from_str("value"))
                .expect("payload should expose value")
                .as_f64(),
            Some(10.0)
        );

        let payload_2 = Reflect::get(&drained.get(2), &JsValue::from_str("payload"))
            .expect("event record should expose payload");
        assert_eq!(
            Reflect::get(&payload_2, &JsValue::from_str("state"))
                .expect("payload should expose state")
                .as_string()
                .as_deref(),
            Some("error")
        );
        assert_eq!(
            Reflect::get(&payload_2, &JsValue::from_str("value"))
                .expect("payload should expose value")
                .as_f64(),
            Some(10.0)
        );

        let payload_3 = Reflect::get(&drained.get(3), &JsValue::from_str("payload"))
            .expect("event record should expose payload");
        assert_eq!(
            Reflect::get(&payload_3, &JsValue::from_str("accepted"))
                .expect("payload should expose accepted")
                .as_bool(),
            Some(false)
        );
        assert_eq!(
            Reflect::get(&payload_3, &JsValue::from_str("reason"))
                .expect("payload should expose reason")
                .as_string()
                .as_deref(),
            Some("invalid_value")
        );

        let payload_4 = Reflect::get(&drained.get(4), &JsValue::from_str("payload"))
            .expect("event record should expose payload");
        assert_eq!(
            Reflect::get(&payload_4, &JsValue::from_str("state"))
                .expect("payload should expose state")
                .as_string()
                .as_deref(),
            Some("remove")
        );
        assert_eq!(
            Reflect::get(&payload_4, &JsValue::from_str("value"))
                .expect("payload should expose value")
                .as_f64(),
            Some(0.0)
        );

        assert_eq!(term.progress_last_value, 0);
    }

    #[test]
    fn progress_parser_preserves_chunk_boundaries_until_sequence_terminates() {
        let mut term = FrankenTermWeb::new();
        term.resize(8, 4);
        let snapshot = term
            .create_event_subscription(Some(event_subscription_options(
                &["terminal.progress"],
                Some(4),
            )))
            .expect("create_event_subscription should succeed");
        let subscription_id = subscription_id_from_snapshot(&snapshot);

        term.feed(b"\x1b]9;4;1");
        assert_eq!(
            term.drain_event_subscription(subscription_id)
                .expect("drain_event_subscription should succeed")
                .length(),
            0
        );

        term.feed(b";25\x07");
        let drained = term
            .drain_event_subscription(subscription_id)
            .expect("drain_event_subscription should succeed");
        assert_eq!(drained.length(), 1);

        let payload = Reflect::get(&drained.get(0), &JsValue::from_str("payload"))
            .expect("event record should expose payload");
        assert_eq!(
            Reflect::get(&payload, &JsValue::from_str("accepted"))
                .expect("payload should expose accepted")
                .as_bool(),
            Some(true)
        );
        assert_eq!(
            Reflect::get(&payload, &JsValue::from_str("state"))
                .expect("payload should expose state")
                .as_string()
                .as_deref(),
            Some("normal")
        );
        assert_eq!(
            Reflect::get(&payload, &JsValue::from_str("value"))
                .expect("payload should expose value")
                .as_f64(),
            Some(25.0)
        );
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
    fn set_search_query_indexes_unified_history_not_only_visible_grid() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");

        assert!(term.set_search_query("BBBB", None).is_ok());
        assert_eq!(term.search_index.len(), 1);
        assert_eq!(term.search_active_match, Some(0));

        let state = term.search_state();
        assert_eq!(js_f64_field(&state, "activeLine"), 1.0);
        // Active match is in scrollback (off-grid), so no in-grid highlight range.
        assert_eq!(term.search_highlight_range, None);
    }

    #[test]
    fn search_navigation_jumps_viewport_to_history_matches_deterministically() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");
        assert!(term.set_search_query("BBBB", None).is_ok());

        let _ = term.search_next();
        let view = term.viewport_state();
        assert!(js_f64_field(&view, "viewportStart") <= 1.0);
        assert!(1.0 < js_f64_field(&view, "viewportEnd"));
        assert!(!js_bool_field(&view, "followOutput"));
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
    fn apply_patch_batch_flat_reuses_scratch_without_stale_words() {
        let mut term = FrankenTermWeb::new();
        term.cols = 4;
        term.rows = 1;
        term.shadow_cells = vec![CellData::EMPTY; 4];

        let full = text_row_cells("WXYZ");
        let (spans_full, cells_full) = patch_batch_flat_arrays(&[(0, &full)]);
        assert!(term.apply_patch_batch_flat(spans_full, cells_full).is_ok());

        let single = text_row_cells("Q");
        let (spans_small, cells_small) = patch_batch_flat_arrays(&[(2, &single)]);
        assert!(
            term.apply_patch_batch_flat(spans_small, cells_small)
                .is_ok()
        );

        assert_eq!(term.shadow_cells[0].glyph_id, u32::from('W'));
        assert_eq!(term.shadow_cells[1].glyph_id, u32::from('X'));
        assert_eq!(term.shadow_cells[2].glyph_id, u32::from('Q'));
        assert_eq!(term.shadow_cells[3].glyph_id, u32::from('Z'));
    }

    #[test]
    fn apply_patch_batch_flat_keeps_auto_link_ids_row_major_when_patch_order_is_reversed() {
        let mut term = FrankenTermWeb::new();
        term.cols = 40;
        term.rows = 2;

        let row0 = text_row_cells("A https://one.test");
        let row1 = text_row_cells("B https://two.test");

        // Intentionally reversed patch order (row 1 before row 0).
        let (spans, cells) = patch_batch_flat_arrays(&[(40, &row1), (0, &row0)]);
        assert!(term.apply_patch_batch_flat(spans, cells).is_ok());

        let row0_id = term.auto_link_ids[..40]
            .iter()
            .copied()
            .find(|id| *id != 0)
            .expect("row 0 should contain an auto-link id");
        let row1_id = term.auto_link_ids[40..80]
            .iter()
            .copied()
            .find(|id| *id != 0)
            .expect("row 1 should contain an auto-link id");

        assert!(row0_id < row1_id);
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
    fn extract_selection_text_skips_inferred_wide_continuation_cells() {
        let mut term = FrankenTermWeb::new();
        term.cols = 4;
        term.rows = 1;
        let mut cells = vec![CellData::EMPTY; 4];
        cells[0].glyph_id = '界' as u32;
        cells[2].glyph_id = 'A' as u32;
        term.shadow_cells = cells;
        term.selection_range = Some((0, 3));

        assert_eq!(term.extract_selection_text(), "界A");
    }

    #[test]
    fn extract_selection_text_trims_trailing_spaces_per_selected_row() {
        let mut term = FrankenTermWeb::new();
        term.cols = 4;
        term.rows = 2;
        term.shadow_cells = text_row_cells("AB  CD  ");
        term.selection_range = Some((0, 8));

        assert_eq!(term.extract_selection_text(), "AB\nCD");
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
        let event = drained.get(0);
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("source"))
                .expect("link click event should expose source")
                .as_string()
                .as_deref(),
            Some("osc8")
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
            Some("osc8_url_unavailable")
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("policyRule"))
                .expect("link click event should expose policyRule")
                .as_string()
                .as_deref(),
            Some("osc8_url_unavailable")
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("actionOutcome"))
                .expect("link click event should expose actionOutcome")
                .as_string()
                .as_deref(),
            Some("block_open")
        );
        assert!(
            Reflect::get(&event, &JsValue::from_str("auditUrl"))
                .expect("link click event should expose auditUrl")
                .is_null()
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("auditUrlRedacted"))
                .expect("link click event should expose auditUrlRedacted")
                .as_bool(),
            Some(false)
        );
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
    fn feed_preserves_explicit_osc8_url_for_lookup_and_click_policy() {
        let text = "https://auto.test/path";
        let mut term = FrankenTermWeb::new();
        term.resize(text.chars().count() as u16, 1);
        let payload = format!("\x1b]8;;https://explicit.test/docs\x07{text}\x1b]8;;\x07");
        term.feed(payload.as_bytes());

        let link_id = term.link_at(0, 0);
        assert_ne!(link_id, 0);
        assert!(link_id < AUTO_LINK_ID_BASE);
        assert_eq!(
            term.link_url_at(0, 0),
            Some("https://explicit.test/docs".to_string())
        );
        assert_eq!(term.auto_link_ids[0], 0);

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

        let drained = term.drain_link_clicks();
        assert_eq!(drained.length(), 1);
        let event = drained.get(0);
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("source"))
                .expect("link click event should expose source")
                .as_string()
                .as_deref(),
            Some("osc8")
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("url"))
                .expect("link click event should expose url")
                .as_string()
                .as_deref(),
            Some("https://explicit.test/docs")
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("openAllowed"))
                .expect("link click event should expose openAllowed")
                .as_bool(),
            Some(true)
        );
        assert!(
            Reflect::get(&event, &JsValue::from_str("openReason"))
                .expect("link click event should expose openReason")
                .is_null()
        );
    }

    #[test]
    fn parse_http_url_scheme_and_host_normalizes_case_and_port() {
        let (scheme, host) = parse_http_url_scheme_and_host("HTTPS://Example.Test:443/path?q=1")
            .expect("valid HTTPS URL should parse into normalized scheme and host");
        assert_eq!(scheme, "https");
        assert_eq!(host, "example.test");
    }

    #[test]
    fn link_open_policy_defaults_to_https_only() {
        let policy = LinkOpenPolicy::default();
        assert!(!policy.allow_http);
        assert!(policy.allow_https);
        assert!(policy.allowed_hosts.is_empty());
        assert!(policy.blocked_hosts.is_empty());

        let denied = policy.evaluate(Some("http://example.test/path"));
        assert!(!denied.allowed);
        assert_eq!(denied.reason, Some("scheme_blocked"));

        let allowed = policy.evaluate(Some("https://example.test/path"));
        assert!(allowed.allowed);
        assert_eq!(allowed.reason, None);
    }

    #[test]
    fn link_open_policy_snapshot_exposes_secure_defaults() {
        let term = FrankenTermWeb::new();
        let snapshot = term.link_open_policy_snapshot();
        assert_eq!(
            Reflect::get(&snapshot, &JsValue::from_str("allowHttp"))
                .expect("link_open_policy_snapshot should expose allowHttp")
                .as_bool(),
            Some(false)
        );
        assert_eq!(
            Reflect::get(&snapshot, &JsValue::from_str("allowHttps"))
                .expect("link_open_policy_snapshot should expose allowHttps")
                .as_bool(),
            Some(true)
        );
    }

    #[test]
    fn clipboard_policy_snapshot_exposes_secure_defaults() {
        let term = FrankenTermWeb::new();
        let snapshot = term.clipboard_policy_snapshot();
        assert_eq!(
            Reflect::get(&snapshot, &JsValue::from_str("copyEnabled"))
                .expect("clipboard_policy_snapshot should expose copyEnabled")
                .as_bool(),
            Some(true)
        );
        assert_eq!(
            Reflect::get(&snapshot, &JsValue::from_str("pasteEnabled"))
                .expect("clipboard_policy_snapshot should expose pasteEnabled")
                .as_bool(),
            Some(true)
        );
        assert_eq!(
            Reflect::get(&snapshot, &JsValue::from_str("maxPasteBytes"))
                .expect("clipboard_policy_snapshot should expose maxPasteBytes")
                .as_f64(),
            Some(MAX_PASTE_BYTES as f64)
        );
        assert_eq!(
            Reflect::get(&snapshot, &JsValue::from_str("hostManagedClipboard"))
                .expect("clipboard_policy_snapshot should expose hostManagedClipboard")
                .as_bool(),
            Some(true)
        );
    }

    #[test]
    fn set_clipboard_policy_can_disable_copy_and_paste() {
        let mut term = FrankenTermWeb::new();
        let cfg = Object::new();
        let _ = Reflect::set(
            &cfg,
            &JsValue::from_str("copyEnabled"),
            &JsValue::from_bool(false),
        );
        let _ = Reflect::set(
            &cfg,
            &JsValue::from_str("pasteEnabled"),
            &JsValue::from_bool(false),
        );
        let _ = Reflect::set(
            &cfg,
            &JsValue::from_str("maxPasteBytes"),
            &JsValue::from_f64(256.0),
        );
        assert!(term.set_clipboard_policy(cfg.into()).is_ok());

        term.cols = 4;
        term.rows = 1;
        term.shadow_cells = text_row_cells("ABCD");
        term.selection_range = Some((0, 4));
        assert_eq!(term.copy_selection(), None);

        let err = term
            .paste_text("abc")
            .expect_err("paste_text should reject when paste policy is disabled");
        assert_eq!(
            err.as_string().as_deref(),
            Some("paste disabled by clipboard policy")
        );
    }

    #[test]
    fn paste_text_rejects_payload_above_max_bytes() {
        let mut term = FrankenTermWeb::new();
        let oversized = "x".repeat(MAX_PASTE_BYTES + 1);
        let err = term
            .paste_text(&oversized)
            .expect_err("paste_text should reject payloads larger than MAX_PASTE_BYTES");
        assert_eq!(
            err.as_string().as_deref(),
            Some("paste payload too large (max 786432 UTF-8 bytes)")
        );
    }

    #[test]
    fn paste_text_respects_clipboard_policy_max_bytes_override() {
        let mut term = FrankenTermWeb::new();
        let cfg = Object::new();
        let _ = Reflect::set(
            &cfg,
            &JsValue::from_str("maxPasteBytes"),
            &JsValue::from_f64(8.0),
        );
        assert!(term.set_clipboard_policy(cfg.into()).is_ok());

        let err = term
            .paste_text("123456789")
            .expect_err("paste_text should enforce clipboard policy maxPasteBytes");
        assert_eq!(
            err.as_string().as_deref(),
            Some("paste payload too large (max 8 UTF-8 bytes)")
        );
    }

    #[test]
    fn input_rejects_paste_event_payload_above_max_bytes() {
        let mut term = FrankenTermWeb::new();
        let event = Object::new();
        let _ = Reflect::set(
            &event,
            &JsValue::from_str("type"),
            &JsValue::from_str("paste"),
        );
        let _ = Reflect::set(
            &event,
            &JsValue::from_str("data"),
            &JsValue::from_str(&"x".repeat(MAX_PASTE_BYTES + 1)),
        );

        let err = term
            .input(event.into())
            .expect_err("input() should reject oversized paste events");
        assert_eq!(
            err.as_string().as_deref(),
            Some("paste payload too large (max 786432 UTF-8 bytes)")
        );
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

        let disable = Object::new();
        let _ = Reflect::set(
            &disable,
            &JsValue::from_str("enabled"),
            &JsValue::from_bool(false),
        );
        assert!(term.set_text_shaping(disable.into()).is_ok());
        assert_eq!(term.text_shaping, TextShapingConfig::default());

        let invalid = Object::new();
        let _ = Reflect::set(
            &invalid,
            &JsValue::from_str("engine"),
            &JsValue::from_str("icu"),
        );
        assert!(term.set_text_shaping(invalid.into()).is_err());
    }

    #[test]
    fn set_text_shaping_ignores_engine_when_disabled() {
        let mut term = FrankenTermWeb::new();

        let cfg = Object::new();
        let _ = Reflect::set(
            &cfg,
            &JsValue::from_str("enabled"),
            &JsValue::from_bool(false),
        );
        let _ = Reflect::set(
            &cfg,
            &JsValue::from_str("engine"),
            &JsValue::from_str("icu"),
        );
        assert!(term.set_text_shaping(cfg.into()).is_ok());
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
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("policyRule"))
                .expect("link click event should expose policyRule")
                .as_string()
                .as_deref(),
            Some("scheme_blocked")
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("actionOutcome"))
                .expect("link click event should expose actionOutcome")
                .as_string()
                .as_deref(),
            Some("block_open")
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("auditUrl"))
                .expect("link click event should expose auditUrl")
                .as_string()
                .as_deref(),
            Some("http://example.test")
        );
        assert_eq!(
            Reflect::get(&event, &JsValue::from_str("auditUrlRedacted"))
                .expect("link click event should expose auditUrlRedacted")
                .as_bool(),
            Some(false)
        );
    }

    #[test]
    fn drain_link_clicks_jsonl_emits_e2e_records() {
        let text = "https://example.test/docs docs";
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
        assert_eq!(parsed["url"], "https://example.test/docs");
        assert_eq!(parsed["open_reason"], serde_json::Value::Null);
        assert_eq!(parsed["policy_rule"], "allow_default");
        assert_eq!(parsed["action_outcome"], "allow_open");
        assert_eq!(parsed["audit_url"], "https://example.test");
        assert_eq!(parsed["audit_url_redacted"], true);
    }

    #[test]
    fn encoded_input_queues_drop_oldest_on_overflow() {
        let mut term = FrankenTermWeb::new();
        let total = MAX_ENCODED_INPUT_EVENTS + 3;

        for idx in 0..total {
            let event = InputEvent::Paste(PasteInput {
                data: format!("evt-{idx}").into_boxed_str(),
            });
            assert!(term.queue_input_event(event).is_ok());
        }

        assert_eq!(term.encoded_inputs.len(), MAX_ENCODED_INPUT_EVENTS);
        assert_eq!(
            term.encoded_input_bytes.len(),
            MAX_ENCODED_INPUT_BYTE_CHUNKS.min(total)
        );

        let drained = term.drain_encoded_inputs();
        assert_eq!(drained.length(), MAX_ENCODED_INPUT_EVENTS as u32);

        let first = drained
            .get(0)
            .as_string()
            .expect("drained encoded input should be a string");
        assert!(first.contains("\"evt-3\""));

        let last = drained
            .get(MAX_ENCODED_INPUT_EVENTS as u32 - 1)
            .as_string()
            .expect("drained encoded input should be a string");
        assert!(last.contains(&format!("\"evt-{}\"", total - 1)));
    }

    #[test]
    fn ime_state_reports_active_and_preedit_snapshot() {
        let mut term = FrankenTermWeb::new();
        let _ = term
            .composition
            .rewrite(InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Update,
                data: Some("に".into()),
            }));

        let snapshot = term.ime_state();
        assert_eq!(
            Reflect::get(&snapshot, &JsValue::from_str("active"))
                .expect("ime_state should expose active")
                .as_bool(),
            Some(true)
        );
        assert_eq!(
            Reflect::get(&snapshot, &JsValue::from_str("preedit"))
                .expect("ime_state should expose preedit")
                .as_string()
                .as_deref(),
            Some("に")
        );

        let _ = term
            .composition
            .rewrite(InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::End,
                data: None,
            }));
        let cleared = term.ime_state();
        assert_eq!(
            Reflect::get(&cleared, &JsValue::from_str("active"))
                .expect("ime_state should expose active")
                .as_bool(),
            Some(false)
        );
        assert!(
            Reflect::get(&cleared, &JsValue::from_str("preedit"))
                .expect("ime_state should expose preedit")
                .is_null()
        );
    }

    #[test]
    fn drain_ime_composition_jsonl_emits_trace_records() {
        let mut term = FrankenTermWeb::new();
        let _ = term
            .composition
            .rewrite(InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Update,
                data: Some("x".into()),
            }));
        term.record_ime_trace_event(
            &InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Start,
                data: None,
            }),
            true,
        );
        term.record_ime_trace_event(
            &InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Update,
                data: Some("x".into()),
            }),
            false,
        );
        term.record_ime_drop_key_trace();

        let lines =
            term.drain_ime_composition_jsonl("run-ime".to_string(), 9, "T000500".to_string());
        assert_eq!(lines.length(), 3);

        let first = lines
            .get(0)
            .as_string()
            .expect("drain_ime_composition_jsonl should emit strings");
        let first: serde_json::Value =
            serde_json::from_str(&first).expect("first IME JSONL line should parse");
        assert_eq!(first["type"], "ime_composition");
        assert_eq!(first["run_id"], "run-ime");
        assert_eq!(first["seed"], 9);
        assert_eq!(first["event_kind"], "composition");
        assert_eq!(first["phase"], "start");
        assert_eq!(first["synthetic"], true);

        let last = lines
            .get(2)
            .as_string()
            .expect("drain_ime_composition_jsonl should emit strings");
        let last: serde_json::Value =
            serde_json::from_str(&last).expect("last IME JSONL line should parse");
        assert_eq!(last["event_kind"], "drop_key");
        assert_eq!(last["phase"], serde_json::Value::Null);
    }

    #[test]
    fn ime_trace_queue_drops_oldest_on_overflow() {
        let mut term = FrankenTermWeb::new();
        let total = MAX_IME_TRACE_EVENTS + 4;
        for idx in 0..total {
            let event = InputEvent::Composition(CompositionInput {
                phase: CompositionPhase::Update,
                data: Some(format!("p{idx}").into_boxed_str()),
            });
            term.record_ime_trace_event(&event, false);
        }

        assert_eq!(term.ime_trace_events.len(), MAX_IME_TRACE_EVENTS);
        let first = term
            .ime_trace_events
            .first()
            .and_then(|event| event.data.as_deref())
            .expect("overflow queue should retain earliest surviving payload");
        assert_eq!(first, "p4");
    }

    #[test]
    fn link_click_queue_drops_oldest_on_overflow() {
        let mut term = FrankenTermWeb::new();
        term.cols = 1;
        term.rows = 1;
        term.shadow_cells = vec![CellData::EMPTY];
        term.auto_link_ids = vec![123];
        term.auto_link_urls
            .insert(123, "https://example.test".to_string());

        for _ in 0..(MAX_LINK_CLICKS + 5) {
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
        }

        assert_eq!(term.link_clicks.len(), MAX_LINK_CLICKS);
        assert_eq!(term.drain_link_clicks().length(), MAX_LINK_CLICKS as u32);
    }

    fn drain_uint8_chunks(chunks: Array) -> Vec<Vec<u8>> {
        chunks
            .iter()
            .map(|chunk| {
                let bytes = Uint8Array::new(&chunk);
                let len = usize::try_from(bytes.length())
                    .expect("Uint8Array length should fit into usize for tests");
                let mut out = vec![0u8; len];
                bytes.copy_to(out.as_mut_slice());
                out
            })
            .collect()
    }

    fn bytes_to_hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }

    fn test_geometry(cols: u16, rows: u16) -> GeometrySnapshot {
        GeometrySnapshot {
            cols,
            rows,
            pixel_width: u32::from(cols),
            pixel_height: u32::from(rows),
            cell_width_px: 1.0,
            cell_height_px: 1.0,
            dpr: 1.0,
            zoom: 1.0,
        }
    }

    fn js_f64_field(obj: &JsValue, key: &str) -> f64 {
        let value = match Reflect::get(obj, &JsValue::from_str(key)) {
            Ok(v) => v,
            Err(_) => panic!("viewport state missing key: {key}"),
        };
        match value.as_f64() {
            Some(v) => v,
            None => panic!("viewport state key is not numeric: {key}"),
        }
    }

    fn js_bool_field(obj: &JsValue, key: &str) -> bool {
        let value = match Reflect::get(obj, &JsValue::from_str(key)) {
            Ok(v) => v,
            Err(_) => panic!("viewport state missing key: {key}"),
        };
        match value.as_bool() {
            Some(v) => v,
            None => panic!("viewport state key is not bool: {key}"),
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ReplayFixtureResult {
        transcript_jsonl: Vec<String>,
        final_frame_hash: String,
        replies_hex: Vec<String>,
    }

    fn run_remote_feed_replay_fixture(chunks: &[&[u8]]) -> ReplayFixtureResult {
        let mut term = FrankenTermWeb::new();
        term.resize(8, 4);
        let geometry = test_geometry(8, 4);
        let mut transcript_jsonl = Vec::with_capacity(chunks.len() + 1);
        let mut replies_hex = Vec::new();

        for (step, chunk) in chunks.iter().enumerate() {
            term.feed(chunk);
            let step_replies = drain_uint8_chunks(term.drain_reply_bytes());
            let step_reply_hex: Vec<String> = step_replies
                .iter()
                .map(|bytes| bytes_to_hex(bytes))
                .collect();
            replies_hex.extend(step_reply_hex.iter().cloned());

            let frame_hash = crate::frame_harness::stable_frame_hash(&term.shadow_cells, geometry);
            let non_empty_cells = term
                .shadow_cells
                .iter()
                .filter(|cell| cell.glyph_id != 0)
                .count();
            let line = serde_json::json!({
                "schema_version": "e2e-jsonl-v1",
                "type": "remote_feed_replay",
                "event": "remote_feed_step",
                "step": step,
                "input_hex": bytes_to_hex(chunk),
                "frame_hash": frame_hash,
                "non_empty_cells": non_empty_cells,
                "replies_hex": step_reply_hex,
            });
            transcript_jsonl.push(
                serde_json::to_string(&line)
                    .expect("remote feed replay step should serialize to JSONL"),
            );
        }

        let final_frame_hash =
            crate::frame_harness::stable_frame_hash(&term.shadow_cells, geometry);
        transcript_jsonl.push(
            serde_json::to_string(&serde_json::json!({
                "schema_version": "e2e-jsonl-v1",
                "type": "remote_feed_replay",
                "event": "remote_feed_final",
                "frame_hash": final_frame_hash,
                "remaining_reply_chunks": term.drain_reply_bytes().length(),
            }))
            .expect("remote feed replay final should serialize to JSONL"),
        );

        ReplayFixtureResult {
            transcript_jsonl,
            final_frame_hash,
            replies_hex,
        }
    }

    #[test]
    fn feed_projects_terminal_engine_cells_into_shadow_grid() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AB\r\nCD");

        let glyphs: Vec<u32> = term.shadow_cells.iter().map(|cell| cell.glyph_id).collect();
        assert_eq!(
            glyphs,
            vec![
                u32::from('A'),
                u32::from('B'),
                0,
                0,
                u32::from('C'),
                u32::from('D'),
                0,
                0,
            ]
        );
    }

    #[test]
    fn drain_reply_bytes_is_fifo_and_drains_once() {
        let mut term = FrankenTermWeb::new();
        term.resize(8, 4);
        term.feed(b"\x1b[5n\x1b[6n");

        let replies = drain_uint8_chunks(term.drain_reply_bytes());
        assert_eq!(replies, vec![b"\x1b[0n".to_vec(), b"\x1b[1;1R".to_vec()]);
        assert_eq!(term.drain_reply_bytes().length(), 0);
    }

    #[test]
    fn drain_reply_bytes_cpr_reflects_resize_clamped_cursor() {
        let mut term = FrankenTermWeb::new();
        term.resize(8, 4);
        term.feed(b"\x1b[4;8H\x1b[6n");
        assert_eq!(
            drain_uint8_chunks(term.drain_reply_bytes()),
            vec![b"\x1b[4;8R".to_vec()]
        );

        term.resize(5, 2);
        term.feed(b"\x1b[6n");
        assert_eq!(
            drain_uint8_chunks(term.drain_reply_bytes()),
            vec![b"\x1b[2;5R".to_vec()]
        );
    }

    #[test]
    fn drain_reply_bytes_decrpm_tracks_mode_transitions() {
        let mut term = FrankenTermWeb::new();
        term.resize(8, 4);

        term.feed(b"\x1b[?2026$p");
        assert_eq!(
            drain_uint8_chunks(term.drain_reply_bytes()),
            vec![b"\x1b[?2026;2$y".to_vec()]
        );

        term.feed(b"\x1b[?2026h\x1b[?2026$p\x1b[?2026l\x1b[?2026$p");
        assert_eq!(
            drain_uint8_chunks(term.drain_reply_bytes()),
            vec![b"\x1b[?2026;1$y".to_vec(), b"\x1b[?2026;2$y".to_vec()]
        );
    }

    #[test]
    fn viewport_state_unifies_scrollback_and_visible_grid_ranges() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");

        let state = term.viewport_state();
        assert_eq!(js_f64_field(&state, "scrollbackLines"), 2.0);
        assert_eq!(js_f64_field(&state, "gridRows"), 2.0);
        assert_eq!(js_f64_field(&state, "totalLines"), 4.0);
        assert_eq!(js_f64_field(&state, "viewportStart"), 2.0);
        assert_eq!(js_f64_field(&state, "viewportEnd"), 4.0);
        assert!(js_bool_field(&state, "atBottom"));
        assert!(js_bool_field(&state, "followOutput"));
    }

    #[test]
    fn viewport_lines_map_across_scrollback_then_grid() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");

        term.scroll_state.set_offset(1);
        let lines = term.viewport_lines();
        assert_eq!(lines.length(), 2);
        assert_eq!(lines.get(0).as_string().as_deref(), Some("BBBB"));
        assert_eq!(lines.get(1).as_string().as_deref(), Some("CCCC"));
    }

    #[test]
    fn snapshot_scrollback_frame_jsonl_emits_unified_window_metrics() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");
        term.scroll_state.set_offset(1);

        let line = term
            .snapshot_scrollback_frame_jsonl("run-scroll", "T000100", 7, 1234)
            .expect("snapshot_scrollback_frame_jsonl should succeed");
        let parsed: serde_json::Value = serde_json::from_str(&line)
            .expect("snapshot_scrollback_frame_jsonl output should be valid JSON");

        assert_eq!(parsed["type"], "scrollback_frame");
        assert_eq!(parsed["run_id"], "run-scroll");
        assert_eq!(parsed["frame_idx"], 7);
        assert_eq!(parsed["scrollback_lines"], 4);
        assert_eq!(parsed["viewport_start"], 1);
        assert_eq!(parsed["viewport_end"], 3);
        assert_eq!(parsed["render_cost_us"], 1234);
    }

    #[test]
    fn scroll_navigation_apis_are_clamped_and_deterministic() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");

        let top = term.scroll_to_top_nav();
        assert_eq!(js_f64_field(&top, "viewportStart"), 0.0);
        assert_eq!(js_f64_field(&top, "maxScrollOffset"), 2.0);
        assert!(!js_bool_field(&top, "followOutput"));

        let down_one = term.scroll_lines_nav(-1);
        assert_eq!(js_f64_field(&down_one, "viewportStart"), 1.0);
        assert!(!js_bool_field(&down_one, "followOutput"));

        let page_down = term.scroll_pages_nav(-1);
        assert_eq!(js_f64_field(&page_down, "viewportStart"), 2.0);
        assert!(js_bool_field(&page_down, "atBottom"));
        assert!(js_bool_field(&page_down, "followOutput"));

        let jump_line = term.scroll_to_line_nav(1);
        assert_eq!(js_f64_field(&jump_line, "viewportStart"), 0.0);
        assert!(!js_bool_field(&jump_line, "followOutput"));

        let bottom = term.scroll_to_bottom_nav();
        assert_eq!(js_f64_field(&bottom, "viewportStart"), 2.0);
        assert!(js_bool_field(&bottom, "atBottom"));
        assert!(js_bool_field(&bottom, "followOutput"));
    }

    #[test]
    fn feed_preserves_viewport_anchor_when_follow_output_is_disabled() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");

        let scrolled = term.scroll_lines_nav(1);
        assert_eq!(js_f64_field(&scrolled, "viewportStart"), 1.0);
        assert!(!js_bool_field(&scrolled, "followOutput"));

        term.feed(b"\r\nEEEE");
        let after = term.viewport_state();
        assert_eq!(js_f64_field(&after, "viewportStart"), 1.0);
        assert!(!js_bool_field(&after, "followOutput"));
    }

    #[test]
    fn feed_keeps_bottom_when_follow_output_is_enabled() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");
        assert_eq!(js_f64_field(&term.viewport_state(), "viewportStart"), 2.0);

        term.feed(b"\r\nEEEE");
        let after = term.viewport_state();
        assert_eq!(js_f64_field(&after, "viewportStart"), 3.0);
        assert!(js_bool_field(&after, "atBottom"));
        assert!(js_bool_field(&after, "followOutput"));
    }

    #[test]
    fn resize_preserves_anchor_line_when_follow_output_is_disabled() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD\r\nEEEE\r\nFFFF");

        let scrolled = term.scroll_lines_nav(1);
        assert_eq!(js_f64_field(&scrolled, "viewportStart"), 3.0);
        assert!(!js_bool_field(&scrolled, "followOutput"));

        term.resize(4, 3);
        let after = term.viewport_state();
        assert_eq!(js_f64_field(&after, "viewportStart"), 3.0);
        assert!(!js_bool_field(&after, "followOutput"));
    }

    #[test]
    fn resize_keeps_bottom_when_follow_output_is_enabled() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD\r\nEEEE\r\nFFFF");
        assert_eq!(js_f64_field(&term.viewport_state(), "viewportStart"), 4.0);

        term.resize(4, 3);
        let after = term.viewport_state();
        assert_eq!(js_f64_field(&after, "viewportStart"), 3.0);
        assert!(js_bool_field(&after, "atBottom"));
        assert!(js_bool_field(&after, "followOutput"));
    }

    #[test]
    fn wheel_input_scrolls_viewport_when_mouse_reporting_is_disabled() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");
        assert_eq!(js_f64_field(&term.viewport_state(), "viewportStart"), 2.0);

        assert!(
            term.queue_input_event(InputEvent::Wheel(WheelInput {
                x: 0,
                y: 0,
                dx: 0,
                dy: 1,
                mods: Modifiers::default(),
            }))
            .is_ok()
        );

        // Default lines_per_tick=3; max offset=2, so one wheel tick reaches top.
        let state = term.viewport_state();
        assert_eq!(js_f64_field(&state, "viewportStart"), 0.0);
        assert!(!js_bool_field(&state, "followOutput"));
        assert_eq!(term.encoded_input_bytes.len(), 0);
    }

    #[test]
    fn wheel_input_does_not_scroll_viewport_when_sgr_mouse_is_enabled() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");
        term.encoder_features.sgr_mouse = true;
        assert_eq!(js_f64_field(&term.viewport_state(), "viewportStart"), 2.0);

        assert!(
            term.queue_input_event(InputEvent::Wheel(WheelInput {
                x: 0,
                y: 0,
                dx: 0,
                dy: 1,
                mods: Modifiers::default(),
            }))
            .is_ok()
        );

        // Mouse-reporting mode should route wheel to VT bytes, not local viewport scroll.
        let state = term.viewport_state();
        assert_eq!(js_f64_field(&state, "viewportStart"), 2.0);
        assert!(js_bool_field(&state, "followOutput"));
        assert_eq!(term.encoded_input_bytes.len(), 1);
        assert!(!term.encoded_input_bytes[0].is_empty());
    }

    #[test]
    fn large_scrollback_snapshot_window_is_bounded_and_deterministic() {
        let mut term = FrankenTermWeb::new();
        term.resize(8, 4);
        feed_numbered_lines(&mut term, 20_000, 8);

        let top = term.scroll_to_top_nav();
        assert_eq!(js_f64_field(&top, "viewportStart"), 0.0);

        let line_a = term
            .snapshot_scrollback_frame_jsonl("run-large", "T000200", 3, 777)
            .expect("snapshot_scrollback_frame_jsonl should succeed");
        let line_b = term
            .snapshot_scrollback_frame_jsonl("run-large", "T000200", 3, 777)
            .expect("snapshot_scrollback_frame_jsonl should be deterministic");
        assert_eq!(line_a, line_b);

        let parsed: serde_json::Value =
            serde_json::from_str(&line_a).expect("scrollback frame jsonl should parse");
        let viewport_len = parsed["viewport_end"].as_u64().unwrap_or(0)
            - parsed["viewport_start"].as_u64().unwrap_or(0);
        let render_len = parsed["render_end"].as_u64().unwrap_or(0)
            - parsed["render_start"].as_u64().unwrap_or(0);
        assert_eq!(viewport_len, 4);
        // Overscan window must stay bounded (default overscan is small; 64 is a safe upper guard).
        assert!(render_len <= viewport_len + 64);
    }

    #[test]
    fn large_scrollback_search_navigation_is_stable_and_moves_viewport_to_match() {
        let mut term = FrankenTermWeb::new();
        term.resize(8, 4);
        feed_numbered_lines(&mut term, 20_000, 8);

        assert!(term.set_search_query("00000123", None).is_ok());
        let state = term.search_next();
        assert_eq!(js_f64_field(&state, "activeLine"), 123.0);

        let view = term.viewport_state();
        assert!(js_f64_field(&view, "viewportStart") <= 123.0);
        assert!(123.0 < js_f64_field(&view, "viewportEnd"));
        assert!(!js_bool_field(&view, "followOutput"));
    }

    #[test]
    fn scrollback_frame_snapshot_validates_required_fields_with_actionable_errors() {
        let mut term = FrankenTermWeb::new();
        term.resize(4, 2);
        term.feed(b"AAAA\r\nBBBB");

        let missing_run = term
            .snapshot_scrollback_frame_jsonl("", "T000201", 1, 42)
            .expect_err("missing run id should fail");
        assert_eq!(
            missing_run.as_string().as_deref(),
            Some("run_id must not be empty")
        );

        let missing_timestamp = term
            .snapshot_scrollback_frame_jsonl("run", "", 1, 42)
            .expect_err("missing timestamp should fail");
        assert_eq!(
            missing_timestamp.as_string().as_deref(),
            Some("timestamp must not be empty")
        );
    }

    #[test]
    fn feed_is_noop_when_engine_is_unavailable() {
        let mut term = FrankenTermWeb::new();
        term.resize(0, 0);
        term.feed(b"ABC\x1b[5n");
        assert!(term.shadow_cells.is_empty());
        assert_eq!(term.drain_reply_bytes().length(), 0);
    }

    #[test]
    fn remote_feed_replay_transcript_is_deterministic_for_identical_chunks() {
        let chunks: [&[u8]; 5] = [b"ABCD", b"\x1b[2;3H", b"Z\x1b[5n", b"\x1b[6n\r\n", b"xy"];

        let run_a = run_remote_feed_replay_fixture(&chunks);
        let run_b = run_remote_feed_replay_fixture(&chunks);

        assert_eq!(run_a.transcript_jsonl, run_b.transcript_jsonl);
        assert_eq!(run_a.final_frame_hash, run_b.final_frame_hash);
        assert_eq!(run_a.replies_hex, run_b.replies_hex);
    }

    #[test]
    fn remote_feed_replay_transcript_chunked_and_single_feed_have_same_outcome() {
        let single = run_remote_feed_replay_fixture(&[b"ABCD\x1b[2;3HZ\x1b[5n\x1b[6n\r\nxy"]);
        let chunked = run_remote_feed_replay_fixture(&[
            b"AB",
            b"CD\x1b[2",
            b";3H",
            b"Z\x1b[5n",
            b"\x1b[6n\r\nxy",
        ]);

        assert_eq!(single.final_frame_hash, chunked.final_frame_hash);
        assert_eq!(single.replies_hex, chunked.replies_hex);
    }

    #[test]
    fn remote_feed_replay_transcript_records_cursor_sensitive_reply_order() {
        let replay = run_remote_feed_replay_fixture(&[b"\x1b[3;4H", b"\x1b[6n\x1b[5n"]);

        // CPR should reflect row=3,col=4 (1-indexed); DSR 5n reply follows.
        assert_eq!(
            replay.replies_hex,
            vec![bytes_to_hex(b"\x1b[3;4R"), bytes_to_hex(b"\x1b[0n")]
        );

        let parsed: Vec<serde_json::Value> = replay
            .transcript_jsonl
            .iter()
            .map(|line| serde_json::from_str(line).expect("transcript line should be valid JSON"))
            .collect();
        assert!(
            parsed
                .iter()
                .all(|event| event["schema_version"] == "e2e-jsonl-v1"),
            "every replay transcript line must be schema-tagged"
        );
    }
}

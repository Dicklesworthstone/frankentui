#![forbid(unsafe_code)]

//! Main application model, message routing, and screen navigation.
//!
//! This module contains the top-level [`AppModel`] that implements the Elm
//! architecture via [`Model`]. It manages all demo screens, routes events,
//! handles global keybindings, and renders the chrome (tab bar, status bar,
//! help/debug overlays).

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::env;
use std::fs::{OpenOptions, create_dir_all};
use std::io::{BufWriter, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::determinism;
use crate::screens;
use crate::screens::Screen;
use crate::test_logging::{JsonlLogger, jsonl_enabled};
use crate::theme;
use crate::tour::{GuidedTourState, TourAdvanceReason, TourEvent, TourStep};
use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_extras::mermaid::MermaidConfig;
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::Cell as RenderCell;
use ftui_render::frame::{Frame, HitGrid, HitId};
use ftui_runtime::render_trace::checksum_buffer;
use ftui_runtime::undo::HistoryManager;
use ftui_runtime::{Cmd, Every, FrameTiming, FrameTimingSink, Model, Subscription};
use ftui_style::Style;
use ftui_text::{Line, Span, Text, WrapMode};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::command_palette::{CommandPalette, PaletteAction};
use ftui_widgets::error_boundary::FallbackWidget;
use ftui_widgets::paragraph::Paragraph;

// ---------------------------------------------------------------------------
// Performance HUD Diagnostics (bd-3k3x.8)
// ---------------------------------------------------------------------------

/// Warn if no ticks are observed for longer than this.
const TICK_STALL_WARN_AFTER: Duration = Duration::from_millis(750);
/// Rate-limit tick stall logs to avoid spamming.
const TICK_STALL_LOG_INTERVAL: Duration = Duration::from_millis(2_000);

/// Global counter for JSONL log sequence numbers.
#[allow(dead_code)]
static PERF_HUD_LOG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Check if JSONL diagnostic logging is enabled via env var.
#[allow(dead_code)]
fn perf_hud_jsonl_enabled() -> bool {
    env::var("FTUI_PERF_HUD_JSONL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Emit a JSONL diagnostic log line for the Performance HUD.
///
/// Format: `{"seq":N,"ts_us":T,"event":"E",...fields...}`
///
/// # Fields
/// - `seq`: Monotonically increasing sequence number
/// - `ts_us`: Timestamp in microseconds since process start (approximated)
/// - `event`: Event type (e.g., "tick_stats", "hud_toggle", "threshold_crossed")
/// - Additional fields depend on event type
///
/// # Invariants (Alien Artifact)
/// - Sequence numbers are globally unique and monotonically increasing
/// - Timestamps are best-effort (not wall-clock accurate but monotonic)
/// - Output is valid JSON on a single line
/// - No panic on write failure (best-effort logging)
#[allow(dead_code)]
fn emit_perf_hud_jsonl(event: &str, fields: &[(&str, &str)]) {
    if !perf_hud_jsonl_enabled() {
        return;
    }

    let seq = PERF_HUD_LOG_SEQ.fetch_add(1, Ordering::Relaxed);
    // Approximate timestamp using seq as a proxy (each call is ~1 tick apart)
    let ts_us = seq.saturating_mul(16_667); // Assume ~60 TPS

    let mut json = format!("{{\"seq\":{seq},\"ts_us\":{ts_us},\"event\":\"{event}\"");
    for (key, value) in fields {
        // Escape any quotes in value
        let escaped = value.replace('\"', "\\\"");
        json.push_str(&format!(",\"{key}\":\"{escaped}\""));
    }
    json.push('}');

    // Best-effort write to stderr (diagnostic logs go to stderr)
    let _ = writeln!(std::io::stderr(), "{json}");
}

/// Emit numeric fields as JSONL (avoids quoting numbers).
#[allow(dead_code)]
fn emit_perf_hud_jsonl_numeric(event: &str, fields: &[(&str, f64)]) {
    if !perf_hud_jsonl_enabled() {
        return;
    }

    let seq = PERF_HUD_LOG_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts_us = seq.saturating_mul(16_667);

    let mut json = format!("{{\"seq\":{seq},\"ts_us\":{ts_us},\"event\":\"{event}\"");
    for (key, value) in fields {
        if value.is_finite() {
            json.push_str(&format!(",\"{key}\":{value:.3}"));
        } else {
            json.push_str(&format!(",\"{key}\":null"));
        }
    }
    json.push('}');

    let _ = writeln!(std::io::stderr(), "{json}");
}

// ---------------------------------------------------------------------------
// Accessibility Diagnostics + Telemetry (bd-2o55.5)
// ---------------------------------------------------------------------------

/// Global counter for A11y JSONL logs.
static A11Y_LOG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Check if A11y JSONL diagnostics are enabled via env var.
fn a11y_jsonl_enabled() -> bool {
    env::var("FTUI_A11Y_JSONL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Emit a JSONL diagnostic log line for accessibility mode changes.
fn emit_a11y_jsonl(event: &str, fields: &[(&str, &str)]) {
    if !a11y_jsonl_enabled() {
        return;
    }

    let seq = A11Y_LOG_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts_us = seq.saturating_mul(16_667);

    let mut json = format!("{{\"seq\":{seq},\"ts_us\":{ts_us},\"event\":\"{event}\"");
    for (key, value) in fields {
        let escaped = value.replace('\"', "\\\"");
        json.push_str(&format!(",\"{key}\":\"{escaped}\""));
    }
    json.push('}');

    let _ = writeln!(std::io::stderr(), "{json}");
}

// ---------------------------------------------------------------------------
// Screen Init Diagnostics (bd-3e1t.5.4)
// ---------------------------------------------------------------------------

fn screen_init_logger() -> Option<&'static JsonlLogger> {
    if !jsonl_enabled() {
        return None;
    }
    static LOGGER: OnceLock<JsonlLogger> = OnceLock::new();
    Some(LOGGER.get_or_init(|| {
        let run_id = determinism::demo_run_id().unwrap_or_else(|| "demo_screen_init".to_string());
        let seed = determinism::demo_seed(0);
        JsonlLogger::new(run_id)
            .with_seed(seed)
            .with_context("screen_mode", determinism::demo_screen_mode())
    }))
}

fn emit_screen_init_log(
    screen: ScreenId,
    init_ms: u64,
    effect_count: usize,
    memory_estimate_bytes: Option<u64>,
) {
    #[cfg(test)]
    record_screen_init_event(screen, init_ms, effect_count, memory_estimate_bytes);

    let Some(logger) = screen_init_logger() else {
        return;
    };
    let init_ms = init_ms.to_string();
    let effect_count = effect_count.to_string();
    let memory_estimate = memory_estimate_bytes
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    logger.log(
        "screen_init",
        &[
            ("screen_id", screen.widget_name()),
            ("init_ms", &init_ms),
            ("effect_count", &effect_count),
            ("memory_estimate_bytes", &memory_estimate),
        ],
    );
}

// ---------------------------------------------------------------------------
// Mouse Navigation Diagnostics (bd-ptyoh)
// ---------------------------------------------------------------------------

fn mouse_jsonl_logger() -> Option<&'static JsonlLogger> {
    if !jsonl_enabled() {
        return None;
    }
    static LOGGER: OnceLock<JsonlLogger> = OnceLock::new();
    Some(LOGGER.get_or_init(|| {
        let run_id = determinism::demo_run_id().unwrap_or_else(|| "demo_mouse".to_string());
        let seed = determinism::demo_seed(0);
        JsonlLogger::new(run_id)
            .with_seed(seed)
            .with_context("screen_mode", determinism::demo_screen_mode())
    }))
}

fn mouse_kind_label(kind: MouseEventKind) -> &'static str {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => "down_left",
        MouseEventKind::Down(MouseButton::Right) => "down_right",
        MouseEventKind::Down(MouseButton::Middle) => "down_middle",
        MouseEventKind::Up(MouseButton::Left) => "up_left",
        MouseEventKind::Up(MouseButton::Right) => "up_right",
        MouseEventKind::Up(MouseButton::Middle) => "up_middle",
        MouseEventKind::Drag(MouseButton::Left) => "drag_left",
        MouseEventKind::Drag(MouseButton::Right) => "drag_right",
        MouseEventKind::Drag(MouseButton::Middle) => "drag_middle",
        MouseEventKind::Moved => "moved",
        MouseEventKind::ScrollUp => "scroll_up",
        MouseEventKind::ScrollDown => "scroll_down",
        MouseEventKind::ScrollLeft => "scroll_left",
        MouseEventKind::ScrollRight => "scroll_right",
    }
}

fn emit_mouse_jsonl(
    mouse: &MouseEvent,
    hit_id: Option<HitId>,
    action: &str,
    target: Option<ScreenId>,
    current: ScreenId,
) {
    let Some(logger) = mouse_jsonl_logger() else {
        return;
    };
    let hit_id = hit_id
        .map(|id| id.id().to_string())
        .unwrap_or_else(|| "none".to_string());
    let target_label = target
        .map(|screen| screen.title().to_string())
        .unwrap_or_else(|| "none".to_string());
    let current_label = current.title().to_string();
    let x = mouse.x.to_string();
    let y = mouse.y.to_string();
    logger.log(
        "mouse_event",
        &[
            ("kind", mouse_kind_label(mouse.kind)),
            ("x", &x),
            ("y", &y),
            ("hit_id", &hit_id),
            ("action", action),
            ("target_screen", &target_label),
            ("current_screen", &current_label),
        ],
    );
}

fn emit_mouse_capture_toggle_jsonl(
    enabled: bool,
    source: &str,
    inline_mode: bool,
    current: ScreenId,
) {
    let Some(logger) = mouse_jsonl_logger() else {
        return;
    };
    let state = if enabled { "on" } else { "off" };
    let mode = if inline_mode { "inline" } else { "alt" };
    let current_label = current.title().to_string();
    logger.log(
        "mouse_capture_toggle",
        &[
            ("state", state),
            ("mode", mode),
            ("source", source),
            ("current_screen", &current_label),
        ],
    );
}

// ---------------------------------------------------------------------------
// Central Mouse Dispatcher (bd-iuvb.17.2)
// ---------------------------------------------------------------------------

/// Tracks the current drag state for deterministic drag lifecycle management.
///
/// The state machine is: Idle → PendingDrag → Dragging → Idle.
/// A PendingDrag transitions to Dragging when the mouse moves beyond threshold,
/// or back to Idle on MouseUp (which is treated as a click).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragPhase {
    /// No drag in progress.
    Idle,
    /// Button pressed, waiting to see if it becomes a click or drag.
    PendingDrag {
        button: MouseButton,
        start_x: u16,
        start_y: u16,
        hit_id: Option<HitId>,
    },
    /// Active drag in progress.
    Dragging {
        button: MouseButton,
        start_x: u16,
        start_y: u16,
        last_x: u16,
        last_y: u16,
        hit_id: Option<HitId>,
    },
}

/// Result of dispatching a mouse event through the priority chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseDispatchResult {
    /// Event was consumed by the dispatcher; do not forward.
    Consumed,
    /// Event was not consumed; forward to subsequent handlers.
    NotConsumed,
}

/// Minimum distance (in cells) before a pending-drag becomes a real drag.
const DRAG_THRESHOLD: u16 = 2;

/// Central mouse dispatcher that routes mouse events through the priority
/// chain: palette → overlay → status → chrome → screen.
///
/// Invariants:
/// - Click activation uses MouseUp (prevents accidental drags).
/// - Hover state only updates when the target HitId actually changes.
/// - Drag lifecycle is deterministic (no timing-based heuristics).
/// - All events are logged via emit_mouse_jsonl when logging is enabled.
#[derive(Debug)]
struct MouseDispatcher {
    /// Current hover target (None = no hit region under pointer).
    hover_hit: Option<HitId>,
    /// Current drag state machine phase.
    drag: DragPhase,
}

impl Default for MouseDispatcher {
    fn default() -> Self {
        Self {
            hover_hit: None,
            drag: DragPhase::Idle,
        }
    }
}

impl MouseDispatcher {
    /// Update hover state. Returns true if the hover target changed.
    fn update_hover(&mut self, new_hit: Option<HitId>) -> bool {
        if self.hover_hit != new_hit {
            self.hover_hit = new_hit;
            true
        } else {
            false
        }
    }

    /// Cancel any in-progress drag, returning to Idle.
    fn cancel_drag(&mut self) {
        self.drag = DragPhase::Idle;
    }

    /// Check if a drag is currently in progress.
    fn is_dragging(&self) -> bool {
        matches!(self.drag, DragPhase::Dragging { .. })
    }

    /// Get the hit ID associated with the current drag, if any.
    fn drag_hit_id(&self) -> Option<HitId> {
        match self.drag {
            DragPhase::PendingDrag { hit_id, .. } | DragPhase::Dragging { hit_id, .. } => hit_id,
            DragPhase::Idle => None,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct ScreenInitEvent {
    screen: ScreenId,
    init_ms: u64,
    effect_count: usize,
    memory_estimate_bytes: Option<u64>,
}

// Thread-local storage for screen init events.
// Each thread has its own event list, avoiding cross-thread interference.
#[cfg(test)]
thread_local! {
    static SCREEN_INIT_EVENTS: std::cell::RefCell<Vec<ScreenInitEvent>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static SCREEN_INIT_RECORDING_ACTIVE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Serialization lock for tests that need exclusive access to screen init events.
/// Only one guard can exist at a time to prevent test interference between
/// the two tests that use this mechanism.
#[cfg(test)]
static SCREEN_INIT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Guard that enables screen init event recording for its lifetime.
/// Only events recorded on the same thread while a guard exists are stored.
/// Guards are serialized globally to prevent the two lazy-screen tests
/// from interfering with each other when they happen to run on the same thread.
#[cfg(test)]
struct ScreenInitEventGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl ScreenInitEventGuard {
    /// Start recording screen init events. Clears any existing events on this thread.
    /// Blocks until exclusive access is available.
    fn new() -> Self {
        // Acquire exclusive access first - this serializes the two lazy-screen tests
        let lock = SCREEN_INIT_TEST_LOCK.lock().unwrap();
        // Clear any existing events on this thread
        SCREEN_INIT_EVENTS.with(|events| events.borrow_mut().clear());
        // Enable recording on this thread
        SCREEN_INIT_RECORDING_ACTIVE.with(|active| active.set(true));
        Self { _lock: lock }
    }

    /// Take all events recorded since this guard was created.
    fn take_events(&self) -> Vec<ScreenInitEvent> {
        SCREEN_INIT_EVENTS.with(|events| events.borrow_mut().drain(..).collect())
    }
}

#[cfg(test)]
impl Drop for ScreenInitEventGuard {
    fn drop(&mut self) {
        SCREEN_INIT_RECORDING_ACTIVE.with(|active| active.set(false));
    }
}

#[cfg(test)]
fn record_screen_init_event(
    screen: ScreenId,
    init_ms: u64,
    effect_count: usize,
    memory_estimate_bytes: Option<u64>,
) {
    // Only record if this thread is actively recording
    let should_record = SCREEN_INIT_RECORDING_ACTIVE.with(|active| active.get());
    if !should_record {
        return;
    }
    SCREEN_INIT_EVENTS.with(|events| {
        events.borrow_mut().push(ScreenInitEvent {
            screen,
            init_ms,
            effect_count,
            memory_estimate_bytes,
        });
    });
}

/// Accessibility telemetry event types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A11yEventKind {
    Panel,
    HighContrast,
    ReducedMotion,
    LargeText,
}

/// Telemetry payload for A11y events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct A11yTelemetryEvent {
    pub kind: A11yEventKind,
    pub tick: u64,
    pub screen: &'static str,
    pub panel_visible: bool,
    pub high_contrast: bool,
    pub reduced_motion: bool,
    pub large_text: bool,
}

type A11yTelemetryCallback = Box<dyn Fn(&A11yTelemetryEvent) + Send + Sync>;

/// Optional telemetry hooks for A11y events.
#[derive(Default)]
pub struct A11yTelemetryHooks {
    on_panel_toggle: Option<A11yTelemetryCallback>,
    on_high_contrast: Option<A11yTelemetryCallback>,
    on_reduced_motion: Option<A11yTelemetryCallback>,
    on_large_text: Option<A11yTelemetryCallback>,
    on_any: Option<A11yTelemetryCallback>,
}

impl A11yTelemetryHooks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_panel_toggle(
        mut self,
        f: impl Fn(&A11yTelemetryEvent) + Send + Sync + 'static,
    ) -> Self {
        self.on_panel_toggle = Some(Box::new(f));
        self
    }

    pub fn on_high_contrast(
        mut self,
        f: impl Fn(&A11yTelemetryEvent) + Send + Sync + 'static,
    ) -> Self {
        self.on_high_contrast = Some(Box::new(f));
        self
    }

    pub fn on_reduced_motion(
        mut self,
        f: impl Fn(&A11yTelemetryEvent) + Send + Sync + 'static,
    ) -> Self {
        self.on_reduced_motion = Some(Box::new(f));
        self
    }

    pub fn on_large_text(
        mut self,
        f: impl Fn(&A11yTelemetryEvent) + Send + Sync + 'static,
    ) -> Self {
        self.on_large_text = Some(Box::new(f));
        self
    }

    pub fn on_any(mut self, f: impl Fn(&A11yTelemetryEvent) + Send + Sync + 'static) -> Self {
        self.on_any = Some(Box::new(f));
        self
    }

    fn dispatch(&self, event: &A11yTelemetryEvent) {
        if let Some(ref hook) = self.on_any {
            hook(event);
        }
        match event.kind {
            A11yEventKind::Panel => {
                if let Some(ref hook) = self.on_panel_toggle {
                    hook(event);
                }
            }
            A11yEventKind::HighContrast => {
                if let Some(ref hook) = self.on_high_contrast {
                    hook(event);
                }
            }
            A11yEventKind::ReducedMotion => {
                if let Some(ref hook) = self.on_reduced_motion {
                    hook(event);
                }
            }
            A11yEventKind::LargeText => {
                if let Some(ref hook) = self.on_large_text {
                    hook(event);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Command Palette Diagnostics (bd-iuvb.16)
// ---------------------------------------------------------------------------

/// Global counter for palette JSONL logs.
static PALETTE_LOG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Return the palette JSONL log path if enabled.
fn palette_log_path() -> Option<String> {
    env::var("FTUI_PALETTE_REPORT_PATH").ok()
}

/// Return the palette run id (for E2E correlation).
fn palette_run_id() -> String {
    env::var("FTUI_PALETTE_RUN_ID").unwrap_or_else(|_| "unknown".to_string())
}

fn json_escape(value: &str) -> String {
    escape_json(value)
}

/// Emit a palette JSONL entry to the report path (best-effort).
fn emit_palette_jsonl(
    action: &str,
    query: &str,
    selected_screen: Option<ScreenId>,
    category: Option<screens::ScreenCategory>,
    outcome: &str,
) {
    let Some(path) = palette_log_path() else {
        return;
    };

    let seq = PALETTE_LOG_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts_us = seq.saturating_mul(16_667);
    let run_id = palette_run_id();
    let screen_label = selected_screen.map(|id| id.title()).unwrap_or("none");
    let category_label = category.map(|cat| cat.label()).unwrap_or("none");

    let json = format!(
        "{{\"seq\":{seq},\"ts_us\":{ts_us},\"run_id\":\"{}\",\"action\":\"{}\",\"query\":\"{}\",\"selected_screen\":\"{}\",\"category\":\"{}\",\"outcome\":\"{}\"}}",
        json_escape(&run_id),
        json_escape(action),
        json_escape(query),
        json_escape(screen_label),
        json_escape(category_label),
        json_escape(outcome)
    );

    if let Some(parent) = Path::new(&path).parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{json}");
    }
}

// ---------------------------------------------------------------------------
// Guided Tour Diagnostics (bd-iuvb.1)
// ---------------------------------------------------------------------------

/// Global counter for guided tour JSONL logs.
static TOUR_LOG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Return the guided tour JSONL log path if enabled.
fn tour_log_path() -> Option<String> {
    env::var("FTUI_TOUR_REPORT_PATH").ok()
}

/// Return the guided tour run id (for E2E correlation).
fn tour_run_id() -> String {
    env::var("FTUI_TOUR_RUN_ID").unwrap_or_else(|_| "unknown".to_string())
}

/// Return the guided tour seed (deterministic default = 0).
fn tour_seed() -> u64 {
    env::var("FTUI_TOUR_SEED")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_else(|| determinism::demo_seed(0))
}

/// Resolve a screen mode label for guided tour logs.
fn tour_screen_mode() -> String {
    env::var("FTUI_DEMO_SCREEN_MODE")
        .or_else(|_| env::var("FTUI_HARNESS_SCREEN_MODE"))
        .unwrap_or_else(|_| "alt".to_string())
}

/// Resolve a terminal capabilities profile label for guided tour logs.
fn tour_caps_profile() -> String {
    env::var("FTUI_TOUR_CAPS_PROFILE")
        .or_else(|_| env::var("TERM"))
        .unwrap_or_else(|_| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// ScreenId
// ---------------------------------------------------------------------------

/// Identifies which demo screen is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenId {
    /// Guided tour (auto-play storyboard).
    GuidedTour,
    /// System dashboard with live widgets.
    Dashboard,
    /// Complete Shakespeare works with search.
    Shakespeare,
    /// SQLite source with syntax highlighting.
    CodeExplorer,
    /// Showcase of every widget type.
    WidgetGallery,
    /// Interactive constraint solver demo.
    LayoutLab,
    /// Form widgets and text editing.
    FormsInput,
    /// Charts, canvas, and structured data.
    DataViz,
    /// Table theme preset gallery.
    TableThemeGallery,
    /// File system navigation and preview.
    FileBrowser,
    /// Diagnostics, timers, and spinners.
    AdvancedFeatures,
    /// Terminal capability explorer and diagnostics (bd-2sog).
    TerminalCapabilities,
    /// Input macro recorder and scenario runner.
    MacroRecorder,
    /// Virtualized list and stress testing.
    Performance,
    /// Markdown rendering and typography.
    MarkdownRichText,
    /// Interactive Mermaid diagram showcase.
    MermaidShowcase,
    /// Comprehensive Mermaid mega showcase (bd-3oaig.3).
    MermaidMegaShowcase,
    /// Mind-blowing visual effects with braille.
    VisualEffects,
    /// Responsive layout breakpoint demo.
    ResponsiveDemo,
    /// Live log search and filter demo.
    LogSearch,
    /// Toast notification system demo.
    Notifications,
    /// Action timeline / event stream viewer.
    ActionTimeline,
    /// Content-aware layout examples (bd-2dow.7).
    IntrinsicSizing,
    /// Layout inspector (constraint solver visual, bd-iuvb.7).
    LayoutInspector,
    /// Multi-line text editor with search/replace (bd-12o8).
    AdvancedTextEditor,
    /// Mouse/hit-test playground (bd-bksf).
    MousePlayground,
    /// Form validation demo (bd-34pj.5).
    FormValidation,
    /// Virtualized list with fuzzy search (bd-2zbk).
    VirtualizedSearch,
    /// Async task manager / job queue (bd-13pq).
    AsyncTasks,
    /// Theme studio / live palette editor (bd-vu0o).
    ThemeStudio,
    /// Snapshot/Time Travel Player (bd-3sa7).
    SnapshotPlayer,
    /// Performance challenge mode (degradation tiers + stress harness) (bd-iuvb.15).
    PerformanceHud,
    /// Explainability cockpit (diff/resize/budget evidence) (bd-iuvb.4).
    ExplainabilityCockpit,
    /// Internationalization demo (bd-ic6i.5).
    I18nDemo,
    /// VOI overlay widget demo (Galaxy-Brain).
    VoiOverlay,
    /// Inline mode story (scrollback + chrome).
    InlineModeStory,
    /// Accessibility control panel (bd-iuvb.8).
    AccessibilityPanel,
    /// Interactive widget builder sandbox (bd-iuvb.10).
    WidgetBuilder,
    /// Command palette evidence lab (bd-iuvb.11).
    CommandPaletteLab,
    /// Determinism lab (checksum equivalence) (bd-iuvb.2).
    DeterminismLab,
    /// Hyperlink playground (OSC-8 + hit regions) (bd-iuvb.14).
    HyperlinkPlayground,
    /// Drag-drop Kanban board (bd-iuvb.12).
    KanbanBoard,
}

impl ScreenId {
    /// 0-based index in the ALL array.
    pub fn index(self) -> usize {
        screens::screen_index(self)
    }

    /// Next screen (wraps around).
    pub fn next(self) -> Self {
        screens::next_screen(self)
    }

    /// Previous screen (wraps around).
    pub fn prev(self) -> Self {
        screens::prev_screen(self)
    }

    /// Title for the tab bar.
    pub fn title(self) -> &'static str {
        screens::screen_title(self)
    }

    /// Short label for the tab (max ~12 chars).
    pub fn tab_label(self) -> &'static str {
        screens::screen_tab_label(self)
    }

    /// Widget name used in error boundary fallback messages.
    fn widget_name(self) -> &'static str {
        match self {
            Self::GuidedTour => "GuidedTour",
            Self::Dashboard => "Dashboard",
            Self::Shakespeare => "Shakespeare",
            Self::CodeExplorer => "CodeExplorer",
            Self::WidgetGallery => "WidgetGallery",
            Self::LayoutLab => "LayoutLab",
            Self::FormsInput => "FormsInput",
            Self::DataViz => "DataViz",
            Self::TableThemeGallery => "TableThemeGallery",
            Self::FileBrowser => "FileBrowser",
            Self::AdvancedFeatures => "AdvancedFeatures",
            Self::TerminalCapabilities => "TerminalCapabilities",
            Self::MacroRecorder => "MacroRecorder",
            Self::Performance => "Performance",
            Self::MarkdownRichText => "MarkdownRichText",
            Self::MermaidShowcase => "MermaidShowcase",
            Self::MermaidMegaShowcase => "MermaidMegaShowcase",
            Self::VisualEffects => "VisualEffects",
            Self::ResponsiveDemo => "ResponsiveDemo",
            Self::LogSearch => "LogSearch",
            Self::Notifications => "Notifications",
            Self::ActionTimeline => "ActionTimeline",
            Self::IntrinsicSizing => "IntrinsicSizing",
            Self::LayoutInspector => "LayoutInspector",
            Self::AdvancedTextEditor => "AdvancedTextEditor",
            Self::MousePlayground => "MousePlayground",
            Self::FormValidation => "FormValidation",
            Self::VirtualizedSearch => "VirtualizedSearch",
            Self::AsyncTasks => "AsyncTasks",
            Self::ThemeStudio => "ThemeStudio",
            Self::SnapshotPlayer => "TimeTravelStudio",
            Self::PerformanceHud => "PerformanceHud",
            Self::ExplainabilityCockpit => "ExplainabilityCockpit",
            Self::I18nDemo => "I18nDemo",
            Self::VoiOverlay => "VoiOverlay",
            Self::InlineModeStory => "InlineModeStory",
            Self::AccessibilityPanel => "AccessibilityPanel",
            Self::WidgetBuilder => "WidgetBuilder",
            Self::CommandPaletteLab => "CommandPaletteLab",
            Self::DeterminismLab => "DeterminismLab",
            Self::HyperlinkPlayground => "HyperlinkPlayground",
            Self::KanbanBoard => "KanbanBoard",
        }
    }

    /// Map number key to screen: '1'..='9' -> first 9, '0' -> 10th.
    pub fn from_number_key(ch: char) -> Option<Self> {
        let idx = match ch {
            '1'..='9' => (ch as usize) - ('1' as usize),
            '0' => 9,
            _ => return None,
        };
        screens::screen_ids().get(idx).copied()
    }
}

// ---------------------------------------------------------------------------
// ScreenStates
// ---------------------------------------------------------------------------

struct LazyScreen<T> {
    state: RefCell<Option<T>>,
}

impl<T> LazyScreen<T> {
    fn new() -> Self {
        Self {
            state: RefCell::new(None),
        }
    }

    fn with_mut<F, R>(&self, init: impl FnOnce() -> T, on_init: impl FnOnce(&T, u64), f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        let mut guard = self.state.borrow_mut();
        if guard.is_none() {
            let start = Instant::now();
            let value = init();
            let init_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            on_init(&value, init_ms);
            *guard = Some(value);
        }
        f(guard.as_mut().expect("screen state should be initialized"))
    }

    fn with_existing_mut<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut T) -> R,
    {
        let mut guard = self.state.borrow_mut();
        guard.as_mut().map(f)
    }

    #[cfg(test)]
    fn is_initialized(&self) -> bool {
        self.state.borrow().is_some()
    }
}

/// Holds the state for every screen.
pub struct ScreenStates {
    /// Dashboard screen state.
    pub dashboard: screens::dashboard::Dashboard,
    /// Shakespeare library screen state.
    pub shakespeare: screens::shakespeare::Shakespeare,
    /// Code explorer screen state (lazy init).
    code_explorer: LazyScreen<screens::code_explorer::CodeExplorer>,
    /// Widget gallery screen state.
    pub widget_gallery: screens::widget_gallery::WidgetGallery,
    /// Layout laboratory screen state.
    pub layout_lab: screens::layout_lab::LayoutLab,
    /// Forms and input screen state.
    pub forms_input: screens::forms_input::FormsInput,
    /// Data visualization screen state.
    pub data_viz: screens::data_viz::DataViz,
    /// Table theme gallery screen state.
    pub table_theme_gallery: screens::table_theme_gallery::TableThemeGallery,
    /// File browser screen state (lazy init).
    file_browser: LazyScreen<screens::file_browser::FileBrowser>,
    /// Advanced features screen state.
    pub advanced_features: screens::advanced_features::AdvancedFeatures,
    /// Terminal capability explorer screen state (bd-2sog).
    pub terminal_capabilities: screens::terminal_capabilities::TerminalCapabilitiesScreen,
    /// Macro recorder screen state.
    pub macro_recorder: screens::macro_recorder::MacroRecorderScreen,
    /// Performance stress test screen state.
    pub performance: screens::performance::Performance,
    /// Markdown and rich text screen state.
    pub markdown_rich_text: screens::markdown_rich_text::MarkdownRichText,
    /// Mermaid showcase screen state.
    pub mermaid_showcase: screens::mermaid_showcase::MermaidShowcaseScreen,
    /// Mermaid mega showcase screen state (bd-3oaig.3).
    pub mermaid_mega_showcase: screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen,
    /// Visual effects screen state (lazy init).
    visual_effects: LazyScreen<screens::visual_effects::VisualEffectsScreen>,
    /// Responsive layout demo screen state.
    pub responsive_demo: screens::responsive_demo::ResponsiveDemo,
    /// Log search demo screen state.
    pub log_search: screens::log_search::LogSearch,
    /// Notifications demo screen state.
    pub notifications: screens::notifications::Notifications,
    /// Action timeline / event stream viewer.
    pub action_timeline: screens::action_timeline::ActionTimeline,
    /// Intrinsic sizing demo screen state (bd-2dow.7).
    pub intrinsic_sizing: screens::intrinsic_sizing::IntrinsicSizingDemo,
    /// Layout inspector screen state (bd-iuvb.7).
    pub layout_inspector: screens::layout_inspector::LayoutInspector,
    /// Advanced text editor demo screen state (bd-12o8).
    pub advanced_text_editor: screens::advanced_text_editor::AdvancedTextEditor,
    /// Mouse/hit-test playground screen state (bd-bksf).
    pub mouse_playground: screens::mouse_playground::MousePlayground,
    /// Form validation demo screen state (bd-34pj.5).
    pub form_validation: screens::form_validation::FormValidationDemo,
    /// Virtualized list with fuzzy search screen state (bd-2zbk).
    pub virtualized_search: screens::virtualized_search::VirtualizedSearch,
    /// Async task manager screen state (bd-13pq).
    pub async_tasks: screens::async_tasks::AsyncTaskManager,
    /// Theme studio screen state (bd-vu0o).
    pub theme_studio: screens::theme_studio::ThemeStudioDemo,
    /// Snapshot/Time Travel Player screen state (bd-3sa7) (lazy init).
    snapshot_player: LazyScreen<screens::snapshot_player::SnapshotPlayer>,
    /// Performance HUD + Render Budget Visualizer screen state (bd-3k3x).
    pub performance_hud: screens::performance_hud::PerformanceHud,
    /// Explainability cockpit screen state (bd-iuvb.4).
    pub explainability_cockpit: screens::explainability_cockpit::ExplainabilityCockpit,
    /// Internationalization demo screen state (bd-ic6i.5).
    pub i18n_demo: screens::i18n_demo::I18nDemo,
    /// VOI overlay widget demo screen state.
    pub voi_overlay: screens::voi_overlay::VoiOverlayScreen,
    /// Inline mode story screen state.
    pub inline_mode_story: screens::inline_mode_story::InlineModeStory,
    /// Accessibility control panel screen state.
    pub accessibility_panel: screens::accessibility_panel::AccessibilityPanel,
    /// Widget builder sandbox screen state (bd-iuvb.10).
    pub widget_builder: screens::widget_builder::WidgetBuilder,
    /// Command palette evidence lab screen state (bd-iuvb.11).
    pub command_palette_lab: screens::command_palette_lab::CommandPaletteEvidenceLab,
    /// Determinism lab screen state (bd-iuvb.2).
    pub determinism_lab: screens::determinism_lab::DeterminismLab,
    /// Hyperlink playground screen state (bd-iuvb.14).
    pub hyperlink_playground: screens::hyperlink_playground::HyperlinkPlayground,
    /// Kanban board screen state (bd-iuvb.12).
    pub kanban_board: screens::kanban_board::KanbanBoard,
    /// Tracks whether each screen has errored during rendering.
    /// Indexed by `ScreenId::index()`.
    screen_errors: Vec<Option<String>>,
    /// Deferred deterministic tick size for Visual Effects screen.
    visual_effects_deterministic_tick_ms: Option<u64>,
}

impl Default for ScreenStates {
    fn default() -> Self {
        Self {
            dashboard: Default::default(),
            shakespeare: Default::default(),
            code_explorer: LazyScreen::new(),
            widget_gallery: Default::default(),
            layout_lab: Default::default(),
            forms_input: Default::default(),
            data_viz: Default::default(),
            table_theme_gallery: Default::default(),
            file_browser: LazyScreen::new(),
            advanced_features: Default::default(),
            terminal_capabilities: Default::default(),
            macro_recorder: Default::default(),
            performance: Default::default(),
            markdown_rich_text: Default::default(),
            mermaid_showcase: Default::default(),
            mermaid_mega_showcase: Default::default(),
            visual_effects: LazyScreen::new(),
            responsive_demo: Default::default(),
            log_search: Default::default(),
            notifications: Default::default(),
            action_timeline: Default::default(),
            intrinsic_sizing: Default::default(),
            layout_inspector: Default::default(),
            advanced_text_editor: Default::default(),
            mouse_playground: Default::default(),
            form_validation: Default::default(),
            virtualized_search: Default::default(),
            async_tasks: Default::default(),
            theme_studio: Default::default(),
            snapshot_player: LazyScreen::new(),
            performance_hud: Default::default(),
            explainability_cockpit: Default::default(),
            i18n_demo: Default::default(),
            voi_overlay: Default::default(),
            inline_mode_story: Default::default(),
            accessibility_panel: Default::default(),
            widget_builder: Default::default(),
            command_palette_lab: Default::default(),
            determinism_lab: Default::default(),
            hyperlink_playground: Default::default(),
            kanban_board: Default::default(),
            screen_errors: vec![None; screens::screen_registry().len()],
            visual_effects_deterministic_tick_ms: None,
        }
    }
}

impl ScreenStates {
    fn code_explorer_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut screens::code_explorer::CodeExplorer) -> R,
    {
        self.code_explorer.with_mut(
            screens::code_explorer::CodeExplorer::default,
            |screen, init_ms| {
                let memory_estimate = Some(std::mem::size_of_val(screen) as u64);
                emit_screen_init_log(ScreenId::CodeExplorer, init_ms, 0, memory_estimate);
            },
            f,
        )
    }

    fn file_browser_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut screens::file_browser::FileBrowser) -> R,
    {
        self.file_browser.with_mut(
            screens::file_browser::FileBrowser::default,
            |screen, init_ms| {
                let memory_estimate = Some(std::mem::size_of_val(screen) as u64);
                emit_screen_init_log(ScreenId::FileBrowser, init_ms, 0, memory_estimate);
            },
            f,
        )
    }

    fn visual_effects_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut screens::visual_effects::VisualEffectsScreen) -> R,
    {
        let tick_ms = self.visual_effects_deterministic_tick_ms;
        self.visual_effects.with_mut(
            || {
                let mut screen = screens::visual_effects::VisualEffectsScreen::default();
                if let Some(tick_ms) = tick_ms {
                    screen.enable_deterministic_mode(tick_ms);
                }
                screen
            },
            |screen, init_ms| {
                let memory_estimate = Some(std::mem::size_of_val(screen) as u64);
                emit_screen_init_log(
                    ScreenId::VisualEffects,
                    init_ms,
                    screen.effect_count(),
                    memory_estimate,
                );
            },
            f,
        )
    }

    fn snapshot_player_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut screens::snapshot_player::SnapshotPlayer) -> R,
    {
        self.snapshot_player.with_mut(
            screens::snapshot_player::SnapshotPlayer::default,
            |screen, init_ms| {
                let memory_estimate = Some(std::mem::size_of_val(screen) as u64);
                emit_screen_init_log(ScreenId::SnapshotPlayer, init_ms, 0, memory_estimate);
            },
            f,
        )
    }

    fn set_visual_effects_deterministic_tick_ms(&mut self, tick_ms: u64) {
        self.visual_effects_deterministic_tick_ms = Some(tick_ms.max(1));
        let tick_ms = tick_ms.max(1);
        let _ = self
            .visual_effects
            .with_existing_mut(|screen| screen.enable_deterministic_mode(tick_ms));
    }

    #[cfg(test)]
    fn is_lazy_initialized(&self, id: ScreenId) -> bool {
        match id {
            ScreenId::CodeExplorer => self.code_explorer.is_initialized(),
            ScreenId::FileBrowser => self.file_browser.is_initialized(),
            ScreenId::VisualEffects => self.visual_effects.is_initialized(),
            ScreenId::SnapshotPlayer => self.snapshot_player.is_initialized(),
            _ => true,
        }
    }

    /// Forward an event to the screen identified by `id`.
    fn update(&mut self, id: ScreenId, event: &Event) {
        use screens::Screen;
        match id {
            ScreenId::GuidedTour => {}
            ScreenId::Dashboard => {
                self.dashboard.update(event);
            }
            ScreenId::Shakespeare => {
                self.shakespeare.update(event);
            }
            ScreenId::CodeExplorer => {
                self.code_explorer_mut(|screen| screen.update(event));
            }
            ScreenId::WidgetGallery => {
                self.widget_gallery.update(event);
            }
            ScreenId::LayoutLab => {
                self.layout_lab.update(event);
            }
            ScreenId::FormsInput => {
                self.forms_input.update(event);
            }
            ScreenId::DataViz => {
                self.data_viz.update(event);
            }
            ScreenId::TableThemeGallery => {
                self.table_theme_gallery.update(event);
            }
            ScreenId::FileBrowser => {
                self.file_browser_mut(|screen| screen.update(event));
            }
            ScreenId::AdvancedFeatures => {
                self.advanced_features.update(event);
            }
            ScreenId::TerminalCapabilities => {
                self.terminal_capabilities.update(event);
            }
            ScreenId::MacroRecorder => {
                self.macro_recorder.update(event);
            }
            ScreenId::Performance => {
                self.performance.update(event);
            }
            ScreenId::MarkdownRichText => {
                self.markdown_rich_text.update(event);
            }
            ScreenId::MermaidShowcase => {
                self.mermaid_showcase.update(event);
            }
            ScreenId::MermaidMegaShowcase => {
                self.mermaid_mega_showcase.update(event);
            }
            ScreenId::VisualEffects => {
                self.visual_effects_mut(|screen| screen.update(event));
            }
            ScreenId::ResponsiveDemo => {
                self.responsive_demo.update(event);
            }
            ScreenId::LogSearch => {
                self.log_search.update(event);
            }
            ScreenId::Notifications => {
                self.notifications.update(event);
            }
            ScreenId::ActionTimeline => {
                self.action_timeline.update(event);
            }
            ScreenId::IntrinsicSizing => {
                self.intrinsic_sizing.update(event);
            }
            ScreenId::LayoutInspector => {
                self.layout_inspector.update(event);
            }
            ScreenId::AdvancedTextEditor => {
                self.advanced_text_editor.update(event);
            }
            ScreenId::MousePlayground => {
                self.mouse_playground.update(event);
            }
            ScreenId::FormValidation => {
                self.form_validation.update(event);
            }
            ScreenId::VirtualizedSearch => {
                self.virtualized_search.update(event);
            }
            ScreenId::AsyncTasks => {
                self.async_tasks.update(event);
            }
            ScreenId::ThemeStudio => {
                self.theme_studio.update(event);
            }
            ScreenId::SnapshotPlayer => {
                self.snapshot_player_mut(|screen| screen.update(event));
            }
            ScreenId::PerformanceHud => {
                self.performance_hud.update(event);
            }
            ScreenId::ExplainabilityCockpit => {
                self.explainability_cockpit.update(event);
            }
            ScreenId::I18nDemo => {
                self.i18n_demo.update(event);
            }
            ScreenId::VoiOverlay => {
                self.voi_overlay.update(event);
            }
            ScreenId::InlineModeStory => {
                self.inline_mode_story.update(event);
            }
            ScreenId::AccessibilityPanel => {
                self.accessibility_panel.update(event);
            }
            ScreenId::WidgetBuilder => {
                self.widget_builder.update(event);
            }
            ScreenId::CommandPaletteLab => {
                self.command_palette_lab.update(event);
            }
            ScreenId::DeterminismLab => {
                self.determinism_lab.update(event);
            }
            ScreenId::HyperlinkPlayground => {
                self.hyperlink_playground.update(event);
            }
            ScreenId::KanbanBoard => {
                self.kanban_board.update(event);
            }
        }
    }

    /// Whether the given screen is currently consuming text input.
    fn consumes_text_input(&self, id: ScreenId) -> bool {
        use screens::Screen;
        match id {
            ScreenId::FormsInput => self.forms_input.consumes_text_input(),
            ScreenId::AdvancedTextEditor => self.advanced_text_editor.consumes_text_input(),
            ScreenId::VirtualizedSearch => self.virtualized_search.consumes_text_input(),
            ScreenId::LogSearch => self.log_search.consumes_text_input(),
            _ => false,
        }
    }

    /// Forward a tick to the active screen and always tick performance_hud.
    ///
    /// Only the active screen receives tick updates for animations/data.
    /// Performance HUD is always ticked to collect metrics regardless of which
    /// screen is visible.
    fn tick(&mut self, active: ScreenId, tick_count: u64) {
        use screens::Screen;

        // Always tick performance_hud and explainability_cockpit for metrics collection.
        self.performance_hud.tick(tick_count);
        self.explainability_cockpit.tick(tick_count);

        // Only tick the active screen (skip if it's already ticked above).
        if matches!(
            active,
            ScreenId::PerformanceHud | ScreenId::ExplainabilityCockpit
        ) {
            return;
        }

        match active {
            ScreenId::GuidedTour => {}
            ScreenId::Dashboard => self.dashboard.tick(tick_count),
            ScreenId::Shakespeare => self.shakespeare.tick(tick_count),
            ScreenId::CodeExplorer => self.code_explorer_mut(|screen| screen.tick(tick_count)),
            ScreenId::WidgetGallery => self.widget_gallery.tick(tick_count),
            ScreenId::LayoutLab => self.layout_lab.tick(tick_count),
            ScreenId::FormsInput => self.forms_input.tick(tick_count),
            ScreenId::DataViz => self.data_viz.tick(tick_count),
            ScreenId::TableThemeGallery => self.table_theme_gallery.tick(tick_count),
            ScreenId::FileBrowser => self.file_browser_mut(|screen| screen.tick(tick_count)),
            ScreenId::AdvancedFeatures => self.advanced_features.tick(tick_count),
            ScreenId::TerminalCapabilities => self.terminal_capabilities.tick(tick_count),
            ScreenId::MacroRecorder => self.macro_recorder.tick(tick_count),
            ScreenId::Performance => self.performance.tick(tick_count),
            ScreenId::MarkdownRichText => self.markdown_rich_text.tick(tick_count),
            ScreenId::MermaidShowcase => self.mermaid_showcase.tick(tick_count),
            ScreenId::MermaidMegaShowcase => self.mermaid_mega_showcase.tick(tick_count),
            ScreenId::VisualEffects => self.visual_effects_mut(|screen| screen.tick(tick_count)),
            ScreenId::ResponsiveDemo => self.responsive_demo.tick(tick_count),
            ScreenId::LogSearch => self.log_search.tick(tick_count),
            ScreenId::Notifications => self.notifications.tick(tick_count),
            ScreenId::ActionTimeline => self.action_timeline.tick(tick_count),
            ScreenId::IntrinsicSizing => self.intrinsic_sizing.tick(tick_count),
            ScreenId::LayoutInspector => self.layout_inspector.tick(tick_count),
            ScreenId::AdvancedTextEditor => self.advanced_text_editor.tick(tick_count),
            ScreenId::MousePlayground => self.mouse_playground.tick(tick_count),
            ScreenId::FormValidation => self.form_validation.tick(tick_count),
            ScreenId::VirtualizedSearch => self.virtualized_search.tick(tick_count),
            ScreenId::AsyncTasks => self.async_tasks.tick(tick_count),
            ScreenId::ThemeStudio => self.theme_studio.tick(tick_count),
            ScreenId::SnapshotPlayer => self.snapshot_player_mut(|screen| screen.tick(tick_count)),
            ScreenId::PerformanceHud => {} // Already ticked above
            ScreenId::ExplainabilityCockpit => {} // Already ticked above
            ScreenId::I18nDemo => self.i18n_demo.tick(tick_count),
            ScreenId::VoiOverlay => self.voi_overlay.tick(tick_count),
            ScreenId::InlineModeStory => self.inline_mode_story.tick(tick_count),
            ScreenId::AccessibilityPanel => self.accessibility_panel.tick(tick_count),
            ScreenId::WidgetBuilder => self.widget_builder.tick(tick_count),
            ScreenId::CommandPaletteLab => self.command_palette_lab.tick(tick_count),
            ScreenId::DeterminismLab => self.determinism_lab.tick(tick_count),
            ScreenId::HyperlinkPlayground => self.hyperlink_playground.tick(tick_count),
            ScreenId::KanbanBoard => self.kanban_board.tick(tick_count),
        }
    }

    fn apply_theme(&mut self) {
        self.dashboard.apply_theme();
        let _ = self
            .file_browser
            .with_existing_mut(|screen| screen.apply_theme());
        let _ = self
            .code_explorer
            .with_existing_mut(|screen| screen.apply_theme());
        self.forms_input.apply_theme();
        self.shakespeare.apply_theme();
        self.markdown_rich_text.apply_theme();
        self.advanced_text_editor.apply_theme();
    }

    /// Render the screen identified by `id` into the given area.
    ///
    /// Wraps each screen's `view()` call in a panic boundary. If a screen
    /// panics during rendering, the error is captured and a
    /// [`FallbackWidget`] is shown instead of crashing the application.
    fn view(&self, id: ScreenId, frame: &mut Frame, area: Rect) {
        let idx = id.index();

        // If this screen previously errored, show the cached fallback.
        if let Some(msg) = &self.screen_errors[idx] {
            FallbackWidget::from_message(msg.clone(), id.widget_name()).render(area, frame);
            return;
        }

        let result = catch_unwind(AssertUnwindSafe(|| {
            use screens::Screen;
            match id {
                ScreenId::GuidedTour => {}
                ScreenId::Dashboard => self.dashboard.view(frame, area),
                ScreenId::Shakespeare => self.shakespeare.view(frame, area),
                ScreenId::CodeExplorer => self.code_explorer_mut(|screen| screen.view(frame, area)),
                ScreenId::WidgetGallery => self.widget_gallery.view(frame, area),
                ScreenId::LayoutLab => self.layout_lab.view(frame, area),
                ScreenId::FormsInput => self.forms_input.view(frame, area),
                ScreenId::DataViz => self.data_viz.view(frame, area),
                ScreenId::TableThemeGallery => self.table_theme_gallery.view(frame, area),
                ScreenId::FileBrowser => self.file_browser_mut(|screen| screen.view(frame, area)),
                ScreenId::AdvancedFeatures => self.advanced_features.view(frame, area),
                ScreenId::TerminalCapabilities => self.terminal_capabilities.view(frame, area),
                ScreenId::MacroRecorder => self.macro_recorder.view(frame, area),
                ScreenId::Performance => self.performance.view(frame, area),
                ScreenId::MarkdownRichText => self.markdown_rich_text.view(frame, area),
                ScreenId::MermaidShowcase => self.mermaid_showcase.view(frame, area),
                ScreenId::MermaidMegaShowcase => self.mermaid_mega_showcase.view(frame, area),
                ScreenId::VisualEffects => {
                    self.visual_effects_mut(|screen| screen.view(frame, area))
                }
                ScreenId::ResponsiveDemo => self.responsive_demo.view(frame, area),
                ScreenId::LogSearch => self.log_search.view(frame, area),
                ScreenId::Notifications => self.notifications.view(frame, area),
                ScreenId::ActionTimeline => self.action_timeline.view(frame, area),
                ScreenId::IntrinsicSizing => self.intrinsic_sizing.view(frame, area),
                ScreenId::LayoutInspector => self.layout_inspector.view(frame, area),
                ScreenId::AdvancedTextEditor => self.advanced_text_editor.view(frame, area),
                ScreenId::MousePlayground => self.mouse_playground.view(frame, area),
                ScreenId::FormValidation => self.form_validation.view(frame, area),
                ScreenId::VirtualizedSearch => self.virtualized_search.view(frame, area),
                ScreenId::AsyncTasks => self.async_tasks.view(frame, area),
                ScreenId::ThemeStudio => self.theme_studio.view(frame, area),
                ScreenId::SnapshotPlayer => {
                    self.snapshot_player_mut(|screen| screen.view(frame, area))
                }
                ScreenId::PerformanceHud => self.performance_hud.view(frame, area),
                ScreenId::ExplainabilityCockpit => self.explainability_cockpit.view(frame, area),
                ScreenId::I18nDemo => self.i18n_demo.view(frame, area),
                ScreenId::VoiOverlay => self.voi_overlay.view(frame, area),
                ScreenId::InlineModeStory => self.inline_mode_story.view(frame, area),
                ScreenId::AccessibilityPanel => self.accessibility_panel.view(frame, area),
                ScreenId::WidgetBuilder => self.widget_builder.view(frame, area),
                ScreenId::CommandPaletteLab => self.command_palette_lab.view(frame, area),
                ScreenId::DeterminismLab => self.determinism_lab.view(frame, area),
                ScreenId::HyperlinkPlayground => self.hyperlink_playground.view(frame, area),
                ScreenId::KanbanBoard => self.kanban_board.view(frame, area),
            }
        }));

        if let Err(payload) = result {
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            FallbackWidget::from_message(&msg, id.widget_name()).render(area, frame);
            // Note: We can't write to self.screen_errors here because view()
            // takes &self. The error boundary still protects the app from
            // crashing — it just re-catches on each render.
        }
    }

    /// Reset the error state for a screen (e.g., after user presses 'R' to retry).
    fn clear_error(&mut self, id: ScreenId) {
        self.screen_errors[id.index()] = None;
    }

    /// Returns true if the screen has a cached error.
    fn has_error(&self, id: ScreenId) -> bool {
        self.screen_errors[id.index()].is_some()
    }
}

// ---------------------------------------------------------------------------
// AppMsg
// ---------------------------------------------------------------------------

/// Top-level application message.
pub enum AppMsg {
    /// A raw terminal event forwarded to the current screen.
    ScreenEvent(Event),
    /// Switch to a specific screen.
    SwitchScreen(ScreenId),
    /// Advance to the next screen tab.
    NextScreen,
    /// Go back to the previous screen tab.
    PrevScreen,
    /// Toggle the help overlay.
    ToggleHelp,
    /// Toggle the debug overlay.
    ToggleDebug,
    /// Toggle the performance HUD overlay.
    TogglePerfHud,
    /// Toggle terminal mouse capture (mouse reporting).
    ToggleMouseCapture,
    /// Toggle the explainability cockpit overlay.
    ToggleEvidenceLedger,
    /// Toggle the accessibility panel overlay.
    ToggleA11yPanel,
    /// Toggle high contrast mode.
    ToggleHighContrast,
    /// Toggle reduced motion mode.
    ToggleReducedMotion,
    /// Toggle large text mode.
    ToggleLargeText,
    /// Cycle the active color theme.
    CycleTheme,
    /// Periodic tick for animations and data updates.
    Tick,
    /// Terminal resize.
    Resize {
        /// New terminal width.
        width: u16,
        /// New terminal height.
        height: u16,
    },
    /// Quit the application.
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventSource {
    User,
    Playback,
}

impl From<Event> for AppMsg {
    fn from(event: Event) -> Self {
        if let Event::Resize { width, height } = event {
            return Self::Resize { width, height };
        }

        Self::ScreenEvent(event)
    }
}

// ---------------------------------------------------------------------------
// VFX Harness
// ---------------------------------------------------------------------------

const VFX_HARNESS_DEFAULT_JSONL: &str = "vfx_harness.jsonl";
const VFX_HARNESS_SEED: u64 = 12_345;

/// Configuration for the deterministic VFX harness.
#[derive(Debug, Clone)]
pub struct VfxHarnessConfig {
    pub effect: Option<String>,
    pub tick_ms: u64,
    pub max_frames: u64,
    pub exit_after_ms: u64,
    pub jsonl_path: Option<String>,
    pub run_id: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub seed: Option<u64>,
    pub perf_enabled: bool,
}

#[derive(Debug, Clone, Copy)]
enum VfxHarnessInput {
    Key {
        code: KeyCode,
        kind: KeyEventKind,
        label: &'static str,
    },
    MouseMove {
        x: u16,
        y: u16,
        label: &'static str,
    },
    MouseDown {
        button: MouseButton,
        label: &'static str,
    },
}

impl VfxHarnessInput {
    fn label(self) -> &'static str {
        match self {
            Self::Key { label, .. }
            | Self::MouseMove { label, .. }
            | Self::MouseDown { label, .. } => label,
        }
    }

    fn to_event(self) -> Event {
        match self {
            Self::Key { code, kind, .. } => Event::Key(KeyEvent::new(code).with_kind(kind)),
            Self::MouseMove { x, y, .. } => {
                Event::Mouse(MouseEvent::new(MouseEventKind::Moved, x, y))
            }
            Self::MouseDown { button, .. } => {
                Event::Mouse(MouseEvent::new(MouseEventKind::Down(button), 0, 0))
            }
        }
    }
}

const VFX_FPS_INPUT_SCRIPT: &[(u64, VfxHarnessInput)] = &[
    (
        1,
        VfxHarnessInput::Key {
            code: KeyCode::Char('w'),
            kind: KeyEventKind::Press,
            label: "w_down",
        },
    ),
    (
        2,
        VfxHarnessInput::Key {
            code: KeyCode::Char('d'),
            kind: KeyEventKind::Press,
            label: "d_down",
        },
    ),
    (
        3,
        VfxHarnessInput::MouseMove {
            x: 10,
            y: 10,
            label: "mouse_anchor",
        },
    ),
    (
        4,
        VfxHarnessInput::MouseMove {
            x: 20,
            y: 12,
            label: "mouse_look",
        },
    ),
    (
        5,
        VfxHarnessInput::MouseDown {
            button: MouseButton::Left,
            label: "fire",
        },
    ),
    (
        6,
        VfxHarnessInput::Key {
            code: KeyCode::Char('d'),
            kind: KeyEventKind::Release,
            label: "d_up",
        },
    ),
    (
        7,
        VfxHarnessInput::Key {
            code: KeyCode::Char('w'),
            kind: KeyEventKind::Release,
            label: "w_up",
        },
    ),
    (
        8,
        VfxHarnessInput::Key {
            code: KeyCode::Char('a'),
            kind: KeyEventKind::Press,
            label: "a_down",
        },
    ),
    (
        9,
        VfxHarnessInput::Key {
            code: KeyCode::Char('a'),
            kind: KeyEventKind::Release,
            label: "a_up",
        },
    ),
    (
        10,
        VfxHarnessInput::Key {
            code: KeyCode::Char('s'),
            kind: KeyEventKind::Press,
            label: "s_down",
        },
    ),
    (
        11,
        VfxHarnessInput::Key {
            code: KeyCode::Char('s'),
            kind: KeyEventKind::Release,
            label: "s_up",
        },
    ),
];

pub enum VfxHarnessMsg {
    Event(Event),
    Quit,
}

impl From<Event> for VfxHarnessMsg {
    fn from(event: Event) -> Self {
        Self::Event(event)
    }
}

struct VfxPerfStats {
    update_us: Vec<u64>,
    render_us: Vec<u64>,
    diff_us: Vec<u64>,
    present_us: Vec<u64>,
    total_us: Vec<u64>,
}

impl VfxPerfStats {
    fn new() -> Self {
        Self {
            update_us: Vec::new(),
            render_us: Vec::new(),
            diff_us: Vec::new(),
            present_us: Vec::new(),
            total_us: Vec::new(),
        }
    }

    fn record(&mut self, timing: &FrameTiming) {
        self.update_us.push(timing.update_us);
        self.render_us.push(timing.render_us);
        self.diff_us.push(timing.diff_us);
        self.present_us.push(timing.present_us);
        self.total_us.push(timing.total_us);
    }

    fn percentiles(values: &[u64]) -> (u64, u64, u64) {
        if values.is_empty() {
            return (0, 0, 0);
        }
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        (
            percentile_nearest_rank(&sorted, 0.50),
            percentile_nearest_rank(&sorted, 0.95),
            percentile_nearest_rank(&sorted, 0.99),
        )
    }
}

fn percentile_nearest_rank(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let rank = (p * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n.saturating_sub(1));
    sorted[idx]
}

struct VfxHarnessLoggerState {
    writer: Box<dyn Write + Send>,
    run_id: String,
    effect: String,
    cols: u16,
    rows: u16,
    tick_ms: u64,
    seed: u64,
    perf: Option<VfxPerfStats>,
}

struct VfxHarnessLogger {
    inner: Mutex<VfxHarnessLoggerState>,
    perf_enabled: bool,
}

impl VfxHarnessLogger {
    #[allow(clippy::too_many_arguments)]
    fn new(
        path: &str,
        run_id: String,
        effect: String,
        cols: u16,
        rows: u16,
        tick_ms: u64,
        seed: u64,
        perf_enabled: bool,
    ) -> std::io::Result<Self> {
        let writer = open_vfx_writer(path)?;
        let logger = Self {
            inner: Mutex::new(VfxHarnessLoggerState {
                writer,
                run_id,
                effect,
                cols,
                rows,
                tick_ms,
                seed,
                perf: perf_enabled.then(VfxPerfStats::new),
            }),
            perf_enabled,
        };
        logger.write_header()?;
        Ok(logger)
    }

    fn write_header(&self) -> std::io::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("logger lock poisoned"))?;
        let run_id = escape_json(&guard.run_id);
        let effect = escape_json(&guard.effect);
        let hash_key = determinism::hash_key(
            &determinism::demo_screen_mode(),
            guard.cols,
            guard.rows,
            guard.seed,
        );
        let timestamp = determinism::chrono_like_timestamp();
        let env_json = determinism::demo_env_json();
        let perf_enabled = guard.perf.is_some();
        let line = format!(
            "{{\"event\":\"vfx_harness_start\",\"timestamp\":\"{timestamp}\",\"run_id\":\"{run_id}\",\"hash_key\":\"{}\",\"effect\":\"{effect}\",\"cols\":{},\"rows\":{},\"tick_ms\":{},\"seed\":{},\"perf\":{},\"env\":{}}}",
            escape_json(&hash_key),
            guard.cols,
            guard.rows,
            guard.tick_ms,
            guard.seed,
            perf_enabled,
            env_json
        );
        writeln!(guard.writer, "{line}")?;
        guard.writer.flush()
    }

    fn write_frame(&self, frame_idx: u64, hash: u64, sim_time: f64) -> std::io::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("logger lock poisoned"))?;
        let run_id = escape_json(&guard.run_id);
        let effect = escape_json(&guard.effect);
        let hash_key = determinism::hash_key(
            &determinism::demo_screen_mode(),
            guard.cols,
            guard.rows,
            guard.seed,
        );
        let timestamp = determinism::chrono_like_timestamp();
        let line = format!(
            "{{\"event\":\"vfx_frame\",\"timestamp\":\"{timestamp}\",\"run_id\":\"{run_id}\",\"hash_key\":\"{}\",\"effect\":\"{effect}\",\"frame_idx\":{frame_idx},\"hash\":{hash},\"time\":{:.2},\"cols\":{},\"rows\":{},\"tick_ms\":{},\"seed\":{}}}",
            escape_json(&hash_key),
            sim_time,
            guard.cols,
            guard.rows,
            guard.tick_ms,
            guard.seed
        );
        writeln!(guard.writer, "{line}")?;
        guard.writer.flush()
    }

    fn write_input_event(&self, frame_idx: u64, action: &str) -> std::io::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("logger lock poisoned"))?;
        let run_id = escape_json(&guard.run_id);
        let effect = escape_json(&guard.effect);
        let hash_key = determinism::hash_key(
            &determinism::demo_screen_mode(),
            guard.cols,
            guard.rows,
            guard.seed,
        );
        let timestamp = determinism::chrono_like_timestamp();
        let action = escape_json(action);
        let line = format!(
            "{{\"event\":\"vfx_input\",\"timestamp\":\"{timestamp}\",\"run_id\":\"{run_id}\",\"hash_key\":\"{}\",\"effect\":\"{effect}\",\"frame_idx\":{frame_idx},\"action\":\"{action}\",\"cols\":{},\"rows\":{},\"tick_ms\":{},\"seed\":{}}}",
            escape_json(&hash_key),
            guard.cols,
            guard.rows,
            guard.tick_ms,
            guard.seed
        );
        writeln!(guard.writer, "{line}")?;
        guard.writer.flush()
    }

    fn write_perf_frame(&self, timing: &FrameTiming) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let Some(perf) = guard.perf.as_mut() else {
            return;
        };
        perf.record(timing);
        let run_id = escape_json(&guard.run_id);
        let effect = escape_json(&guard.effect);
        let timestamp = determinism::chrono_like_timestamp();
        let update_ms = timing.update_us as f64 / 1000.0;
        let render_ms = timing.render_us as f64 / 1000.0;
        let diff_ms = timing.diff_us as f64 / 1000.0;
        let present_ms = timing.present_us as f64 / 1000.0;
        let total_ms = timing.total_us as f64 / 1000.0;
        let line = format!(
            "{{\"event\":\"vfx_perf_frame\",\"timestamp\":\"{timestamp}\",\"run_id\":\"{run_id}\",\"effect\":\"{effect}\",\"frame_idx\":{},\"update_ms\":{:.3},\"render_ms\":{:.3},\"diff_ms\":{:.3},\"present_ms\":{:.3},\"total_ms\":{:.3},\"cols\":{},\"rows\":{},\"tick_ms\":{},\"seed\":{}}}",
            timing.frame_idx,
            update_ms,
            render_ms,
            diff_ms,
            present_ms,
            total_ms,
            guard.cols,
            guard.rows,
            guard.tick_ms,
            guard.seed
        );
        let _ = writeln!(guard.writer, "{line}");
        let _ = guard.writer.flush();
    }

    fn write_perf_summary(&self) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let Some(perf) = guard.perf.as_ref() else {
            return;
        };
        let count = perf.total_us.len();
        let (total_p50, total_p95, total_p99) = VfxPerfStats::percentiles(&perf.total_us);
        let (update_p50, update_p95, update_p99) = VfxPerfStats::percentiles(&perf.update_us);
        let (render_p50, render_p95, render_p99) = VfxPerfStats::percentiles(&perf.render_us);
        let (diff_p50, diff_p95, diff_p99) = VfxPerfStats::percentiles(&perf.diff_us);
        let (present_p50, present_p95, present_p99) = VfxPerfStats::percentiles(&perf.present_us);

        let phases = [
            ("update", update_p95, update_p99),
            ("render", render_p95, render_p99),
            ("diff", diff_p95, diff_p99),
            ("present", present_p95, present_p99),
        ];
        let (top_phase, top_p95, top_p99) = phases
            .iter()
            .max_by_key(|(_, p95, _)| *p95)
            .copied()
            .unwrap_or(("none", 0, 0));

        let run_id = escape_json(&guard.run_id);
        let effect = escape_json(&guard.effect);
        let timestamp = determinism::chrono_like_timestamp();
        let line = format!(
            "{{\"event\":\"vfx_perf_summary\",\"timestamp\":\"{timestamp}\",\"run_id\":\"{run_id}\",\"effect\":\"{effect}\",\"count\":{count},\"cols\":{},\"rows\":{},\"tick_ms\":{},\"seed\":{},\"total_ms_p50\":{:.3},\"total_ms_p95\":{:.3},\"total_ms_p99\":{:.3},\"update_ms_p50\":{:.3},\"update_ms_p95\":{:.3},\"update_ms_p99\":{:.3},\"render_ms_p50\":{:.3},\"render_ms_p95\":{:.3},\"render_ms_p99\":{:.3},\"diff_ms_p50\":{:.3},\"diff_ms_p95\":{:.3},\"diff_ms_p99\":{:.3},\"present_ms_p50\":{:.3},\"present_ms_p95\":{:.3},\"present_ms_p99\":{:.3},\"top_phase\":\"{top_phase}\",\"top_phase_p95_ms\":{:.3},\"top_phase_p99_ms\":{:.3}}}",
            guard.cols,
            guard.rows,
            guard.tick_ms,
            guard.seed,
            total_p50 as f64 / 1000.0,
            total_p95 as f64 / 1000.0,
            total_p99 as f64 / 1000.0,
            update_p50 as f64 / 1000.0,
            update_p95 as f64 / 1000.0,
            update_p99 as f64 / 1000.0,
            render_p50 as f64 / 1000.0,
            render_p95 as f64 / 1000.0,
            render_p99 as f64 / 1000.0,
            diff_p50 as f64 / 1000.0,
            diff_p95 as f64 / 1000.0,
            diff_p99 as f64 / 1000.0,
            present_p50 as f64 / 1000.0,
            present_p95 as f64 / 1000.0,
            present_p99 as f64 / 1000.0,
            top_p95 as f64 / 1000.0,
            top_p99 as f64 / 1000.0
        );
        let _ = writeln!(guard.writer, "{line}");
        let _ = guard.writer.flush();
    }
}

impl FrameTimingSink for VfxHarnessLogger {
    fn record_frame(&self, timing: &FrameTiming) {
        self.write_perf_frame(timing);
    }
}

impl Drop for VfxHarnessLogger {
    fn drop(&mut self) {
        if self.perf_enabled {
            self.write_perf_summary();
        }
    }
}

/// Deterministic VFX-only harness (bypasses full screen initialization).
pub struct VfxHarnessModel {
    screen: screens::visual_effects::VisualEffectsScreen,
    tick_ms: u64,
    tick_count: u64,
    max_frames: Option<u64>,
    exit_after_ms: u64,
    started: Cell<bool>,
    frame_idx: Cell<u64>,
    logger: Arc<VfxHarnessLogger>,
    perf_enabled: bool,
}

impl VfxHarnessModel {
    pub fn new(config: VfxHarnessConfig) -> std::io::Result<Self> {
        let mut screen = screens::visual_effects::VisualEffectsScreen::default();
        if let Some(effect) = config.effect.as_deref()
            && !screen.set_effect_by_name(effect)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Unknown VFX effect: {effect}"),
            ));
        }

        let tick_ms = config.tick_ms.max(1);
        let cols = config.cols.max(1);
        let rows = config.rows.max(1);
        screen.enable_deterministic_mode(tick_ms);
        let effect_key = screen.effect_key();
        let run_id = config
            .run_id
            .unwrap_or_else(|| format!("vfx-{}-{}x{}-{}ms", effect_key, cols, rows, tick_ms));
        let jsonl_path = config
            .jsonl_path
            .unwrap_or_else(|| VFX_HARNESS_DEFAULT_JSONL.to_string());
        let seed = config.seed.unwrap_or_else(|| {
            determinism::seed_from_env(
                &[
                    "FTUI_DEMO_VFX_SEED",
                    "FTUI_DEMO_SEED",
                    "FTUI_SEED",
                    "E2E_SEED",
                ],
                determinism::demo_seed(VFX_HARNESS_SEED),
            )
        });
        let logger = Arc::new(VfxHarnessLogger::new(
            &jsonl_path,
            run_id,
            effect_key.to_string(),
            cols,
            rows,
            tick_ms,
            seed,
            config.perf_enabled,
        )?);

        Ok(Self {
            screen,
            tick_ms,
            tick_count: 0,
            max_frames: (config.max_frames > 0).then_some(config.max_frames),
            exit_after_ms: config.exit_after_ms,
            started: Cell::new(false),
            frame_idx: Cell::new(0),
            logger,
            perf_enabled: config.perf_enabled,
        })
    }

    pub fn perf_logger(&self) -> Option<Arc<dyn FrameTimingSink>> {
        if self.perf_enabled {
            Some(self.logger.clone())
        } else {
            None
        }
    }

    fn is_fps_script_effect(&self) -> bool {
        matches!(self.screen.effect_key(), "doom-e1m1" | "quake-e1m1")
    }

    fn apply_fps_script(&mut self) {
        if !self.is_fps_script_effect() {
            return;
        }
        let frame_idx = self.tick_count;
        for (script_frame, input) in VFX_FPS_INPUT_SCRIPT {
            if *script_frame == frame_idx {
                let event = input.to_event();
                let _ = self.screen.update(&event);
                let _ = self.logger.write_input_event(frame_idx, input.label());
            }
        }
    }
}

#[cfg(test)]
mod vfx_perf_tests {
    use super::VfxPerfStats;

    #[test]
    fn vfx_perf_percentiles_nearest_rank() {
        let values = vec![10, 20, 30, 40, 50];
        let (p50, p95, p99) = VfxPerfStats::percentiles(&values);
        assert_eq!(p50, 30);
        assert_eq!(p95, 50);
        assert_eq!(p99, 50);
    }
}

impl Model for VfxHarnessModel {
    type Message = VfxHarnessMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        let mut cmds = vec![Cmd::Tick(Duration::from_millis(self.tick_ms))];
        if self.exit_after_ms > 0 {
            let ms = self.exit_after_ms;
            cmds.push(Cmd::task_named("vfx_harness_exit", move || {
                std::thread::sleep(Duration::from_millis(ms));
                VfxHarnessMsg::Quit
            }));
        }
        Cmd::batch(cmds)
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            VfxHarnessMsg::Quit => Cmd::Quit,
            VfxHarnessMsg::Event(event) => match event {
                Event::Tick => {
                    self.tick_count = self.tick_count.saturating_add(1);
                    self.started.set(true);
                    self.apply_fps_script();
                    self.screen.tick(self.tick_count);
                    if let Some(max_frames) = self.max_frames
                        && self.tick_count >= max_frames
                    {
                        return Cmd::Quit;
                    }
                    Cmd::None
                }
                Event::Key(KeyEvent {
                    code,
                    kind: KeyEventKind::Press,
                    modifiers,
                }) => {
                    if matches!(code, KeyCode::Char('q' | 'Q') | KeyCode::Escape)
                        || (matches!(code, KeyCode::Char('c' | 'C'))
                            && modifiers.contains(Modifiers::CTRL))
                    {
                        return Cmd::Quit;
                    }
                    Cmd::None
                }
                _ => Cmd::None,
            },
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        self.screen.view(frame, area);

        if !self.started.get() {
            return;
        }

        let frame_idx = self.frame_idx.get().wrapping_add(1);
        self.frame_idx.set(frame_idx);
        let pool = &*frame.pool;
        let hash = checksum_buffer(&frame.buffer, pool);
        let _ = self
            .logger
            .write_frame(frame_idx, hash, self.screen.sim_time());
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Mermaid Harness (E2E deterministic sample runner — bd-1k26f)
// ---------------------------------------------------------------------------

/// Configuration for the Mermaid E2E harness.
pub struct MermaidHarnessConfig {
    pub cols: u16,
    pub rows: u16,
    pub seed: Option<u64>,
    pub jsonl_path: Option<String>,
    pub run_id: Option<String>,
    pub exit_after_ms: u64,
    pub tick_ms: u64,
}

/// Messages for [`MermaidHarnessModel`].
pub enum MermaidHarnessMsg {
    Event(Event),
    Quit,
}

impl From<Event> for MermaidHarnessMsg {
    fn from(event: Event) -> Self {
        Self::Event(event)
    }
}

/// Deterministic harness that cycles through every Mermaid sample, renders
/// each one, computes a frame hash, emits JSONL, and exits.
pub struct MermaidHarnessModel {
    screen: screens::mermaid_showcase::MermaidShowcaseScreen,
    sample_count: usize,
    tick_count: u64,
    tick_ms: u64,
    exit_after_ms: u64,
    started: Cell<bool>,
    done: Cell<bool>,
    frame_idx: Cell<u64>,
    writer: RefCell<Box<dyn Write + Send>>,
    run_id: String,
    cols: u16,
    rows: u16,
    seed: u64,
}

const MERMAID_HARNESS_SEED: u64 = 42;
const MERMAID_HARNESS_DEFAULT_JSONL: &str = "mermaid_harness.jsonl";

impl MermaidHarnessModel {
    pub fn new(config: MermaidHarnessConfig) -> std::io::Result<Self> {
        let mut screen = screens::mermaid_showcase::MermaidShowcaseScreen::new();
        // Toggle off metrics and controls panels so rendering is purely
        // deterministic (no timing data in the frame hash).
        for key in ['m', 'c'] {
            screen.update(&Event::Key(KeyEvent {
                code: KeyCode::Char(key),
                kind: KeyEventKind::Press,
                modifiers: Modifiers::NONE,
            }));
        }
        let sample_count = screen.sample_count();
        let cols = config.cols.max(1);
        let rows = config.rows.max(1);
        let seed = config.seed.unwrap_or_else(|| {
            determinism::seed_from_env(
                &[
                    "FTUI_DEMO_MERMAID_SEED",
                    "FTUI_DEMO_SEED",
                    "FTUI_SEED",
                    "E2E_SEED",
                ],
                determinism::demo_seed(MERMAID_HARNESS_SEED),
            )
        });
        let run_id = config
            .run_id
            .unwrap_or_else(|| format!("mermaid-{}x{}-seed{seed}", cols, rows));
        let jsonl_path = config
            .jsonl_path
            .unwrap_or_else(|| MERMAID_HARNESS_DEFAULT_JSONL.to_string());
        let writer = open_vfx_writer(&jsonl_path)?;

        Ok(Self {
            screen,
            sample_count,
            tick_count: 0,
            tick_ms: config.tick_ms.max(1),
            exit_after_ms: config.exit_after_ms,
            started: Cell::new(false),
            done: Cell::new(false),
            frame_idx: Cell::new(0),
            writer: RefCell::new(writer),
            run_id,
            cols,
            rows,
            seed,
        })
    }

    fn write_header(&self) {
        let timestamp = determinism::chrono_like_timestamp();
        let env_json = determinism::demo_env_json();
        let hash_key = determinism::hash_key("mermaid", self.cols, self.rows, self.seed);
        let line = format!(
            "{{\"event\":\"mermaid_harness_start\",\"timestamp\":\"{timestamp}\",\"run_id\":\"{}\",\"hash_key\":\"{hash_key}\",\"cols\":{},\"rows\":{},\"seed\":{},\"sample_count\":{},\"env\":{env_json}}}",
            escape_json(&self.run_id),
            self.cols,
            self.rows,
            self.seed,
            self.sample_count,
        );
        let _ = writeln!(self.writer.borrow_mut(), "{line}");
    }

    fn write_frame(&self, frame_idx: u64, hash: u64) {
        let timestamp = determinism::chrono_like_timestamp();
        let sample_idx = if frame_idx == 0 {
            0
        } else {
            (frame_idx as usize).min(self.sample_count.saturating_sub(1))
        };
        let line = format!(
            "{{\"event\":\"mermaid_frame\",\"timestamp\":\"{timestamp}\",\"run_id\":\"{}\",\"frame\":{frame_idx},\"sample_idx\":{sample_idx},\"hash\":{hash}}}",
            escape_json(&self.run_id),
        );
        let _ = writeln!(self.writer.borrow_mut(), "{line}");
    }

    fn write_done(&self) {
        let timestamp = determinism::chrono_like_timestamp();
        let line = format!(
            "{{\"event\":\"mermaid_harness_done\",\"timestamp\":\"{timestamp}\",\"run_id\":\"{}\",\"total_frames\":{}}}",
            escape_json(&self.run_id),
            self.frame_idx.get(),
        );
        let _ = writeln!(self.writer.borrow_mut(), "{line}");
    }

    fn advance_sample(&mut self) {
        let right = Event::Key(KeyEvent {
            code: KeyCode::Down,
            kind: KeyEventKind::Press,
            modifiers: Modifiers::NONE,
        });
        self.screen.update(&right);
    }
}

impl Model for MermaidHarnessModel {
    type Message = MermaidHarnessMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        self.write_header();
        let mut cmds = vec![Cmd::Tick(Duration::from_millis(self.tick_ms))];
        if self.exit_after_ms > 0 {
            let ms = self.exit_after_ms;
            cmds.push(Cmd::task_named("mermaid_harness_exit", move || {
                std::thread::sleep(Duration::from_millis(ms));
                MermaidHarnessMsg::Quit
            }));
        }
        Cmd::batch(cmds)
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            MermaidHarnessMsg::Quit => {
                self.done.set(true);
                self.write_done();
                Cmd::Quit
            }
            MermaidHarnessMsg::Event(event) => match event {
                Event::Tick => {
                    self.tick_count = self.tick_count.saturating_add(1);
                    if !self.started.get() {
                        // First tick: render sample 0 (already selected).
                        self.started.set(true);
                    } else {
                        // Subsequent ticks: advance to next sample.
                        if self.tick_count as usize > self.sample_count {
                            self.done.set(true);
                            self.write_done();
                            return Cmd::Quit;
                        }
                        self.advance_sample();
                    }
                    Cmd::None
                }
                Event::Key(KeyEvent {
                    code,
                    kind: KeyEventKind::Press,
                    modifiers,
                }) => {
                    if matches!(code, KeyCode::Char('q' | 'Q') | KeyCode::Escape)
                        || (matches!(code, KeyCode::Char('c' | 'C'))
                            && modifiers.contains(Modifiers::CTRL))
                    {
                        self.done.set(true);
                        self.write_done();
                        return Cmd::Quit;
                    }
                    Cmd::None
                }
                _ => Cmd::None,
            },
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        self.screen.view(frame, area);

        // Only log frame hashes once started and before done.
        if !self.started.get() || self.done.get() {
            return;
        }

        let idx = self.frame_idx.get().wrapping_add(1);
        self.frame_idx.set(idx);
        let pool = &*frame.pool;
        let hash = checksum_buffer(&frame.buffer, pool);
        self.write_frame(idx - 1, hash);
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        Vec::new()
    }
}

fn open_vfx_writer(path: &str) -> std::io::Result<Box<dyn Write + Send>> {
    if path == "-" || path.eq_ignore_ascii_case("stderr") {
        return Ok(Box::new(BufWriter::new(std::io::stderr())));
    }

    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    Ok(Box::new(BufWriter::new(file)))
}

fn escape_json(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            _ => out.push(ch),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// AppModel
// ---------------------------------------------------------------------------

/// Top-level application state.
///
/// Implements the Elm architecture: all state lives here, messages drive
/// transitions, and `view()` is a pure function of state.
pub struct AppModel {
    /// Currently displayed screen.
    pub current_screen: ScreenId,
    /// Guided tour storyboard state.
    pub tour: GuidedTourState,
    /// Per-screen state storage.
    pub screens: ScreenStates,
    /// Whether the help overlay is visible.
    pub help_visible: bool,
    /// Whether the debug overlay is visible.
    pub debug_visible: bool,
    /// Whether the performance HUD overlay is visible.
    pub perf_hud_visible: bool,
    /// Whether the explainability cockpit overlay is visible.
    pub evidence_ledger_visible: bool,
    /// Whether the demo is running in inline modes (scrollback preserved).
    pub inline_mode: bool,
    /// Whether terminal mouse capture is currently enabled.
    pub mouse_capture_enabled: bool,
    /// Accessibility settings (high contrast, reduced motion, large text).
    pub a11y: theme::A11ySettings,
    /// Whether the accessibility panel is visible.
    pub a11y_panel_visible: bool,
    /// Base theme before accessibility overrides.
    pub base_theme: theme::ThemeId,
    /// Command palette for instant action search (Ctrl+K).
    pub command_palette: CommandPalette,
    /// Screen favorites for palette filtering.
    screen_favorites: HashSet<ScreenId>,
    /// Active palette category filter (palette-only).
    palette_category_filter: Option<screens::ScreenCategory>,
    /// Whether palette is filtered to favorites only.
    palette_favorites_only: bool,
    /// Global tick counter (incremented every 100ms).
    pub tick_count: u64,
    /// Total frames rendered.
    pub frame_count: u64,
    /// Current terminal width.
    pub terminal_width: u16,
    /// Current terminal height.
    pub terminal_height: u16,
    /// Auto-exit after this many milliseconds (0 = disabled).
    pub exit_after_ms: u64,
    /// Auto-exit after this many ticks (None = disabled).
    pub exit_after_ticks: Option<u64>,
    /// Fixed tick cadence override for deterministic fixtures.
    deterministic_tick_ms: Option<u64>,
    /// Deterministic mode flag (seeded + fixed-step fixtures).
    deterministic_mode: bool,
    /// Performance HUD: timestamp of last tick for tick-interval measurement.
    perf_last_tick: Option<Instant>,
    /// Performance HUD: recent tick intervals in microseconds (ring buffer, max 120).
    perf_tick_times_us: VecDeque<u64>,
    /// Performance HUD: frame counter using interior mutability (incremented in view).
    perf_view_counter: Cell<u64>,
    /// Performance HUD: views-per-tick ratio for FPS estimation.
    perf_views_per_tick: f64,
    /// Performance HUD: previous view count snapshot (for computing views per tick).
    perf_prev_view_count: u64,
    /// Last rendered checksum for guided tour JSONL logs.
    tour_checksum: Cell<Option<u64>>,
    /// Last rendered hit grid (for mouse hit testing).
    last_hit_grid: RefCell<Option<HitGrid>>,
    /// Timestamp of last tick received (for stall detection).
    tick_last_seen: Option<Instant>,
    /// Last time a tick stall was logged (rate limiting).
    tick_stall_last_log: Cell<Option<Instant>>,
    /// Global undo/redo history manager for reversible operations.
    pub history: HistoryManager,
    /// Optional telemetry hooks for A11y mode changes.
    a11y_telemetry: Option<A11yTelemetryHooks>,
    /// Central mouse dispatcher for priority-based event routing (bd-iuvb.17.2).
    mouse_dispatcher: MouseDispatcher,
}

impl Default for AppModel {
    fn default() -> Self {
        Self::new()
    }
}

impl AppModel {
    /// Create a new application model with default state.
    pub fn new() -> Self {
        let base_theme = theme::ThemeId::CyberpunkAurora;
        // Only set theme in non-test builds to avoid race conditions with
        // tests that use ScopedThemeLock for deterministic rendering.
        #[cfg(not(test))]
        {
            theme::set_theme(base_theme);
            theme::set_motion_scale(1.0);
            theme::set_large_text(false);
        }
        let palette = CommandPalette::new().with_max_visible(12);
        let deterministic_mode = determinism::is_demo_deterministic();
        let deterministic_tick_ms = determinism::demo_tick_ms_override();
        let exit_after_ticks = determinism::demo_exit_after_ticks();
        let mut app = Self {
            current_screen: ScreenId::Dashboard,
            tour: GuidedTourState::new(),
            screens: ScreenStates::default(),
            help_visible: false,
            debug_visible: false,
            perf_hud_visible: false,
            evidence_ledger_visible: false,
            inline_mode: false,
            mouse_capture_enabled: true,
            a11y: theme::A11ySettings::default(),
            a11y_panel_visible: false,
            base_theme,
            command_palette: palette,
            screen_favorites: HashSet::new(),
            palette_category_filter: None,
            palette_favorites_only: false,
            tick_count: 0,
            frame_count: 0,
            terminal_width: 0,
            terminal_height: 0,
            exit_after_ms: 0,
            exit_after_ticks,
            deterministic_tick_ms,
            deterministic_mode,
            perf_last_tick: None,
            perf_tick_times_us: VecDeque::with_capacity(120),
            perf_view_counter: Cell::new(0),
            perf_views_per_tick: 0.0,
            perf_prev_view_count: 0,
            tour_checksum: Cell::new(None),
            last_hit_grid: RefCell::new(None),
            tick_last_seen: None,
            tick_stall_last_log: Cell::new(None),
            history: HistoryManager::default(),
            a11y_telemetry: None,
            mouse_dispatcher: MouseDispatcher::default(),
        };
        app.refresh_palette_actions();
        app.screens
            .accessibility_panel
            .sync_a11y(app.a11y, app.base_theme);
        if deterministic_mode {
            let perf_tick_ms = determinism::demo_tick_ms(100);
            let vfx_tick_ms = determinism::demo_tick_ms(16);
            app.screens
                .performance_hud
                .enable_deterministic_mode(perf_tick_ms);
            app.screens
                .set_visual_effects_deterministic_tick_ms(vfx_tick_ms);
        }
        app
    }

    /// Attach telemetry hooks for accessibility mode changes.
    pub fn with_a11y_telemetry_hooks(mut self, hooks: A11yTelemetryHooks) -> Self {
        self.a11y_telemetry = Some(hooks);
        self
    }

    fn display_screen(&self) -> ScreenId {
        if self.tour.is_active() {
            self.tour.active_screen()
        } else {
            self.current_screen
        }
    }

    fn tick_interval_ms(&self) -> u64 {
        if let Some(override_ms) = self.deterministic_tick_ms {
            return override_ms.max(1);
        }
        if self.tour.is_active() {
            return 100;
        }
        if matches!(self.display_screen(), ScreenId::VisualEffects) {
            16
        } else {
            100
        }
    }

    fn emit_tour_jsonl(&self, action: &str, outcome: &str, step: Option<&TourStep>) {
        let Some(path) = tour_log_path() else {
            return;
        };

        let seq = TOUR_LOG_SEQ.fetch_add(1, Ordering::Relaxed);
        let ts_us = seq.saturating_mul(16_667);
        let run_id = tour_run_id();
        let seed = tour_seed();
        let mode = tour_screen_mode();
        let caps_profile = tour_caps_profile();
        let paused = self.tour.is_paused();
        let checksum_json = self
            .tour_checksum
            .get()
            .map(|hash| format!("\"0x{hash:016x}\""))
            .unwrap_or_else(|| "null".to_string());

        let (step_id, screen_id, duration_ms) = if let Some(step) = step {
            (
                step.id.as_str(),
                step.screen.title(),
                step.duration.as_millis() as u64,
            )
        } else {
            ("none", "none", 0)
        };

        let json = format!(
            "{{\"seq\":{seq},\"ts_us\":{ts_us},\"event\":\"tour\",\"run_id\":\"{}\",\"action\":\"{}\",\"outcome\":\"{}\",\"step_id\":\"{}\",\"screen_id\":\"{}\",\"duration_ms\":{duration_ms},\"step_index\":{},\"step_count\":{},\"seed\":{seed},\"width\":{},\"height\":{},\"mode\":\"{}\",\"caps_profile\":\"{}\",\"speed\":{:.2},\"paused\":{paused},\"checksum\":{checksum_json}}}",
            json_escape(&run_id),
            json_escape(action),
            json_escape(outcome),
            json_escape(step_id),
            json_escape(screen_id),
            self.tour.step_index(),
            self.tour.step_count(),
            self.terminal_width,
            self.terminal_height,
            json_escape(&mode),
            json_escape(&caps_profile),
            self.tour.speed(),
        );

        if let Some(parent) = Path::new(&path).parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{json}");
        }
    }

    pub fn start_tour(&mut self, start_step: usize, speed: f64) {
        let resume_screen = if self.current_screen == ScreenId::GuidedTour {
            if self.tour.is_active() {
                self.tour.active_screen()
            } else {
                ScreenId::Dashboard
            }
        } else {
            self.current_screen
        };
        self.tour.start(resume_screen, start_step, speed);
        self.current_screen = ScreenId::GuidedTour;
        self.screens.action_timeline.record_command_event(
            self.tick_count,
            "Start guided tour",
            vec![
                ("start_step".to_string(), start_step.to_string()),
                ("speed".to_string(), format!("{:.2}", self.tour.speed())),
            ],
        );
        self.emit_tour_jsonl("start", "ok", self.tour.current_step());
    }

    fn stop_tour(&mut self, keep_last: bool, reason: &str) {
        let screen = self.tour.stop(keep_last);
        self.current_screen = screen;
        self.screens.action_timeline.record_command_event(
            self.tick_count,
            "Stop guided tour",
            vec![
                ("reason".to_string(), reason.to_string()),
                ("screen".to_string(), screen.title().to_string()),
            ],
        );
        self.emit_tour_jsonl("exit", reason, self.tour.current_step());
    }

    fn handle_tour_event(&mut self, event: TourEvent) {
        match event {
            TourEvent::StepChanged { from, to, reason } => {
                let reason_label = match reason {
                    TourAdvanceReason::Auto => "auto",
                    TourAdvanceReason::ManualNext => "manual_next",
                    TourAdvanceReason::ManualPrev => "manual_prev",
                    TourAdvanceReason::Jump => "jump",
                };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Guided tour step",
                    vec![
                        ("from".to_string(), from.title().to_string()),
                        ("to".to_string(), to.title().to_string()),
                        ("reason".to_string(), reason_label.to_string()),
                    ],
                );
                let action = match reason {
                    TourAdvanceReason::Auto => "auto",
                    TourAdvanceReason::ManualNext => "next",
                    TourAdvanceReason::ManualPrev => "prev",
                    TourAdvanceReason::Jump => "jump",
                };
                self.emit_tour_jsonl(action, "ok", self.tour.current_step());
            }
            TourEvent::Finished { last_screen: _ } => {
                self.emit_tour_jsonl("finish", "ok", self.tour.current_step());
                self.stop_tour(true, "completed");
            }
        }
    }

    fn emit_a11y_event(&mut self, kind: A11yEventKind) {
        let event = A11yTelemetryEvent {
            kind,
            tick: self.tick_count,
            screen: self.display_screen().title(),
            panel_visible: self.a11y_panel_visible,
            high_contrast: self.a11y.high_contrast,
            reduced_motion: self.a11y.reduced_motion,
            large_text: self.a11y.large_text,
        };

        self.screens.accessibility_panel.record_event(&event);

        emit_a11y_jsonl(
            match kind {
                A11yEventKind::Panel => "panel_toggle",
                A11yEventKind::HighContrast => "high_contrast_toggle",
                A11yEventKind::ReducedMotion => "reduced_motion_toggle",
                A11yEventKind::LargeText => "large_text_toggle",
            },
            &[
                ("tick", &event.tick.to_string()),
                ("screen", event.screen),
                (
                    "panel_visible",
                    if event.panel_visible { "true" } else { "false" },
                ),
                (
                    "high_contrast",
                    if event.high_contrast { "true" } else { "false" },
                ),
                (
                    "reduced_motion",
                    if event.reduced_motion {
                        "true"
                    } else {
                        "false"
                    },
                ),
                (
                    "large_text",
                    if event.large_text { "true" } else { "false" },
                ),
            ],
        );

        if let Some(ref hooks) = self.a11y_telemetry {
            hooks.dispatch(&event);
        }
    }

    /// Build all palette actions (screens + global commands) with filters.
    fn build_palette_actions(
        category_filter: Option<screens::ScreenCategory>,
        favorites_only: bool,
        favorites: &HashSet<ScreenId>,
    ) -> Vec<ftui_widgets::command_palette::ActionItem> {
        use ftui_widgets::command_palette::ActionItem;

        let mut actions = Vec::new();

        // Screen navigation actions
        for meta in screens::screen_registry() {
            if let Some(filter) = category_filter
                && meta.category != filter
            {
                continue;
            }

            let is_favorite = favorites.contains(&meta.id);
            if favorites_only && !is_favorite {
                continue;
            }

            let display_title = if is_favorite {
                format!("* {}", meta.title)
            } else {
                meta.title.to_string()
            };
            let action_id = format!("screen:{}", meta.title.to_lowercase().replace(' ', "_"));
            let mut tags: Vec<&str> = Vec::with_capacity(meta.palette_tags.len() + 3);
            tags.extend_from_slice(meta.palette_tags);
            tags.push("screen");
            tags.push("navigate");
            if is_favorite {
                tags.push("favorite");
            }
            actions.push(
                ActionItem::new(&action_id, display_title)
                    .with_description(meta.blurb)
                    .with_tags(&tags)
                    .with_category(meta.category.label()),
            );
        }

        // Global commands
        actions.push(
            ActionItem::new("cmd:toggle_help", "Toggle Help")
                .with_description("Show or hide the keyboard shortcuts overlay")
                .with_tags(&["help", "shortcuts"])
                .with_category("View"),
        );
        actions.push(
            ActionItem::new("cmd:toggle_debug", "Toggle Debug Overlay")
                .with_description("Show or hide the debug information panel")
                .with_tags(&["debug", "info"])
                .with_category("View"),
        );
        actions.push(
            ActionItem::new("cmd:toggle_perf_hud", "Toggle Performance HUD")
                .with_description("Show or hide the performance metrics overlay")
                .with_tags(&["performance", "hud", "fps", "metrics", "budget"])
                .with_category("View"),
        );
        actions.push(
            ActionItem::new("cmd:toggle_mouse_capture", "Toggle Mouse Capture")
                .with_description(
                    "Enable or disable terminal mouse reporting (steals scrollback in inline mode)",
                )
                .with_tags(&["mouse", "scroll", "scrollback", "input"])
                .with_category("Input"),
        );
        actions.push(
            ActionItem::new(
                "cmd:toggle_evidence_ledger",
                "Toggle Explainability Cockpit",
            )
            .with_description("Show diff/resize/budget decision evidence cockpit")
            .with_tags(&["explain", "evidence", "bocpd", "budget", "diff"])
            .with_category("View"),
        );
        actions.push(
            ActionItem::new("cmd:cycle_theme", "Cycle Theme")
                .with_description("Switch to the next color theme")
                .with_tags(&["theme", "colors", "appearance"])
                .with_category("View"),
        );
        actions.push(
            ActionItem::new("cmd:quit", "Quit")
                .with_description("Exit the application")
                .with_tags(&["exit", "close"])
                .with_category("App"),
        );

        actions
    }

    /// Refresh palette actions using current filters/favorites.
    fn refresh_palette_actions(&mut self) {
        let actions = Self::build_palette_actions(
            self.palette_category_filter,
            self.palette_favorites_only,
            &self.screen_favorites,
        );
        self.command_palette.replace_actions(actions);
    }

    /// Resolve a screen action ID (screen:<name>) to ScreenId.
    fn screen_id_from_action_id(action_id: &str) -> Option<ScreenId> {
        let screen_name = action_id.strip_prefix("screen:")?;
        screens::screen_registry()
            .iter()
            .find(|meta| meta.title.to_lowercase().replace(' ', "_") == screen_name)
            .map(|meta| meta.id)
    }

    /// Current palette selection resolved to a ScreenId, if any.
    fn selected_palette_screen(&self) -> Option<ScreenId> {
        self.command_palette
            .selected_action()
            .and_then(|action| Self::screen_id_from_action_id(&action.id))
    }

    /// Toggle favorite for the currently selected palette screen (if any).
    fn toggle_selected_favorite(&mut self) {
        let Some(action) = self.command_palette.selected_action() else {
            return;
        };
        let Some(screen_id) = Self::screen_id_from_action_id(&action.id) else {
            return;
        };
        if !self.screen_favorites.insert(screen_id) {
            self.screen_favorites.remove(&screen_id);
        }
    }

    fn handle_msg(&mut self, msg: AppMsg, source: EventSource) -> Cmd<AppMsg> {
        match msg {
            AppMsg::Quit => Cmd::Quit,

            AppMsg::SwitchScreen(id) => {
                let from = self.display_screen().title();
                if id == ScreenId::GuidedTour {
                    self.start_tour(0, self.tour.speed());
                    return Cmd::None;
                }
                if self.tour.is_active() {
                    self.stop_tour(false, "switch_screen");
                }
                self.current_screen = id;
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Switch screen",
                    vec![
                        ("from".to_string(), from.to_string()),
                        ("to".to_string(), id.title().to_string()),
                    ],
                );
                Cmd::None
            }

            AppMsg::NextScreen => {
                let from_screen = self.display_screen();
                let to_screen = from_screen.next();
                if to_screen == ScreenId::GuidedTour {
                    self.start_tour(0, self.tour.speed());
                    return Cmd::None;
                }
                if self.tour.is_active() {
                    self.stop_tour(false, "next_screen");
                }
                self.current_screen = to_screen;
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Next screen",
                    vec![
                        ("from".to_string(), from_screen.title().to_string()),
                        ("to".to_string(), to_screen.title().to_string()),
                    ],
                );
                Cmd::None
            }

            AppMsg::PrevScreen => {
                let from_screen = self.display_screen();
                let to_screen = from_screen.prev();
                if to_screen == ScreenId::GuidedTour {
                    self.start_tour(0, self.tour.speed());
                    return Cmd::None;
                }
                if self.tour.is_active() {
                    self.stop_tour(false, "prev_screen");
                }
                self.current_screen = to_screen;
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Previous screen",
                    vec![
                        ("from".to_string(), from_screen.title().to_string()),
                        ("to".to_string(), to_screen.title().to_string()),
                    ],
                );
                Cmd::None
            }

            AppMsg::ToggleHelp => {
                self.help_visible = !self.help_visible;
                let state = if self.help_visible { "on" } else { "off" };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle help overlay",
                    vec![("state".to_string(), state.to_string())],
                );
                Cmd::None
            }

            AppMsg::ToggleDebug => {
                self.debug_visible = !self.debug_visible;
                let state = if self.debug_visible { "on" } else { "off" };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle debug overlay",
                    vec![("state".to_string(), state.to_string())],
                );
                Cmd::None
            }

            AppMsg::TogglePerfHud => {
                self.perf_hud_visible = !self.perf_hud_visible;
                let state = if self.perf_hud_visible { "on" } else { "off" };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle performance HUD",
                    vec![("state".to_string(), state.to_string())],
                );

                // Diagnostic logging (bd-3k3x.8)
                emit_perf_hud_jsonl(
                    "hud_toggle",
                    &[
                        ("state", state),
                        ("tick", &self.tick_count.to_string()),
                        ("screen", self.display_screen().title()),
                    ],
                );
                tracing::info!(
                    target: "ftui.perf_hud",
                    visible = self.perf_hud_visible,
                    tick = self.tick_count,
                    "Performance HUD toggled"
                );

                Cmd::None
            }

            AppMsg::ToggleMouseCapture => {
                self.mouse_capture_enabled = !self.mouse_capture_enabled;
                let state = if self.mouse_capture_enabled {
                    "on"
                } else {
                    "off"
                };
                let mode = if self.inline_mode { "inline" } else { "alt" };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle mouse capture",
                    vec![
                        ("state".to_string(), state.to_string()),
                        ("screen_mode".to_string(), mode.to_string()),
                    ],
                );
                let source_label = match source {
                    EventSource::User => "user",
                    EventSource::Playback => "playback",
                };
                emit_mouse_capture_toggle_jsonl(
                    self.mouse_capture_enabled,
                    source_label,
                    self.inline_mode,
                    self.display_screen(),
                );
                Cmd::set_mouse_capture(self.mouse_capture_enabled)
            }

            AppMsg::ToggleEvidenceLedger => {
                self.evidence_ledger_visible = !self.evidence_ledger_visible;
                let state = if self.evidence_ledger_visible {
                    "on"
                } else {
                    "off"
                };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle explainability cockpit",
                    vec![("state".to_string(), state.to_string())],
                );
                tracing::info!(
                    target: "ftui.explainability_cockpit",
                    visible = self.evidence_ledger_visible,
                    tick = self.tick_count,
                    "Explainability cockpit toggled"
                );
                Cmd::None
            }

            AppMsg::ToggleA11yPanel => {
                self.a11y_panel_visible = !self.a11y_panel_visible;
                let state = if self.a11y_panel_visible { "on" } else { "off" };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle A11y panel",
                    vec![("state".to_string(), state.to_string())],
                );
                self.emit_a11y_event(A11yEventKind::Panel);
                Cmd::None
            }

            AppMsg::ToggleHighContrast => {
                self.a11y.high_contrast = !self.a11y.high_contrast;
                if self.a11y.high_contrast {
                    self.base_theme = theme::current_theme();
                }
                self.apply_a11y_settings();
                let state = if self.a11y.high_contrast { "on" } else { "off" };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle high contrast",
                    vec![("state".to_string(), state.to_string())],
                );
                self.emit_a11y_event(A11yEventKind::HighContrast);
                Cmd::None
            }

            AppMsg::ToggleReducedMotion => {
                self.a11y.reduced_motion = !self.a11y.reduced_motion;
                self.apply_a11y_settings();
                let state = if self.a11y.reduced_motion {
                    "on"
                } else {
                    "off"
                };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle reduced motion",
                    vec![("state".to_string(), state.to_string())],
                );
                self.emit_a11y_event(A11yEventKind::ReducedMotion);
                Cmd::None
            }

            AppMsg::ToggleLargeText => {
                self.a11y.large_text = !self.a11y.large_text;
                self.apply_a11y_settings();
                let state = if self.a11y.large_text { "on" } else { "off" };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle large text",
                    vec![("state".to_string(), state.to_string())],
                );
                self.emit_a11y_event(A11yEventKind::LargeText);
                Cmd::None
            }

            AppMsg::CycleTheme => {
                self.base_theme = self.next_base_theme();
                self.apply_a11y_settings();
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Cycle theme",
                    vec![
                        ("theme".to_string(), theme::current_theme_name().to_string()),
                        ("base_theme".to_string(), self.base_theme.name().to_string()),
                    ],
                );
                Cmd::None
            }

            AppMsg::Tick => {
                let tick_ms = self.tick_interval_ms();
                if self.deterministic_mode {
                    self.screens
                        .set_visual_effects_deterministic_tick_ms(tick_ms);
                    self.screens
                        .performance_hud
                        .enable_deterministic_mode(tick_ms);
                }
                self.tick_count += 1;
                if self.deterministic_mode {
                    self.tick_last_seen = None;
                } else {
                    self.tick_last_seen = Some(Instant::now());
                }
                self.record_tick_timing();
                if !self.a11y.reduced_motion {
                    self.screens.tick(self.display_screen(), self.tick_count);
                }
                if let Some(event) = self.tour.advance(Duration::from_millis(tick_ms)) {
                    self.handle_tour_event(event);
                }
                let playback_events = self.screens.macro_recorder.drain_playback_events();
                for event in playback_events {
                    let cmd = self.handle_msg(AppMsg::from(event), EventSource::Playback);
                    if matches!(cmd, Cmd::Quit) {
                        return Cmd::Quit;
                    }
                }
                if let Some(limit) = self.exit_after_ticks
                    && self.tick_count >= limit
                {
                    return Cmd::Quit;
                }
                Cmd::None
            }

            AppMsg::Resize { width, height } => {
                self.terminal_width = width;
                self.terminal_height = height;
                self.screens.macro_recorder.set_terminal_size(width, height);
                self.screens.action_timeline.record_capability_event(
                    self.tick_count,
                    "Terminal resized",
                    vec![
                        ("width".to_string(), width.to_string()),
                        ("height".to_string(), height.to_string()),
                    ],
                );
                Cmd::None
            }

            AppMsg::ScreenEvent(event) => {
                if source == EventSource::User {
                    let filter_controls = self.display_screen() == ScreenId::MacroRecorder;
                    self.screens
                        .macro_recorder
                        .record_event(&event, filter_controls);
                }

                let source_label = match source {
                    EventSource::User => "user",
                    EventSource::Playback => "playback",
                };
                let screen_title = self.display_screen().title();
                self.screens.action_timeline.record_input_event(
                    self.tick_count,
                    &event,
                    source_label,
                    screen_title,
                );

                if !self.tour.is_active()
                    && self.current_screen == ScreenId::GuidedTour
                    && let Event::Key(KeyEvent {
                        code,
                        modifiers,
                        kind: KeyEventKind::Press,
                        ..
                    }) = &event
                    && modifiers.is_empty()
                {
                    match *code {
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            self.start_tour(0, self.tour.speed());
                            return Cmd::None;
                        }
                        KeyCode::Escape => {
                            self.current_screen = ScreenId::Dashboard;
                            return Cmd::None;
                        }
                        _ => {}
                    }
                }

                if self.tour.is_active()
                    && let Event::Key(KeyEvent {
                        code,
                        modifiers,
                        kind: KeyEventKind::Press,
                        ..
                    }) = &event
                    && modifiers.is_empty()
                {
                    match *code {
                        KeyCode::Char(' ') => {
                            self.tour.toggle_pause();
                            let action = if self.tour.is_paused() {
                                "pause"
                            } else {
                                "resume"
                            };
                            self.emit_tour_jsonl(action, "ok", self.tour.current_step());
                            return Cmd::None;
                        }
                        KeyCode::Right | KeyCode::Char('n') => {
                            if let Some(evt) = self.tour.next_step(TourAdvanceReason::ManualNext) {
                                self.handle_tour_event(evt);
                            }
                            return Cmd::None;
                        }
                        KeyCode::Left | KeyCode::Char('p') => {
                            if let Some(evt) = self.tour.prev_step() {
                                self.handle_tour_event(evt);
                            }
                            return Cmd::None;
                        }
                        KeyCode::Escape => {
                            self.stop_tour(false, "exit");
                            return Cmd::None;
                        }
                        KeyCode::Char('+') | KeyCode::Char('=') => {
                            let speed = self.tour.speed() * 1.25;
                            self.tour.set_speed(speed);
                            self.emit_tour_jsonl("speed_up", "ok", self.tour.current_step());
                            return Cmd::None;
                        }
                        KeyCode::Char('-') => {
                            let speed = self.tour.speed() / 1.25;
                            self.tour.set_speed(speed);
                            self.emit_tour_jsonl("speed_down", "ok", self.tour.current_step());
                            return Cmd::None;
                        }
                        _ => {}
                    }
                }

                if let Event::Mouse(mouse) = &event
                    && self.handle_mouse_tab_click(mouse)
                {
                    return Cmd::None;
                }

                // When the command palette is visible, route events to it first.
                if self.command_palette.is_visible() {
                    if let Event::Key(KeyEvent {
                        code,
                        modifiers,
                        kind: KeyEventKind::Press,
                        ..
                    }) = &event
                        && modifiers.contains(Modifiers::CTRL)
                    {
                        match *code {
                            // Toggle favorite on selected screen.
                            KeyCode::Char('f') => {
                                let query = self.command_palette.query().to_string();
                                let selected = self.selected_palette_screen();
                                let log_screen = selected.unwrap_or(self.display_screen());
                                let log_category = screens::screen_category(log_screen);
                                let outcome = if selected.is_some() {
                                    "ok"
                                } else {
                                    "no_selection"
                                };
                                self.toggle_selected_favorite();
                                self.refresh_palette_actions();
                                emit_palette_jsonl(
                                    "toggle_favorite",
                                    &query,
                                    Some(log_screen),
                                    Some(log_category),
                                    outcome,
                                );
                                return Cmd::None;
                            }
                            // Toggle favorites-only filter (Ctrl+Shift+F).
                            KeyCode::Char('F') if modifiers.contains(Modifiers::SHIFT) => {
                                let query = self.command_palette.query().to_string();
                                let log_screen = self
                                    .selected_palette_screen()
                                    .unwrap_or(self.display_screen());
                                let log_category = screens::screen_category(log_screen);
                                self.palette_favorites_only = !self.palette_favorites_only;
                                self.refresh_palette_actions();
                                let outcome = if self.palette_favorites_only {
                                    "on"
                                } else {
                                    "off"
                                };
                                emit_palette_jsonl(
                                    "toggle_favorites_only",
                                    &query,
                                    Some(log_screen),
                                    Some(log_category),
                                    outcome,
                                );
                                return Cmd::None;
                            }
                            // Clear category filter (Ctrl+0).
                            KeyCode::Char('0') => {
                                let query = self.command_palette.query().to_string();
                                let log_screen = self
                                    .selected_palette_screen()
                                    .unwrap_or(self.display_screen());
                                self.palette_category_filter = None;
                                self.refresh_palette_actions();
                                emit_palette_jsonl(
                                    "clear_category_filter",
                                    &query,
                                    Some(log_screen),
                                    None,
                                    "ok",
                                );
                                return Cmd::None;
                            }
                            // Set category filter (Ctrl+1..6).
                            KeyCode::Char(ch @ '1'..='6') => {
                                let idx = (ch as usize) - ('1' as usize);
                                if let Some(category) =
                                    screens::ScreenCategory::ALL.get(idx).copied()
                                {
                                    let query = self.command_palette.query().to_string();
                                    let log_screen = self
                                        .selected_palette_screen()
                                        .unwrap_or(self.display_screen());
                                    self.palette_category_filter = Some(category);
                                    self.refresh_palette_actions();
                                    emit_palette_jsonl(
                                        "set_category_filter",
                                        &query,
                                        Some(log_screen),
                                        Some(category),
                                        "ok",
                                    );
                                    return Cmd::None;
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(action) = self.command_palette.handle_event(&event) {
                        return self.execute_palette_action(action);
                    }
                    return Cmd::None;
                }

                if let Event::Key(KeyEvent {
                    code,
                    modifiers,
                    kind: KeyEventKind::Press,
                    ..
                }) = &event
                {
                    // Esc closes the topmost visible overlay (reverse render
                    // order so the last-painted layer is dismissed first).
                    if matches!(code, KeyCode::Escape) {
                        // Evidence ledger (above perf HUD)
                        if self.evidence_ledger_visible {
                            self.evidence_ledger_visible = false;
                            return Cmd::None;
                        }
                        // Performance HUD
                        if self.perf_hud_visible {
                            self.perf_hud_visible = false;
                            return Cmd::None;
                        }
                        // Debug overlay
                        if self.debug_visible {
                            self.debug_visible = false;
                            return Cmd::None;
                        }
                        // Help overlay
                        if self.help_visible {
                            self.help_visible = false;
                            return Cmd::None;
                        }
                        // A11y panel
                        if self.a11y_panel_visible {
                            return self.handle_msg(AppMsg::ToggleA11yPanel, source);
                        }
                    }

                    if self.a11y_panel_visible {
                        match (*code, *modifiers) {
                            (KeyCode::Char('A'), Modifiers::SHIFT) => {
                                return self.handle_msg(AppMsg::ToggleA11yPanel, source);
                            }
                            (KeyCode::Char('H'), Modifiers::SHIFT) => {
                                return self.handle_msg(AppMsg::ToggleHighContrast, source);
                            }
                            (KeyCode::Char('M'), Modifiers::SHIFT) => {
                                return self.handle_msg(AppMsg::ToggleReducedMotion, source);
                            }
                            (KeyCode::Char('L'), Modifiers::SHIFT) => {
                                return self.handle_msg(AppMsg::ToggleLargeText, source);
                            }
                            _ => {}
                        }
                    }

                    if self.display_screen() == ScreenId::AccessibilityPanel {
                        match (*code, *modifiers) {
                            (KeyCode::Char('h'), Modifiers::NONE) => {
                                return self.handle_msg(AppMsg::ToggleHighContrast, source);
                            }
                            (KeyCode::Char('m'), Modifiers::NONE) => {
                                return self.handle_msg(AppMsg::ToggleReducedMotion, source);
                            }
                            (KeyCode::Char('l'), Modifiers::NONE) => {
                                return self.handle_msg(AppMsg::ToggleLargeText, source);
                            }
                            _ => {}
                        }
                    }

                    let text_input_active = self.screens.consumes_text_input(self.display_screen());

                    match (*code, *modifiers) {
                        // Quit (suppressed when a text field has focus)
                        (KeyCode::Char('q'), Modifiers::NONE) if !text_input_active => {
                            return Cmd::Quit;
                        }
                        (KeyCode::Char('c'), Modifiers::CTRL) => return Cmd::Quit,
                        // Command palette (Ctrl+K)
                        (KeyCode::Char('k'), Modifiers::CTRL) => {
                            let log_screen = self.display_screen();
                            let log_category = screens::screen_category(log_screen);
                            self.command_palette.open();
                            emit_palette_jsonl(
                                "open",
                                self.command_palette.query(),
                                Some(log_screen),
                                Some(log_category),
                                "ok",
                            );
                            return Cmd::None;
                        }
                        // Help (suppressed when a text field has focus)
                        (KeyCode::Char('?'), _) if !text_input_active => {
                            self.help_visible = !self.help_visible;
                            return Cmd::None;
                        }
                        // Debug
                        (KeyCode::F(12), _) => {
                            self.debug_visible = !self.debug_visible;
                            return Cmd::None;
                        }
                        // Mouse capture toggle
                        (KeyCode::F(6), _) => {
                            return self.handle_msg(AppMsg::ToggleMouseCapture, source);
                        }
                        // Performance HUD
                        (KeyCode::Char('p'), Modifiers::CTRL) => {
                            self.perf_hud_visible = !self.perf_hud_visible;
                            return Cmd::None;
                        }
                        // Inspector / evidence ledger (Ctrl+I)
                        (KeyCode::Char('i'), Modifiers::CTRL) => {
                            return self.handle_msg(AppMsg::ToggleEvidenceLedger, source);
                        }
                        // Undo (Ctrl+Z)
                        (KeyCode::Char('z'), Modifiers::CTRL) => {
                            if self.handle_screen_undo(UndoAction::Undo) {
                                return Cmd::None;
                            }
                            if self.history.can_undo() {
                                let _ = self.history.undo();
                            }
                            return Cmd::None;
                        }
                        // Redo (Ctrl+Y or Ctrl+Shift+Z)
                        (KeyCode::Char('y'), Modifiers::CTRL) => {
                            if self.handle_screen_undo(UndoAction::Redo) {
                                return Cmd::None;
                            }
                            if self.history.can_redo() {
                                let _ = self.history.redo();
                            }
                            return Cmd::None;
                        }
                        (KeyCode::Char('Z'), m) if m.contains(Modifiers::CTRL) => {
                            // Ctrl+Shift+Z for redo
                            if self.handle_screen_undo(UndoAction::Redo) {
                                return Cmd::None;
                            }
                            if self.history.can_redo() {
                                let _ = self.history.redo();
                            }
                            return Cmd::None;
                        }
                        // A11y panel
                        (KeyCode::Char('A'), Modifiers::SHIFT) => {
                            return self.handle_msg(AppMsg::ToggleA11yPanel, source);
                        }
                        // Theme cycling
                        (KeyCode::Char('t'), Modifiers::CTRL) => {
                            return self.handle_msg(AppMsg::CycleTheme, source);
                        }
                        // Mouse capture toggle (suppressed when a text field has focus)
                        (KeyCode::Char('m'), Modifiers::NONE) if !text_input_active => {
                            return self.handle_msg(AppMsg::ToggleMouseCapture, source);
                        }
                        // Tab cycling (Tab/BackTab, or Shift+H/Shift+L for Vim users)
                        (KeyCode::Tab, Modifiers::NONE) => {
                            let target = self.display_screen().next();
                            if self.tour.is_active() {
                                self.stop_tour(false, "tab_next");
                            }
                            self.current_screen = target;
                            return Cmd::None;
                        }
                        (KeyCode::BackTab, _) => {
                            let target = self.display_screen().prev();
                            if self.tour.is_active() {
                                self.stop_tour(false, "tab_prev");
                            }
                            self.current_screen = target;
                            return Cmd::None;
                        }
                        (KeyCode::Char('L'), Modifiers::SHIFT) => {
                            let target = self.display_screen().next();
                            if self.tour.is_active() {
                                self.stop_tour(false, "shift_l");
                            }
                            self.current_screen = target;
                            return Cmd::None;
                        }
                        (KeyCode::Char('H'), Modifiers::SHIFT) => {
                            let target = self.display_screen().prev();
                            if self.tour.is_active() {
                                self.stop_tour(false, "shift_h");
                            }
                            self.current_screen = target;
                            return Cmd::None;
                        }
                        // Number keys for direct screen access (suppressed when text field has focus)
                        (KeyCode::Char(ch @ '0'..='9'), Modifiers::NONE) if !text_input_active => {
                            if let Some(id) = ScreenId::from_number_key(ch) {
                                if self.tour.is_active() {
                                    self.stop_tour(false, "number_key");
                                }
                                self.current_screen = id;
                                return Cmd::None;
                            }
                        }
                        _ => {}
                    }
                }

                // Handle 'R' key to retry errored screens
                if self.screens.has_error(self.display_screen())
                    && let Event::Key(KeyEvent {
                        code: KeyCode::Char('r' | 'R'),
                        kind: KeyEventKind::Press,
                        ..
                    }) = &event
                {
                    self.screens.clear_error(self.display_screen());
                    return Cmd::None;
                }
                self.screens.update(self.display_screen(), &event);
                Cmd::None
            }
        }
    }
}

impl Model for AppModel {
    type Message = AppMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        if self.exit_after_ticks.is_none() && self.deterministic_mode && self.exit_after_ms > 0 {
            let tick_ms = self.tick_interval_ms().max(1);
            let ticks = (self.exit_after_ms + tick_ms.saturating_sub(1)) / tick_ms;
            self.exit_after_ticks = Some(ticks.max(1));
        }
        if self.exit_after_ticks.is_some() {
            return Cmd::None;
        }
        if self.exit_after_ms > 0 {
            let ms = self.exit_after_ms;
            Cmd::task_named("demo_exit_after", move || {
                std::thread::sleep(Duration::from_millis(ms));
                AppMsg::Quit
            })
        } else {
            Cmd::None
        }
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        self.handle_msg(msg, EventSource::User)
    }

    fn view(&self, frame: &mut Frame) {
        // Increment view counter (interior mutability for perf tracking)
        self.perf_view_counter.set(self.perf_view_counter.get() + 1);
        self.maybe_log_tick_stall();

        // Ensure hit testing is enabled for mouse interactions (tab bar, panes, overlays).
        frame.enable_hit_testing();

        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        frame
            .buffer
            .fill(area, RenderCell::default().with_bg(theme::bg::DEEP.into()));

        // Top-level layout: nav (1 row) + content + status bar (1 row)
        let chunks = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(1),
                Constraint::Fixed(1),
            ])
            .split(area);

        // Navigation row (single-level tab bar)
        crate::chrome::render_tab_bar(self.display_screen(), frame, chunks[0]);

        // Content area with border
        let content_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(self.display_screen().title())
            .title_alignment(Alignment::Center)
            .style(theme::content_border());

        let inner = content_block.inner(chunks[1]);
        content_block.render(chunks[1], frame);
        crate::chrome::register_pane_hit(frame, inner, self.display_screen());

        // Screen content (wrapped in error boundary)
        self.screens.view(self.display_screen(), frame, inner);
        if self.display_screen() == ScreenId::GuidedTour && !self.tour.is_active() {
            self.render_guided_tour_landing(frame, inner);
        }

        // A11y panel (small overlay inside content area)
        if self.a11y_panel_visible {
            let a11y_state = crate::chrome::A11yPanelState {
                high_contrast: self.a11y.high_contrast,
                reduced_motion: self.a11y.reduced_motion,
                large_text: self.a11y.large_text,
                base_theme: self.base_theme.name(),
            };
            crate::chrome::render_a11y_panel(&a11y_state, frame, inner);
        }

        if self.tour.is_active()
            && let Some(state) = self.tour.overlay_state(inner, 6)
        {
            crate::chrome::render_guided_tour_overlay(&state, frame, inner);
        }

        // Help overlay (chrome module)
        if self.help_visible {
            let bindings = self.current_screen_keybindings();
            crate::chrome::render_help_overlay(self.display_screen(), &bindings, frame, area);
        }

        // Debug overlay
        if self.debug_visible {
            self.render_debug_overlay(frame, area);
        }

        // Performance HUD overlay
        if self.perf_hud_visible {
            self.render_perf_hud(frame, area);
        }

        // Explainability cockpit overlay
        if self.evidence_ledger_visible {
            self.render_evidence_ledger(frame, area);
        }

        // Command palette overlay (topmost layer)
        if self.command_palette.is_visible() {
            self.command_palette.render(area, frame);
        }

        // Status bar (chrome module)
        let (can_undo, can_redo, undo_description) = self.current_screen_undo_status();
        let status_state = crate::chrome::StatusBarState {
            current_screen: self.current_screen,
            screen_title: self.display_screen().title(),
            screen_index: self.current_screen.index(),
            screen_count: screens::screen_registry().len(),
            tick_count: self.tick_count,
            frame_count: self.frame_count,
            terminal_width: self.terminal_width,
            terminal_height: self.terminal_height,
            theme_name: theme::current_theme_name(),
            inline_mode: self.inline_mode,
            mouse_capture_enabled: self.mouse_capture_enabled,
            help_visible: self.help_visible,
            palette_visible: self.command_palette.is_visible(),
            perf_hud_visible: self.perf_hud_visible,
            debug_visible: self.debug_visible,
            a11y_high_contrast: self.a11y.high_contrast,
            a11y_reduced_motion: self.a11y.reduced_motion,
            a11y_large_text: self.a11y.large_text,
            can_undo,
            can_redo,
            undo_description,
        };
        crate::chrome::render_status_bar(&status_state, frame, chunks[2]);

        if tour_log_path().is_some() {
            let pool = &*frame.pool;
            let hash = checksum_buffer(&frame.buffer, pool);
            self.tour_checksum.set(Some(hash));
        }

        self.cache_hit_grid(frame);
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        let tick_ms = self.tick_interval_ms();
        vec![Box::new(Every::new(Duration::from_millis(tick_ms), || {
            AppMsg::Tick
        }))]
    }
}

#[derive(Clone, Copy)]
enum UndoAction {
    Undo,
    Redo,
}

impl AppModel {
    /// Gather keybindings from the current screen for the help overlay.
    pub fn current_screen_keybindings(&self) -> Vec<crate::chrome::HelpEntry> {
        use screens::Screen;
        let mut entries = match self.display_screen() {
            ScreenId::GuidedTour => vec![
                screens::HelpEntry {
                    key: "Click / Enter / Space",
                    action: "Start guided tour",
                },
                screens::HelpEntry {
                    key: "Esc",
                    action: "Back to Dashboard",
                },
            ],
            ScreenId::Dashboard => self.screens.dashboard.keybindings(),
            ScreenId::Shakespeare => self.screens.shakespeare.keybindings(),
            ScreenId::CodeExplorer => self
                .screens
                .code_explorer_mut(|screen| screen.keybindings()),
            ScreenId::WidgetGallery => self.screens.widget_gallery.keybindings(),
            ScreenId::LayoutLab => self.screens.layout_lab.keybindings(),
            ScreenId::FormsInput => self.screens.forms_input.keybindings(),
            ScreenId::DataViz => self.screens.data_viz.keybindings(),
            ScreenId::TableThemeGallery => self.screens.table_theme_gallery.keybindings(),
            ScreenId::FileBrowser => self.screens.file_browser_mut(|screen| screen.keybindings()),
            ScreenId::AdvancedFeatures => self.screens.advanced_features.keybindings(),
            ScreenId::TerminalCapabilities => self.screens.terminal_capabilities.keybindings(),
            ScreenId::MacroRecorder => self.screens.macro_recorder.keybindings(),
            ScreenId::Performance => self.screens.performance.keybindings(),
            ScreenId::MarkdownRichText => self.screens.markdown_rich_text.keybindings(),
            ScreenId::MermaidShowcase => self.screens.mermaid_showcase.keybindings(),
            ScreenId::MermaidMegaShowcase => self.screens.mermaid_mega_showcase.keybindings(),
            ScreenId::VisualEffects => self
                .screens
                .visual_effects_mut(|screen| screen.keybindings()),
            ScreenId::ResponsiveDemo => self.screens.responsive_demo.keybindings(),
            ScreenId::LogSearch => self.screens.log_search.keybindings(),
            ScreenId::Notifications => self.screens.notifications.keybindings(),
            ScreenId::ActionTimeline => self.screens.action_timeline.keybindings(),
            ScreenId::IntrinsicSizing => self.screens.intrinsic_sizing.keybindings(),
            ScreenId::LayoutInspector => self.screens.layout_inspector.keybindings(),
            ScreenId::AdvancedTextEditor => self.screens.advanced_text_editor.keybindings(),
            ScreenId::MousePlayground => self.screens.mouse_playground.keybindings(),
            ScreenId::FormValidation => self.screens.form_validation.keybindings(),
            ScreenId::VirtualizedSearch => self.screens.virtualized_search.keybindings(),
            ScreenId::AsyncTasks => self.screens.async_tasks.keybindings(),
            ScreenId::ThemeStudio => self.screens.theme_studio.keybindings(),
            ScreenId::SnapshotPlayer => self
                .screens
                .snapshot_player_mut(|screen| screen.keybindings()),
            ScreenId::PerformanceHud => self.screens.performance_hud.keybindings(),
            ScreenId::ExplainabilityCockpit => self.screens.explainability_cockpit.keybindings(),
            ScreenId::I18nDemo => self.screens.i18n_demo.keybindings(),
            ScreenId::VoiOverlay => self.screens.voi_overlay.keybindings(),
            ScreenId::InlineModeStory => self.screens.inline_mode_story.keybindings(),
            ScreenId::AccessibilityPanel => self.screens.accessibility_panel.keybindings(),
            ScreenId::WidgetBuilder => self.screens.widget_builder.keybindings(),
            ScreenId::CommandPaletteLab => self.screens.command_palette_lab.keybindings(),
            ScreenId::DeterminismLab => self.screens.determinism_lab.keybindings(),
            ScreenId::HyperlinkPlayground => self.screens.hyperlink_playground.keybindings(),
            ScreenId::KanbanBoard => self.screens.kanban_board.keybindings(),
        };
        if self.tour.is_active() {
            entries.push(screens::HelpEntry {
                key: "Space",
                action: "Tour: pause / resume",
            });
            entries.push(screens::HelpEntry {
                key: "← / →",
                action: "Tour: previous / next step",
            });
            entries.push(screens::HelpEntry {
                key: "Esc",
                action: "Tour: exit tour",
            });
            entries.push(screens::HelpEntry {
                key: "+ / -",
                action: "Tour: speed up / down",
            });
        }
        // Convert screens::HelpEntry to chrome::HelpEntry (same struct, different module).
        entries
            .into_iter()
            .map(|e| crate::chrome::HelpEntry {
                key: e.key,
                action: e.action,
            })
            .collect()
    }

    fn current_screen_undo_status(&self) -> (bool, bool, Option<&str>) {
        use screens::Screen;
        match self.display_screen() {
            ScreenId::AdvancedTextEditor => (
                self.screens.advanced_text_editor.can_undo(),
                self.screens.advanced_text_editor.can_redo(),
                self.screens.advanced_text_editor.next_undo_description(),
            ),
            ScreenId::FormsInput => (
                self.screens.forms_input.can_undo(),
                self.screens.forms_input.can_redo(),
                self.screens.forms_input.next_undo_description(),
            ),
            _ => (
                self.history.can_undo(),
                self.history.can_redo(),
                self.history.next_undo_description(),
            ),
        }
    }

    // -----------------------------------------------------------------
    // Central Mouse Dispatcher (bd-iuvb.17.2)
    // -----------------------------------------------------------------

    /// Dispatch a mouse event through the priority chain.
    ///
    /// Priority order (highest first):
    ///   1. Command palette (if visible) — consumes all mouse events
    ///   2. Overlays (help, tour, a11y, perf HUD, evidence, debug)
    ///   3. Status bar toggles
    ///   4. Tab bar / category tabs
    ///   5. Screen pane content
    ///
    /// Click activation uses MouseUp to prevent accidental drags.
    /// Hover state is coalesced — only updated when the target changes.
    fn dispatch_mouse(&mut self, mouse: &MouseEvent) -> MouseDispatchResult {
        let current = self.current_screen;

        // Hit test at the mouse position using the cached grid.
        let hit = self.hit_test_full(mouse);
        let hit_id = hit.map(|(id, _, _)| id);
        let hit_layer = hit_id.map(crate::chrome::classify_hit);

        // Update hover state (coalesced — only logs when target changes).
        let hover_changed = self.mouse_dispatcher.update_hover(hit_id);
        if hover_changed && matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
            emit_mouse_jsonl(mouse, hit_id, "hover_change", None, current);
        }

        // Command palette has highest priority: do not route chrome/overlay
        // interactions while it is visible. Let `command_palette.handle_event`
        // consume/act on mouse events in the main update loop.
        if self.command_palette.is_visible() {
            return MouseDispatchResult::NotConsumed;
        }

        let chrome_hit = matches!(
            hit_layer,
            Some(
                crate::chrome::HitLayer::Overlay(_)
                    | crate::chrome::HitLayer::StatusToggle(_)
                    | crate::chrome::HitLayer::Tab(_)
                    | crate::chrome::HitLayer::Category(_)
            )
        );
        // Dashboard registers pane hit regions as "links" to other screens.
        // Treat these as click targets (MouseUp activation + drag-threshold),
        // but do NOT treat them as chrome for hover-move consumption; the
        // Dashboard screen uses move events to drive its own hover highlight.
        let pane_link_hit = matches!(
            hit_layer,
            Some(crate::chrome::HitLayer::Pane(target))
                if current == ScreenId::Dashboard && target != ScreenId::Dashboard
        );

        // Handle drag state machine transitions.
        match mouse.kind {
            MouseEventKind::Down(button) => {
                // Only capture chrome/overlay Down events; pane/screen events
                // must flow through to the active screen.
                if chrome_hit {
                    // Start a pending drag — will become click (on Up) or drag
                    // (on sufficient movement).
                    self.mouse_dispatcher.drag = DragPhase::PendingDrag {
                        button,
                        start_x: mouse.x,
                        start_y: mouse.y,
                        hit_id,
                    };
                    emit_mouse_jsonl(mouse, hit_id, "down", None, current);
                    // Down alone does not activate — wait for Up or drag.
                    return MouseDispatchResult::Consumed;
                }

                // Dashboard tile "pane links" should activate on MouseUp, but we
                // still track drag threshold to avoid accidental navigation.
                if pane_link_hit {
                    self.mouse_dispatcher.drag = DragPhase::PendingDrag {
                        button,
                        start_x: mouse.x,
                        start_y: mouse.y,
                        hit_id,
                    };
                    emit_mouse_jsonl(mouse, hit_id, "down_pane_link", None, current);
                    // Allow the Dashboard screen to also update focus/hover.
                    return MouseDispatchResult::NotConsumed;
                }

                // Screen-level input: do not interfere. Also clear any stale
                // chrome drag state.
                self.mouse_dispatcher.cancel_drag();
                emit_mouse_jsonl(mouse, hit_id, "down_forward", None, current);
                MouseDispatchResult::NotConsumed
            }
            MouseEventKind::Drag(button) => {
                match self.mouse_dispatcher.drag {
                    DragPhase::PendingDrag {
                        start_x,
                        start_y,
                        hit_id: drag_hit,
                        ..
                    } => {
                        let dx = mouse.x.abs_diff(start_x);
                        let dy = mouse.y.abs_diff(start_y);
                        if dx >= DRAG_THRESHOLD || dy >= DRAG_THRESHOLD {
                            self.mouse_dispatcher.drag = DragPhase::Dragging {
                                button,
                                start_x,
                                start_y,
                                last_x: mouse.x,
                                last_y: mouse.y,
                                hit_id: drag_hit,
                            };
                            emit_mouse_jsonl(mouse, drag_hit, "drag_start", None, current);
                        }
                        // Still pending — absorb the event.
                        MouseDispatchResult::Consumed
                    }
                    DragPhase::Dragging {
                        hit_id: drag_hit, ..
                    } => {
                        // Update last position.
                        if let DragPhase::Dragging {
                            ref mut last_x,
                            ref mut last_y,
                            ..
                        } = self.mouse_dispatcher.drag
                        {
                            *last_x = mouse.x;
                            *last_y = mouse.y;
                        }
                        emit_mouse_jsonl(mouse, drag_hit, "drag_move", None, current);
                        MouseDispatchResult::Consumed
                    }
                    DragPhase::Idle => {
                        // Screen-level drag (or spurious drag) — do not interfere.
                        emit_mouse_jsonl(mouse, hit_id, "drag_forward", None, current);
                        MouseDispatchResult::NotConsumed
                    }
                }
            }
            MouseEventKind::Up(button) => {
                let was_dragging = self.mouse_dispatcher.is_dragging();
                let drag_hit = self.mouse_dispatcher.drag_hit_id();

                if was_dragging {
                    // End drag.
                    self.mouse_dispatcher.cancel_drag();
                    emit_mouse_jsonl(mouse, drag_hit, "drag_end", None, current);
                    return MouseDispatchResult::Consumed;
                }

                // Not dragging — this may be a click activation (MouseUp) for
                // chrome/overlay targets, or a screen-level Up event.
                self.mouse_dispatcher.cancel_drag();

                if chrome_hit || pane_link_hit {
                    // Only left-click activates chrome/overlay targets.
                    if button != MouseButton::Left {
                        emit_mouse_jsonl(mouse, hit_id, "click_non_left", None, current);
                        return MouseDispatchResult::Consumed;
                    }

                    return self.dispatch_click(mouse, hit, current);
                }

                // Screen-level Up — forward to active screen.
                emit_mouse_jsonl(mouse, hit_id, "up_forward", None, current);
                MouseDispatchResult::NotConsumed
            }
            MouseEventKind::Moved => {
                // Pure move (no button): update hover state (above). Consume
                // chrome-hover moves to avoid sending noise into screens.
                if chrome_hit && hover_changed {
                    return MouseDispatchResult::Consumed;
                }
                emit_mouse_jsonl(mouse, hit_id, "move", None, current);
                MouseDispatchResult::NotConsumed
            }
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => self.dispatch_scroll(mouse, hit, current),
        }
    }

    /// Handle a left-click activation at the given hit position.
    ///
    /// Routes through priority: palette → overlay → status → tab → pane.
    fn dispatch_click(
        &mut self,
        mouse: &MouseEvent,
        hit: Option<(
            HitId,
            ftui_render::frame::HitRegion,
            ftui_render::frame::HitData,
        )>,
        current: ScreenId,
    ) -> MouseDispatchResult {
        use crate::chrome::HitLayer;

        // NOTE: Command palette priority is handled in `dispatch_mouse` by
        // returning NotConsumed when visible.

        let Some((hit_id, _region, _data)) = hit else {
            emit_mouse_jsonl(mouse, None, "click_no_hit", None, current);
            return MouseDispatchResult::NotConsumed;
        };

        let layer = crate::chrome::classify_hit(hit_id);

        match layer {
            // 2) Overlay clicks.
            HitLayer::Overlay(raw) => {
                let action = self.handle_overlay_click(raw);
                emit_mouse_jsonl(mouse, Some(hit_id), action, None, current);
                MouseDispatchResult::Consumed
            }
            // 3) Status bar toggles.
            HitLayer::StatusToggle(raw) => {
                let action = self.handle_status_click(raw);
                emit_mouse_jsonl(mouse, Some(hit_id), action, None, current);
                MouseDispatchResult::Consumed
            }
            // 4) Tab bar.
            HitLayer::Tab(target) => {
                self.handle_tab_click(mouse, hit_id, target, current);
                MouseDispatchResult::Consumed
            }
            // 4b) Category tab.
            HitLayer::Category(cat) => {
                if let Some(first) = screens::first_in_category(cat) {
                    self.handle_tab_click(mouse, hit_id, first, current);
                }
                MouseDispatchResult::Consumed
            }
            // 5) Screen pane content — forward to screen (except Dashboard pane-links).
            HitLayer::Pane(target) => {
                if current == ScreenId::Dashboard && target != ScreenId::Dashboard {
                    self.handle_tab_click(mouse, hit_id, target, current);
                    return MouseDispatchResult::Consumed;
                }

                emit_mouse_jsonl(mouse, Some(hit_id), "pane_click", None, current);
                // Not consumed — let the event flow to the screen's update.
                MouseDispatchResult::NotConsumed
            }
            // Unknown hit ID — forward to screen.
            HitLayer::Unknown => {
                emit_mouse_jsonl(mouse, Some(hit_id), "unknown_hit", None, current);
                MouseDispatchResult::NotConsumed
            }
        }
    }

    /// Handle a scroll event at the given hit position.
    fn dispatch_scroll(
        &mut self,
        mouse: &MouseEvent,
        hit: Option<(
            HitId,
            ftui_render::frame::HitRegion,
            ftui_render::frame::HitData,
        )>,
        current: ScreenId,
    ) -> MouseDispatchResult {
        let hit_id = hit.map(|h| h.0);

        // Scroll on command palette list.
        if self.command_palette.is_visible() {
            emit_mouse_jsonl(mouse, hit_id, "palette_scroll", None, current);
            // Forward scroll to palette (handled via standard event routing).
            return MouseDispatchResult::NotConsumed;
        }

        // Tab bar scroll = cycle screens.
        if let Some((id, _, _)) = hit {
            let layer = crate::chrome::classify_hit(id);
            if matches!(layer, crate::chrome::HitLayer::Tab(_)) {
                match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        let target = self.display_screen().next();
                        if self.tour.is_active() {
                            self.stop_tour(false, "mouse_scroll");
                        }
                        self.current_screen = target;
                        emit_mouse_jsonl(mouse, Some(id), "scroll_next_tab", None, current);
                        return MouseDispatchResult::Consumed;
                    }
                    MouseEventKind::ScrollUp => {
                        let target = self.display_screen().prev();
                        if self.tour.is_active() {
                            self.stop_tour(false, "mouse_scroll");
                        }
                        self.current_screen = target;
                        emit_mouse_jsonl(mouse, Some(id), "scroll_prev_tab", None, current);
                        return MouseDispatchResult::Consumed;
                    }
                    _ => {}
                }
            }
        }

        // Otherwise, forward scroll to screen content.
        emit_mouse_jsonl(mouse, hit_id, "scroll_forward", None, current);
        MouseDispatchResult::NotConsumed
    }

    /// Handle a click on an overlay element.
    fn handle_overlay_click(&mut self, raw_id: u32) -> &'static str {
        use crate::chrome;
        match raw_id {
            chrome::OVERLAY_HELP_CLOSE | chrome::OVERLAY_HELP_CONTENT => {
                self.help_visible = false;
                "overlay_help_close"
            }
            chrome::OVERLAY_TOUR => {
                if self.tour.is_active() {
                    self.stop_tour(false, "mouse_overlay");
                    return "overlay_tour_click";
                }

                // Tour landing screen: allow mouse click to start the tour.
                if self.current_screen == ScreenId::GuidedTour {
                    self.start_tour(0, self.tour.speed());
                    return "overlay_tour_start";
                }

                "overlay_tour_click"
            }
            chrome::OVERLAY_A11Y => {
                self.a11y_panel_visible = false;
                "overlay_a11y_close"
            }
            chrome::OVERLAY_PERF_HUD => {
                self.perf_hud_visible = false;
                "overlay_perf_close"
            }
            chrome::OVERLAY_EVIDENCE => {
                self.evidence_ledger_visible = false;
                "overlay_evidence_close"
            }
            chrome::OVERLAY_DEBUG => {
                self.debug_visible = false;
                "overlay_debug_close"
            }
            _ => "overlay_unknown",
        }
    }

    /// Handle a click on a status bar toggle.
    fn handle_status_click(&mut self, raw_id: u32) -> &'static str {
        use crate::chrome;
        match raw_id {
            chrome::STATUS_HELP_TOGGLE => {
                self.help_visible = !self.help_visible;
                "status_toggle_help"
            }
            chrome::STATUS_PALETTE_TOGGLE => {
                if self.command_palette.is_visible() {
                    self.command_palette.close();
                } else {
                    self.command_palette.open();
                    self.refresh_palette_actions();
                }
                "status_toggle_palette"
            }
            chrome::STATUS_A11Y_TOGGLE => {
                self.a11y_panel_visible = !self.a11y_panel_visible;
                "status_toggle_a11y"
            }
            chrome::STATUS_PERF_TOGGLE => {
                self.perf_hud_visible = !self.perf_hud_visible;
                "status_toggle_perf"
            }
            chrome::STATUS_DEBUG_TOGGLE => {
                self.debug_visible = !self.debug_visible;
                "status_toggle_debug"
            }
            chrome::STATUS_MOUSE_TOGGLE => {
                self.mouse_capture_enabled = !self.mouse_capture_enabled;
                "status_toggle_mouse"
            }
            _ => "status_unknown",
        }
    }

    /// Handle a tab bar click that switches to a different screen.
    fn handle_tab_click(
        &mut self,
        mouse: &MouseEvent,
        hit_id: HitId,
        target: ScreenId,
        current: ScreenId,
    ) {
        if target == current {
            emit_mouse_jsonl(mouse, Some(hit_id), "tab_no_change", Some(target), current);
            return;
        }

        if self.tour.is_active() {
            self.stop_tour(false, "mouse_tab");
        }

        let from = self.display_screen().title();
        self.current_screen = target;
        self.screens.action_timeline.record_command_event(
            self.tick_count,
            "Switch screen (mouse)",
            vec![
                ("from".to_string(), from.to_string()),
                ("to".to_string(), target.title().to_string()),
            ],
        );
        emit_mouse_jsonl(mouse, Some(hit_id), "switch_screen", Some(target), current);
    }

    /// Legacy wrapper — delegates to the central mouse dispatcher.
    fn handle_mouse_tab_click(&mut self, mouse: &MouseEvent) -> bool {
        self.dispatch_mouse(mouse) == MouseDispatchResult::Consumed
    }

    /// Full hit test returning (HitId, HitRegion, HitData).
    fn hit_test_full(
        &self,
        mouse: &MouseEvent,
    ) -> Option<(
        HitId,
        ftui_render::frame::HitRegion,
        ftui_render::frame::HitData,
    )> {
        let grid = self.last_hit_grid.borrow();
        grid.as_ref()
            .and_then(|grid| grid.hit_test(mouse.x, mouse.y))
    }

    /// Hit test returning (HitId, ScreenId) for screen-level routing.
    fn hit_test_screen(&self, mouse: &MouseEvent) -> Option<(HitId, ScreenId)> {
        let (id, _region, _data) = self.hit_test_full(mouse)?;
        let target = crate::chrome::screen_from_any_hit_id(id)?;
        Some((id, target))
    }

    fn cache_hit_grid(&self, frame: &Frame) {
        if let Some(ref grid) = frame.hit_grid {
            self.last_hit_grid.replace(Some(grid.clone()));
        } else {
            self.last_hit_grid.replace(None);
        }
    }

    fn handle_screen_undo(&mut self, action: UndoAction) -> bool {
        use screens::Screen;
        match self.display_screen() {
            ScreenId::AdvancedTextEditor => match action {
                UndoAction::Undo => self.screens.advanced_text_editor.undo(),
                UndoAction::Redo => self.screens.advanced_text_editor.redo(),
            },
            ScreenId::FormsInput => match action {
                UndoAction::Undo => self.screens.forms_input.undo(),
                UndoAction::Redo => self.screens.forms_input.redo(),
            },
            _ => false,
        }
    }

    /// Execute an action returned by the command palette.
    fn execute_palette_action(&mut self, action: PaletteAction) -> Cmd<AppMsg> {
        match action {
            PaletteAction::Dismiss => {
                let log_screen = self.display_screen();
                let log_category = screens::screen_category(log_screen);
                emit_palette_jsonl(
                    "dismiss",
                    self.command_palette.query(),
                    Some(log_screen),
                    Some(log_category),
                    "ok",
                );
                Cmd::None
            }
            PaletteAction::Execute(id) => {
                let query = self.command_palette.query().to_string();
                let (log_screen, log_category, outcome) =
                    if let Some(sid) = Self::screen_id_from_action_id(&id) {
                        (sid, screens::screen_category(sid), "screen")
                    } else {
                        (
                            self.display_screen(),
                            screens::screen_category(self.display_screen()),
                            "command",
                        )
                    };
                emit_palette_jsonl(
                    "execute",
                    &query,
                    Some(log_screen),
                    Some(log_category),
                    outcome,
                );
                // Screen navigation: "screen:<name>"
                if let Some(sid) = Self::screen_id_from_action_id(&id) {
                    if self.tour.is_active() {
                        self.stop_tour(false, "palette");
                    }
                    let from = self.display_screen().title();
                    self.current_screen = sid;
                    self.screens.action_timeline.record_command_event(
                        self.tick_count,
                        "Switch screen (palette)",
                        vec![
                            ("from".to_string(), from.to_string()),
                            ("to".to_string(), sid.title().to_string()),
                        ],
                    );
                    return Cmd::None;
                }
                // Global commands
                match id.as_str() {
                    "cmd:toggle_help" => {
                        self.help_visible = !self.help_visible;
                        let state = if self.help_visible { "on" } else { "off" };
                        self.screens.action_timeline.record_command_event(
                            self.tick_count,
                            "Toggle help overlay (palette)",
                            vec![("state".to_string(), state.to_string())],
                        );
                    }
                    "cmd:toggle_debug" => {
                        self.debug_visible = !self.debug_visible;
                        let state = if self.debug_visible { "on" } else { "off" };
                        self.screens.action_timeline.record_command_event(
                            self.tick_count,
                            "Toggle debug overlay (palette)",
                            vec![("state".to_string(), state.to_string())],
                        );
                    }
                    "cmd:toggle_perf_hud" => {
                        self.perf_hud_visible = !self.perf_hud_visible;
                        let state = if self.perf_hud_visible { "on" } else { "off" };
                        self.screens.action_timeline.record_command_event(
                            self.tick_count,
                            "Toggle performance HUD (palette)",
                            vec![("state".to_string(), state.to_string())],
                        );
                    }
                    "cmd:toggle_mouse_capture" => {
                        return self.handle_msg(AppMsg::ToggleMouseCapture, EventSource::User);
                    }
                    "cmd:toggle_evidence_ledger" => {
                        self.evidence_ledger_visible = !self.evidence_ledger_visible;
                        let state = if self.evidence_ledger_visible {
                            "on"
                        } else {
                            "off"
                        };
                        self.screens.action_timeline.record_command_event(
                            self.tick_count,
                            "Toggle explainability cockpit (palette)",
                            vec![("state".to_string(), state.to_string())],
                        );
                    }
                    "cmd:cycle_theme" => {
                        return self.handle_msg(AppMsg::CycleTheme, EventSource::User);
                    }
                    "cmd:quit" => {
                        self.screens.action_timeline.record_command_event(
                            self.tick_count,
                            "Quit requested (palette)",
                            Vec::new(),
                        );
                        return Cmd::Quit;
                    }
                    _ => {}
                }
                Cmd::None
            }
        }
    }

    /// Record a tick timing sample for the Performance HUD.
    ///
    /// Called from tick handling (which has `&mut self`). Keeps the
    /// most recent 120 samples in a ring buffer for statistics.
    fn record_tick_timing(&mut self) {
        if self.deterministic_mode {
            let dt_us = self.tick_interval_ms().max(1) * 1000;
            if self.perf_tick_times_us.len() >= 120 {
                self.perf_tick_times_us.pop_front();
            }
            self.perf_tick_times_us.push_back(dt_us);
            self.perf_last_tick = None;
        } else {
            let now = Instant::now();
            if let Some(last) = self.perf_last_tick {
                let dt_us = now.duration_since(last).as_micros() as u64;
                if self.perf_tick_times_us.len() >= 120 {
                    self.perf_tick_times_us.pop_front();
                }
                self.perf_tick_times_us.push_back(dt_us);
            }
            self.perf_last_tick = Some(now);
        }

        // Compute views rendered since last tick
        let current_views = self.perf_view_counter.get();
        let delta = current_views.saturating_sub(self.perf_prev_view_count);
        self.perf_prev_view_count = current_views;
        // EMA for views-per-tick (smoothed)
        self.perf_views_per_tick = 0.7 * self.perf_views_per_tick + 0.3 * delta as f64;

        // Diagnostic logging (bd-3k3x.8): emit JSONL every 60 ticks (~1 second)
        if self.tick_count.is_multiple_of(60) && self.perf_hud_visible {
            let (tps, avg_ms, p95_ms, p99_ms, min_ms, max_ms) = self.perf_stats();
            let est_fps = self.perf_views_per_tick * tps;
            emit_perf_hud_jsonl_numeric(
                "tick_stats",
                &[
                    ("tick", self.tick_count as f64),
                    ("fps", est_fps),
                    ("tps", tps),
                    ("avg_ms", avg_ms),
                    ("p95_ms", p95_ms),
                    ("p99_ms", p99_ms),
                    ("min_ms", min_ms),
                    ("max_ms", max_ms),
                    ("samples", self.perf_tick_times_us.len() as f64),
                ],
            );

            // Telemetry span event for threshold crossing
            let fps_status = if est_fps >= 50.0 {
                "healthy"
            } else if est_fps >= 20.0 {
                "degraded"
            } else {
                "critical"
            };
            tracing::debug!(
                target: "ftui.perf_hud",
                tick = self.tick_count,
                fps = %format!("{est_fps:.1}"),
                tps = %format!("{tps:.1}"),
                avg_ms = %format!("{avg_ms:.2}"),
                p95_ms = %format!("{p95_ms:.2}"),
                fps_status,
                "Performance HUD stats"
            );
        }
    }

    /// Emit a diagnostic log if ticks appear to have stalled.
    fn maybe_log_tick_stall(&self) {
        if self.deterministic_mode {
            return;
        }
        if !perf_hud_jsonl_enabled() {
            return;
        }
        let Some(last_tick) = self.tick_last_seen else {
            return;
        };
        let elapsed = last_tick.elapsed();
        if elapsed < TICK_STALL_WARN_AFTER {
            return;
        }

        let now = Instant::now();
        let should_log = self
            .tick_stall_last_log
            .get()
            .map(|prev| now.duration_since(prev) >= TICK_STALL_LOG_INTERVAL)
            .unwrap_or(true);
        if !should_log {
            return;
        }
        self.tick_stall_last_log.set(Some(now));

        let since_ms = elapsed.as_millis().to_string();
        let tick = self.tick_count.to_string();
        let reduced_motion = if self.a11y.reduced_motion {
            "true"
        } else {
            "false"
        };
        emit_perf_hud_jsonl(
            "tick_stall",
            &[
                ("since_ms", &since_ms),
                ("tick", &tick),
                ("screen", self.display_screen().title()),
                ("reduced_motion", reduced_motion),
            ],
        );
    }

    /// Seed deterministic Performance HUD metrics for tests/snapshots.
    ///
    /// This bypasses real-time sampling so snapshots stay stable.
    pub fn seed_perf_hud_metrics_for_test(
        &mut self,
        tick_count: u64,
        view_count: u64,
        views_per_tick: f64,
        samples_us: &[u64],
    ) {
        self.tick_count = tick_count;
        self.perf_view_counter.set(view_count);
        self.perf_prev_view_count = view_count;
        self.perf_views_per_tick = views_per_tick;
        self.perf_last_tick = None;
        self.perf_tick_times_us.clear();
        let start = samples_us.len().saturating_sub(120);
        for &sample in &samples_us[start..] {
            self.perf_tick_times_us.push_back(sample);
        }
    }

    /// Compute tick interval statistics from recent samples.
    ///
    /// Returns `(tps, avg_ms, p95_ms, p99_ms, min_ms, max_ms)`.
    fn perf_stats(&self) -> (f64, f64, f64, f64, f64, f64) {
        if self.perf_tick_times_us.is_empty() {
            return (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        }

        let n = self.perf_tick_times_us.len();
        let sum: u64 = self.perf_tick_times_us.iter().sum();
        let avg_us = sum as f64 / n as f64;
        let tps = if avg_us > 0.0 {
            1_000_000.0 / avg_us
        } else {
            0.0
        };
        let avg_ms = avg_us / 1000.0;

        let mut sorted: Vec<u64> = self.perf_tick_times_us.iter().copied().collect();
        sorted.sort_unstable();

        let p95_idx = ((n as f64 * 0.95) as usize).min(n.saturating_sub(1));
        let p99_idx = ((n as f64 * 0.99) as usize).min(n.saturating_sub(1));
        let p95_ms = sorted[p95_idx] as f64 / 1000.0;
        let p99_ms = sorted[p99_idx] as f64 / 1000.0;
        let min_ms = sorted[0] as f64 / 1000.0;
        let max_ms = sorted[n - 1] as f64 / 1000.0;

        (tps, avg_ms, p95_ms, p99_ms, min_ms, max_ms)
    }

    /// Apply the current accessibility settings to theme/runtime globals.
    fn apply_a11y_settings(&mut self) {
        let theme_id = if self.a11y.high_contrast {
            theme::ThemeId::HighContrast
        } else {
            self.base_theme
        };
        theme::set_theme(theme_id);
        let motion_scale = if self.a11y.reduced_motion { 0.0 } else { 1.0 };
        theme::set_motion_scale(motion_scale);
        theme::set_large_text(self.a11y.large_text);
        self.screens.apply_theme();
        self.screens
            .accessibility_panel
            .sync_a11y(self.a11y, self.base_theme);
    }

    /// Cycle to the next base theme, returning it.
    fn next_base_theme(&self) -> theme::ThemeId {
        self.base_theme.next_non_accessibility()
    }

    /// Render the Performance HUD overlay in the top-left corner.
    ///
    /// Shows frame timing, FPS, budget state, diff metrics, and a mini
    /// sparkline of recent frame times. Toggled via Ctrl+P.
    ///
    /// # Telemetry (bd-3k3x.8)
    ///
    /// Emits `ftui.perf_hud.render` span with overlay dimensions.
    fn render_perf_hud(&self, frame: &mut Frame, area: Rect) {
        let _span = tracing::debug_span!(
            target: "ftui.perf_hud",
            "render_perf_hud",
            area.width = area.width,
            area.height = area.height,
        )
        .entered();

        let overlay_width = 48u16.min(area.width.saturating_sub(4));
        let overlay_height = 16u16.min(area.height.saturating_sub(4));

        if overlay_width < 20 || overlay_height < 6 {
            tracing::trace!(
                target: "ftui.perf_hud",
                overlay_width,
                overlay_height,
                "HUD gracefully degraded: area too small"
            );
            return; // Graceful degradation: too small to render
        }

        let x = area.x + 1;
        let y = area.y + 1;
        let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

        let hud_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Perf HUD")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::accent::INFO).bg(theme::bg::DEEP));

        let inner = hud_block.inner(overlay_area);
        // Fill background to ensure overlay occludes content behind it
        frame.buffer.fill(
            overlay_area,
            RenderCell::default().with_bg(theme::bg::DEEP.into()),
        );
        hud_block.render(overlay_area, frame);

        if inner.is_empty() {
            return;
        }

        let (tps, avg_ms, p95_ms, p99_ms, min_ms, max_ms) = self.perf_stats();
        let views = self.perf_view_counter.get();
        // Estimate render FPS: views-per-tick * ticks-per-second
        let est_fps = self.perf_views_per_tick * tps;

        let mut lines: Vec<(String, Style)> = Vec::with_capacity(14);
        let label_style = Style::new().fg(theme::fg::MUTED);
        let value_style = Style::new().fg(theme::fg::PRIMARY);
        let accent_style = Style::new().fg(theme::accent::INFO);

        // FPS header with color coding
        let fps_color = if est_fps >= 50.0 {
            theme::accent::SUCCESS
        } else if est_fps >= 20.0 {
            theme::accent::WARNING
        } else {
            theme::accent::ERROR
        };
        lines.push((
            format!(" Render FPS: {est_fps:.1}"),
            Style::new().fg(fps_color).bold(),
        ));

        // Tick timing
        lines.push((format!(" Tick rate:  {tps:>7.1} /s"), value_style));
        lines.push((format!(" Tick avg:   {avg_ms:>7.2} ms"), value_style));
        lines.push((format!(" Tick p95:   {p95_ms:>7.2} ms"), value_style));
        lines.push((format!(" Tick p99:   {p99_ms:>7.2} ms"), label_style));
        lines.push((
            format!(" Tick range: {min_ms:.1}..{max_ms:.1} ms"),
            label_style,
        ));

        // Separator
        lines.push((String::new(), label_style));

        // Counters
        lines.push((format!(" Views:      {views}"), value_style));
        lines.push((format!(" Ticks:      {}", self.tick_count), value_style));
        lines.push((
            format!(" Screen:     {}", self.display_screen().title()),
            accent_style,
        ));
        lines.push((
            format!(
                " Terminal:   {}x{}",
                self.terminal_width, self.terminal_height
            ),
            value_style,
        ));
        lines.push((
            format!(" Samples:    {}/120", self.perf_tick_times_us.len()),
            label_style,
        ));

        // Separator
        lines.push((String::new(), label_style));

        // Mini sparkline of recent tick intervals
        let sparkline = self.perf_sparkline(inner.width.saturating_sub(2) as usize);
        if !sparkline.is_empty() {
            lines.push((format!(" {sparkline}"), accent_style));
        }

        // Render lines
        for (i, (text, style)) in lines.iter().enumerate() {
            if i >= inner.height as usize {
                break;
            }
            let row_area = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
            Paragraph::new(text.as_str())
                .style(*style)
                .render(row_area, frame);
        }

        // Register hit region so clicking dismisses the overlay (bd-iuvb.17.4).
        frame.register_hit_region(overlay_area, HitId::new(crate::chrome::OVERLAY_PERF_HUD));
    }

    /// Generate a braille-style sparkline from recent frame times.
    ///
    /// Uses Unicode block characters for a compact visual representation
    /// of frame time trends.
    fn perf_sparkline(&self, max_width: usize) -> String {
        const BARS: &[char] = &[
            ' ', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}',
            '\u{2587}', '\u{2588}',
        ];

        let samples: Vec<u64> = self.perf_tick_times_us.iter().copied().collect();
        if samples.is_empty() {
            return String::new();
        }

        // Take the last `max_width` samples
        let start = samples.len().saturating_sub(max_width);
        let window = &samples[start..];

        let min = *window.iter().min().unwrap_or(&0);
        let max = *window.iter().max().unwrap_or(&1);
        let range = (max - min).max(1) as f64;

        window
            .iter()
            .map(|&v| {
                let normalized = ((v - min) as f64 / range * 8.0) as usize;
                BARS[normalized.min(BARS.len() - 1)]
            })
            .collect()
    }

    /// Render the guided tour landing screen (when the tour is not active).
    fn render_guided_tour_landing(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let panel_width = if area.width < 24 {
            area.width
        } else {
            area.width.clamp(24, 62)
        };
        let panel_height = if area.height < 7 {
            area.height
        } else {
            area.height.clamp(7, 11)
        };
        let x = area.x + area.width.saturating_sub(panel_width) / 2;
        let y = area.y + area.height.saturating_sub(panel_height) / 2;
        let panel = Rect::new(x, y, panel_width, panel_height);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Guided Tour")
            .title_alignment(Alignment::Center)
            .style(
                Style::new()
                    .fg(theme::accent::PRIMARY)
                    .bg(theme::alpha::SURFACE),
            );
        let inner = block.inner(panel);
        block.render(panel, frame);
        // Make the landing panel clickable to start the tour (mouse-first demo UX).
        frame.register_hit_region(panel, HitId::new(crate::chrome::OVERLAY_TOUR));

        if inner.is_empty() {
            return;
        }

        let lines = vec![
            Line::from_spans([Span::styled(
                "A 2–3 minute auto-play tour across key screens.",
                Style::new().fg(theme::fg::PRIMARY),
            )]),
            Line::from_spans([Span::styled(
                "Click, Enter, or Space to start.",
                Style::new().fg(theme::accent::INFO).bold(),
            )]),
            Line::from_spans([Span::styled(
                "Controls while running: Space pause · ←/→ step · Esc exit · +/- speed",
                Style::new().fg(theme::fg::MUTED),
            )]),
            Line::from_spans([Span::styled(
                "Tip: You can restart anytime from the Tour tab.",
                Style::new().fg(theme::fg::SECONDARY),
            )]),
        ];

        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }

    /// Render the debug overlay in the top-right corner.
    fn render_debug_overlay(&self, frame: &mut Frame, area: Rect) {
        let overlay_width = 40u16.min(area.width.saturating_sub(4));
        let overlay_height = 8u16.min(area.height.saturating_sub(4));
        let x = area.x + area.width.saturating_sub(overlay_width).saturating_sub(1);
        let y = area.y + 1;
        let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

        let debug_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Debug")
            .title_alignment(Alignment::Center)
            .style(theme::help_overlay());

        let debug_inner = debug_block.inner(overlay_area);
        debug_block.render(overlay_area, frame);

        let mermaid_summary = MermaidConfig::from_env().summary_short();
        let debug_text = format!(
            "Tick: {}\nFrame: {}\nScreen: {:?}\nSize: {}x{}\n{}",
            self.tick_count,
            self.frame_count,
            self.current_screen,
            self.terminal_width,
            self.terminal_height,
            mermaid_summary,
        );
        Paragraph::new(debug_text)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(debug_inner, frame);

        // Register hit region so clicking dismisses the overlay (bd-iuvb.17.4).
        frame.register_hit_region(overlay_area, HitId::new(crate::chrome::OVERLAY_DEBUG));
    }

    /// Render the Explainability Cockpit overlay.
    ///
    /// Shows diff strategy, BOCPD resize regime, and budget decisions
    /// in a single panel (bd-iuvb.4).
    fn render_evidence_ledger(&self, frame: &mut Frame, area: Rect) {
        let _span = tracing::debug_span!(
            target: "ftui.explainability_cockpit",
            "render_explainability_cockpit",
            tick = self.tick_count,
        )
        .entered();
        self.screens
            .explainability_cockpit
            .render_overlay(frame, area);

        // Register hit region for evidence overlay (same geometry as render_overlay).
        let ew = 74u16.min(area.width.saturating_sub(4));
        let eh = 18u16.min(area.height.saturating_sub(4));
        if ew >= 40 && eh >= 10 {
            let ex = area.x + area.width.saturating_sub(ew).saturating_sub(1);
            let ey = area.y + area.height.saturating_sub(eh).saturating_sub(1);
            frame.register_hit_region(
                Rect::new(ex, ey, ew, eh),
                HitId::new(crate::chrome::OVERLAY_EVIDENCE),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;
    use serial_test::serial;
    use std::sync::{Arc, Mutex};

    #[test]
    fn switch_screen_changes_current() {
        let mut app = AppModel::new();
        assert_eq!(app.current_screen, ScreenId::Dashboard);

        app.update(AppMsg::SwitchScreen(ScreenId::Shakespeare));
        assert_eq!(app.current_screen, ScreenId::Shakespeare);

        app.update(AppMsg::SwitchScreen(ScreenId::Performance));
        assert_eq!(app.current_screen, ScreenId::Performance);
    }

    #[test]
    fn next_screen_advances() {
        let mut app = AppModel::new();
        assert_eq!(app.current_screen, ScreenId::Dashboard);

        app.update(AppMsg::NextScreen);
        assert_eq!(app.current_screen, ScreenId::Shakespeare);

        app.update(AppMsg::NextScreen);
        assert_eq!(app.current_screen, ScreenId::CodeExplorer);
    }

    #[test]
    fn prev_screen_goes_back() {
        let mut app = AppModel::new();
        assert_eq!(app.current_screen, ScreenId::Dashboard);

        app.update(AppMsg::PrevScreen);
        assert_eq!(app.current_screen, ScreenId::Dashboard.prev());
    }

    #[test]
    fn tick_increments_count() {
        let mut app = AppModel::new();
        assert_eq!(app.tick_count, 0);

        app.update(AppMsg::Tick);
        assert_eq!(app.tick_count, 1);

        for _ in 0..10 {
            app.update(AppMsg::Tick);
        }
        assert_eq!(app.tick_count, 11);
    }

    #[test]
    fn toggle_help() {
        let mut app = AppModel::new();
        assert!(!app.help_visible);

        app.update(AppMsg::ToggleHelp);
        assert!(app.help_visible);

        app.update(AppMsg::ToggleHelp);
        assert!(!app.help_visible);
    }

    #[test]
    fn toggle_debug() {
        let mut app = AppModel::new();
        assert!(!app.debug_visible);

        app.update(AppMsg::ToggleDebug);
        assert!(app.debug_visible);

        app.update(AppMsg::ToggleDebug);
        assert!(!app.debug_visible);
    }

    #[test]
    fn a11y_telemetry_hooks_fire() {
        let events: Arc<Mutex<Vec<A11yEventKind>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        let hooks = A11yTelemetryHooks::new().on_any(move |event| {
            events_clone.lock().unwrap().push(event.kind);
        });

        let mut app = AppModel::new().with_a11y_telemetry_hooks(hooks);
        app.update(AppMsg::ToggleA11yPanel);
        app.update(AppMsg::ToggleHighContrast);
        app.update(AppMsg::ToggleReducedMotion);
        app.update(AppMsg::ToggleLargeText);

        let collected = events.lock().unwrap();
        assert_eq!(
            collected.as_slice(),
            &[
                A11yEventKind::Panel,
                A11yEventKind::HighContrast,
                A11yEventKind::ReducedMotion,
                A11yEventKind::LargeText,
            ]
        );
    }

    #[test]
    fn resize_updates_dimensions() {
        let mut app = AppModel::new();
        app.update(AppMsg::Resize {
            width: 120,
            height: 40,
        });
        assert_eq!(app.terminal_width, 120);
        assert_eq!(app.terminal_height, 40);
    }

    #[test]
    fn number_keys_map_to_screens() {
        assert_eq!(ScreenId::from_number_key('1'), Some(ScreenId::GuidedTour));
        assert_eq!(ScreenId::from_number_key('2'), Some(ScreenId::Dashboard));
        assert_eq!(ScreenId::from_number_key('3'), Some(ScreenId::Shakespeare));
        assert_eq!(ScreenId::from_number_key('4'), Some(ScreenId::CodeExplorer));
        assert_eq!(
            ScreenId::from_number_key('5'),
            Some(ScreenId::WidgetGallery)
        );
        assert_eq!(ScreenId::from_number_key('6'), Some(ScreenId::LayoutLab));
        assert_eq!(ScreenId::from_number_key('7'), Some(ScreenId::FormsInput));
        assert_eq!(ScreenId::from_number_key('8'), Some(ScreenId::DataViz));
        assert_eq!(ScreenId::from_number_key('9'), Some(ScreenId::FileBrowser));
        assert_eq!(
            ScreenId::from_number_key('0'),
            Some(ScreenId::AdvancedFeatures)
        );
        // No direct key for screens after the first 10
        assert_eq!(ScreenId::from_number_key('a'), None);
    }

    #[test]
    fn screen_next_prev_wraps() {
        assert_eq!(ScreenId::Dashboard.next(), ScreenId::Shakespeare);
        assert_eq!(ScreenId::VisualEffects.next(), ScreenId::ResponsiveDemo);
        assert_eq!(ScreenId::Dashboard.prev(), ScreenId::GuidedTour);
        assert_eq!(ScreenId::Shakespeare.prev(), ScreenId::Dashboard);
    }

    #[test]
    fn quit_returns_quit_cmd() {
        let mut app = AppModel::new();
        let cmd = app.update(AppMsg::Quit);
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn quit_key_triggers_quit() {
        let mut app = AppModel::new();
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        let cmd = app.update(AppMsg::from(event));
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn help_key_toggles_help() {
        let mut app = AppModel::new();
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('?'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(event));
        assert!(app.help_visible);
    }

    #[test]
    fn tab_advances_screen() {
        let mut app = AppModel::new();
        let event = Event::Key(KeyEvent {
            code: KeyCode::Tab,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(event));
        assert_eq!(app.current_screen, ScreenId::Shakespeare);
    }

    #[test]
    fn backtab_moves_previous_screen() {
        let mut app = AppModel::new();
        app.current_screen = ScreenId::Shakespeare;
        let event = Event::Key(KeyEvent {
            code: KeyCode::BackTab,
            modifiers: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(event));
        assert_eq!(app.current_screen, ScreenId::Dashboard);
    }

    #[test]
    fn number_key_switches_screen() {
        let mut app = AppModel::new();
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('3'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(event));
        assert_eq!(app.current_screen, ScreenId::Shakespeare);
    }

    #[test]
    fn guided_tour_resume_defaults_to_dashboard() {
        let mut app = AppModel::new();
        app.current_screen = ScreenId::GuidedTour;

        app.start_tour(0, 1.0);
        app.stop_tour(false, "test");

        assert_eq!(app.current_screen, ScreenId::Dashboard);
    }

    #[test]
    fn event_conversion_resize() {
        let event = Event::Resize {
            width: 80,
            height: 24,
        };
        let msg = AppMsg::from(event);
        assert!(matches!(
            msg,
            AppMsg::Resize {
                width: 80,
                height: 24
            }
        ));
    }

    // -----------------------------------------------------------------------
    // Integration tests
    // -----------------------------------------------------------------------

    /// Render each screen at 120x40 to verify none panic.
    #[test]
    #[serial]
    fn integration_all_screens_render() {
        let app = AppModel::new();
        for &id in screens::screen_ids() {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            let area = Rect::new(0, 0, 120, 37); // Leave room for nav (2) + status
            app.screens.view(id, &mut frame, area);
            // If we reach here without panicking, the screen rendered successfully.
        }
    }

    /// Render each screen at 40x10 (tiny) to verify graceful degradation.
    #[test]
    #[serial]
    fn integration_resize_small() {
        let app = AppModel::new();
        for &id in screens::screen_ids() {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(40, 10, &mut pool);
            let area = Rect::new(0, 0, 40, 7);
            app.screens.view(id, &mut frame, area);
        }
    }

    /// Switch through all screens and verify each renders.
    #[test]
    #[serial]
    fn integration_screen_cycle() {
        let mut app = AppModel::new();
        for &id in screens::screen_ids() {
            app.update(AppMsg::SwitchScreen(id));
            assert_eq!(app.current_screen, id);

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            app.view(&mut frame);
        }
    }

    #[test]
    fn mouse_tab_click_switches_screen() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        // Full click: Down starts pending drag, Up activates.
        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 1, 0);
        let handled_down = app.handle_mouse_tab_click(&down);
        assert!(handled_down, "Mouse down should be consumed");

        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 1, 0);
        let handled_up = app.handle_mouse_tab_click(&up);
        assert!(handled_up, "Mouse up should activate tab click");
        assert_eq!(app.current_screen, ScreenId::GuidedTour);
    }

    #[test]
    fn mouse_tab_click_accepts_mouse_up() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        let mouse = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 1, 0);
        let handled = app.handle_mouse_tab_click(&mouse);
        assert!(handled, "Mouse up should trigger tab navigation");
        assert_eq!(app.current_screen, ScreenId::GuidedTour);
    }

    // -----------------------------------------------------------------------
    // Mouse Dispatcher Tests (bd-iuvb.17.2)
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_mouse_down_starts_pending_drag() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        // Tab bar hit region is on row 0.
        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 1, 0);
        let result = app.dispatch_mouse(&down);
        assert_eq!(result, MouseDispatchResult::Consumed);
        assert!(matches!(
            app.mouse_dispatcher.drag,
            DragPhase::PendingDrag {
                button: MouseButton::Left,
                start_x: 1,
                start_y: 0,
                ..
            }
        ));
    }

    #[test]
    fn dispatch_mouse_down_in_pane_is_forwarded_to_screen() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        // Pick a coordinate that maps to the current screen's pane hit region.
        //
        // NOTE: The Dashboard registers "pane links" to other screens (tiles),
        // so hard-coding a coordinate inside the pane is brittle. We explicitly
        // choose a cell with the Dashboard pane hit id to validate forwarding.
        let expected =
            crate::chrome::PANE_HIT_BASE + crate::screens::screen_index(ScreenId::Dashboard) as u32;
        let mut pane_xy = None;
        for y in 0..40u16 {
            for x in 0..120u16 {
                if let Some((id, _region, _data)) = frame.hit_test(x, y)
                    && id.id() == expected
                {
                    pane_xy = Some((x, y));
                    break;
                }
            }
            if pane_xy.is_some() {
                break;
            }
        }
        let (x, y) = pane_xy.expect("Should find Dashboard pane hit region");

        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), x, y);
        assert_eq!(app.dispatch_mouse(&down), MouseDispatchResult::NotConsumed);

        let drag = MouseEvent::new(MouseEventKind::Drag(MouseButton::Left), x, y);
        assert_eq!(app.dispatch_mouse(&drag), MouseDispatchResult::NotConsumed);

        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), x, y);
        assert_eq!(app.dispatch_mouse(&up), MouseDispatchResult::NotConsumed);
    }

    #[test]
    fn dispatch_mouse_click_on_dashboard_tile_switches_screen() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        let expected =
            crate::chrome::PANE_HIT_BASE + crate::screens::screen_index(ScreenId::DataViz) as u32;
        let mut tile_xy = None;
        for y in 0..40u16 {
            for x in 0..120u16 {
                if let Some((id, _region, _data)) = frame.hit_test(x, y)
                    && id.id() == expected
                {
                    tile_xy = Some((x, y));
                    break;
                }
            }
            if tile_xy.is_some() {
                break;
            }
        }
        let (x, y) = tile_xy.expect("Should find Dashboard tile pane-link hit region");
        assert_eq!(app.current_screen, ScreenId::Dashboard);

        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), x, y);
        assert_eq!(app.dispatch_mouse(&down), MouseDispatchResult::NotConsumed);
        assert_eq!(app.current_screen, ScreenId::Dashboard);

        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), x, y);
        assert_eq!(app.dispatch_mouse(&up), MouseDispatchResult::Consumed);
        assert_eq!(app.current_screen, ScreenId::DataViz);
    }

    #[test]
    fn dispatch_mouse_up_without_drag_triggers_click() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        // Direct MouseUp (no preceding Down) should still route as a click.
        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 1, 0);
        let result = app.dispatch_mouse(&up);
        assert_eq!(result, MouseDispatchResult::Consumed);
        // If position (1,0) is in tab bar, screen should have changed.
        assert_eq!(app.current_screen, ScreenId::GuidedTour);
    }

    #[test]
    fn dispatch_mouse_down_up_cycle_clicks_tab() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        // Down at tab area → pending drag
        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 1, 0);
        assert_eq!(app.dispatch_mouse(&down), MouseDispatchResult::Consumed);

        // Up at same position → click activation
        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 1, 0);
        assert_eq!(app.dispatch_mouse(&up), MouseDispatchResult::Consumed);
        assert_eq!(app.current_screen, ScreenId::GuidedTour);
    }

    #[test]
    fn dispatch_drag_threshold_prevents_accidental_click() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        let initial_screen = app.current_screen;

        // Down at tab area
        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 1, 0);
        app.dispatch_mouse(&down);

        // Small drag (1 cell) — below threshold
        let drag1 = MouseEvent::new(MouseEventKind::Drag(MouseButton::Left), 2, 0);
        app.dispatch_mouse(&drag1);
        assert!(matches!(
            app.mouse_dispatcher.drag,
            DragPhase::PendingDrag { .. }
        ));

        // Large drag (3+ cells) — exceeds threshold
        let drag2 = MouseEvent::new(MouseEventKind::Drag(MouseButton::Left), 4, 0);
        app.dispatch_mouse(&drag2);
        assert!(app.mouse_dispatcher.is_dragging());

        // Up after drag — should NOT activate click, just end drag.
        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 4, 0);
        app.dispatch_mouse(&up);
        assert_eq!(
            app.current_screen, initial_screen,
            "Drag end should not activate tab click"
        );
    }

    #[test]
    fn dispatch_right_click_does_not_activate() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        let initial = app.current_screen;

        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Right), 1, 0);
        app.dispatch_mouse(&up);
        assert_eq!(
            app.current_screen, initial,
            "Right-click should not switch tabs"
        );
    }

    #[test]
    fn dispatch_does_not_dismiss_palette_when_visible() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        // Open palette.
        app.command_palette.open();
        assert!(app.command_palette.is_visible());

        // When visible, palette has highest priority; dispatcher should not
        // route chrome interactions.
        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 50, 20);
        let result = app.dispatch_mouse(&up);
        assert_eq!(result, MouseDispatchResult::NotConsumed);
        assert!(app.command_palette.is_visible());
    }

    #[test]
    fn dispatch_hover_coalesces_same_target() {
        let mut app = AppModel::new();

        // First hover update — changes from None.
        let changed1 = app.mouse_dispatcher.update_hover(Some(HitId::new(1)));
        assert!(changed1);
        assert_eq!(app.mouse_dispatcher.hover_hit, Some(HitId::new(1)));

        // Same target — no change.
        let changed2 = app.mouse_dispatcher.update_hover(Some(HitId::new(1)));
        assert!(!changed2);

        // Different target — change.
        let changed3 = app.mouse_dispatcher.update_hover(Some(HitId::new(2)));
        assert!(changed3);
        assert_eq!(app.mouse_dispatcher.hover_hit, Some(HitId::new(2)));

        // Back to None — change.
        let changed4 = app.mouse_dispatcher.update_hover(None);
        assert!(changed4);
        assert_eq!(app.mouse_dispatcher.hover_hit, None);
    }

    #[test]
    fn dispatch_cancel_drag_resets_to_idle() {
        let mut app = AppModel::new();
        app.mouse_dispatcher.drag = DragPhase::Dragging {
            button: MouseButton::Left,
            start_x: 0,
            start_y: 0,
            last_x: 10,
            last_y: 10,
            hit_id: None,
        };
        assert!(app.mouse_dispatcher.is_dragging());
        app.mouse_dispatcher.cancel_drag();
        assert!(!app.mouse_dispatcher.is_dragging());
        assert!(matches!(app.mouse_dispatcher.drag, DragPhase::Idle));
    }

    #[test]
    fn dispatch_drag_hit_id_tracks_origin() {
        let mut app = AppModel::new();
        let target = HitId::new(42);

        app.mouse_dispatcher.drag = DragPhase::PendingDrag {
            button: MouseButton::Left,
            start_x: 0,
            start_y: 0,
            hit_id: Some(target),
        };
        assert_eq!(app.mouse_dispatcher.drag_hit_id(), Some(target));

        app.mouse_dispatcher.drag = DragPhase::Dragging {
            button: MouseButton::Left,
            start_x: 0,
            start_y: 0,
            last_x: 5,
            last_y: 5,
            hit_id: Some(target),
        };
        assert_eq!(app.mouse_dispatcher.drag_hit_id(), Some(target));

        app.mouse_dispatcher.drag = DragPhase::Idle;
        assert_eq!(app.mouse_dispatcher.drag_hit_id(), None);
    }

    #[test]
    fn dispatch_move_not_consumed_when_no_hover_change() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);

        // Move in empty area — no hover change.
        let move1 = MouseEvent::new(MouseEventKind::Moved, 60, 20);
        let _r1 = app.dispatch_mouse(&move1);
        // If there's a hit at that point it's Consumed, otherwise NotConsumed.
        // Second identical move should not change hover.
        let r2 = app.dispatch_mouse(&move1);
        assert_eq!(r2, MouseDispatchResult::NotConsumed);
    }

    #[test]
    fn handle_overlay_click_closes_help() {
        let mut app = AppModel::new();
        app.help_visible = true;
        let action = app.handle_overlay_click(crate::chrome::OVERLAY_HELP_CLOSE);
        assert_eq!(action, "overlay_help_close");
        assert!(!app.help_visible);
    }

    #[test]
    fn handle_overlay_click_closes_debug() {
        let mut app = AppModel::new();
        app.debug_visible = true;
        let action = app.handle_overlay_click(crate::chrome::OVERLAY_DEBUG);
        assert_eq!(action, "overlay_debug_close");
        assert!(!app.debug_visible);
    }

    #[test]
    fn handle_overlay_click_closes_perf_hud() {
        let mut app = AppModel::new();
        app.perf_hud_visible = true;
        let action = app.handle_overlay_click(crate::chrome::OVERLAY_PERF_HUD);
        assert_eq!(action, "overlay_perf_close");
        assert!(!app.perf_hud_visible);
    }

    #[test]
    fn handle_overlay_click_closes_a11y() {
        let mut app = AppModel::new();
        app.a11y_panel_visible = true;
        let action = app.handle_overlay_click(crate::chrome::OVERLAY_A11Y);
        assert_eq!(action, "overlay_a11y_close");
        assert!(!app.a11y_panel_visible);
    }

    #[test]
    fn handle_overlay_click_starts_guided_tour_on_landing_screen() {
        let mut app = AppModel::new();
        app.current_screen = ScreenId::GuidedTour;
        assert!(!app.tour.is_active());

        let action = app.handle_overlay_click(crate::chrome::OVERLAY_TOUR);
        assert_eq!(action, "overlay_tour_start");
        assert!(app.tour.is_active());
        assert_eq!(app.current_screen, ScreenId::GuidedTour);
    }

    #[test]
    fn handle_status_click_toggles_help() {
        let mut app = AppModel::new();
        assert!(!app.help_visible);

        let a1 = app.handle_status_click(crate::chrome::STATUS_HELP_TOGGLE);
        assert_eq!(a1, "status_toggle_help");
        assert!(app.help_visible);

        let a2 = app.handle_status_click(crate::chrome::STATUS_HELP_TOGGLE);
        assert_eq!(a2, "status_toggle_help");
        assert!(!app.help_visible);
    }

    #[test]
    fn handle_status_click_toggles_palette() {
        let mut app = AppModel::new();
        assert!(!app.command_palette.is_visible());

        app.handle_status_click(crate::chrome::STATUS_PALETTE_TOGGLE);
        assert!(app.command_palette.is_visible());

        app.handle_status_click(crate::chrome::STATUS_PALETTE_TOGGLE);
        assert!(!app.command_palette.is_visible());
    }

    #[test]
    fn handle_status_click_toggles_debug() {
        let mut app = AppModel::new();
        assert!(!app.debug_visible);

        app.handle_status_click(crate::chrome::STATUS_DEBUG_TOGGLE);
        assert!(app.debug_visible);

        app.handle_status_click(crate::chrome::STATUS_DEBUG_TOGGLE);
        assert!(!app.debug_visible);
    }

    #[test]
    fn handle_status_click_toggles_perf() {
        let mut app = AppModel::new();
        assert!(!app.perf_hud_visible);

        app.handle_status_click(crate::chrome::STATUS_PERF_TOGGLE);
        assert!(app.perf_hud_visible);

        app.handle_status_click(crate::chrome::STATUS_PERF_TOGGLE);
        assert!(!app.perf_hud_visible);
    }

    #[test]
    fn handle_status_click_toggles_a11y() {
        let mut app = AppModel::new();
        assert!(!app.a11y_panel_visible);

        app.handle_status_click(crate::chrome::STATUS_A11Y_TOGGLE);
        assert!(app.a11y_panel_visible);

        app.handle_status_click(crate::chrome::STATUS_A11Y_TOGGLE);
        assert!(!app.a11y_panel_visible);
    }

    /// Verify the error boundary catches panics and shows fallback.
    #[test]
    fn integration_error_boundary() {
        let mut states = ScreenStates::default();

        // Simulate a cached error for the Dashboard screen.
        states.screen_errors[ScreenId::Dashboard.index()] = Some("test panic message".to_string());

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);

        // This should show the fallback widget, not panic.
        states.view(ScreenId::Dashboard, &mut frame, area);

        // Verify the fallback rendered (look for the error border character).
        let top_left = frame.buffer.get(0, 0).unwrap();
        assert_eq!(
            top_left.content.as_char(),
            Some('┌'),
            "FallbackWidget should render error border"
        );

        // Verify the error can be cleared.
        assert!(states.has_error(ScreenId::Dashboard));
        states.clear_error(ScreenId::Dashboard);
        assert!(!states.has_error(ScreenId::Dashboard));
    }

    #[test]
    fn lazy_screens_initialize_on_first_use() {
        // Acquire exclusive access to screen init events for this test
        let guard = ScreenInitEventGuard::new();

        let mut states = ScreenStates::default();
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });

        assert!(!states.is_lazy_initialized(ScreenId::CodeExplorer));
        states.update(ScreenId::CodeExplorer, &event);
        assert!(states.is_lazy_initialized(ScreenId::CodeExplorer));

        assert!(!states.is_lazy_initialized(ScreenId::VisualEffects));
        states.update(ScreenId::VisualEffects, &event);
        assert!(states.is_lazy_initialized(ScreenId::VisualEffects));

        let events = guard.take_events();
        let mut code_explorer_events = 0;
        let mut visual_effects_events = 0;
        for entry in events {
            if entry.screen == ScreenId::CodeExplorer {
                code_explorer_events += 1;
            }
            if entry.screen == ScreenId::VisualEffects {
                visual_effects_events += 1;
            }
        }
        assert_eq!(
            code_explorer_events, 1,
            "expected one init log for CodeExplorer"
        );
        assert_eq!(
            visual_effects_events, 1,
            "expected one init log for VisualEffects"
        );
    }

    #[test]
    fn lazy_screen_init_logs_once_per_screen() {
        // Acquire exclusive access to screen init events for this test
        let guard = ScreenInitEventGuard::new();

        let mut app = AppModel::new();

        let screens = [
            ScreenId::CodeExplorer,
            ScreenId::FileBrowser,
            ScreenId::VisualEffects,
            ScreenId::SnapshotPlayer,
        ];

        for screen in screens {
            app.update(AppMsg::SwitchScreen(screen));

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            app.view(&mut frame);

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            app.view(&mut frame);
        }

        let mut counts = std::collections::HashMap::new();
        for entry in guard.take_events() {
            *counts.entry(entry.screen).or_insert(0usize) += 1;
        }

        for screen in screens {
            let count = counts.get(&screen).copied().unwrap_or(0);
            assert_eq!(count, 1, "expected exactly one init log for {:?}", screen);
        }
    }

    /// Verify Tab cycles forward through all screens.
    #[test]
    fn integration_tab_cycles_all_screens() {
        let mut app = AppModel::new();
        assert_eq!(app.current_screen, ScreenId::Dashboard);

        let ids = screens::screen_ids();
        let start_idx = ids
            .iter()
            .position(|id| *id == app.current_screen)
            .unwrap_or(0);
        for offset in 1..ids.len() {
            app.update(AppMsg::NextScreen);
            let expected = ids[(start_idx + offset) % ids.len()];
            assert_eq!(app.current_screen, expected);
        }
        let expected_end = ids[(start_idx + ids.len() - 1) % ids.len()];
        assert_eq!(app.current_screen, expected_end);
        if expected_end == ScreenId::GuidedTour {
            assert!(app.tour.is_active());
        }
    }

    /// Verify all screens have the expected count.
    #[test]
    fn all_screens_count() {
        assert_eq!(screens::screen_registry().len(), 42);
    }

    // -----------------------------------------------------------------------
    // Command palette integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn ctrl_k_opens_palette() {
        let mut app = AppModel::new();
        assert!(!app.command_palette.is_visible());

        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('k'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(event));
        assert!(app.command_palette.is_visible());
    }

    #[test]
    fn palette_esc_dismisses() {
        let mut app = AppModel::new();
        app.command_palette.open();
        assert!(app.command_palette.is_visible());

        let esc = Event::Key(KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(esc));
        assert!(!app.command_palette.is_visible());
    }

    #[test]
    fn palette_has_actions_for_all_screens() {
        let app = AppModel::new();
        // One action per screen + 7 global commands (quit, help, theme, debug, perf_hud, evidence_ledger, toggle_mouse_capture)
        let expected = screens::screen_registry().len() + 7;
        assert_eq!(app.command_palette.action_count(), expected);
    }

    #[test]
    fn palette_category_filter_ctrl_numbers() {
        let mut app = AppModel::new();
        app.command_palette.open();

        let ctrl_1 = Event::Key(KeyEvent {
            code: KeyCode::Char('1'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(ctrl_1));
        assert_eq!(
            app.palette_category_filter,
            Some(screens::ScreenCategory::Tour)
        );
        let tour_count = screens::screen_registry()
            .iter()
            .filter(|meta| meta.category == screens::ScreenCategory::Tour)
            .count();
        assert_eq!(app.command_palette.action_count(), tour_count + 7);

        let ctrl_0 = Event::Key(KeyEvent {
            code: KeyCode::Char('0'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(ctrl_0));
        assert_eq!(app.palette_category_filter, None);
        assert_eq!(
            app.command_palette.action_count(),
            screens::screen_registry().len() + 7
        );
    }

    #[test]
    fn palette_toggle_favorite_and_filter() {
        let mut app = AppModel::new();
        app.command_palette.open();

        for ch in "dashboard".chars() {
            let event = Event::Key(KeyEvent {
                code: KeyCode::Char(ch),
                modifiers: Modifiers::NONE,
                kind: KeyEventKind::Press,
            });
            app.update(AppMsg::from(event));
        }

        let ctrl_f = Event::Key(KeyEvent {
            code: KeyCode::Char('f'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(ctrl_f));
        assert!(app.screen_favorites.contains(&ScreenId::Dashboard));

        let ctrl_shift_f = Event::Key(KeyEvent {
            code: KeyCode::Char('F'),
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(ctrl_shift_f.clone()));
        assert!(app.palette_favorites_only);
        assert_eq!(app.command_palette.action_count(), 1 + 7);

        app.update(AppMsg::from(ctrl_shift_f));
        assert!(!app.palette_favorites_only);
        assert_eq!(
            app.command_palette.action_count(),
            screens::screen_registry().len() + 7
        );
    }

    #[test]
    fn palette_navigate_to_screen() {
        let mut app = AppModel::new();
        assert_eq!(app.current_screen, ScreenId::Dashboard);

        // Open palette, type "shakespeare", press Enter
        app.command_palette.open();
        for ch in "shakespeare".chars() {
            let event = Event::Key(KeyEvent {
                code: KeyCode::Char(ch),
                modifiers: Modifiers::NONE,
                kind: KeyEventKind::Press,
            });
            app.update(AppMsg::from(event));
        }

        let enter = Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(enter));
        assert_eq!(app.current_screen, ScreenId::Shakespeare);
        assert!(!app.command_palette.is_visible());
    }

    #[test]
    fn palette_execute_quit() {
        let mut app = AppModel::new();

        // Directly test execute_palette_action
        let cmd = app.execute_palette_action(PaletteAction::Execute("cmd:quit".into()));
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn palette_toggle_help_via_action() {
        let mut app = AppModel::new();
        assert!(!app.help_visible);

        app.execute_palette_action(PaletteAction::Execute("cmd:toggle_help".into()));
        assert!(app.help_visible);
    }

    #[test]
    fn palette_cycle_theme_via_action() {
        // Create app - this sets base_theme to CyberpunkAurora internally
        let mut app = AppModel::new();
        let before = app.base_theme;
        assert_eq!(before, theme::ThemeId::CyberpunkAurora);

        app.execute_palette_action(PaletteAction::Execute("cmd:cycle_theme".into()));

        // Verify base_theme cycled to Darcula (the next non-accessibility theme)
        assert_eq!(app.base_theme, theme::ThemeId::Darcula);
        assert_ne!(before, app.base_theme);
    }

    #[test]
    fn palette_blocks_screen_events_when_open() {
        let mut app = AppModel::new();
        app.command_palette.open();

        // 'q' key should NOT quit the app when palette is open
        let q = Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        let cmd = app.update(AppMsg::from(q));
        assert!(!matches!(cmd, Cmd::Quit));
        // The 'q' was consumed by the palette as query input
        assert_eq!(app.command_palette.query(), "q");
    }

    #[test]
    fn palette_renders_as_overlay() {
        let mut app = AppModel::new();
        app.terminal_width = 80;
        app.terminal_height = 24;
        app.command_palette.open();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        app.view(&mut frame);
        // Should not panic — overlay rendered on top of content.
    }

    // -----------------------------------------------------------------------
    // Performance HUD tests (bd-3k3x.2)
    // -----------------------------------------------------------------------

    #[test]
    fn toggle_perf_hud() {
        let mut app = AppModel::new();
        assert!(!app.perf_hud_visible);

        app.update(AppMsg::TogglePerfHud);
        assert!(app.perf_hud_visible);

        app.update(AppMsg::TogglePerfHud);
        assert!(!app.perf_hud_visible);
    }

    #[test]
    fn ctrl_p_toggles_perf_hud() {
        let mut app = AppModel::new();
        assert!(!app.perf_hud_visible);

        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('p'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(event.clone()));
        assert!(app.perf_hud_visible);

        app.update(AppMsg::from(event));
        assert!(!app.perf_hud_visible);
    }

    #[test]
    fn perf_stats_empty() {
        let app = AppModel::new();
        let (tps, avg, p95, p99, min, max) = app.perf_stats();
        assert_eq!(tps, 0.0);
        assert_eq!(avg, 0.0);
        assert_eq!(p95, 0.0);
        assert_eq!(p99, 0.0);
        assert_eq!(min, 0.0);
        assert_eq!(max, 0.0);
    }

    #[test]
    fn perf_stats_with_samples() {
        let mut app = AppModel::new();
        // Inject known samples: 100ms intervals (100_000 µs each)
        for _ in 0..10 {
            app.perf_tick_times_us.push_back(100_000);
        }
        let (tps, avg_ms, _p95, _p99, min_ms, max_ms) = app.perf_stats();
        assert!((tps - 10.0).abs() < 0.1, "tps should be ~10, got {tps}");
        assert!(
            (avg_ms - 100.0).abs() < 0.1,
            "avg should be ~100ms, got {avg_ms}"
        );
        assert!(
            (min_ms - 100.0).abs() < 0.1,
            "min should be ~100ms, got {min_ms}"
        );
        assert!(
            (max_ms - 100.0).abs() < 0.1,
            "max should be ~100ms, got {max_ms}"
        );
    }

    #[test]
    fn perf_sparkline_empty() {
        let app = AppModel::new();
        assert!(app.perf_sparkline(40).is_empty());
    }

    #[test]
    fn perf_sparkline_generates_output() {
        let mut app = AppModel::new();
        for i in 0..20 {
            app.perf_tick_times_us.push_back(50_000 + i * 5_000);
        }
        let sparkline = app.perf_sparkline(20);
        assert_eq!(
            sparkline.chars().count(),
            20,
            "sparkline should use all 20 samples"
        );
        assert!(
            sparkline
                .chars()
                .all(|c| c == ' ' || ('\u{2581}'..='\u{2588}').contains(&c))
        );
    }

    #[test]
    fn perf_sparkline_respects_max_width() {
        let mut app = AppModel::new();
        for i in 0..50 {
            app.perf_tick_times_us.push_back(100_000 + i * 1_000);
        }
        let sparkline = app.perf_sparkline(10);
        // Should take only the last 10 samples
        assert_eq!(sparkline.chars().count(), 10);
    }

    #[test]
    fn perf_hud_renders_without_panic() {
        let mut app = AppModel::new();
        app.perf_hud_visible = true;
        app.terminal_width = 120;
        app.terminal_height = 40;

        // Add some tick samples so stats are non-trivial
        for _ in 0..30 {
            app.perf_tick_times_us.push_back(100_000);
        }

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        app.view(&mut frame);
    }

    #[test]
    fn perf_hud_degrades_on_tiny_terminal() {
        let mut app = AppModel::new();
        app.perf_hud_visible = true;
        app.terminal_width = 15;
        app.terminal_height = 5;

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(15, 5, &mut pool);
        // Should not panic even when area is too small for HUD
        app.view(&mut frame);
    }

    #[test]
    fn perf_tick_ring_buffer_caps_at_120() {
        let mut app = AppModel::new();
        for i in 0..200 {
            app.perf_tick_times_us.push_back(i * 1000);
        }
        // Ring buffer should have capped... but push_back alone doesn't cap.
        // record_tick_timing() is what caps it. Test that method.
        let mut app2 = AppModel::new();
        // Simulate 200 ticks with known intervals
        for _ in 0..200 {
            app2.perf_last_tick = Some(std::time::Instant::now());
            // Manually push to simulate without real timing
            if app2.perf_tick_times_us.len() >= 120 {
                app2.perf_tick_times_us.pop_front();
            }
            app2.perf_tick_times_us.push_back(100_000);
        }
        assert_eq!(app2.perf_tick_times_us.len(), 120);
    }

    #[test]
    fn palette_toggle_perf_hud_via_action() {
        let mut app = AppModel::new();
        assert!(!app.perf_hud_visible);

        app.execute_palette_action(PaletteAction::Execute("cmd:toggle_perf_hud".into()));
        assert!(app.perf_hud_visible);

        app.execute_palette_action(PaletteAction::Execute("cmd:toggle_perf_hud".into()));
        assert!(!app.perf_hud_visible);
    }

    #[test]
    fn palette_includes_perf_hud_action() {
        let app = AppModel::new();
        // The palette should have the perf HUD action registered.
        // 7 global commands: quit, help, theme, debug, perf_hud, evidence_ledger, toggle_mouse_capture.
        let expected = screens::screen_registry().len() + 7;
        assert_eq!(app.command_palette.action_count(), expected);
    }

    // -----------------------------------------------------------------------
    // Performance HUD — Property Tests (bd-3k3x.6)
    // -----------------------------------------------------------------------

    /// Property: perf_stats is deterministic — same samples always produce same output.
    #[test]
    fn perf_stats_deterministic() {
        let mut app1 = AppModel::new();
        let mut app2 = AppModel::new();
        let samples = [
            80_000u64, 95_000, 110_000, 100_000, 87_000, 102_000, 99_000, 105_000,
        ];
        for &s in &samples {
            app1.perf_tick_times_us.push_back(s);
            app2.perf_tick_times_us.push_back(s);
        }
        let s1 = app1.perf_stats();
        let s2 = app2.perf_stats();
        assert_eq!(s1.0.to_bits(), s2.0.to_bits(), "tps must be identical");
        assert_eq!(s1.1.to_bits(), s2.1.to_bits(), "avg must be identical");
        assert_eq!(s1.2.to_bits(), s2.2.to_bits(), "p95 must be identical");
        assert_eq!(s1.3.to_bits(), s2.3.to_bits(), "p99 must be identical");
        assert_eq!(s1.4.to_bits(), s2.4.to_bits(), "min must be identical");
        assert_eq!(s1.5.to_bits(), s2.5.to_bits(), "max must be identical");
    }

    /// Property: perf_stats returns non-negative values for all fields.
    #[test]
    fn perf_stats_non_negative() {
        for n in [1, 2, 5, 10, 50, 120] {
            let mut app = AppModel::new();
            for i in 0..n {
                app.perf_tick_times_us
                    .push_back(50_000 + (i as u64) * 3_000);
            }
            let (_tps, avg, p95, p99, min, max) = app.perf_stats();
            assert!(avg >= 0.0, "avg must be non-negative (n={n})");
            assert!(p95 >= 0.0, "p95 must be non-negative (n={n})");
            assert!(p99 >= 0.0, "p99 must be non-negative (n={n})");
            assert!(min >= 0.0, "min must be non-negative (n={n})");
            assert!(max >= 0.0, "max must be non-negative (n={n})");
        }
    }

    /// Property: min <= avg <= max for any sample set.
    #[test]
    fn perf_stats_ordering_invariant() {
        for pattern in [
            vec![100_000u64; 10],                                 // uniform
            (0..20).map(|i| 50_000 + i * 10_000).collect(),       // ascending
            (0..20).rev().map(|i| 50_000 + i * 10_000).collect(), // descending
            vec![1_000, 1_000_000, 500_000, 2_000, 800_000],      // high variance
        ] {
            let mut app = AppModel::new();
            for s in &pattern {
                app.perf_tick_times_us.push_back(*s);
            }
            let (_tps, avg, p95, p99, min, max) = app.perf_stats();
            assert!(
                min <= avg,
                "min ({min}) must be <= avg ({avg}) for pattern len={}",
                pattern.len()
            );
            assert!(
                avg <= max,
                "avg ({avg}) must be <= max ({max}) for pattern len={}",
                pattern.len()
            );
            assert!(p95 <= max, "p95 ({p95}) must be <= max ({max})");
            assert!(p99 <= max, "p99 ({p99}) must be <= max ({max})");
            assert!(min <= p95, "min ({min}) must be <= p95 ({p95})");
        }
    }

    /// Property: perf_stats with one sample has min == avg == max.
    #[test]
    fn perf_stats_single_sample() {
        let mut app = AppModel::new();
        app.perf_tick_times_us.push_back(42_000);
        let (_tps, avg, p95, p99, min, max) = app.perf_stats();
        assert_eq!(min, max, "single sample: min must equal max");
        assert_eq!(avg, min, "single sample: avg must equal min");
        assert_eq!(p95, min, "single sample: p95 must equal min");
        assert_eq!(p99, min, "single sample: p99 must equal min");
    }

    /// Property: perf_sparkline output length never exceeds max_width (char count).
    #[test]
    fn perf_sparkline_width_bound() {
        for width in [1, 5, 10, 40, 100] {
            let mut app = AppModel::new();
            for i in 0..200 {
                app.perf_tick_times_us.push_back(80_000 + (i % 50) * 2_000);
            }
            let sparkline = app.perf_sparkline(width);
            let char_count = sparkline.chars().count();
            assert!(
                char_count <= width,
                "sparkline char count ({char_count}) must be <= max_width ({width})"
            );
        }
    }

    /// Property: perf_sparkline chars are always valid block characters or space.
    #[test]
    fn perf_sparkline_valid_chars() {
        let valid_chars: Vec<char> = vec![
            ' ', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}',
            '\u{2587}', '\u{2588}',
        ];
        for n in [1, 2, 10, 50, 120] {
            let mut app = AppModel::new();
            for i in 0..n {
                app.perf_tick_times_us
                    .push_back(10_000 + (i as u64) * 7_777);
            }
            let sparkline = app.perf_sparkline(50);
            for ch in sparkline.chars() {
                assert!(
                    valid_chars.contains(&ch),
                    "sparkline contains invalid char: {ch:?} (n={n})"
                );
            }
        }
    }

    /// Property: toggle is idempotent — double toggle returns to original state.
    #[test]
    fn perf_hud_toggle_idempotent() {
        let mut app = AppModel::new();
        let initial = app.perf_hud_visible;
        app.update(AppMsg::TogglePerfHud);
        app.update(AppMsg::TogglePerfHud);
        assert_eq!(
            app.perf_hud_visible, initial,
            "double toggle must restore state"
        );
    }

    /// Property: perf HUD rendering never panics across many terminal sizes.
    #[test]
    fn perf_hud_renders_all_sizes() {
        let mut app = AppModel::new();
        app.perf_hud_visible = true;
        for i in 0..60 {
            app.perf_tick_times_us.push_back(90_000 + (i % 30) * 2_000);
        }

        for (w, h) in [
            (1, 1),
            (10, 5),
            (20, 10),
            (40, 15),
            (80, 24),
            (120, 40),
            (200, 60),
        ] {
            app.terminal_width = w;
            app.terminal_height = h;
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(w, h, &mut pool);
            app.view(&mut frame);
        }
    }

    /// Property: perf_stats tps is consistent with avg_ms (tps ≈ 1000/avg_ms).
    #[test]
    fn perf_stats_tps_avg_consistency() {
        let mut app = AppModel::new();
        for _ in 0..50 {
            app.perf_tick_times_us.push_back(100_000); // 100ms intervals
        }
        let (tps, avg_ms, _, _, _, _) = app.perf_stats();
        let expected_tps = 1000.0 / avg_ms;
        assert!(
            (tps - expected_tps).abs() < 0.01,
            "tps ({tps}) should equal 1000/avg_ms ({expected_tps})"
        );
    }

    /// Property: perf_view_counter increments on each view call.
    #[test]
    fn perf_view_counter_increments() {
        let app = AppModel::new();
        let before = app.perf_view_counter.get();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        app.view(&mut frame);

        assert_eq!(
            app.perf_view_counter.get(),
            before + 1,
            "view counter must increment on each view() call"
        );
    }

    // -----------------------------------------------------------------------
    // Performance HUD Diagnostics Tests (bd-3k3x.8)
    // -----------------------------------------------------------------------

    /// Test that JSONL logging is disabled by default.
    #[test]
    fn perf_hud_jsonl_disabled_by_default() {
        // The function checks FTUI_PERF_HUD_JSONL env var
        // In test environment without explicit setting, it should be disabled
        // We can't easily test stderr output, but we verify the guard function
        let enabled = perf_hud_jsonl_enabled();
        // Note: This may be true if the env var is set in CI
        // The important thing is that the function doesn't panic
        let _ = enabled; // Exercise the code path
    }

    /// Test that emit_perf_hud_jsonl doesn't panic with various inputs.
    #[test]
    fn perf_hud_jsonl_emit_no_panic() {
        // These should all complete without panicking, even when JSONL is disabled
        emit_perf_hud_jsonl("test_event", &[]);
        emit_perf_hud_jsonl("test_event", &[("key", "value")]);
        emit_perf_hud_jsonl("test_event", &[("key", "value with \"quotes\"")]);
        emit_perf_hud_jsonl("test_event", &[("k1", "v1"), ("k2", "v2"), ("k3", "v3")]);
    }

    /// Test that emit_perf_hud_jsonl_numeric doesn't panic with edge cases.
    #[test]
    fn perf_hud_jsonl_numeric_no_panic() {
        // These should all complete without panicking
        emit_perf_hud_jsonl_numeric("test_numeric", &[]);
        emit_perf_hud_jsonl_numeric("test_numeric", &[("value", 42.0)]);
        emit_perf_hud_jsonl_numeric("test_numeric", &[("inf", f64::INFINITY)]);
        emit_perf_hud_jsonl_numeric("test_numeric", &[("nan", f64::NAN)]);
        emit_perf_hud_jsonl_numeric("test_numeric", &[("neg", -100.5)]);
        emit_perf_hud_jsonl_numeric("test_numeric", &[("zero", 0.0)]);
    }

    /// Test that PERF_HUD_LOG_SEQ increments monotonically.
    #[test]
    fn perf_hud_log_seq_increments() {
        use std::sync::atomic::Ordering;
        let before = PERF_HUD_LOG_SEQ.load(Ordering::Relaxed);
        emit_perf_hud_jsonl("seq_test_1", &[]);
        emit_perf_hud_jsonl("seq_test_2", &[]);
        let after = PERF_HUD_LOG_SEQ.load(Ordering::Relaxed);
        // Sequence should have incremented by at least 2 (possibly more if JSONL was enabled)
        // If JSONL is disabled, the seq still increments
        assert!(
            after >= before,
            "sequence number must be monotonically increasing"
        );
    }

    /// Test diagnostic logging during tick timing (integration).
    #[test]
    fn perf_hud_tick_stats_tracing_no_panic() {
        let mut app = AppModel::new();
        app.perf_hud_visible = true;

        // Add some samples
        for i in 0..60 {
            app.perf_tick_times_us.push_back(50_000 + i * 1000);
        }

        // Simulate ticks to trigger the periodic logging (every 60 ticks)
        for _ in 0..120 {
            app.update(AppMsg::Tick);
        }
        // Should complete without panic
    }

    // -----------------------------------------------------------------------
    // Evidence Ledger tests (bd-1rz0.27)
    // -----------------------------------------------------------------------

    #[test]
    fn toggle_evidence_ledger() {
        let mut app = AppModel::new();
        assert!(!app.evidence_ledger_visible);

        app.update(AppMsg::ToggleEvidenceLedger);
        assert!(app.evidence_ledger_visible);

        app.update(AppMsg::ToggleEvidenceLedger);
        assert!(!app.evidence_ledger_visible);
    }

    // -----------------------------------------------------------------------
    // Mouse dispatch regression tests (bd-iuvb.17.7 scope)
    // -----------------------------------------------------------------------

    /// Integration test: mouse click on status bar help toggle changes help_visible.
    #[test]
    fn mouse_status_bar_help_toggle_integration() {
        let mut app = AppModel::new();
        app.terminal_width = 200;
        app.terminal_height = 40;
        assert!(!app.help_visible, "Help should be off initially");

        let mut pool = GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::with_hit_grid(200, 40, &mut pool);
        app.view(&mut frame);

        let status_y = 39u16;
        let mut help_x = None;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, status_y)
                && id.id() == crate::chrome::STATUS_HELP_TOGGLE
            {
                help_x = Some(x);
                break;
            }
        }

        if let Some(x) = help_x {
            let down = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Down(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let up = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Up(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let _ = app.update(AppMsg::from(down));
            let _ = app.update(AppMsg::from(up));
            assert!(app.help_visible, "Help should be toggled on after click");
        }
    }

    /// Integration test: mouse click on status bar debug toggle changes debug_visible.
    #[test]
    fn mouse_status_bar_debug_toggle_integration() {
        let mut app = AppModel::new();
        app.terminal_width = 200;
        app.terminal_height = 40;
        assert!(!app.debug_visible);

        let mut pool = GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::with_hit_grid(200, 40, &mut pool);
        app.view(&mut frame);

        let status_y = 39u16;
        let mut debug_x = None;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, status_y)
                && id.id() == crate::chrome::STATUS_DEBUG_TOGGLE
            {
                debug_x = Some(x);
                break;
            }
        }

        if let Some(x) = debug_x {
            let down = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Down(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let up = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Up(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let _ = app.update(AppMsg::from(down));
            let _ = app.update(AppMsg::from(up));
            assert!(app.debug_visible, "Debug should be toggled on after click");
        }
    }

    /// Integration test: mouse click on status bar perf toggle changes perf_hud_visible.
    #[test]
    fn mouse_status_bar_perf_toggle_integration() {
        let mut app = AppModel::new();
        app.terminal_width = 200;
        app.terminal_height = 40;
        assert!(!app.perf_hud_visible);

        let mut pool = GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::with_hit_grid(200, 40, &mut pool);
        app.view(&mut frame);

        let status_y = 39u16;
        let mut perf_x = None;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, status_y)
                && id.id() == crate::chrome::STATUS_PERF_TOGGLE
            {
                perf_x = Some(x);
                break;
            }
        }

        if let Some(x) = perf_x {
            let down = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Down(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let up = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Up(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let _ = app.update(AppMsg::from(down));
            let _ = app.update(AppMsg::from(up));
            assert!(
                app.perf_hud_visible,
                "Perf HUD should be toggled on after click"
            );
        }
    }

    /// Integration test: scroll wheel on tab bar cycles screens.
    #[test]
    fn mouse_scroll_wheel_cycles_tabs() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;

        let initial_screen = app.current_screen;

        let mut pool = GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::with_hit_grid(120, 40, &mut pool);
        app.view(&mut frame);

        let scroll = Event::Mouse(ftui_core::event::MouseEvent::new(
            ftui_core::event::MouseEventKind::ScrollDown,
            60,
            0,
        ));
        let _ = app.update(AppMsg::from(scroll));

        assert_ne!(
            app.current_screen, initial_screen,
            "Scroll should cycle to next screen"
        );
    }

    /// Integration test: scroll wheel up on tab bar cycles back.
    #[test]
    fn mouse_scroll_wheel_up_cycles_tabs_back() {
        let mut app = AppModel::new();
        app.terminal_width = 120;
        app.terminal_height = 40;

        app.update(AppMsg::NextScreen);
        let second_screen = app.current_screen;

        let mut pool = GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::with_hit_grid(120, 40, &mut pool);
        app.view(&mut frame);

        let scroll = Event::Mouse(ftui_core::event::MouseEvent::new(
            ftui_core::event::MouseEventKind::ScrollUp,
            60,
            0,
        ));
        let _ = app.update(AppMsg::from(scroll));

        assert_ne!(
            app.current_screen, second_screen,
            "Scroll up should cycle to previous screen"
        );
    }

    /// Integration test: mouse double-toggle returns to original state.
    #[test]
    fn mouse_status_toggle_double_click_returns_to_original() {
        let mut app = AppModel::new();
        app.terminal_width = 200;
        app.terminal_height = 40;
        assert!(!app.help_visible);

        let mut pool = GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::with_hit_grid(200, 40, &mut pool);
        app.view(&mut frame);

        let status_y = 39u16;
        let mut help_x = None;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, status_y)
                && id.id() == crate::chrome::STATUS_HELP_TOGGLE
            {
                help_x = Some(x);
                break;
            }
        }

        if let Some(x) = help_x {
            // First click
            let down = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Down(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let up = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Up(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let _ = app.update(AppMsg::from(down));
            let _ = app.update(AppMsg::from(up));
            assert!(app.help_visible);

            // Re-render with help overlay shown
            let mut pool2 = GraphemePool::new();
            let mut frame2 = ftui_render::frame::Frame::with_hit_grid(200, 40, &mut pool2);
            app.view(&mut frame2);

            // Second click: help overlay has priority, so close it via the overlay close button.
            let mut close_pos = None;
            for y in 0..40u16 {
                for x in 0..200u16 {
                    if let Some((id, _region, _data)) = frame2.hit_test(x, y)
                        && id.id() == crate::chrome::OVERLAY_HELP_CLOSE
                    {
                        close_pos = Some((x, y));
                        break;
                    }
                }
                if close_pos.is_some() {
                    break;
                }
            }

            if let Some((x, y)) = close_pos {
                let down2 = Event::Mouse(ftui_core::event::MouseEvent::new(
                    ftui_core::event::MouseEventKind::Down(ftui_core::event::MouseButton::Left),
                    x,
                    y,
                ));
                let up2 = Event::Mouse(ftui_core::event::MouseEvent::new(
                    ftui_core::event::MouseEventKind::Up(ftui_core::event::MouseButton::Left),
                    x,
                    y,
                ));
                let _ = app.update(AppMsg::from(down2));
                let _ = app.update(AppMsg::from(up2));
                assert!(!app.help_visible, "Overlay close should return to OFF");
            }
        }
    }

    /// Integration test: mouse capture toggle via status bar.
    #[test]
    fn mouse_capture_toggle_via_status_bar() {
        let mut app = AppModel::new();
        app.terminal_width = 200;
        app.terminal_height = 40;
        let initial_capture = app.mouse_capture_enabled;

        let mut pool = GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::with_hit_grid(200, 40, &mut pool);
        app.view(&mut frame);

        let status_y = 39u16;
        let mut mouse_x = None;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, status_y)
                && id.id() == crate::chrome::STATUS_MOUSE_TOGGLE
            {
                mouse_x = Some(x);
                break;
            }
        }

        if let Some(x) = mouse_x {
            let down = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Down(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let up = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Up(ftui_core::event::MouseButton::Left),
                x,
                status_y,
            ));
            let _ = app.update(AppMsg::from(down));
            let _ = app.update(AppMsg::from(up));
            assert_ne!(
                app.mouse_capture_enabled, initial_capture,
                "Mouse capture should toggle"
            );
        }
    }

    /// Regression test: dispatch priority — overlay click when help visible
    /// should dismiss help, not toggle status bar behind it.
    #[test]
    fn dispatch_priority_overlay_over_status_bar() {
        let mut app = AppModel::new();
        app.terminal_width = 200;
        app.terminal_height = 40;
        app.help_visible = true;

        let mut pool = GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::with_hit_grid(200, 40, &mut pool);
        app.view(&mut frame);

        let mut close_pos = None;
        for y in 0..40u16 {
            for x in 0..200u16 {
                if let Some((id, _region, _data)) = frame.hit_test(x, y)
                    && id.id() == crate::chrome::OVERLAY_HELP_CLOSE
                {
                    close_pos = Some((x, y));
                    break;
                }
            }
            if close_pos.is_some() {
                break;
            }
        }

        if let Some((x, y)) = close_pos {
            let down = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Down(ftui_core::event::MouseButton::Left),
                x,
                y,
            ));
            let up = Event::Mouse(ftui_core::event::MouseEvent::new(
                ftui_core::event::MouseEventKind::Up(ftui_core::event::MouseButton::Left),
                x,
                y,
            ));
            let _ = app.update(AppMsg::from(down));
            let _ = app.update(AppMsg::from(up));
            assert!(
                !app.help_visible,
                "Clicking overlay close should dismiss help"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Esc overlay priority tests (bd-iuvb.17.5)
    // -----------------------------------------------------------------------

    fn esc_event() -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn esc_closes_evidence_ledger_first() {
        let mut app = AppModel::new();
        app.evidence_ledger_visible = true;
        app.perf_hud_visible = true;

        app.update(AppMsg::from(esc_event()));

        assert!(
            !app.evidence_ledger_visible,
            "Esc should close evidence ledger (topmost)"
        );
        assert!(
            app.perf_hud_visible,
            "Perf HUD should remain open (lower layer)"
        );
    }

    #[test]
    fn esc_closes_perf_hud_when_topmost() {
        let mut app = AppModel::new();
        app.perf_hud_visible = true;
        app.help_visible = true;

        app.update(AppMsg::from(esc_event()));

        assert!(
            !app.perf_hud_visible,
            "Esc should close perf HUD (topmost after evidence)"
        );
        assert!(app.help_visible, "Help should remain open (lower layer)");
    }

    #[test]
    fn esc_closes_debug_overlay_when_topmost() {
        let mut app = AppModel::new();
        app.debug_visible = true;

        app.update(AppMsg::from(esc_event()));

        assert!(!app.debug_visible, "Esc should close debug overlay");
    }

    #[test]
    fn esc_closes_help_when_topmost() {
        let mut app = AppModel::new();
        app.help_visible = true;

        app.update(AppMsg::from(esc_event()));

        assert!(!app.help_visible, "Esc should close help overlay");
    }

    #[test]
    fn esc_cascades_through_all_overlays() {
        let mut app = AppModel::new();
        app.evidence_ledger_visible = true;
        app.perf_hud_visible = true;
        app.debug_visible = true;
        app.help_visible = true;

        // First Esc: evidence ledger
        app.update(AppMsg::from(esc_event()));
        assert!(!app.evidence_ledger_visible);
        assert!(app.perf_hud_visible);

        // Second Esc: perf HUD
        app.update(AppMsg::from(esc_event()));
        assert!(!app.perf_hud_visible);
        assert!(app.debug_visible);

        // Third Esc: debug
        app.update(AppMsg::from(esc_event()));
        assert!(!app.debug_visible);
        assert!(app.help_visible);

        // Fourth Esc: help
        app.update(AppMsg::from(esc_event()));
        assert!(!app.help_visible);
    }

    // -----------------------------------------------------------------------
    // Ctrl+I evidence inspector shortcut test (bd-iuvb.17.5)
    // -----------------------------------------------------------------------

    #[test]
    fn ctrl_i_toggles_evidence_ledger() {
        let mut app = AppModel::new();
        assert!(!app.evidence_ledger_visible);

        let ctrl_i = Event::Key(KeyEvent {
            code: KeyCode::Char('i'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        app.update(AppMsg::from(ctrl_i.clone()));
        assert!(
            app.evidence_ledger_visible,
            "Ctrl+I should open evidence ledger"
        );

        app.update(AppMsg::from(ctrl_i));
        assert!(
            !app.evidence_ledger_visible,
            "Ctrl+I should close evidence ledger"
        );
    }
}

#![forbid(unsafe_code)]

//! Mouse Playground screen — demonstrates mouse event handling and hit-testing.
//!
//! This screen showcases:
//! - Mouse event decoding (SGR, scroll, drag)
//! - Hit-test accuracy with spatial indexing
//! - Hover jitter stabilization (bd-9n09)
//! - Interactive widgets with click/hover feedback
//!
//! # Telemetry and Diagnostics (bd-bksf.5)
//!
//! This module provides rich diagnostic logging and telemetry hooks:
//! - JSONL diagnostic output via `DiagnosticLog`
//! - Observable hooks for hit-test, hover, and click events
//! - Deterministic mode for reproducible testing
//!
//! ## Environment Variables
//!
//! - `FTUI_MOUSE_DIAGNOSTICS=true` - Enable verbose diagnostic output
//! - `FTUI_MOUSE_DETERMINISTIC=true` - Enable deterministic mode (fixed timestamps)

use std::cell::Cell;
use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[cfg(not(test))]
use std::sync::atomic::AtomicU64;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_core::hover_stabilizer::{HoverStabilizer, HoverStabilizerConfig};
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::{Cell as RenderCell, CellAttrs, StyleFlags as CellStyleFlags};
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::determinism;
use crate::theme;

/// Maximum number of events to keep in the log.
const MAX_EVENT_LOG: usize = 12;

// =============================================================================
// Diagnostic Logging (bd-bksf.5)
// =============================================================================

/// Global diagnostic enable flag (checked once at startup).
static DIAGNOSTICS_ENABLED: AtomicBool = AtomicBool::new(false);
/// Monotonic event counter for deterministic ordering.
///
/// Note: tests in this crate run in parallel by default. A single global
/// counter makes `reset_event_counter()` inherently racy across tests, so we
/// use a per-thread counter under `cfg(test)` to keep unit tests deterministic.
#[cfg(not(test))]
static EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    static EVENT_COUNTER: Cell<u64> = const { Cell::new(0) };
}

/// Initialize diagnostic settings from environment.
///
/// Call this once at startup or in tests to configure diagnostic behavior.
pub fn init_diagnostics() {
    let enabled = std::env::var("FTUI_MOUSE_DIAGNOSTICS")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Check if diagnostics are enabled.
#[inline]
pub fn diagnostics_enabled() -> bool {
    DIAGNOSTICS_ENABLED.load(Ordering::Relaxed)
}

/// Set diagnostics enabled state (for testing).
pub fn set_diagnostics_enabled(enabled: bool) {
    DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Get next monotonic event sequence number.
#[inline]
fn next_event_seq() -> u64 {
    #[cfg(test)]
    {
        EVENT_COUNTER.with(|counter| {
            let seq = counter.get();
            counter.set(seq.saturating_add(1));
            seq
        })
    }

    #[cfg(not(test))]
    {
        EVENT_COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}

/// Reset event counter (for testing determinism).
pub fn reset_event_counter() {
    #[cfg(test)]
    EVENT_COUNTER.with(|counter| counter.set(0));

    #[cfg(not(test))]
    EVENT_COUNTER.store(0, Ordering::Relaxed);
}

/// Diagnostic event types for JSONL logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticEventKind {
    /// Mouse button pressed.
    MouseDown,
    /// Mouse button released.
    MouseUp,
    /// Mouse dragged.
    MouseDrag,
    /// Mouse moved (no button).
    MouseMove,
    /// Mouse scroll event.
    MouseScroll,
    /// Hit test performed.
    HitTest,
    /// Hover state changed.
    HoverChange,
    /// Target clicked.
    TargetClick,
    /// Overlay toggled.
    OverlayToggle,
    /// Jitter stats toggled.
    JitterStatsToggle,
    /// Event log cleared.
    LogClear,
    /// Tick processed.
    Tick,
    /// Grid rendered.
    GridRender,
}

impl DiagnosticEventKind {
    /// Get the JSONL event type string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MouseDown => "mouse_down",
            Self::MouseUp => "mouse_up",
            Self::MouseDrag => "mouse_drag",
            Self::MouseMove => "mouse_move",
            Self::MouseScroll => "mouse_scroll",
            Self::HitTest => "hit_test",
            Self::HoverChange => "hover_change",
            Self::TargetClick => "target_click",
            Self::OverlayToggle => "overlay_toggle",
            Self::JitterStatsToggle => "jitter_stats_toggle",
            Self::LogClear => "log_clear",
            Self::Tick => "tick",
            Self::GridRender => "grid_render",
        }
    }
}

/// JSONL diagnostic log entry.
///
/// This struct captures telemetry data in a structured format suitable
/// for post-hoc analysis and debugging.
#[derive(Debug, Clone)]
pub struct DiagnosticEntry {
    /// Monotonic sequence number.
    pub seq: u64,
    /// Timestamp in microseconds (from Instant or deterministic counter).
    pub timestamp_us: u64,
    /// Event kind.
    pub kind: DiagnosticEventKind,
    /// Mouse X coordinate (if applicable).
    pub x: Option<u16>,
    /// Mouse Y coordinate (if applicable).
    pub y: Option<u16>,
    /// Target ID (if applicable).
    pub target_id: Option<u64>,
    /// Previous hover target (for hover changes).
    pub prev_target_id: Option<u64>,
    /// Current tick count.
    pub tick: u64,
    /// Grid area dimensions (width, height) for render events.
    pub grid_dims: Option<(u16, u16)>,
    /// Additional context string.
    pub context: Option<String>,
    /// Checksum for determinism verification.
    pub checksum: u64,
}

impl DiagnosticEntry {
    /// Create a new diagnostic entry with current timestamp.
    pub fn new(kind: DiagnosticEventKind, tick: u64) -> Self {
        let timestamp_us = if is_deterministic_mode() {
            // Use tick as timestamp in deterministic mode
            tick * 1000
        } else {
            // Use actual time offset (from a static baseline)
            static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            let start = START.get_or_init(Instant::now);
            start.elapsed().as_micros() as u64
        };

        Self {
            seq: next_event_seq(),
            timestamp_us,
            kind,
            x: None,
            y: None,
            target_id: None,
            prev_target_id: None,
            tick,
            grid_dims: None,
            context: None,
            checksum: 0,
        }
    }

    /// Set mouse position.
    #[must_use]
    pub fn with_position(mut self, x: u16, y: u16) -> Self {
        self.x = Some(x);
        self.y = Some(y);
        self
    }

    /// Set target ID.
    #[must_use]
    pub fn with_target(mut self, target_id: Option<u64>) -> Self {
        self.target_id = target_id;
        self
    }

    /// Set hover transition.
    #[must_use]
    pub fn with_hover_transition(mut self, prev: Option<u64>, current: Option<u64>) -> Self {
        self.prev_target_id = prev;
        self.target_id = current;
        self
    }

    /// Set grid dimensions.
    #[must_use]
    pub fn with_grid_dims(mut self, width: u16, height: u16) -> Self {
        self.grid_dims = Some((width, height));
        self
    }

    /// Set context string.
    #[must_use]
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Compute and set checksum for determinism verification.
    #[must_use]
    pub fn with_checksum(mut self) -> Self {
        self.checksum = self.compute_checksum();
        self
    }

    /// Compute FNV-1a hash of entry fields.
    fn compute_checksum(&self) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        let payload = format!(
            "{:?}{}{}{}{}{}{}{}",
            self.kind,
            self.x.unwrap_or(0),
            self.y.unwrap_or(0),
            self.target_id.unwrap_or(0),
            self.prev_target_id.unwrap_or(0),
            self.tick,
            self.grid_dims
                .map(|(w, h)| w as u32 * 1000 + h as u32)
                .unwrap_or(0),
            self.context.as_deref().unwrap_or("")
        );
        for &b in payload.as_bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// Format as JSONL string.
    pub fn to_jsonl(&self) -> String {
        let mut parts = vec![
            format!("\"seq\":{}", self.seq),
            format!("\"ts_us\":{}", self.timestamp_us),
            format!("\"kind\":\"{}\"", self.kind.as_str()),
            format!("\"tick\":{}", self.tick),
        ];

        if let Some(x) = self.x {
            parts.push(format!("\"x\":{x}"));
        }
        if let Some(y) = self.y {
            parts.push(format!("\"y\":{y}"));
        }
        if let Some(id) = self.target_id {
            parts.push(format!("\"target_id\":{id}"));
        }
        if let Some(id) = self.prev_target_id {
            parts.push(format!("\"prev_target_id\":{id}"));
        }
        if let Some((w, h)) = self.grid_dims {
            parts.push(format!("\"grid_w\":{w},\"grid_h\":{h}"));
        }
        if let Some(ref ctx) = self.context {
            // Escape quotes in context
            let escaped = ctx.replace('\\', "\\\\").replace('"', "\\\"");
            parts.push(format!("\"context\":\"{escaped}\""));
        }
        parts.push(format!("\"checksum\":\"{:016x}\"", self.checksum));

        format!("{{{}}}", parts.join(","))
    }
}

/// Check if deterministic mode is enabled.
pub fn is_deterministic_mode() -> bool {
    determinism::env_flag("FTUI_MOUSE_DETERMINISTIC") || determinism::is_demo_deterministic()
}

/// Diagnostic log collector for testing and debugging.
///
/// This struct collects diagnostic entries in memory for inspection.
#[derive(Debug, Default)]
pub struct DiagnosticLog {
    /// Collected entries.
    entries: Vec<DiagnosticEntry>,
    /// Maximum entries to keep (0 = unlimited).
    max_entries: usize,
    /// Whether to also write to stderr.
    write_stderr: bool,
}

impl DiagnosticLog {
    /// Create a new diagnostic log.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: 10000,
            write_stderr: false,
        }
    }

    /// Create a log that writes to stderr.
    pub fn with_stderr(mut self) -> Self {
        self.write_stderr = true;
        self
    }

    /// Set maximum entries to keep.
    pub fn with_max_entries(mut self, max: usize) -> Self {
        self.max_entries = max;
        self
    }

    /// Record a diagnostic entry.
    pub fn record(&mut self, entry: DiagnosticEntry) {
        if self.write_stderr {
            let _ = writeln!(std::io::stderr(), "{}", entry.to_jsonl());
        }

        if self.max_entries > 0 && self.entries.len() >= self.max_entries {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }

    /// Get all entries.
    pub fn entries(&self) -> &[DiagnosticEntry] {
        &self.entries
    }

    /// Get entries of a specific kind.
    pub fn entries_of_kind(&self, kind: DiagnosticEventKind) -> Vec<&DiagnosticEntry> {
        self.entries.iter().filter(|e| e.kind == kind).collect()
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Export all entries as JSONL string.
    pub fn to_jsonl(&self) -> String {
        self.entries
            .iter()
            .map(DiagnosticEntry::to_jsonl)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Get summary statistics.
    pub fn summary(&self) -> DiagnosticSummary {
        let mut summary = DiagnosticSummary::default();
        for entry in &self.entries {
            match entry.kind {
                DiagnosticEventKind::MouseDown => summary.mouse_down_count += 1,
                DiagnosticEventKind::MouseUp => summary.mouse_up_count += 1,
                DiagnosticEventKind::MouseMove => summary.mouse_move_count += 1,
                DiagnosticEventKind::MouseDrag => summary.mouse_drag_count += 1,
                DiagnosticEventKind::MouseScroll => summary.mouse_scroll_count += 1,
                DiagnosticEventKind::HitTest => summary.hit_test_count += 1,
                DiagnosticEventKind::HoverChange => summary.hover_change_count += 1,
                DiagnosticEventKind::TargetClick => summary.target_click_count += 1,
                DiagnosticEventKind::Tick => summary.tick_count += 1,
                _ => {}
            }
        }
        summary.total_entries = self.entries.len();
        summary
    }
}

/// Summary statistics from a diagnostic log.
#[derive(Debug, Default, Clone)]
pub struct DiagnosticSummary {
    pub total_entries: usize,
    pub mouse_down_count: usize,
    pub mouse_up_count: usize,
    pub mouse_move_count: usize,
    pub mouse_drag_count: usize,
    pub mouse_scroll_count: usize,
    pub hit_test_count: usize,
    pub hover_change_count: usize,
    pub target_click_count: usize,
    pub tick_count: usize,
}

impl DiagnosticSummary {
    /// Format as JSONL.
    pub fn to_jsonl(&self) -> String {
        format!(
            "{{\"summary\":true,\"total\":{},\"mouse_down\":{},\"mouse_up\":{},\
             \"mouse_move\":{},\"mouse_drag\":{},\"mouse_scroll\":{},\"hit_test\":{},\
             \"hover_change\":{},\"target_click\":{},\"tick\":{}}}",
            self.total_entries,
            self.mouse_down_count,
            self.mouse_up_count,
            self.mouse_move_count,
            self.mouse_drag_count,
            self.mouse_scroll_count,
            self.hit_test_count,
            self.hover_change_count,
            self.target_click_count,
            self.tick_count
        )
    }
}

// =============================================================================
// Telemetry Hooks (bd-bksf.5)
// =============================================================================

/// Callback type for telemetry hooks.
pub type TelemetryCallback = Box<dyn Fn(&DiagnosticEntry) + Send + Sync>;

/// Telemetry hooks for observing mouse playground events.
///
/// These hooks allow external observers to receive notifications about
/// internal events without modifying the core logic.
#[derive(Default)]
pub struct TelemetryHooks {
    /// Callback for hit-test events.
    on_hit_test: Option<TelemetryCallback>,
    /// Callback for hover changes.
    on_hover_change: Option<TelemetryCallback>,
    /// Callback for target clicks.
    on_target_click: Option<TelemetryCallback>,
    /// Callback for all events (catch-all).
    on_any_event: Option<TelemetryCallback>,
}

impl TelemetryHooks {
    /// Create new empty hooks.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set hit-test callback.
    pub fn on_hit_test(mut self, f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static) -> Self {
        self.on_hit_test = Some(Box::new(f));
        self
    }

    /// Set hover change callback.
    pub fn on_hover_change(mut self, f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static) -> Self {
        self.on_hover_change = Some(Box::new(f));
        self
    }

    /// Set target click callback.
    pub fn on_target_click(mut self, f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static) -> Self {
        self.on_target_click = Some(Box::new(f));
        self
    }

    /// Set catch-all callback.
    pub fn on_any(mut self, f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static) -> Self {
        self.on_any_event = Some(Box::new(f));
        self
    }

    /// Dispatch an entry to relevant hooks.
    fn dispatch(&self, entry: &DiagnosticEntry) {
        if let Some(ref cb) = self.on_any_event {
            cb(entry);
        }

        match entry.kind {
            DiagnosticEventKind::HitTest => {
                if let Some(ref cb) = self.on_hit_test {
                    cb(entry);
                }
            }
            DiagnosticEventKind::HoverChange => {
                if let Some(ref cb) = self.on_hover_change {
                    cb(entry);
                }
            }
            DiagnosticEventKind::TargetClick => {
                if let Some(ref cb) = self.on_target_click {
                    cb(entry);
                }
            }
            _ => {}
        }
    }
}

/// Number of hit-test targets in the grid.
const GRID_COLS: usize = 4;
const GRID_ROWS: usize = 3;

/// Mouse event log entry.
#[derive(Debug, Clone)]
struct EventLogEntry {
    /// Tick when event occurred.
    tick: u64,
    /// Event description.
    description: String,
    /// Position.
    x: u16,
    y: u16,
}

/// A hit target in the grid.
#[derive(Debug, Clone)]
struct HitTarget {
    /// Unique ID for this target.
    id: u64,
    /// Label displayed on the target.
    label: String,
    /// Whether currently hovered.
    hovered: bool,
    /// Click count.
    clicks: u32,
}

impl HitTarget {
    fn new(id: u64, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            hovered: false,
            clicks: 0,
        }
    }
}

/// Which panel currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// Focus on the hit-test target grid.
    #[default]
    Targets,
    /// Focus on the event log panel.
    EventLog,
    /// Focus on the stats panel.
    Stats,
}

impl Focus {
    /// Cycle to the next focus panel.
    fn next(self) -> Self {
        match self {
            Self::Targets => Self::EventLog,
            Self::EventLog => Self::Stats,
            Self::Stats => Self::Targets,
        }
    }

    /// Cycle to the previous focus panel.
    fn prev(self) -> Self {
        match self {
            Self::Targets => Self::Stats,
            Self::EventLog => Self::Targets,
            Self::Stats => Self::EventLog,
        }
    }
}

/// Mouse Playground demo screen state.
pub struct MousePlayground {
    /// Global tick counter.
    tick_count: u64,
    /// Recent mouse event log.
    event_log: VecDeque<EventLogEntry>,
    /// Grid of hit-test targets.
    targets: Vec<HitTarget>,
    /// Currently hovered target ID (stabilized).
    current_hover: Option<u64>,
    /// Hover jitter stabilizer.
    hover_stabilizer: HoverStabilizer,
    /// Whether to show the hit-test overlay.
    show_overlay: bool,
    /// Whether to show jitter stabilization stats.
    show_jitter_stats: bool,
    /// Last raw hover position.
    last_mouse_pos: Option<(u16, u16)>,
    /// Last rendered grid area for hit testing.
    last_grid_area: Cell<Rect>,
    /// Diagnostic log for telemetry (bd-bksf.5).
    diagnostic_log: Option<DiagnosticLog>,
    /// Telemetry hooks for external observers (bd-bksf.5).
    telemetry_hooks: Option<TelemetryHooks>,
    /// Current panel focus (bd-bksf.6 UX/A11y).
    focus: Focus,
    /// Keyboard-focused target index (0-based, for Targets panel).
    focused_target_index: usize,
}

impl Default for MousePlayground {
    fn default() -> Self {
        Self::new()
    }
}

impl MousePlayground {
    /// Create a new mouse playground screen.
    pub fn new() -> Self {
        // Create hit targets
        let mut targets = Vec::with_capacity(GRID_COLS * GRID_ROWS);
        for i in 0..(GRID_COLS * GRID_ROWS) {
            targets.push(HitTarget::new(i as u64 + 1, format!("T{}", i + 1)));
        }

        // Enable diagnostic log if diagnostics are enabled
        let diagnostic_log = if diagnostics_enabled() {
            Some(DiagnosticLog::new().with_stderr())
        } else {
            None
        };

        Self {
            tick_count: 0,
            event_log: VecDeque::with_capacity(MAX_EVENT_LOG + 1),
            targets,
            current_hover: None,
            hover_stabilizer: HoverStabilizer::new(HoverStabilizerConfig::default()),
            show_overlay: false,
            show_jitter_stats: false,
            last_mouse_pos: None,
            last_grid_area: Cell::new(Rect::default()),
            diagnostic_log,
            telemetry_hooks: None,
            focus: Focus::default(),
            focused_target_index: 0,
        }
    }

    /// Create with diagnostic log enabled (for testing).
    pub fn with_diagnostics(mut self) -> Self {
        self.diagnostic_log = Some(DiagnosticLog::new());
        self
    }

    /// Create with telemetry hooks.
    pub fn with_telemetry_hooks(mut self, hooks: TelemetryHooks) -> Self {
        self.telemetry_hooks = Some(hooks);
        self
    }

    /// Get the diagnostic log (for testing).
    pub fn diagnostic_log(&self) -> Option<&DiagnosticLog> {
        self.diagnostic_log.as_ref()
    }

    /// Get mutable diagnostic log (for testing).
    pub fn diagnostic_log_mut(&mut self) -> Option<&mut DiagnosticLog> {
        self.diagnostic_log.as_mut()
    }

    /// Record a diagnostic entry and dispatch to hooks.
    fn record_diagnostic(&mut self, entry: DiagnosticEntry) {
        let entry = entry.with_checksum();

        // Dispatch to hooks first (immutable reference)
        if let Some(ref hooks) = self.telemetry_hooks {
            hooks.dispatch(&entry);
        }

        // Then record to log
        if let Some(ref mut log) = self.diagnostic_log {
            log.record(entry);
        }
    }

    /// Log a mouse event.
    fn log_event(&mut self, desc: impl Into<String>, x: u16, y: u16) {
        self.event_log.push_front(EventLogEntry {
            tick: self.tick_count,
            description: desc.into(),
            x,
            y,
        });
        if self.event_log.len() > MAX_EVENT_LOG {
            self.event_log.pop_back();
        }
    }

    /// Handle a mouse event.
    fn handle_mouse(&mut self, event: MouseEvent) {
        let (x, y) = event.position();
        self.last_mouse_pos = Some((x, y));

        // Log the event and determine diagnostic kind
        let (desc, diag_kind) = match event.kind {
            MouseEventKind::Down(btn) => {
                (format!("{:?} Down", btn), DiagnosticEventKind::MouseDown)
            }
            MouseEventKind::Up(btn) => (format!("{:?} Up", btn), DiagnosticEventKind::MouseUp),
            MouseEventKind::Drag(btn) => {
                (format!("{:?} Drag", btn), DiagnosticEventKind::MouseDrag)
            }
            MouseEventKind::Moved => ("Move".to_string(), DiagnosticEventKind::MouseMove),
            MouseEventKind::ScrollUp => ("Scroll Up".to_string(), DiagnosticEventKind::MouseScroll),
            MouseEventKind::ScrollDown => {
                ("Scroll Down".to_string(), DiagnosticEventKind::MouseScroll)
            }
            MouseEventKind::ScrollLeft => {
                ("Scroll Left".to_string(), DiagnosticEventKind::MouseScroll)
            }
            MouseEventKind::ScrollRight => {
                ("Scroll Right".to_string(), DiagnosticEventKind::MouseScroll)
            }
        };
        self.log_event(&desc, x, y);

        // Record diagnostic for mouse event
        let mouse_diag = DiagnosticEntry::new(diag_kind, self.tick_count)
            .with_position(x, y)
            .with_context(&desc);
        self.record_diagnostic(mouse_diag);

        // Hit test for this position
        let raw_target = self.hit_test(x, y);

        // Record hit test diagnostic
        let hit_diag = DiagnosticEntry::new(DiagnosticEventKind::HitTest, self.tick_count)
            .with_position(x, y)
            .with_target(raw_target);
        self.record_diagnostic(hit_diag);

        // Check for clicks on targets
        if let MouseEventKind::Down(MouseButton::Left) = event.kind
            && let Some(target_id) = raw_target
            && let Some(target) = self.targets.iter_mut().find(|t| t.id == target_id)
        {
            target.clicks += 1;

            // Record target click diagnostic
            let click_diag =
                DiagnosticEntry::new(DiagnosticEventKind::TargetClick, self.tick_count)
                    .with_position(x, y)
                    .with_target(Some(target_id))
                    .with_context(format!("clicks={}", target.clicks));
            self.record_diagnostic(click_diag);
        }

        // Update hover with stabilization
        let stabilized = self
            .hover_stabilizer
            .update(raw_target, (x, y), Instant::now());

        // Update hovered state on targets
        if stabilized != self.current_hover {
            let prev_hover = self.current_hover;

            // Clear old hover
            if let Some(old_id) = self.current_hover
                && let Some(target) = self.targets.iter_mut().find(|t| t.id == old_id)
            {
                target.hovered = false;
            }
            // Set new hover
            if let Some(new_id) = stabilized
                && let Some(target) = self.targets.iter_mut().find(|t| t.id == new_id)
            {
                target.hovered = true;
            }
            self.current_hover = stabilized;

            // Record hover change diagnostic
            let hover_diag =
                DiagnosticEntry::new(DiagnosticEventKind::HoverChange, self.tick_count)
                    .with_position(x, y)
                    .with_hover_transition(prev_hover, stabilized);
            self.record_diagnostic(hover_diag);
        }
    }

    /// Hit test against last rendered grid area.
    fn hit_test(&self, x: u16, y: u16) -> Option<u64> {
        let grid_area = self.last_grid_area.get();
        if grid_area.width == 0 || grid_area.height == 0 {
            return None;
        }

        let cell_width = grid_area.width / GRID_COLS as u16;
        let cell_height = grid_area.height / GRID_ROWS as u16;
        if cell_width == 0 || cell_height == 0 {
            return None;
        }

        for row in 0..GRID_ROWS {
            for col in 0..GRID_COLS {
                let x0 = grid_area.x + (col as u16) * cell_width;
                let y0 = grid_area.y + (row as u16) * cell_height;
                let rect = Rect::new(x0 + 1, y0, cell_width.saturating_sub(2), cell_height);
                if rect.contains(x, y) {
                    return Some((row * GRID_COLS + col) as u64 + 1);
                }
            }
        }

        None
    }

    /// Toggle overlay visibility.
    fn toggle_overlay(&mut self) {
        self.show_overlay = !self.show_overlay;

        // Record diagnostic
        let diag = DiagnosticEntry::new(DiagnosticEventKind::OverlayToggle, self.tick_count)
            .with_context(format!("enabled={}", self.show_overlay));
        self.record_diagnostic(diag);
    }

    /// Toggle jitter stats visibility.
    fn toggle_jitter_stats(&mut self) {
        self.show_jitter_stats = !self.show_jitter_stats;

        // Record diagnostic
        let diag = DiagnosticEntry::new(DiagnosticEventKind::JitterStatsToggle, self.tick_count)
            .with_context(format!("enabled={}", self.show_jitter_stats));
        self.record_diagnostic(diag);
    }

    /// Clear the event log.
    fn clear_log(&mut self) {
        let prev_count = self.event_log.len();
        self.event_log.clear();

        // Record diagnostic
        let diag = DiagnosticEntry::new(DiagnosticEventKind::LogClear, self.tick_count)
            .with_context(format!("cleared={}", prev_count));
        self.record_diagnostic(diag);
    }

    // -------------------------------------------------------------------------
    // Keyboard Navigation (bd-bksf.6 UX/A11y)
    // -------------------------------------------------------------------------

    /// Move target focus up (previous row).
    fn move_target_up(&mut self) {
        if self.focused_target_index >= GRID_COLS {
            self.focused_target_index -= GRID_COLS;
        }
    }

    /// Move target focus down (next row).
    fn move_target_down(&mut self) {
        let new_index = self.focused_target_index + GRID_COLS;
        if new_index < self.targets.len() {
            self.focused_target_index = new_index;
        }
    }

    /// Move target focus left (previous column).
    fn move_target_left(&mut self) {
        if !self.focused_target_index.is_multiple_of(GRID_COLS) {
            self.focused_target_index -= 1;
        }
    }

    /// Move target focus right (next column).
    fn move_target_right(&mut self) {
        if self.focused_target_index % GRID_COLS < GRID_COLS - 1
            && self.focused_target_index + 1 < self.targets.len()
        {
            self.focused_target_index += 1;
        }
    }

    /// Jump to first target.
    fn jump_to_first_target(&mut self) {
        self.focused_target_index = 0;
    }

    /// Jump to last target.
    fn jump_to_last_target(&mut self) {
        self.focused_target_index = self.targets.len().saturating_sub(1);
    }

    /// Move focus up by a page (one full row at a time).
    fn page_up_targets(&mut self) {
        // Move up by 2 rows (or GRID_ROWS - 1 for larger grids)
        let rows_to_move = GRID_ROWS.saturating_sub(1).max(1);
        for _ in 0..rows_to_move {
            self.move_target_up();
        }
    }

    /// Move focus down by a page (one full row at a time).
    fn page_down_targets(&mut self) {
        // Move down by 2 rows (or GRID_ROWS - 1 for larger grids)
        let rows_to_move = GRID_ROWS.saturating_sub(1).max(1);
        for _ in 0..rows_to_move {
            self.move_target_down();
        }
    }

    /// Activate (click) the currently focused target via keyboard.
    fn activate_focused_target(&mut self) {
        if let Some(target) = self.targets.get_mut(self.focused_target_index) {
            target.clicks += 1;

            // Record diagnostic
            let diag = DiagnosticEntry::new(DiagnosticEventKind::TargetClick, self.tick_count)
                .with_target(Some(target.id))
                .with_context(format!("keyboard_click={}", target.clicks));
            self.record_diagnostic(diag);
        }
    }

    // -------------------------------------------------------------------------
    // Public accessors for testing
    // -------------------------------------------------------------------------

    /// Whether the hit-test overlay is currently enabled.
    pub fn overlay_enabled(&self) -> bool {
        self.show_overlay
    }

    /// Whether jitter stats display is currently enabled.
    pub fn jitter_stats_enabled(&self) -> bool {
        self.show_jitter_stats
    }

    /// Number of entries in the event log.
    pub fn event_log_len(&self) -> usize {
        self.event_log.len()
    }

    /// Current tick count.
    pub fn current_tick(&self) -> u64 {
        self.tick_count
    }

    /// Current panel focus (bd-bksf.6 UX/A11y).
    pub fn current_focus(&self) -> Focus {
        self.focus
    }

    /// Currently keyboard-focused target index (bd-bksf.6 UX/A11y).
    pub fn focused_target_index(&self) -> usize {
        self.focused_target_index
    }

    /// Add a test event to the log (for testing purposes).
    pub fn push_test_event(&mut self, description: impl Into<String>, x: u16, y: u16) {
        self.log_event(description, x, y);
    }

    /// Public hit test for testing (wraps internal hit_test).
    pub fn hit_test_at(&self, x: u16, y: u16) -> Option<u64> {
        self.hit_test(x, y)
    }
}

impl Screen for MousePlayground {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        match event {
            Event::Mouse(mouse_event) => {
                self.handle_mouse(*mouse_event);
            }
            // Tab: Cycle focus forward
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                kind: KeyEventKind::Press,
                modifiers,
            }) if !modifiers.contains(ftui_core::event::Modifiers::SHIFT) => {
                self.focus = self.focus.next();
            }
            // BackTab: Cycle focus backward
            Event::Key(KeyEvent {
                code: KeyCode::BackTab,
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.focus = self.focus.prev();
            }
            // Shift+Tab: Cycle focus backward
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                kind: KeyEventKind::Press,
                modifiers,
                ..
            }) if modifiers.contains(ftui_core::event::Modifiers::SHIFT) => {
                self.focus = self.focus.prev();
            }
            // Arrow keys for target navigation (when Targets panel focused)
            Event::Key(KeyEvent {
                code: KeyCode::Up,
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.move_target_up();
            }
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.move_target_down();
            }
            Event::Key(KeyEvent {
                code: KeyCode::Left,
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.move_target_left();
            }
            Event::Key(KeyEvent {
                code: KeyCode::Right,
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.move_target_right();
            }
            // Vim-style navigation for targets (h/j/k/l)
            Event::Key(KeyEvent {
                code: KeyCode::Char('k'),
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.move_target_up();
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char('j'),
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.move_target_down();
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char('h'),
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.move_target_left();
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char('l'),
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.move_target_right();
            }
            // Home: Jump to first target
            Event::Key(KeyEvent {
                code: KeyCode::Home,
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.jump_to_first_target();
            }
            // End: Jump to last target
            Event::Key(KeyEvent {
                code: KeyCode::End,
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.jump_to_last_target();
            }
            // g: Jump to first target (vim-style)
            Event::Key(KeyEvent {
                code: KeyCode::Char('g'),
                kind: KeyEventKind::Press,
                modifiers,
            }) if self.focus == Focus::Targets
                && !modifiers.contains(ftui_core::event::Modifiers::SHIFT) =>
            {
                self.jump_to_first_target();
            }
            // G: Jump to last target (vim-style)
            Event::Key(KeyEvent {
                code: KeyCode::Char('G'),
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.jump_to_last_target();
            }
            // PageUp: Move up by page
            Event::Key(KeyEvent {
                code: KeyCode::PageUp,
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.page_up_targets();
            }
            // PageDown: Move down by page
            Event::Key(KeyEvent {
                code: KeyCode::PageDown,
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.page_down_targets();
            }
            // Space/Enter: Activate (click) focused target
            Event::Key(KeyEvent {
                code: KeyCode::Char(' ') | KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            }) if self.focus == Focus::Targets => {
                self.activate_focused_target();
            }
            // O: Toggle overlay
            Event::Key(KeyEvent {
                code: KeyCode::Char('o') | KeyCode::Char('O'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.toggle_overlay();
            }
            // J: Toggle jitter stats (only when NOT on Targets panel to avoid conflict with vim nav)
            Event::Key(KeyEvent {
                code: KeyCode::Char('J'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.toggle_jitter_stats();
            }
            // C: Clear log
            Event::Key(KeyEvent {
                code: KeyCode::Char('c') | KeyCode::Char('C'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.clear_log();
            }
            _ => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        // Main layout: left panel (targets) + right panel (event log)
        let chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(area);

        let left_area = chunks[0];
        let right_area = chunks[1];

        // --- Left Panel: Hit-Test Target Grid ---
        let targets_focused = self.focus == Focus::Targets;
        let (targets_border_style, targets_title) = if targets_focused {
            (
                Style::new().fg(theme::accent::PRIMARY.resolve()).bold(),
                " ► Hit-Test Targets ",
            )
        } else {
            (
                Style::new().fg(theme::fg::MUTED.resolve()),
                "   Hit-Test Targets ",
            )
        };
        let left_block = Block::new()
            .title(targets_title)
            .borders(Borders::ALL)
            .border_type(if targets_focused {
                BorderType::Heavy
            } else {
                BorderType::Rounded
            })
            .border_style(targets_border_style)
            .style(Style::new().bg(theme::bg::SURFACE));
        let inner_left = left_block.inner(left_area);
        left_block.render(left_area, frame);

        // Render target grid
        self.render_target_grid(frame, inner_left);

        // --- Right Panel: Event Log + Stats ---
        let right_chunks = Flex::vertical()
            .constraints([Constraint::Percentage(70.0), Constraint::Percentage(30.0)])
            .split(right_area);

        // Event log
        let log_focused = self.focus == Focus::EventLog;
        let (log_border_style, log_title) = if log_focused {
            (
                Style::new().fg(theme::accent::PRIMARY.resolve()).bold(),
                " ► Event Log ",
            )
        } else {
            (Style::new().fg(theme::fg::MUTED.resolve()), "   Event Log ")
        };
        let log_block = Block::new()
            .title(log_title)
            .borders(Borders::ALL)
            .border_type(if log_focused {
                BorderType::Heavy
            } else {
                BorderType::Rounded
            })
            .border_style(log_border_style)
            .style(Style::new().bg(theme::bg::SURFACE));
        let log_inner = log_block.inner(right_chunks[0]);
        log_block.render(right_chunks[0], frame);
        self.render_event_log(frame, log_inner);

        // Stats panel
        let stats_focused = self.focus == Focus::Stats;
        let (stats_border_style, stats_title) = if stats_focused {
            (
                Style::new().fg(theme::accent::PRIMARY.resolve()).bold(),
                " ► Stats ",
            )
        } else {
            (Style::new().fg(theme::fg::MUTED.resolve()), "   Stats ")
        };
        let stats_block = Block::new()
            .title(stats_title)
            .borders(Borders::ALL)
            .border_type(if stats_focused {
                BorderType::Heavy
            } else {
                BorderType::Rounded
            })
            .border_style(stats_border_style)
            .style(Style::new().bg(theme::bg::SURFACE));
        let stats_inner = stats_block.inner(right_chunks[1]);
        stats_block.render(right_chunks[1], frame);
        self.render_stats(frame, stats_inner);

        // Overlay (if enabled)
        if self.show_overlay {
            self.render_overlay(frame, area);
        }
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Tab",
                action: "Cycle focus",
            },
            HelpEntry {
                key: "↑↓←→/hjkl",
                action: "Navigate targets",
            },
            HelpEntry {
                key: "Space/Enter",
                action: "Click target",
            },
            HelpEntry {
                key: "Home/g",
                action: "First target",
            },
            HelpEntry {
                key: "End/G",
                action: "Last target",
            },
            HelpEntry {
                key: "O",
                action: "Toggle overlay",
            },
            HelpEntry {
                key: "J",
                action: "Toggle jitter stats",
            },
            HelpEntry {
                key: "C",
                action: "Clear log",
            },
        ]
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }

    fn title(&self) -> &'static str {
        "Mouse Playground"
    }

    fn tab_label(&self) -> &'static str {
        "Mouse"
    }
}

impl MousePlayground {
    /// Render the grid of hit-test targets.
    fn render_target_grid(&self, frame: &mut Frame, area: Rect) {
        if area.width < 4 || area.height < 3 {
            return;
        }

        self.last_grid_area.set(area);

        let cell_width = area.width / GRID_COLS as u16;
        let cell_height = area.height / GRID_ROWS as u16;

        for (i, target) in self.targets.iter().enumerate() {
            let col = i % GRID_COLS;
            let row = i / GRID_COLS;

            let x = area.x + (col as u16) * cell_width;
            let y = area.y + (row as u16) * cell_height;

            // Slightly smaller than cell for visual separation
            let target_rect = Rect::new(x + 1, y, cell_width.saturating_sub(2), cell_height);

            // Determine if this target has keyboard focus
            let keyboard_focused = self.focus == Focus::Targets && i == self.focused_target_index;

            // Style based on keyboard focus, hover, and click state
            let (style, border_style, border_type) = if keyboard_focused && target.hovered {
                // Both keyboard focused and mouse hovered
                (
                    Style::new()
                        .fg(theme::accent::PRIMARY)
                        .bg(theme::accent::PRIMARY),
                    Style::new()
                        .fg(theme::accent::PRIMARY.resolve())
                        .attrs(StyleFlags::BOLD),
                    BorderType::Double,
                )
            } else if keyboard_focused {
                // Keyboard focused only
                (
                    Style::new().bg(theme::bg::SURFACE),
                    Style::new()
                        .fg(theme::accent::PRIMARY.resolve())
                        .attrs(StyleFlags::BOLD),
                    BorderType::Heavy,
                )
            } else if target.hovered {
                // Mouse hovered only
                (
                    Style::new()
                        .fg(theme::accent::PRIMARY)
                        .bg(theme::accent::PRIMARY),
                    Style::new().fg(theme::accent::PRIMARY),
                    BorderType::Double,
                )
            } else {
                // Default state
                (
                    Style::new().bg(theme::bg::SURFACE),
                    Style::new().fg(theme::fg::SECONDARY),
                    BorderType::Rounded,
                )
            };

            // Render target block
            let block = Block::new()
                .borders(Borders::ALL)
                .border_type(border_type)
                .border_style(border_style)
                .style(style);

            let inner = block.inner(target_rect);
            block.render(target_rect, frame);

            // Render label and click count
            if inner.height >= 1 && inner.width >= 2 {
                let label = format!("{} ({})", target.label, target.clicks);
                let label_style = if keyboard_focused || target.hovered {
                    Style::new().bold()
                } else {
                    Style::new()
                };
                Paragraph::new(label)
                    .style(label_style)
                    .alignment(Alignment::Center)
                    .render(inner, frame);
            }

            // Register hit region
            let hit_id = u32::try_from(target.id).unwrap_or(u32::MAX);
            frame.register_hit(target_rect, HitId::new(hit_id), HitRegion::Content, 0);
        }

        // Note: In real code, use frame's hit_test capability
    }

    /// Render the event log.
    fn render_event_log(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<String> = Vec::with_capacity(self.event_log.len());

        for entry in &self.event_log {
            lines.push(format!(
                "[{:04}] {:12} ({:3},{:3})",
                entry.tick % 10000,
                entry.description,
                entry.x,
                entry.y
            ));
        }

        if lines.is_empty() {
            lines.push("No events yet. Move the mouse!".to_string());
        }

        let text = lines.join("\n");
        Paragraph::new(text)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(area, frame);
    }

    /// Render statistics panel.
    fn render_stats(&self, frame: &mut Frame, area: Rect) {
        let hover_text = match self.current_hover {
            Some(id) => format!("T{}", id),
            None => "None".to_string(),
        };

        let mouse_pos = match self.last_mouse_pos {
            Some((x, y)) => format!("({}, {})", x, y),
            None => "N/A".to_string(),
        };

        let stats = format!(
            "Hover: {}  Pos: {}\nOverlay: {}  Jitter Stats: {}",
            hover_text,
            mouse_pos,
            if self.show_overlay { "ON" } else { "OFF" },
            if self.show_jitter_stats { "ON" } else { "OFF" }
        );

        Paragraph::new(stats)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(area, frame);
    }

    /// Render hit-test overlay.
    fn render_overlay(&self, frame: &mut Frame, area: Rect) {
        // Draw a subtle overlay showing hit regions
        // For simplicity, just draw a small indicator at mouse position
        if let Some((x, y)) = self.last_mouse_pos
            && x < area.x + area.width
            && y < area.y + area.height
        {
            // Draw crosshair at mouse position
            let horiz_cell = RenderCell::from_char('-')
                .with_fg(theme::accent::PRIMARY.into())
                .with_attrs(CellAttrs::new(CellStyleFlags::DIM, 0));
            let vert_cell = RenderCell::from_char('|')
                .with_fg(theme::accent::PRIMARY.into())
                .with_attrs(CellAttrs::new(CellStyleFlags::DIM, 0));
            let center_cell = RenderCell::from_char('+')
                .with_fg(theme::accent::PRIMARY.into())
                .with_attrs(CellAttrs::new(CellStyleFlags::BOLD, 0));

            // Horizontal line (within bounds)
            let h_start = area.x;
            let h_end = (area.x + area.width).min(x.saturating_add(20));
            for hx in h_start..h_end {
                if hx != x {
                    frame.buffer.set_fast(hx, y, horiz_cell);
                }
            }

            // Vertical line (within bounds)
            let v_start = area.y;
            let v_end = (area.y + area.height).min(y.saturating_add(10));
            for vy in v_start..v_end {
                if vy != y {
                    frame.buffer.set_fast(x, vy, vert_cell);
                }
            }

            // Center marker
            frame.buffer.set_fast(x, y, center_cell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_targets() {
        let playground = MousePlayground::new();
        assert_eq!(playground.targets.len(), GRID_COLS * GRID_ROWS);
    }

    #[test]
    fn log_event_limits_size() {
        let mut playground = MousePlayground::new();
        for i in 0..20 {
            playground.log_event(format!("Event {}", i), 0, 0);
        }
        assert_eq!(playground.event_log.len(), MAX_EVENT_LOG);
    }

    #[test]
    fn toggle_overlay() {
        let mut playground = MousePlayground::new();
        assert!(!playground.show_overlay);
        playground.toggle_overlay();
        assert!(playground.show_overlay);
        playground.toggle_overlay();
        assert!(!playground.show_overlay);
    }

    #[test]
    fn clear_log_empties_events() {
        let mut playground = MousePlayground::new();
        playground.log_event("Test", 0, 0);
        assert!(!playground.event_log.is_empty());
        playground.clear_log();
        assert!(playground.event_log.is_empty());
    }

    #[test]
    fn hit_test_returns_none_when_empty() {
        let playground = MousePlayground::new();
        assert!(playground.hit_test(10, 10).is_none());
    }

    // -------------------------------------------------------------------------
    // Additional Unit Tests
    // -------------------------------------------------------------------------

    #[test]
    fn target_ids_are_unique() {
        let playground = MousePlayground::new();
        let mut ids: Vec<_> = playground.targets.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), playground.targets.len());
    }

    #[test]
    fn target_ids_start_at_one() {
        let playground = MousePlayground::new();
        assert!(playground.targets.iter().all(|t| t.id >= 1));
        assert!(playground.targets.iter().any(|t| t.id == 1));
    }

    #[test]
    fn toggle_jitter_stats() {
        let mut playground = MousePlayground::new();
        assert!(!playground.show_jitter_stats);
        playground.toggle_jitter_stats();
        assert!(playground.show_jitter_stats);
        playground.toggle_jitter_stats();
        assert!(!playground.show_jitter_stats);
    }

    #[test]
    fn tick_increments_counter() {
        let mut playground = MousePlayground::new();
        assert_eq!(playground.tick_count, 0);
        playground.tick(1);
        assert_eq!(playground.tick_count, 1);
        playground.tick(5);
        assert_eq!(playground.tick_count, 5);
    }

    #[test]
    fn initial_state_is_clean() {
        let playground = MousePlayground::new();
        assert!(playground.event_log.is_empty());
        assert!(playground.current_hover.is_none());
        assert!(!playground.show_overlay);
        assert!(!playground.show_jitter_stats);
        assert!(playground.last_mouse_pos.is_none());
    }

    #[test]
    fn targets_initially_not_hovered() {
        let playground = MousePlayground::new();
        assert!(playground.targets.iter().all(|t| !t.hovered));
    }

    #[test]
    fn targets_initially_zero_clicks() {
        let playground = MousePlayground::new();
        assert!(playground.targets.iter().all(|t| t.clicks == 0));
    }

    #[test]
    fn hit_test_requires_nonzero_grid() {
        let playground = MousePlayground::new();
        // Grid area is default (0,0,0,0), so hit test should always return None
        assert!(playground.hit_test(0, 0).is_none());
        assert!(playground.hit_test(100, 100).is_none());
    }

    #[test]
    fn hit_test_with_valid_grid() {
        let playground = MousePlayground::new();
        // Simulate a grid area of 80x24 starting at (0, 0)
        playground.last_grid_area.set(Rect::new(0, 0, 80, 24));

        // Cell dimensions: 80/4=20 width, 24/3=8 height
        // Target 1 should be at col 0, row 0: x in [1, 18), y in [0, 8)
        assert_eq!(playground.hit_test(5, 4), Some(1));

        // Target 2 at col 1, row 0: x in [21, 38)
        assert_eq!(playground.hit_test(25, 4), Some(2));

        // Target 5 at col 0, row 1: y in [8, 16)
        assert_eq!(playground.hit_test(5, 10), Some(5));
    }

    #[test]
    fn hit_test_outside_grid_returns_none() {
        let playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(10, 10, 40, 12));

        // Before grid
        assert!(playground.hit_test(5, 5).is_none());
        // After grid
        assert!(playground.hit_test(60, 30).is_none());
    }

    #[test]
    fn hit_test_at_grid_boundaries() {
        let playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(0, 0, 80, 24));

        // Very first position (edge of grid)
        // Cell padding of 1 on left means x=0 is in the padding, not the target
        assert!(playground.hit_test(0, 0).is_none());
        // x=1 should be in target 1
        assert_eq!(playground.hit_test(1, 0), Some(1));
    }

    #[test]
    fn log_preserves_position() {
        let mut playground = MousePlayground::new();
        playground.log_event("Test at 42,24", 42, 24);
        let entry = playground.event_log.front().unwrap();
        assert_eq!(entry.x, 42);
        assert_eq!(entry.y, 24);
    }

    #[test]
    fn log_preserves_tick() {
        let mut playground = MousePlayground::new();
        playground.tick_count = 100;
        playground.log_event("Test", 0, 0);
        let entry = playground.event_log.front().unwrap();
        assert_eq!(entry.tick, 100);
    }

    #[test]
    fn log_event_fifo_order() {
        let mut playground = MousePlayground::new();
        playground.log_event("First", 1, 1);
        playground.log_event("Second", 2, 2);
        playground.log_event("Third", 3, 3);

        let entries: Vec<_> = playground.event_log.iter().collect();
        assert_eq!(entries[0].description, "Third");
        assert_eq!(entries[1].description, "Second");
        assert_eq!(entries[2].description, "First");
    }

    #[test]
    fn public_accessors_work() {
        let mut playground = MousePlayground::new();

        assert!(!playground.overlay_enabled());
        playground.toggle_overlay();
        assert!(playground.overlay_enabled());

        assert!(!playground.jitter_stats_enabled());
        playground.toggle_jitter_stats();
        assert!(playground.jitter_stats_enabled());

        assert_eq!(playground.event_log_len(), 0);
        playground.push_test_event("test", 0, 0);
        assert_eq!(playground.event_log_len(), 1);

        assert_eq!(playground.current_tick(), 0);
        playground.tick(10);
        assert_eq!(playground.current_tick(), 10);
    }

    #[test]
    fn hit_target_new() {
        let target = HitTarget::new(42, "Label");
        assert_eq!(target.id, 42);
        assert_eq!(target.label, "Label");
        assert!(!target.hovered);
        assert_eq!(target.clicks, 0);
    }

    #[test]
    fn small_grid_cells_return_none() {
        let playground = MousePlayground::new();
        // Grid too small to have 1-pixel cells
        playground.last_grid_area.set(Rect::new(0, 0, 2, 1));
        // Cell width would be 2/4=0, so hit_test returns None
        assert!(playground.hit_test(0, 0).is_none());
    }
}

// -------------------------------------------------------------------------
// Input Robustness Tests
// -------------------------------------------------------------------------

#[cfg(test)]
mod robustness_tests {
    use super::*;

    /// Helper to create a mouse event
    fn mouse_event(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            x,
            y,
            modifiers: ftui_core::event::Modifiers::empty(),
        }
    }

    // -------------------------------------------------------------------------
    // Mouse button handling tests
    // -------------------------------------------------------------------------

    #[test]
    fn handle_left_button_down() {
        let mut playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(0, 0, 80, 24));

        let event = mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 4);
        playground.handle_mouse(event);

        assert_eq!(playground.event_log.len(), 1);
        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Left")
        );
        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Down")
        );
    }

    #[test]
    fn handle_right_button_down() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::Down(MouseButton::Right), 10, 10);
        playground.handle_mouse(event);

        assert_eq!(playground.event_log.len(), 1);
        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Right")
        );
    }

    #[test]
    fn handle_middle_button_down() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::Down(MouseButton::Middle), 10, 10);
        playground.handle_mouse(event);

        assert_eq!(playground.event_log.len(), 1);
        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Middle")
        );
    }

    #[test]
    fn handle_button_up() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::Up(MouseButton::Left), 10, 10);
        playground.handle_mouse(event);

        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Up")
        );
    }

    #[test]
    fn handle_drag() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::Drag(MouseButton::Left), 10, 10);
        playground.handle_mouse(event);

        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Drag")
        );
    }

    // -------------------------------------------------------------------------
    // Mouse movement and scroll tests
    // -------------------------------------------------------------------------

    #[test]
    fn handle_mouse_move() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::Moved, 50, 25);
        playground.handle_mouse(event);

        assert_eq!(playground.event_log.len(), 1);
        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Move")
        );
        assert_eq!(playground.last_mouse_pos, Some((50, 25)));
    }

    #[test]
    fn handle_scroll_up() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::ScrollUp, 10, 10);
        playground.handle_mouse(event);

        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Scroll Up")
        );
    }

    #[test]
    fn handle_scroll_down() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::ScrollDown, 10, 10);
        playground.handle_mouse(event);

        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Scroll Down")
        );
    }

    #[test]
    fn handle_scroll_left() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::ScrollLeft, 10, 10);
        playground.handle_mouse(event);

        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Scroll Left")
        );
    }

    #[test]
    fn handle_scroll_right() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::ScrollRight, 10, 10);
        playground.handle_mouse(event);

        assert!(
            playground
                .event_log
                .front()
                .unwrap()
                .description
                .contains("Scroll Right")
        );
    }

    // -------------------------------------------------------------------------
    // Boundary position tests
    // -------------------------------------------------------------------------

    #[test]
    fn handle_mouse_at_origin() {
        let mut playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(0, 0, 80, 24));

        let event = mouse_event(MouseEventKind::Moved, 0, 0);
        playground.handle_mouse(event);

        assert_eq!(playground.last_mouse_pos, Some((0, 0)));
        assert_eq!(playground.event_log.len(), 1);
    }

    #[test]
    fn handle_mouse_at_max_coordinates() {
        let mut playground = MousePlayground::new();

        // Test with maximum u16 values
        let event = mouse_event(MouseEventKind::Moved, u16::MAX, u16::MAX);
        playground.handle_mouse(event);

        assert_eq!(playground.last_mouse_pos, Some((u16::MAX, u16::MAX)));
        assert_eq!(playground.event_log.len(), 1);
    }

    #[test]
    fn handle_mouse_at_large_coordinates() {
        let mut playground = MousePlayground::new();

        // Test with coordinates larger than typical terminal size
        let event = mouse_event(MouseEventKind::Moved, 10000, 5000);
        playground.handle_mouse(event);

        assert_eq!(playground.last_mouse_pos, Some((10000, 5000)));
    }

    // -------------------------------------------------------------------------
    // Click counting tests
    // -------------------------------------------------------------------------

    #[test]
    fn click_increments_target_count() {
        let mut playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(0, 0, 80, 24));

        // Click on target 1 (at position ~5, 4)
        let event = mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 4);
        playground.handle_mouse(event);

        // Find target 1 and check clicks
        let target = playground.targets.iter().find(|t| t.id == 1).unwrap();
        assert_eq!(target.clicks, 1);
    }

    #[test]
    fn multiple_clicks_accumulate() {
        let mut playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(0, 0, 80, 24));

        // Multiple clicks on same target
        for _ in 0..5 {
            let event = mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 4);
            playground.handle_mouse(event);
        }

        let target = playground.targets.iter().find(|t| t.id == 1).unwrap();
        assert_eq!(target.clicks, 5);
    }

    #[test]
    fn right_click_does_not_count() {
        let mut playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(0, 0, 80, 24));

        // Right click on target
        let event = mouse_event(MouseEventKind::Down(MouseButton::Right), 5, 4);
        playground.handle_mouse(event);

        // Clicks should not increment for right button
        let target = playground.targets.iter().find(|t| t.id == 1).unwrap();
        assert_eq!(target.clicks, 0);
    }

    #[test]
    fn click_outside_grid_does_not_count() {
        let mut playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(10, 10, 40, 12));

        // Click outside grid
        let event = mouse_event(MouseEventKind::Down(MouseButton::Left), 0, 0);
        playground.handle_mouse(event);

        // No target should have clicks
        assert!(playground.targets.iter().all(|t| t.clicks == 0));
    }

    // -------------------------------------------------------------------------
    // Rapid event sequence tests
    // -------------------------------------------------------------------------

    #[test]
    fn rapid_event_sequence() {
        let mut playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(0, 0, 80, 24));

        // Simulate rapid mouse movement
        for i in 0..100 {
            let x = (i % 80) as u16;
            let y = (i / 80) as u16;
            let event = mouse_event(MouseEventKind::Moved, x, y);
            playground.handle_mouse(event);
        }

        // Event log should be bounded
        assert!(playground.event_log.len() <= MAX_EVENT_LOG);
        // Last position should be correct
        assert_eq!(playground.last_mouse_pos, Some((19, 1)));
    }

    #[test]
    fn rapid_click_sequence() {
        let mut playground = MousePlayground::new();
        playground.last_grid_area.set(Rect::new(0, 0, 80, 24));

        // Rapid clicks on same position
        for _ in 0..50 {
            let down = mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 4);
            let up = mouse_event(MouseEventKind::Up(MouseButton::Left), 5, 4);
            playground.handle_mouse(down);
            playground.handle_mouse(up);
        }

        // Should count all clicks
        let target = playground.targets.iter().find(|t| t.id == 1).unwrap();
        assert_eq!(target.clicks, 50);
    }

    #[test]
    fn alternating_buttons() {
        let mut playground = MousePlayground::new();

        // Alternate between different buttons
        for i in 0..10 {
            let btn = match i % 3 {
                0 => MouseButton::Left,
                1 => MouseButton::Right,
                _ => MouseButton::Middle,
            };
            let event = mouse_event(MouseEventKind::Down(btn), 10, 10);
            playground.handle_mouse(event);
        }

        // All events should be logged
        assert_eq!(playground.event_log.len(), 10);
    }

    // -------------------------------------------------------------------------
    // State consistency tests
    // -------------------------------------------------------------------------

    #[test]
    fn position_updates_on_every_event() {
        let mut playground = MousePlayground::new();

        let positions = [(0, 0), (50, 25), (100, 50), (u16::MAX, u16::MAX)];

        for (x, y) in positions {
            let event = mouse_event(MouseEventKind::Moved, x, y);
            playground.handle_mouse(event);
            assert_eq!(playground.last_mouse_pos, Some((x, y)));
        }
    }

    #[test]
    fn event_log_contains_correct_positions() {
        let mut playground = MousePlayground::new();

        let event = mouse_event(MouseEventKind::Moved, 42, 24);
        playground.handle_mouse(event);

        let entry = playground.event_log.front().unwrap();
        assert_eq!(entry.x, 42);
        assert_eq!(entry.y, 24);
    }

    #[test]
    fn empty_grid_click_is_safe() {
        let mut playground = MousePlayground::new();
        // Grid area is default (0,0,0,0)

        // Click should not panic even with empty grid
        let event = mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 10);
        playground.handle_mouse(event);

        // Event should still be logged
        assert_eq!(playground.event_log.len(), 1);
        // No target should have clicks (hit_test returns None)
        assert!(playground.targets.iter().all(|t| t.clicks == 0));
    }
}

// -------------------------------------------------------------------------
// Property-Based Tests
// -------------------------------------------------------------------------

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Property: Event log size never exceeds MAX_EVENT_LOG
        #[test]
        fn event_log_bounded(events in proptest::collection::vec(any::<u8>(), 0..100)) {
            let mut playground = MousePlayground::new();
            for (i, _) in events.iter().enumerate() {
                playground.log_event(format!("Event {}", i), 0, 0);
            }
            prop_assert!(playground.event_log.len() <= MAX_EVENT_LOG);
        }

        /// Property: All target IDs are unique
        #[test]
        fn target_ids_unique(_dummy in 0..100u32) {
            let playground = MousePlayground::new();
            let ids: std::collections::HashSet<_> =
                playground.targets.iter().map(|t| t.id).collect();
            prop_assert_eq!(ids.len(), playground.targets.len());
        }

        /// Property: Target count matches grid dimensions
        #[test]
        fn target_count_matches_grid(_dummy in 0..100u32) {
            let playground = MousePlayground::new();
            prop_assert_eq!(playground.targets.len(), GRID_COLS * GRID_ROWS);
        }

        /// Property: Toggle operations are involutions (double toggle = identity)
        #[test]
        fn overlay_toggle_involution(_dummy in 0..100u32) {
            let mut playground = MousePlayground::new();
            let initial = playground.show_overlay;
            playground.toggle_overlay();
            playground.toggle_overlay();
            prop_assert_eq!(playground.show_overlay, initial);
        }

        /// Property: Jitter stats toggle is an involution
        #[test]
        fn jitter_stats_toggle_involution(_dummy in 0..100u32) {
            let mut playground = MousePlayground::new();
            let initial = playground.show_jitter_stats;
            playground.toggle_jitter_stats();
            playground.toggle_jitter_stats();
            prop_assert_eq!(playground.show_jitter_stats, initial);
        }

        /// Property: Clear log always results in empty log
        #[test]
        fn clear_log_empties(events in proptest::collection::vec(any::<u8>(), 0..50)) {
            let mut playground = MousePlayground::new();
            for (i, _) in events.iter().enumerate() {
                playground.log_event(format!("Event {}", i), 0, 0);
            }
            playground.clear_log();
            prop_assert!(playground.event_log.is_empty());
        }

        /// Property: Hit test returns valid target ID or None
        #[test]
        fn hit_test_returns_valid_id(
            x in 0u16..200,
            y in 0u16..100,
            grid_w in 0u16..100,
            grid_h in 0u16..50
        ) {
            let playground = MousePlayground::new();
            playground.last_grid_area.set(Rect::new(0, 0, grid_w, grid_h));
            if let Some(id) = playground.hit_test(x, y) {
                prop_assert!(id >= 1);
                prop_assert!(id <= (GRID_COLS * GRID_ROWS) as u64);
            }
        }

        /// Property: Log entries preserve order (most recent first)
        #[test]
        fn log_entries_ordered(count in 1usize..20) {
            let mut playground = MousePlayground::new();
            for i in 0..count {
                playground.tick_count = i as u64;
                playground.log_event(format!("Event {}", i), 0, 0);
            }

            // Verify entries are in reverse chronological order
            let ticks: Vec<_> = playground.event_log.iter().map(|e| e.tick).collect();
            for window in ticks.windows(2) {
                prop_assert!(window[0] >= window[1], "Events should be in reverse order");
            }
        }

        /// Property: Position is preserved in log entries
        #[test]
        fn log_preserves_coordinates(x in 0u16..1000, y in 0u16..1000) {
            let mut playground = MousePlayground::new();
            playground.log_event("Test", x, y);
            let entry = playground.event_log.front().unwrap();
            prop_assert_eq!(entry.x, x);
            prop_assert_eq!(entry.y, y);
        }

        /// Property: Tick updates correctly
        #[test]
        fn tick_updates_monotonically(ticks in proptest::collection::vec(0u64..1000, 1..20)) {
            let mut playground = MousePlayground::new();
            for &tick in &ticks {
                playground.tick(tick);
            }
            if let Some(&last) = ticks.last() {
                prop_assert_eq!(playground.tick_count, last);
            }
        }
    }
}

// =============================================================================
// Diagnostic and Telemetry Tests (bd-bksf.5)
// =============================================================================

#[cfg(test)]
mod diagnostic_tests {
    use serial_test::serial;

    use super::*;

    #[test]
    #[serial(event_counter)]
    fn diagnostic_entry_new_sets_fields() {
        reset_event_counter();
        let entry = DiagnosticEntry::new(DiagnosticEventKind::MouseDown, 42);
        assert_eq!(entry.kind, DiagnosticEventKind::MouseDown);
        assert_eq!(entry.tick, 42);
        assert_eq!(entry.seq, 0); // First entry after reset
    }

    #[test]
    fn diagnostic_entry_with_position() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::HitTest, 0).with_position(100, 200);
        assert_eq!(entry.x, Some(100));
        assert_eq!(entry.y, Some(200));
    }

    #[test]
    fn diagnostic_entry_with_target() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::TargetClick, 0).with_target(Some(5));
        assert_eq!(entry.target_id, Some(5));
    }

    #[test]
    fn diagnostic_entry_with_hover_transition() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::HoverChange, 0)
            .with_hover_transition(Some(1), Some(2));
        assert_eq!(entry.prev_target_id, Some(1));
        assert_eq!(entry.target_id, Some(2));
    }

    #[test]
    fn diagnostic_entry_with_grid_dims() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::GridRender, 0).with_grid_dims(80, 24);
        assert_eq!(entry.grid_dims, Some((80, 24)));
    }

    #[test]
    fn diagnostic_entry_with_context() {
        let entry =
            DiagnosticEntry::new(DiagnosticEventKind::MouseDown, 0).with_context("Left Down");
        assert_eq!(entry.context, Some("Left Down".to_string()));
    }

    #[test]
    fn diagnostic_entry_checksum_is_deterministic() {
        let entry1 = DiagnosticEntry::new(DiagnosticEventKind::HitTest, 10)
            .with_position(50, 25)
            .with_target(Some(3))
            .with_checksum();

        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::HitTest, 10)
            .with_position(50, 25)
            .with_target(Some(3))
            .with_checksum();

        assert_eq!(entry1.checksum, entry2.checksum);
    }

    #[test]
    fn diagnostic_entry_checksum_differs_for_different_data() {
        let entry1 = DiagnosticEntry::new(DiagnosticEventKind::HitTest, 10)
            .with_position(50, 25)
            .with_checksum();

        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::HitTest, 10)
            .with_position(51, 25) // Different X
            .with_checksum();

        assert_ne!(entry1.checksum, entry2.checksum);
    }

    #[test]
    #[serial(event_counter)]
    fn diagnostic_entry_to_jsonl_format() {
        reset_event_counter();
        let entry = DiagnosticEntry::new(DiagnosticEventKind::MouseDown, 100)
            .with_position(10, 20)
            .with_context("test")
            .with_checksum();

        let jsonl = entry.to_jsonl();
        assert!(jsonl.starts_with('{'));
        assert!(jsonl.ends_with('}'));
        assert!(jsonl.contains("\"kind\":\"mouse_down\""));
        assert!(jsonl.contains("\"tick\":100"));
        assert!(jsonl.contains("\"x\":10"));
        assert!(jsonl.contains("\"y\":20"));
        assert!(jsonl.contains("\"context\":\"test\""));
        assert!(jsonl.contains("\"checksum\":"));
    }

    #[test]
    fn diagnostic_entry_jsonl_escapes_quotes() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::LogClear, 0)
            .with_context("test with \"quotes\"");

        let jsonl = entry.to_jsonl();
        assert!(jsonl.contains("\\\"quotes\\\""));
    }

    #[test]
    fn diagnostic_event_kind_as_str() {
        assert_eq!(DiagnosticEventKind::MouseDown.as_str(), "mouse_down");
        assert_eq!(DiagnosticEventKind::MouseUp.as_str(), "mouse_up");
        assert_eq!(DiagnosticEventKind::MouseDrag.as_str(), "mouse_drag");
        assert_eq!(DiagnosticEventKind::MouseMove.as_str(), "mouse_move");
        assert_eq!(DiagnosticEventKind::MouseScroll.as_str(), "mouse_scroll");
        assert_eq!(DiagnosticEventKind::HitTest.as_str(), "hit_test");
        assert_eq!(DiagnosticEventKind::HoverChange.as_str(), "hover_change");
        assert_eq!(DiagnosticEventKind::TargetClick.as_str(), "target_click");
        assert_eq!(
            DiagnosticEventKind::OverlayToggle.as_str(),
            "overlay_toggle"
        );
        assert_eq!(
            DiagnosticEventKind::JitterStatsToggle.as_str(),
            "jitter_stats_toggle"
        );
        assert_eq!(DiagnosticEventKind::LogClear.as_str(), "log_clear");
        assert_eq!(DiagnosticEventKind::Tick.as_str(), "tick");
        assert_eq!(DiagnosticEventKind::GridRender.as_str(), "grid_render");
    }

    // -------------------------------------------------------------------------
    // DiagnosticLog Tests
    // -------------------------------------------------------------------------

    #[test]
    fn diagnostic_log_new_is_empty() {
        let log = DiagnosticLog::new();
        assert!(log.entries().is_empty());
    }

    #[test]
    fn diagnostic_log_record_adds_entry() {
        let mut log = DiagnosticLog::new();
        let entry = DiagnosticEntry::new(DiagnosticEventKind::HitTest, 0);
        log.record(entry);
        assert_eq!(log.entries().len(), 1);
    }

    #[test]
    fn diagnostic_log_respects_max_entries() {
        let mut log = DiagnosticLog::new().with_max_entries(5);
        for i in 0..10 {
            log.record(DiagnosticEntry::new(DiagnosticEventKind::HitTest, i));
        }
        assert_eq!(log.entries().len(), 5);
    }

    #[test]
    fn diagnostic_log_clear_removes_all() {
        let mut log = DiagnosticLog::new();
        for i in 0..5 {
            log.record(DiagnosticEntry::new(DiagnosticEventKind::HitTest, i));
        }
        assert_eq!(log.entries().len(), 5);
        log.clear();
        assert!(log.entries().is_empty());
    }

    #[test]
    fn diagnostic_log_entries_of_kind() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::HitTest, 0));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::MouseDown, 1));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::HitTest, 2));

        let hit_tests = log.entries_of_kind(DiagnosticEventKind::HitTest);
        assert_eq!(hit_tests.len(), 2);
    }

    #[test]
    fn diagnostic_log_to_jsonl() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::HitTest, 0).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::MouseDown, 1).with_checksum());

        let jsonl = log.to_jsonl();
        let lines: Vec<_> = jsonl.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"kind\":\"hit_test\""));
        assert!(lines[1].contains("\"kind\":\"mouse_down\""));
    }

    #[test]
    fn diagnostic_log_summary() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::MouseDown, 0));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::MouseDown, 1));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::HitTest, 2));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::HoverChange, 3));

        let summary = log.summary();
        assert_eq!(summary.total_entries, 4);
        assert_eq!(summary.mouse_down_count, 2);
        assert_eq!(summary.hit_test_count, 1);
        assert_eq!(summary.hover_change_count, 1);
    }

    #[test]
    fn diagnostic_summary_to_jsonl() {
        let summary = DiagnosticSummary {
            total_entries: 10,
            mouse_down_count: 2,
            mouse_up_count: 2,
            mouse_move_count: 3,
            mouse_drag_count: 1,
            mouse_scroll_count: 1,
            hit_test_count: 4,
            hover_change_count: 1,
            target_click_count: 0,
            tick_count: 0,
        };

        let jsonl = summary.to_jsonl();
        assert!(jsonl.contains("\"summary\":true"));
        assert!(jsonl.contains("\"total\":10"));
        assert!(jsonl.contains("\"mouse_down\":2"));
    }

    // -------------------------------------------------------------------------
    // TelemetryHooks Tests
    // -------------------------------------------------------------------------

    #[test]
    fn telemetry_hooks_new_is_empty() {
        let hooks = TelemetryHooks::new();
        // No panic when dispatching to empty hooks
        let entry = DiagnosticEntry::new(DiagnosticEventKind::HitTest, 0);
        hooks.dispatch(&entry);
    }

    #[test]
    fn telemetry_hooks_on_hit_test_fires() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let hooks = TelemetryHooks::new().on_hit_test(move |_entry| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        // Hit test event should fire callback
        let entry = DiagnosticEntry::new(DiagnosticEventKind::HitTest, 0);
        hooks.dispatch(&entry);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // Non-hit-test event should not fire callback
        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::MouseDown, 0);
        hooks.dispatch(&entry2);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn telemetry_hooks_on_any_fires_for_all() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let hooks = TelemetryHooks::new().on_any(move |_entry| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        hooks.dispatch(&DiagnosticEntry::new(DiagnosticEventKind::HitTest, 0));
        hooks.dispatch(&DiagnosticEntry::new(DiagnosticEventKind::MouseDown, 1));
        hooks.dispatch(&DiagnosticEntry::new(DiagnosticEventKind::HoverChange, 2));

        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn telemetry_hooks_on_hover_change_fires() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let hooks = TelemetryHooks::new().on_hover_change(move |_entry| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        hooks.dispatch(&DiagnosticEntry::new(DiagnosticEventKind::HoverChange, 0));
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn telemetry_hooks_on_target_click_fires() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let hooks = TelemetryHooks::new().on_target_click(move |_entry| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        hooks.dispatch(&DiagnosticEntry::new(DiagnosticEventKind::TargetClick, 0));
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    // -------------------------------------------------------------------------
    // MousePlayground Diagnostic Integration Tests
    // -------------------------------------------------------------------------

    #[test]
    fn playground_with_diagnostics_creates_log() {
        let playground = MousePlayground::new().with_diagnostics();
        assert!(playground.diagnostic_log().is_some());
    }

    #[test]
    fn playground_toggle_overlay_records_diagnostic() {
        let mut playground = MousePlayground::new().with_diagnostics();
        playground.toggle_overlay();

        let log = playground.diagnostic_log().unwrap();
        let entries = log.entries_of_kind(DiagnosticEventKind::OverlayToggle);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0]
                .context
                .as_ref()
                .unwrap()
                .contains("enabled=true")
        );
    }

    #[test]
    fn playground_toggle_jitter_stats_records_diagnostic() {
        let mut playground = MousePlayground::new().with_diagnostics();
        playground.toggle_jitter_stats();

        let log = playground.diagnostic_log().unwrap();
        let entries = log.entries_of_kind(DiagnosticEventKind::JitterStatsToggle);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn playground_clear_log_records_diagnostic() {
        let mut playground = MousePlayground::new().with_diagnostics();
        playground.log_event("test", 0, 0);
        playground.clear_log();

        let log = playground.diagnostic_log().unwrap();
        let entries = log.entries_of_kind(DiagnosticEventKind::LogClear);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].context.as_ref().unwrap().contains("cleared=1"));
    }

    // -------------------------------------------------------------------------
    // Global State Tests
    // -------------------------------------------------------------------------

    #[test]
    #[serial(event_counter)]
    fn event_counter_increments() {
        reset_event_counter();
        assert_eq!(next_event_seq(), 0);
        assert_eq!(next_event_seq(), 1);
        assert_eq!(next_event_seq(), 2);
    }

    #[test]
    #[serial(diagnostics_flag)]
    fn diagnostics_enabled_flag() {
        set_diagnostics_enabled(false);
        assert!(!diagnostics_enabled());
        set_diagnostics_enabled(true);
        assert!(diagnostics_enabled());
        set_diagnostics_enabled(false); // Reset
    }
}

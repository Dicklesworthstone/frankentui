#![forbid(unsafe_code)]

//! Theme Studio — Live palette editor and theme inspector.
//!
//! Demonstrates:
//! - Preset theme list with live switching
//! - Token inspector with color swatches
//! - WCAG contrast ratio validation
//! - Export to FrankenTUI JSON and Ghostty formats

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_text::display_width;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::determinism;
#[cfg(test)]
use crate::theme::ScopedThemeLock;
use crate::theme::{self, ThemeId};
use std::cell::Cell as StdCell;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

// =============================================================================
// Diagnostics + Telemetry (bd-vu0o.5)
// =============================================================================

/// Global diagnostic enable flag (checked once at startup).
static THEME_STUDIO_DIAGNOSTICS_ENABLED: AtomicBool = AtomicBool::new(false);
/// Global monotonic event counter for deterministic ordering.
static THEME_STUDIO_EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Initialize diagnostic settings from environment.
pub fn init_diagnostics() {
    let enabled = std::env::var("FTUI_THEME_STUDIO_DIAGNOSTICS")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    THEME_STUDIO_DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Check if diagnostics are enabled.
#[inline]
pub fn diagnostics_enabled() -> bool {
    THEME_STUDIO_DIAGNOSTICS_ENABLED.load(Ordering::Relaxed)
}

/// Set diagnostics enabled state (for testing).
pub fn set_diagnostics_enabled(enabled: bool) {
    THEME_STUDIO_DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Get next monotonic event sequence number.
#[inline]
fn next_event_seq() -> u64 {
    THEME_STUDIO_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Reset event counter (for testing determinism).
pub fn reset_event_counter() {
    THEME_STUDIO_EVENT_COUNTER.store(0, Ordering::Relaxed);
}

/// Check if deterministic mode is enabled.
pub fn is_deterministic_mode() -> bool {
    determinism::env_flag("FTUI_THEME_STUDIO_DETERMINISTIC") || determinism::is_demo_deterministic()
}

/// Diagnostic event types for JSONL logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticEventKind {
    /// Focus panel changed.
    FocusChanged,
    /// Preset selection changed.
    PresetChanged,
    /// Token selection changed.
    TokenChanged,
    /// Theme applied from preset list.
    ThemeApplied,
    /// Theme cycled globally.
    ThemeCycled,
    /// Theme export performed.
    ThemeExported,
    /// Tick processed.
    Tick,
}

impl DiagnosticEventKind {
    /// Get the JSONL event type string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FocusChanged => "focus_changed",
            Self::PresetChanged => "preset_changed",
            Self::TokenChanged => "token_changed",
            Self::ThemeApplied => "theme_applied",
            Self::ThemeCycled => "theme_cycled",
            Self::ThemeExported => "theme_exported",
            Self::Tick => "tick",
        }
    }
}

/// JSONL diagnostic log entry.
#[derive(Debug, Clone)]
pub struct DiagnosticEntry {
    /// Monotonic sequence number.
    pub seq: u64,
    /// Timestamp in microseconds.
    pub timestamp_us: u64,
    /// Event kind.
    pub kind: DiagnosticEventKind,
    /// Current focus panel.
    pub focus: Option<String>,
    /// Selected preset name.
    pub preset: Option<String>,
    /// Selected preset index.
    pub preset_index: Option<usize>,
    /// Selected swatch name.
    pub swatch: Option<String>,
    /// Selected swatch index.
    pub swatch_index: Option<usize>,
    /// Exported payload size.
    pub export_bytes: Option<usize>,
    /// Current tick count.
    pub tick: u64,
    /// Additional context.
    pub context: Option<String>,
    /// Checksum for determinism verification.
    pub checksum: u64,
}

impl DiagnosticEntry {
    /// Create a new diagnostic entry with current timestamp.
    pub fn new(kind: DiagnosticEventKind, tick: u64) -> Self {
        let timestamp_us = if is_deterministic_mode() {
            tick * 1000
        } else {
            static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            let start = START.get_or_init(Instant::now);
            start.elapsed().as_micros() as u64
        };

        Self {
            seq: next_event_seq(),
            timestamp_us,
            kind,
            focus: None,
            preset: None,
            preset_index: None,
            swatch: None,
            swatch_index: None,
            export_bytes: None,
            tick,
            context: None,
            checksum: 0,
        }
    }

    /// Set focus panel name.
    #[must_use]
    pub fn with_focus(mut self, focus: impl Into<String>) -> Self {
        self.focus = Some(focus.into());
        self
    }

    /// Set preset name.
    #[must_use]
    pub fn with_preset(mut self, preset: impl Into<String>) -> Self {
        self.preset = Some(preset.into());
        self
    }

    /// Set preset index.
    #[must_use]
    pub fn with_preset_index(mut self, preset_index: usize) -> Self {
        self.preset_index = Some(preset_index);
        self
    }

    /// Set swatch name.
    #[must_use]
    pub fn with_swatch(mut self, label: impl Into<String>) -> Self {
        let label = label.into();
        self.swatch = Some(label);
        self
    }

    /// Set swatch index.
    #[must_use]
    pub fn with_swatch_index(mut self, swatch_index: usize) -> Self {
        self.swatch_index = Some(swatch_index);
        self
    }

    /// Set export payload size.
    #[must_use]
    pub fn with_export_bytes(mut self, export_bytes: usize) -> Self {
        self.export_bytes = Some(export_bytes);
        self
    }

    /// Set context string.
    #[must_use]
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Compute and set checksum.
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
            self.focus.as_deref().unwrap_or(""),
            self.preset.as_deref().unwrap_or(""),
            self.preset_index.unwrap_or(0),
            self.swatch.as_deref().unwrap_or(""),
            self.swatch_index.unwrap_or(0),
            self.export_bytes.unwrap_or(0),
            self.tick
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

        if let Some(ref focus) = self.focus {
            let escaped = focus.replace('\\', "\\\\").replace('"', "\\\"");
            parts.push(format!("\"focus\":\"{escaped}\""));
        }
        if let Some(ref preset) = self.preset {
            let escaped = preset.replace('\\', "\\\\").replace('"', "\\\"");
            parts.push(format!("\"preset\":\"{escaped}\""));
        }
        if let Some(index) = self.preset_index {
            parts.push(format!("\"preset_index\":{index}"));
        }
        if let Some(ref swatch) = self.swatch {
            let escaped = swatch.replace('\\', "\\\\").replace('"', "\\\"");
            parts.push(format!("\"token\":\"{escaped}\""));
        }
        if let Some(index) = self.swatch_index {
            parts.push(format!("\"token_index\":{index}"));
        }
        if let Some(bytes) = self.export_bytes {
            parts.push(format!("\"export_bytes\":{bytes}"));
        }
        if let Some(ref ctx) = self.context {
            let escaped = ctx.replace('\\', "\\\\").replace('"', "\\\"");
            parts.push(format!("\"context\":\"{escaped}\""));
        }
        parts.push(format!("\"checksum\":\"{:016x}\"", self.checksum));

        format!("{{{}}}", parts.join(","))
    }
}

/// Diagnostic log collector.
#[derive(Debug, Default)]
pub struct DiagnosticLog {
    entries: Vec<DiagnosticEntry>,
    max_entries: usize,
    write_stderr: bool,
}

impl DiagnosticLog {
    /// Create a new diagnostic log.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: 5000,
            write_stderr: false,
        }
    }

    /// Create a log that writes to stderr.
    #[must_use]
    pub fn with_stderr(mut self) -> Self {
        self.write_stderr = true;
        self
    }

    /// Set maximum entries to keep.
    #[must_use]
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
                DiagnosticEventKind::FocusChanged => summary.focus_changed_count += 1,
                DiagnosticEventKind::PresetChanged => summary.preset_changed_count += 1,
                DiagnosticEventKind::TokenChanged => summary.token_changed_count += 1,
                DiagnosticEventKind::ThemeApplied => summary.theme_applied_count += 1,
                DiagnosticEventKind::ThemeCycled => summary.theme_cycled_count += 1,
                DiagnosticEventKind::ThemeExported => summary.theme_exported_count += 1,
                DiagnosticEventKind::Tick => summary.tick_count += 1,
            }
        }
        summary
    }
}

/// Diagnostic summary counts.
#[derive(Debug, Default)]
pub struct DiagnosticSummary {
    pub focus_changed_count: usize,
    pub preset_changed_count: usize,
    pub token_changed_count: usize,
    pub theme_applied_count: usize,
    pub theme_cycled_count: usize,
    pub theme_exported_count: usize,
    pub tick_count: usize,
}

/// Callback type for telemetry hooks.
pub type TelemetryCallback = Box<dyn Fn(&DiagnosticEntry) + Send + Sync>;

/// Telemetry hooks for observing theme studio events.
pub struct TelemetryHooks {
    on_focus_change: Option<TelemetryCallback>,
    on_preset_change: Option<TelemetryCallback>,
    on_token_change: Option<TelemetryCallback>,
    on_theme_applied: Option<TelemetryCallback>,
    on_theme_cycled: Option<TelemetryCallback>,
    on_theme_exported: Option<TelemetryCallback>,
    on_any_event: Option<TelemetryCallback>,
}

impl Default for TelemetryHooks {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryHooks {
    pub fn new() -> Self {
        Self {
            on_focus_change: None,
            on_preset_change: None,
            on_token_change: None,
            on_theme_applied: None,
            on_theme_cycled: None,
            on_theme_exported: None,
            on_any_event: None,
        }
    }

    #[must_use]
    pub fn on_focus_change(
        mut self,
        callback: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_focus_change = Some(Box::new(callback));
        self
    }

    #[must_use]
    pub fn on_preset_change(
        mut self,
        callback: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_preset_change = Some(Box::new(callback));
        self
    }

    #[must_use]
    pub fn on_token_change(
        mut self,
        callback: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_token_change = Some(Box::new(callback));
        self
    }

    #[must_use]
    pub fn on_theme_applied(
        mut self,
        callback: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_theme_applied = Some(Box::new(callback));
        self
    }

    #[must_use]
    pub fn on_theme_cycled(
        mut self,
        callback: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_theme_cycled = Some(Box::new(callback));
        self
    }

    #[must_use]
    pub fn on_theme_exported(
        mut self,
        callback: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_theme_exported = Some(Box::new(callback));
        self
    }

    #[must_use]
    pub fn on_any(mut self, callback: impl Fn(&DiagnosticEntry) + Send + Sync + 'static) -> Self {
        self.on_any_event = Some(Box::new(callback));
        self
    }

    fn dispatch(&self, entry: &DiagnosticEntry) {
        match entry.kind {
            DiagnosticEventKind::FocusChanged => {
                if let Some(ref cb) = self.on_focus_change {
                    cb(entry);
                }
            }
            DiagnosticEventKind::PresetChanged => {
                if let Some(ref cb) = self.on_preset_change {
                    cb(entry);
                }
            }
            DiagnosticEventKind::TokenChanged => {
                if let Some(ref cb) = self.on_token_change {
                    cb(entry);
                }
            }
            DiagnosticEventKind::ThemeApplied => {
                if let Some(ref cb) = self.on_theme_applied {
                    cb(entry);
                }
            }
            DiagnosticEventKind::ThemeCycled => {
                if let Some(ref cb) = self.on_theme_cycled {
                    cb(entry);
                }
            }
            DiagnosticEventKind::ThemeExported => {
                if let Some(ref cb) = self.on_theme_exported {
                    cb(entry);
                }
            }
            DiagnosticEventKind::Tick => {}
        }

        if let Some(ref cb) = self.on_any_event {
            cb(entry);
        }
    }
}

/// Focus panel in the Theme Studio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// Focus on presets panel.
    #[default]
    Presets,
    /// Focus on token inspector panel.
    TokenInspector,
}

impl Focus {
    fn toggle(self) -> Self {
        match self {
            Self::Presets => Self::TokenInspector,
            Self::TokenInspector => Self::Presets,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Presets => "presets",
            Self::TokenInspector => "token_inspector",
        }
    }
}

/// A semantic color token for display.
#[derive(Debug, Clone)]
struct ColorToken {
    name: &'static str,
    category: &'static str,
    get_color: fn() -> PackedRgba,
}

impl ColorToken {
    const fn new(
        name: &'static str,
        category: &'static str,
        get_color: fn() -> PackedRgba,
    ) -> Self {
        Self {
            name,
            category,
            get_color,
        }
    }
}

/// Theme Studio demo screen state.
pub struct ThemeStudioDemo {
    /// Current focus panel.
    pub focus: Focus,
    /// Selected preset index.
    pub preset_index: usize,
    /// Selected swatch index.
    pub swatch_index: usize,
    /// List of color tokens for inspection (private, implementation detail).
    tokens: Vec<ColorToken>,
    /// Tick counter for animations.
    pub tick_count: u64,
    /// Export status message.
    pub export_status: Option<String>,
    /// Diagnostic log for telemetry (bd-vu0o.5).
    diagnostic_log: Option<DiagnosticLog>,
    /// Telemetry hooks for external observers (bd-vu0o.5).
    telemetry_hooks: Option<TelemetryHooks>,
    /// Cached presets panel area for mouse hit-testing.
    layout_presets: StdCell<Rect>,
    /// Cached inspector panel area for mouse hit-testing.
    layout_inspector: StdCell<Rect>,
}

impl Default for ThemeStudioDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeStudioDemo {
    /// Create a new theme studio demo.
    pub fn new() -> Self {
        let tokens = Self::build_token_list();
        let diagnostic_log = if diagnostics_enabled() {
            Some(DiagnosticLog::new().with_stderr())
        } else {
            None
        };
        Self {
            focus: Focus::default(),
            preset_index: theme::current_theme().index(),
            swatch_index: 0,
            tokens,
            tick_count: 0,
            export_status: None,
            diagnostic_log,
            telemetry_hooks: None,
            layout_presets: StdCell::new(Rect::default()),
            layout_inspector: StdCell::new(Rect::default()),
        }
    }

    /// Create with diagnostic log enabled (for testing).
    #[must_use]
    pub fn with_diagnostics(mut self) -> Self {
        self.diagnostic_log = Some(DiagnosticLog::new());
        self
    }

    /// Create with telemetry hooks.
    #[must_use]
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

    fn record_diagnostic(&mut self, entry: DiagnosticEntry) {
        let entry = entry.with_checksum();

        if let Some(ref hooks) = self.telemetry_hooks {
            hooks.dispatch(&entry);
        }

        if let Some(ref mut log) = self.diagnostic_log {
            log.record(entry);
        }
    }

    /// Handle mouse events on the presets and token inspector panels.
    fn handle_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        let presets = self.layout_presets.get();
        let inspector = self.layout_inspector.get();

        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if presets.contains(x, y) {
                    self.focus = Focus::Presets;
                    let row = (y - presets.y) as usize;
                    // Row 0 is the border, rows 1+ map to preset indices
                    if row >= 1 && row <= ThemeId::ALL.len() {
                        self.preset_index = row - 1;
                    }
                } else if inspector.contains(x, y) {
                    self.focus = Focus::TokenInspector;
                    let row = (y - inspector.y) as usize;
                    if row >= 1 && row <= self.tokens.len() {
                        self.swatch_index = row - 1;
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if presets.contains(x, y) {
                    self.focus = Focus::Presets;
                    self.apply_preset();
                }
            }
            MouseEventKind::ScrollUp => {
                if presets.contains(x, y) {
                    self.focus = Focus::Presets;
                    if self.preset_index > 0 {
                        self.preset_index -= 1;
                    }
                } else if inspector.contains(x, y) {
                    self.focus = Focus::TokenInspector;
                    if self.swatch_index > 0 {
                        self.swatch_index -= 1;
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if presets.contains(x, y) {
                    self.focus = Focus::Presets;
                    if self.preset_index < ThemeId::ALL.len() - 1 {
                        self.preset_index += 1;
                    }
                } else if inspector.contains(x, y) {
                    self.focus = Focus::TokenInspector;
                    if self.swatch_index < self.tokens.len() - 1 {
                        self.swatch_index += 1;
                    }
                }
            }
            _ => {}
        }
    }

    fn current_preset_name(&self) -> &'static str {
        ThemeId::from_index(self.preset_index).name()
    }

    fn current_token_name(&self) -> Option<&'static str> {
        self.tokens.get(self.swatch_index).map(|token| token.name)
    }

    /// Build the list of inspectable color tokens.
    fn build_token_list() -> Vec<ColorToken> {
        vec![
            // Foreground colors
            ColorToken::new("fg::PRIMARY", "Foreground", || theme::fg::PRIMARY.resolve()),
            ColorToken::new("fg::SECONDARY", "Foreground", || {
                theme::fg::SECONDARY.resolve()
            }),
            ColorToken::new("fg::MUTED", "Foreground", || theme::fg::MUTED.resolve()),
            ColorToken::new("fg::DISABLED", "Foreground", || {
                theme::fg::DISABLED.resolve()
            }),
            // Background colors
            ColorToken::new("bg::DEEP", "Background", || theme::bg::DEEP.resolve()),
            ColorToken::new("bg::BASE", "Background", || theme::bg::BASE.resolve()),
            ColorToken::new("bg::SURFACE", "Background", || theme::bg::SURFACE.resolve()),
            ColorToken::new("bg::OVERLAY", "Background", || theme::bg::OVERLAY.resolve()),
            ColorToken::new("bg::HIGHLIGHT", "Background", || {
                theme::bg::HIGHLIGHT.resolve()
            }),
            // Accent colors
            ColorToken::new("accent::PRIMARY", "Accent", || {
                theme::accent::PRIMARY.resolve()
            }),
            ColorToken::new("accent::SECONDARY", "Accent", || {
                theme::accent::SECONDARY.resolve()
            }),
            ColorToken::new("accent::SUCCESS", "Accent", || {
                theme::accent::SUCCESS.resolve()
            }),
            ColorToken::new("accent::WARNING", "Accent", || {
                theme::accent::WARNING.resolve()
            }),
            ColorToken::new("accent::ERROR", "Accent", || theme::accent::ERROR.resolve()),
            ColorToken::new("accent::INFO", "Accent", || theme::accent::INFO.resolve()),
            ColorToken::new("accent::LINK", "Accent", || theme::accent::LINK.resolve()),
            // Status colors
            ColorToken::new("StatusOpen", "Status", || {
                theme::ColorToken::StatusOpen.resolve()
            }),
            ColorToken::new("StatusInProgress", "Status", || {
                theme::ColorToken::StatusInProgress.resolve()
            }),
            ColorToken::new("StatusBlocked", "Status", || {
                theme::ColorToken::StatusBlocked.resolve()
            }),
            ColorToken::new("StatusClosed", "Status", || {
                theme::ColorToken::StatusClosed.resolve()
            }),
            // Priority colors
            ColorToken::new("PriorityP0", "Priority", || {
                theme::ColorToken::PriorityP0.resolve()
            }),
            ColorToken::new("PriorityP1", "Priority", || {
                theme::ColorToken::PriorityP1.resolve()
            }),
            ColorToken::new("PriorityP2", "Priority", || {
                theme::ColorToken::PriorityP2.resolve()
            }),
            ColorToken::new("PriorityP3", "Priority", || {
                theme::ColorToken::PriorityP3.resolve()
            }),
            ColorToken::new("PriorityP4", "Priority", || {
                theme::ColorToken::PriorityP4.resolve()
            }),
        ]
    }

    /// Calculate WCAG contrast ratio between two colors.
    pub fn contrast_ratio(fg: PackedRgba, bg: PackedRgba) -> f32 {
        fn linearize(v: f32) -> f32 {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(c: PackedRgba) -> f32 {
            let r = linearize(c.r() as f32 / 255.0);
            let g = linearize(c.g() as f32 / 255.0);
            let b = linearize(c.b() as f32 / 255.0);
            0.2126 * r + 0.7152 * g + 0.0722 * b
        }
        let l1 = luminance(fg);
        let l2 = luminance(bg);
        let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Get WCAG rating for a contrast ratio.
    pub fn wcag_rating(ratio: f32) -> (&'static str, PackedRgba) {
        if ratio >= 7.0 {
            ("AAA", PackedRgba::rgb(0, 200, 83)) // Green
        } else if ratio >= 4.5 {
            ("AA", PackedRgba::rgb(100, 180, 100)) // Light green
        } else if ratio >= 3.0 {
            ("AA Large", PackedRgba::rgb(255, 193, 7)) // Yellow
        } else {
            ("Fail", PackedRgba::rgb(244, 67, 54)) // Red
        }
    }

    /// Format color as hex string.
    pub fn color_hex(c: PackedRgba) -> String {
        format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
    }

    /// Export current theme to JSON format.
    pub fn export_json(&self) -> String {
        let theme_id = theme::current_theme();
        let palette = theme::palette(theme_id);
        format!(
            r#"{{
  "name": "{}",
  "version": "1.0.0",
  "colors": {{
    "bg_base": "{}",
    "bg_surface": "{}",
    "fg_primary": "{}",
    "fg_secondary": "{}",
    "accent_primary": "{}",
    "accent_secondary": "{}",
    "accent_success": "{}",
    "accent_warning": "{}",
    "accent_error": "{}"
  }}
}}"#,
            theme_id.name(),
            Self::color_hex(palette.bg_base),
            Self::color_hex(palette.bg_surface),
            Self::color_hex(palette.fg_primary),
            Self::color_hex(palette.fg_secondary),
            Self::color_hex(palette.accent_primary),
            Self::color_hex(palette.accent_secondary),
            Self::color_hex(palette.accent_success),
            Self::color_hex(palette.accent_warning),
            Self::color_hex(palette.accent_error),
        )
    }

    /// Export current theme to Ghostty format.
    fn export_ghostty(&self) -> String {
        let theme_id = theme::current_theme();
        let palette = theme::palette(theme_id);
        format!(
            r#"# Ghostty theme: {}
# Generated by FrankenTUI Theme Studio

background = {}
foreground = {}
selection-background = {}
selection-foreground = {}

# ANSI Colors
palette = 0={}
palette = 1={}
palette = 2={}
palette = 3={}
palette = 4={}
palette = 5={}
palette = 6={}
palette = 7={}
"#,
            theme_id.name(),
            Self::color_hex(palette.bg_base),
            Self::color_hex(palette.fg_primary),
            Self::color_hex(palette.bg_highlight),
            Self::color_hex(palette.fg_primary),
            Self::color_hex(palette.bg_deep),
            Self::color_hex(palette.accent_error),
            Self::color_hex(palette.accent_success),
            Self::color_hex(palette.accent_warning),
            Self::color_hex(palette.accent_primary),
            Self::color_hex(palette.accent_secondary),
            Self::color_hex(palette.accent_info),
            Self::color_hex(palette.fg_secondary),
        )
    }

    /// Apply the selected theme preset.
    fn apply_preset(&mut self) {
        let theme_id = ThemeId::from_index(self.preset_index);
        theme::set_theme(theme_id);
    }

    /// Render the preset list panel.
    fn render_presets(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.focus == Focus::Presets;
        let (border_style, title) = if is_focused {
            (
                Style::new()
                    .fg(theme::accent::PRIMARY.resolve())
                    .attrs(StyleFlags::BOLD),
                " ► Presets ",
            )
        } else {
            (Style::new().fg(theme::fg::MUTED.resolve()), "   Presets ")
        };

        let block = Block::new()
            .title(title)
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        // Render preset list
        for (i, theme_id) in ThemeId::ALL.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }

            let is_selected = i == self.preset_index;
            let is_current = *theme_id == theme::current_theme();

            let prefix = if is_selected && is_focused {
                "▶ "
            } else if is_current {
                "● "
            } else {
                "  "
            };

            let style = if is_selected && is_focused {
                Style::new()
                    .fg(theme::fg::PRIMARY.resolve())
                    .bg(theme::alpha::HIGHLIGHT.resolve())
                    .attrs(StyleFlags::BOLD)
            } else if is_current {
                Style::new()
                    .fg(theme::accent::PRIMARY.resolve())
                    .attrs(StyleFlags::BOLD)
            } else {
                Style::new().fg(theme::fg::PRIMARY.resolve())
            };

            let text = format!("{}{}", prefix, theme_id.name());
            let line_area = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
            Paragraph::new(text).style(style).render(line_area, frame);
        }
    }

    /// Render the token inspector panel.
    fn render_token_inspector(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.focus == Focus::TokenInspector;
        let (border_style, title) = if is_focused {
            (
                Style::new()
                    .fg(theme::accent::PRIMARY.resolve())
                    .attrs(StyleFlags::BOLD),
                " ► Token Inspector ",
            )
        } else {
            (
                Style::new().fg(theme::fg::MUTED.resolve()),
                "   Token Inspector ",
            )
        };

        let block = Block::new()
            .title(title)
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        let bg_color = theme::bg::BASE.resolve();

        // Render token list with contrast info
        for (i, token) in self.tokens.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }

            let is_selected = i == self.swatch_index && is_focused;
            let color = (token.get_color)();
            let contrast = Self::contrast_ratio(color, bg_color);
            let (rating, rating_color) = Self::wcag_rating(contrast);

            let prefix = if is_selected { "▶ " } else { "  " };

            // Format: "▶ token::NAME     #RRGGBB  4.52:1 AA"
            let hex = Self::color_hex(color);
            let name_width = 20;
            let padded_name = format!("{:<width$}", token.name, width = name_width);

            let line_area = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);

            // Render prefix
            let prefix_style = if is_selected {
                Style::new()
                    .fg(theme::fg::PRIMARY.resolve())
                    .bg(theme::alpha::HIGHLIGHT.resolve())
            } else {
                Style::new().fg(theme::fg::MUTED.resolve())
            };
            Paragraph::new(prefix)
                .style(prefix_style)
                .render(Rect::new(line_area.x, line_area.y, 2, 1), frame);

            // Render color swatch (2 cells of solid color)
            let swatch_x = line_area.x + 2;
            if swatch_x + 2 <= line_area.x + line_area.width {
                for dx in 0..2 {
                    if let Some(cell) = frame.buffer.get_mut(swatch_x + dx, line_area.y) {
                        *cell = Cell::from_char(' ').with_bg(color);
                    }
                }
            }

            // Render token name
            let name_x = swatch_x + 3;
            let name_style = if is_selected {
                Style::new()
                    .fg(theme::fg::PRIMARY.resolve())
                    .bg(theme::alpha::HIGHLIGHT.resolve())
            } else {
                Style::new().fg(theme::fg::PRIMARY.resolve())
            };
            if name_x < line_area.x + line_area.width {
                let available = (line_area.x + line_area.width).saturating_sub(name_x);
                Paragraph::new(&*padded_name).style(name_style).render(
                    Rect::new(name_x, line_area.y, available.min(name_width as u16), 1),
                    frame,
                );
            }

            // Render hex value
            let hex_x = name_x + name_width as u16 + 1;
            if hex_x + 8 <= line_area.x + line_area.width {
                let hex_style = Style::new().fg(theme::fg::SECONDARY.resolve());
                Paragraph::new(&*hex)
                    .style(hex_style)
                    .render(Rect::new(hex_x, line_area.y, 8, 1), frame);
            }

            // Render contrast ratio
            let ratio_x = hex_x + 9;
            if ratio_x + 8 <= line_area.x + line_area.width {
                let ratio_text = format!("{:.1}:1", contrast);
                let ratio_style = Style::new().fg(theme::fg::MUTED.resolve());
                Paragraph::new(&*ratio_text)
                    .style(ratio_style)
                    .render(Rect::new(ratio_x, line_area.y, 6, 1), frame);
            }

            // Render WCAG rating
            let rating_x = ratio_x + 7;
            let rating_width = display_width(rating) as u16;
            if rating_x + rating_width <= line_area.x + line_area.width {
                let rating_style = Style::new().fg(rating_color).attrs(StyleFlags::BOLD);
                Paragraph::new(rating)
                    .style(rating_style)
                    .render(Rect::new(rating_x, line_area.y, rating_width, 1), frame);
            }
        }
    }

    /// Render the export status bar.
    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let status = self
            .export_status
            .as_deref()
            .unwrap_or("Press E to export theme");

        let style = Style::new()
            .fg(theme::fg::MUTED.resolve())
            .bg(theme::alpha::SURFACE.resolve());

        Paragraph::new(status).style(style).render(area, frame);
    }
}

/// Message type for Theme Studio.
#[derive(Debug, Clone)]
pub enum ThemeStudioMsg {
    Noop,
}

impl Screen for ThemeStudioDemo {
    type Message = ThemeStudioMsg;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        // Clear export status on any key
        self.export_status = None;
        let prev_focus = self.focus;
        let prev_preset = self.preset_index;
        let prev_index = self.swatch_index;

        if let Event::Mouse(mouse) = event {
            self.handle_mouse(mouse.kind, mouse.x, mouse.y);
        } else if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers,
        }) = event
        {
            match code {
                // Tab to switch focus
                KeyCode::Tab => {
                    self.focus = self.focus.toggle();
                }
                // Navigation
                KeyCode::Up | KeyCode::Char('k') => match self.focus {
                    Focus::Presets => {
                        if self.preset_index > 0 {
                            self.preset_index -= 1;
                        }
                    }
                    Focus::TokenInspector => {
                        if self.swatch_index > 0 {
                            self.swatch_index -= 1;
                        }
                    }
                },
                KeyCode::Down | KeyCode::Char('j') => match self.focus {
                    Focus::Presets => {
                        if self.preset_index < ThemeId::ALL.len() - 1 {
                            self.preset_index += 1;
                        }
                    }
                    Focus::TokenInspector => {
                        if self.swatch_index < self.tokens.len() - 1 {
                            self.swatch_index += 1;
                        }
                    }
                },
                // Home: Jump to first item
                KeyCode::Home | KeyCode::Char('g') if !modifiers.contains(Modifiers::SHIFT) => {
                    match self.focus {
                        Focus::Presets => self.preset_index = 0,
                        Focus::TokenInspector => self.swatch_index = 0,
                    }
                }
                // End: Jump to last item (or G for vim-style)
                KeyCode::End | KeyCode::Char('G') => match self.focus {
                    Focus::Presets => self.preset_index = ThemeId::ALL.len().saturating_sub(1),
                    Focus::TokenInspector => {
                        self.swatch_index = self.tokens.len().saturating_sub(1);
                    }
                },
                // PageUp: Move up by 10 items
                KeyCode::PageUp => match self.focus {
                    Focus::Presets => {
                        self.preset_index = self.preset_index.saturating_sub(10);
                    }
                    Focus::TokenInspector => {
                        self.swatch_index = self.swatch_index.saturating_sub(10);
                    }
                },
                // PageDown: Move down by 10 items
                KeyCode::PageDown => match self.focus {
                    Focus::Presets => {
                        self.preset_index =
                            (self.preset_index + 10).min(ThemeId::ALL.len().saturating_sub(1));
                    }
                    Focus::TokenInspector => {
                        self.swatch_index =
                            (self.swatch_index + 10).min(self.tokens.len().saturating_sub(1));
                    }
                },
                // Apply preset
                KeyCode::Enter => {
                    if self.focus == Focus::Presets {
                        self.apply_preset();
                        let theme_name = theme::current_theme().name();
                        self.export_status = Some(format!("Applied theme: {theme_name}"));
                        self.record_diagnostic(
                            DiagnosticEntry::new(
                                DiagnosticEventKind::ThemeApplied,
                                self.tick_count,
                            )
                            .with_focus(self.focus.as_str())
                            .with_preset(theme_name)
                            .with_preset_index(self.preset_index),
                        );
                    }
                }
                // Cycle theme globally (Ctrl+T)
                KeyCode::Char('t') if modifiers.contains(Modifiers::CTRL) => {
                    theme::cycle_theme();
                    self.preset_index = theme::current_theme().index();
                    let theme_name = theme::current_theme().name();
                    self.export_status = Some(format!("Switched to: {theme_name}"));
                    self.record_diagnostic(
                        DiagnosticEntry::new(DiagnosticEventKind::ThemeCycled, self.tick_count)
                            .with_focus(self.focus.as_str())
                            .with_preset(theme_name)
                            .with_preset_index(self.preset_index),
                    );
                }
                // Export JSON (e)
                KeyCode::Char('e') => {
                    let json = self.export_json();
                    let theme_name = theme::current_theme().name();
                    self.export_status = Some(format!(
                        "Exported JSON: {} ({} bytes)",
                        theme_name,
                        json.len()
                    ));
                    self.record_diagnostic(
                        DiagnosticEntry::new(DiagnosticEventKind::ThemeExported, self.tick_count)
                            .with_focus(self.focus.as_str())
                            .with_preset(theme_name)
                            .with_preset_index(self.preset_index)
                            .with_export_bytes(json.len())
                            .with_context("json"),
                    );
                }
                // Export Ghostty (Shift+E)
                KeyCode::Char('E') => {
                    let ghostty = self.export_ghostty();
                    let theme_name = theme::current_theme().name();
                    self.export_status = Some(format!(
                        "Exported Ghostty: {} ({} bytes)",
                        theme_name,
                        ghostty.len()
                    ));
                    self.record_diagnostic(
                        DiagnosticEntry::new(DiagnosticEventKind::ThemeExported, self.tick_count)
                            .with_focus(self.focus.as_str())
                            .with_preset(theme_name)
                            .with_preset_index(self.preset_index)
                            .with_export_bytes(ghostty.len())
                            .with_context("ghostty"),
                    );
                }
                _ => {}
            }
        }

        if self.focus != prev_focus {
            let mut entry =
                DiagnosticEntry::new(DiagnosticEventKind::FocusChanged, self.tick_count)
                    .with_focus(self.focus.as_str())
                    .with_preset(self.current_preset_name())
                    .with_preset_index(self.preset_index)
                    .with_swatch_index(self.swatch_index);
            if let Some(name) = self.current_token_name() {
                entry = entry.with_swatch(name);
            }
            self.record_diagnostic(entry);
        }

        if self.preset_index != prev_preset {
            self.record_diagnostic(
                DiagnosticEntry::new(DiagnosticEventKind::PresetChanged, self.tick_count)
                    .with_focus(self.focus.as_str())
                    .with_preset(self.current_preset_name())
                    .with_preset_index(self.preset_index),
            );
        }

        if self.swatch_index != prev_index {
            let mut entry =
                DiagnosticEntry::new(DiagnosticEventKind::TokenChanged, self.tick_count)
                    .with_focus(self.focus.as_str())
                    .with_preset(self.current_preset_name())
                    .with_preset_index(self.preset_index)
                    .with_swatch_index(self.swatch_index);
            if let Some(name) = self.current_token_name() {
                entry = entry.with_swatch(name);
            }
            self.record_diagnostic(entry);
        }

        Cmd::none()
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.width < 4 || area.height < 4 {
            return;
        }

        // Layout: left panel (presets) | right panel (token inspector)
        // Bottom: status bar
        let status_height = 1;
        let main_height = area.height.saturating_sub(status_height);

        let main_area = Rect::new(area.x, area.y, area.width, main_height);
        let status_area = Rect::new(area.x, area.y + main_height, area.width, status_height);

        // Split main area into two columns
        let preset_width = 25.min(area.width / 3);
        let inspector_width = area.width.saturating_sub(preset_width).saturating_sub(1);

        let preset_area = Rect::new(main_area.x, main_area.y, preset_width, main_area.height);
        let inspector_area = Rect::new(
            main_area.x + preset_width + 1,
            main_area.y,
            inspector_width,
            main_area.height,
        );

        // Store areas for mouse hit-testing
        self.layout_presets.set(preset_area);
        self.layout_inspector.set(inspector_area);

        // Render panels
        self.render_presets(frame, preset_area);
        self.render_token_inspector(frame, inspector_area);
        self.render_status(frame, status_area);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Tab",
                action: "Switch panel",
            },
            HelpEntry {
                key: "j/k/↑/↓",
                action: "Navigate",
            },
            HelpEntry {
                key: "g/G",
                action: "First/Last",
            },
            HelpEntry {
                key: "PgUp/Dn",
                action: "Page scroll",
            },
            HelpEntry {
                key: "Enter",
                action: "Apply theme",
            },
            HelpEntry {
                key: "Ctrl+T",
                action: "Cycle theme",
            },
            HelpEntry {
                key: "e",
                action: "Export JSON",
            },
            HelpEntry {
                key: "E",
                action: "Export Ghostty",
            },
            HelpEntry {
                key: "Click",
                action: "Select item",
            },
            HelpEntry {
                key: "Scroll",
                action: "Navigate list",
            },
            HelpEntry {
                key: "Right-click",
                action: "Apply preset",
            },
        ]
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        if self.diagnostic_log.is_some() || self.telemetry_hooks.is_some() {
            self.record_diagnostic(
                DiagnosticEntry::new(DiagnosticEventKind::Tick, tick_count)
                    .with_focus(self.focus.as_str())
                    .with_preset(self.current_preset_name())
                    .with_preset_index(self.preset_index),
            );
        }
    }

    fn title(&self) -> &'static str {
        "Theme Studio"
    }

    fn tab_label(&self) -> &'static str {
        "Themes"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
    }

    fn ctrl_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        })
    }

    // -----------------------------------------------------------------------
    // Initialization Tests
    // -----------------------------------------------------------------------

    #[test]
    fn new_creates_valid_instance() {
        let demo = ThemeStudioDemo::new();
        assert!(!demo.tokens.is_empty());
        assert_eq!(demo.focus, Focus::Presets);
    }

    #[test]
    fn default_same_as_new() {
        let a = ThemeStudioDemo::new();
        let b = ThemeStudioDemo::default();
        assert_eq!(a.focus, b.focus);
        assert_eq!(a.preset_index, b.preset_index);
        assert_eq!(a.swatch_index, b.swatch_index);
    }

    #[test]
    fn token_list_has_expected_categories() {
        let demo = ThemeStudioDemo::new();
        let categories: std::collections::HashSet<_> =
            demo.tokens.iter().map(|t| t.category).collect();
        assert!(categories.contains(&"Foreground"));
        assert!(categories.contains(&"Background"));
        assert!(categories.contains(&"Accent"));
    }

    // -----------------------------------------------------------------------
    // Contrast Ratio + WCAG Tests
    // -----------------------------------------------------------------------

    #[test]
    fn contrast_ratio_calculation() {
        // Black on white should be maximum contrast
        let white = PackedRgba::rgb(255, 255, 255);
        let black = PackedRgba::rgb(0, 0, 0);
        let ratio = ThemeStudioDemo::contrast_ratio(black, white);
        assert!(ratio > 20.0, "Black on white should have high contrast");
        assert!(ratio < 22.0, "Contrast ratio should be ~21:1");
    }

    #[test]
    fn contrast_ratio_symmetric() {
        let a = PackedRgba::rgb(100, 150, 200);
        let b = PackedRgba::rgb(50, 75, 100);
        let ratio_ab = ThemeStudioDemo::contrast_ratio(a, b);
        let ratio_ba = ThemeStudioDemo::contrast_ratio(b, a);
        assert!(
            (ratio_ab - ratio_ba).abs() < 0.01,
            "Contrast ratio should be symmetric"
        );
    }

    #[test]
    fn contrast_ratio_same_color() {
        let color = PackedRgba::rgb(128, 128, 128);
        let ratio = ThemeStudioDemo::contrast_ratio(color, color);
        assert!(
            (ratio - 1.0).abs() < 0.01,
            "Same colors should have 1:1 contrast"
        );
    }

    #[test]
    fn wcag_rating_thresholds() {
        assert_eq!(ThemeStudioDemo::wcag_rating(7.5).0, "AAA");
        assert_eq!(ThemeStudioDemo::wcag_rating(5.0).0, "AA");
        assert_eq!(ThemeStudioDemo::wcag_rating(3.5).0, "AA Large");
        assert_eq!(ThemeStudioDemo::wcag_rating(2.0).0, "Fail");
    }

    #[test]
    fn wcag_rating_boundary_values() {
        // Exact boundaries
        assert_eq!(ThemeStudioDemo::wcag_rating(7.0).0, "AAA");
        assert_eq!(ThemeStudioDemo::wcag_rating(4.5).0, "AA");
        assert_eq!(ThemeStudioDemo::wcag_rating(3.0).0, "AA Large");
        // Just below boundaries
        assert_eq!(ThemeStudioDemo::wcag_rating(6.99).0, "AA");
        assert_eq!(ThemeStudioDemo::wcag_rating(4.49).0, "AA Large");
        assert_eq!(ThemeStudioDemo::wcag_rating(2.99).0, "Fail");
    }

    // -----------------------------------------------------------------------
    // Color Hex Formatting Tests
    // -----------------------------------------------------------------------

    #[test]
    fn color_hex_format() {
        let color = PackedRgba::rgb(255, 128, 0);
        let hex = ThemeStudioDemo::color_hex(color);
        assert_eq!(hex, "#FF8000");
    }

    #[test]
    fn color_hex_black() {
        let black = PackedRgba::rgb(0, 0, 0);
        assert_eq!(ThemeStudioDemo::color_hex(black), "#000000");
    }

    #[test]
    fn color_hex_white() {
        let white = PackedRgba::rgb(255, 255, 255);
        assert_eq!(ThemeStudioDemo::color_hex(white), "#FFFFFF");
    }

    #[test]
    fn color_hex_always_uppercase() {
        let color = PackedRgba::rgb(171, 205, 239);
        let hex = ThemeStudioDemo::color_hex(color);
        assert_eq!(hex, hex.to_uppercase());
    }

    // -----------------------------------------------------------------------
    // Export Tests
    // -----------------------------------------------------------------------

    #[test]
    fn export_json_produces_valid_output() {
        let demo = ThemeStudioDemo::new();
        let json = demo.export_json();
        assert!(json.contains("\"name\":"));
        assert!(json.contains("\"colors\":"));
        assert!(json.contains("\"bg_base\":"));
    }

    #[test]
    fn export_json_contains_all_color_keys() {
        let demo = ThemeStudioDemo::new();
        let json = demo.export_json();
        // Check key colors are present
        assert!(json.contains("\"fg_primary\":"));
        assert!(json.contains("\"fg_secondary\":"));
        assert!(json.contains("\"accent_primary\":"));
        assert!(json.contains("\"accent_success\":"));
        assert!(json.contains("\"accent_error\":"));
    }

    #[test]
    fn export_json_hex_format() {
        let demo = ThemeStudioDemo::new();
        let json = demo.export_json();
        // All color values should start with #
        assert!(json.matches('#').count() > 5, "Should have hex colors");
    }

    // -----------------------------------------------------------------------
    // Focus and Navigation Tests
    // -----------------------------------------------------------------------

    #[test]
    fn focus_toggle_cycles() {
        let focus = Focus::Presets;
        assert_eq!(focus.toggle(), Focus::TokenInspector);
        assert_eq!(focus.toggle().toggle(), Focus::Presets);
    }

    #[test]
    fn tab_toggles_focus() {
        let mut demo = ThemeStudioDemo::new();
        assert_eq!(demo.focus, Focus::Presets);
        demo.update(&press(KeyCode::Tab));
        assert_eq!(demo.focus, Focus::TokenInspector);
        demo.update(&press(KeyCode::Tab));
        assert_eq!(demo.focus, Focus::Presets);
    }

    #[test]
    fn preset_navigation_up_saturates() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = 0;
        demo.update(&press(KeyCode::Up));
        assert_eq!(demo.preset_index, 0, "Should not go below 0");
    }

    #[test]
    fn preset_navigation_down_saturates() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = ThemeId::ALL.len() - 1;
        demo.update(&press(KeyCode::Down));
        assert_eq!(
            demo.preset_index,
            ThemeId::ALL.len() - 1,
            "Should not exceed max"
        );
    }

    #[test]
    fn token_navigation_requires_focus() {
        let mut demo = ThemeStudioDemo::new();
        demo.swatch_index = 0;
        // Switch to TokenInspector
        demo.update(&press(KeyCode::Tab));
        demo.swatch_index = 0;
        demo.update(&press(KeyCode::Down));
        assert_eq!(demo.swatch_index, 1, "Should navigate tokens when focused");
    }

    #[test]
    fn vim_navigation_j_moves_down() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = 0;
        demo.update(&press(KeyCode::Char('j')));
        assert!(demo.preset_index > 0 || ThemeId::ALL.len() == 1);
    }

    #[test]
    fn vim_navigation_k_moves_up() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = 2;
        demo.update(&press(KeyCode::Char('k')));
        assert_eq!(demo.preset_index, 1);
    }

    #[test]
    fn home_jumps_to_first() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = 5;
        demo.update(&press(KeyCode::Home));
        assert_eq!(demo.preset_index, 0);
    }

    #[test]
    fn end_jumps_to_last() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = 0;
        demo.update(&press(KeyCode::End));
        assert_eq!(demo.preset_index, ThemeId::ALL.len() - 1);
    }

    #[test]
    fn vim_g_jumps_to_first() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = 3;
        demo.update(&press(KeyCode::Char('g')));
        assert_eq!(demo.preset_index, 0);
    }

    #[test]
    fn vim_shift_g_jumps_to_last() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = 0;
        demo.update(&press(KeyCode::Char('G')));
        assert_eq!(demo.preset_index, ThemeId::ALL.len() - 1);
    }

    #[test]
    fn page_up_moves_by_10() {
        let mut demo = ThemeStudioDemo::new();
        // Switch to token inspector which has more items
        demo.update(&press(KeyCode::Tab));
        demo.swatch_index = 15;
        demo.update(&press(KeyCode::PageUp));
        assert_eq!(demo.swatch_index, 5);
    }

    #[test]
    fn page_up_saturates_at_zero() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = 3;
        demo.update(&press(KeyCode::PageUp));
        assert_eq!(demo.preset_index, 0);
    }

    #[test]
    fn page_down_moves_by_10() {
        let mut demo = ThemeStudioDemo::new();
        // Switch to token inspector which has more items
        demo.update(&press(KeyCode::Tab));
        demo.swatch_index = 0;
        demo.update(&press(KeyCode::PageDown));
        assert_eq!(demo.swatch_index, 10.min(demo.tokens.len() - 1));
    }

    #[test]
    fn page_down_saturates_at_max() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = ThemeId::ALL.len() - 2;
        demo.update(&press(KeyCode::PageDown));
        assert_eq!(demo.preset_index, ThemeId::ALL.len() - 1);
    }

    #[test]
    fn home_end_work_in_token_inspector() {
        let mut demo = ThemeStudioDemo::new();
        demo.update(&press(KeyCode::Tab)); // Switch to TokenInspector
        demo.swatch_index = 5;
        demo.update(&press(KeyCode::Home));
        assert_eq!(demo.swatch_index, 0);
        demo.update(&press(KeyCode::End));
        assert_eq!(demo.swatch_index, demo.tokens.len() - 1);
    }

    // -----------------------------------------------------------------------
    // Theme Application Tests
    // -----------------------------------------------------------------------

    #[test]
    fn enter_applies_preset() {
        let mut demo = ThemeStudioDemo::new();
        demo.preset_index = 1; // Darcula
        demo.update(&press(KeyCode::Enter));
        assert!(demo.export_status.is_some(), "Should show status message");
    }

    #[test]
    fn ctrl_t_cycles_theme() {
        // Hold lock for entire test to prevent races with parallel tests
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let mut demo = ThemeStudioDemo::new();
        demo.update(&ctrl_press(KeyCode::Char('t')));
        // Preset index should update to match global theme (Darcula after cycling)
        assert_eq!(demo.preset_index, theme::current_theme().index());
        assert!(demo.export_status.is_some(), "Should show status message");
    }

    // -----------------------------------------------------------------------
    // Export Key Tests
    // -----------------------------------------------------------------------

    #[test]
    fn e_key_exports_json() {
        let mut demo = ThemeStudioDemo::new();
        demo.update(&press(KeyCode::Char('e')));
        assert!(demo.export_status.is_some());
        assert!(
            demo.export_status.as_ref().unwrap().contains("JSON"),
            "Should export JSON format"
        );
    }

    #[test]
    fn shift_e_key_exports_ghostty() {
        let mut demo = ThemeStudioDemo::new();
        demo.update(&press(KeyCode::Char('E')));
        assert!(demo.export_status.is_some());
        assert!(
            demo.export_status.as_ref().unwrap().contains("Ghostty"),
            "Should export Ghostty format"
        );
    }

    #[test]
    fn export_ghostty_produces_valid_output() {
        let demo = ThemeStudioDemo::new();
        let ghostty = demo.export_ghostty();
        assert!(ghostty.contains("background ="));
        assert!(ghostty.contains("foreground ="));
        assert!(ghostty.contains("palette ="));
    }

    #[test]
    fn export_status_clears_on_next_key() {
        let mut demo = ThemeStudioDemo::new();
        demo.update(&press(KeyCode::Char('e')));
        assert!(demo.export_status.is_some());
        demo.update(&press(KeyCode::Down));
        assert!(
            demo.export_status.is_none(),
            "Status should clear on key press"
        );
    }

    // -----------------------------------------------------------------------
    // Diagnostics + Telemetry Tests
    // -----------------------------------------------------------------------

    #[test]
    fn diagnostic_entry_checksum_deterministic() {
        let entry1 = DiagnosticEntry::new(DiagnosticEventKind::ThemeExported, 10)
            .with_focus("presets")
            .with_preset("Solar")
            .with_preset_index(2)
            .with_export_bytes(120)
            .with_context("json")
            .with_checksum();
        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::ThemeExported, 10)
            .with_focus("presets")
            .with_preset("Solar")
            .with_preset_index(2)
            .with_export_bytes(120)
            .with_context("json")
            .with_checksum();
        assert_eq!(entry1.checksum, entry2.checksum);
        assert_ne!(entry1.checksum, 0);
    }

    #[test]
    fn diagnostic_log_records_focus_and_token_changes() {
        let mut demo = ThemeStudioDemo::new().with_diagnostics();
        demo.update(&press(KeyCode::Tab));
        demo.update(&press(KeyCode::Down));
        let log = demo.diagnostic_log().expect("diagnostic log should exist");
        assert!(!log.entries().is_empty());
        assert!(
            !log.entries_of_kind(DiagnosticEventKind::FocusChanged)
                .is_empty()
        );
        assert!(
            !log.entries_of_kind(DiagnosticEventKind::TokenChanged)
                .is_empty()
        );
    }

    #[test]
    fn telemetry_hooks_on_any_fires() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_ref = Arc::clone(&count);
        let hooks = TelemetryHooks::new().on_any(move |_| {
            count_ref.fetch_add(1, Ordering::Relaxed);
        });
        let mut demo = ThemeStudioDemo::new()
            .with_diagnostics()
            .with_telemetry_hooks(hooks);
        demo.update(&press(KeyCode::Tab));
        assert!(count.load(Ordering::Relaxed) > 0);
    }

    // -----------------------------------------------------------------------
    // Screen Trait Tests
    // -----------------------------------------------------------------------

    #[test]
    fn title_is_theme_studio() {
        let demo = ThemeStudioDemo::new();
        assert_eq!(demo.title(), "Theme Studio");
    }

    #[test]
    fn tab_label_is_themes() {
        let demo = ThemeStudioDemo::new();
        assert_eq!(demo.tab_label(), "Themes");
    }

    #[test]
    fn keybindings_not_empty() {
        let demo = ThemeStudioDemo::new();
        let bindings = demo.keybindings();
        assert!(!bindings.is_empty());
    }

    #[test]
    fn keybindings_contain_expected_keys() {
        let demo = ThemeStudioDemo::new();
        let bindings = demo.keybindings();
        let keys: Vec<_> = bindings.iter().map(|b| b.key).collect();
        assert!(keys.iter().any(|k| k.contains("Tab")));
        assert!(keys.iter().any(|k| k.contains("j") || k.contains("k")));
    }

    #[test]
    fn tick_updates_tick_count() {
        let mut demo = ThemeStudioDemo::new();
        assert_eq!(demo.tick_count, 0);
        demo.tick(1);
        assert_eq!(demo.tick_count, 1);
        demo.tick(100);
        assert_eq!(demo.tick_count, 100);
    }

    // -----------------------------------------------------------------------
    // Edge Case Tests
    // -----------------------------------------------------------------------

    #[test]
    fn update_ignores_non_key_events() {
        let mut demo = ThemeStudioDemo::new();
        let initial_focus = demo.focus;
        let initial_preset = demo.preset_index;
        // Mouse event should be ignored
        let mouse_event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: ftui_core::event::MouseEventKind::Moved,
            x: 0,
            y: 0,
            modifiers: Modifiers::NONE,
        });
        demo.update(&mouse_event);
        assert_eq!(demo.focus, initial_focus);
        assert_eq!(demo.preset_index, initial_preset);
    }

    #[test]
    fn click_preset_selects() {
        let _lock = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let mut demo = ThemeStudioDemo::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::new(80, 24, &mut pool);
        demo.view(&mut frame, Rect::new(0, 0, 80, 24));

        let presets = demo.layout_presets.get();
        assert!(!presets.is_empty());

        // Click row 3 in presets panel (should select preset index 2)
        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: presets.x + 1,
            y: presets.y + 3,
            modifiers: Modifiers::NONE,
        });
        demo.update(&event);
        assert_eq!(demo.preset_index, 2);
        assert_eq!(demo.focus, Focus::Presets);
    }

    #[test]
    fn click_inspector_selects_token() {
        let _lock = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let mut demo = ThemeStudioDemo::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::new(80, 24, &mut pool);
        demo.view(&mut frame, Rect::new(0, 0, 80, 24));

        let inspector = demo.layout_inspector.get();
        assert!(!inspector.is_empty());

        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: inspector.x + 1,
            y: inspector.y + 2,
            modifiers: Modifiers::NONE,
        });
        demo.update(&event);
        assert_eq!(demo.swatch_index, 1);
        assert_eq!(demo.focus, Focus::TokenInspector);
    }

    #[test]
    fn scroll_navigates_presets() {
        let _lock = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let mut demo = ThemeStudioDemo::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::new(80, 24, &mut pool);
        demo.view(&mut frame, Rect::new(0, 0, 80, 24));

        let presets = demo.layout_presets.get();
        demo.preset_index = 0;

        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            x: presets.x + 1,
            y: presets.y + 1,
            modifiers: Modifiers::NONE,
        });
        demo.update(&event);
        assert_eq!(demo.preset_index, 1);
    }

    #[test]
    fn right_click_applies_preset() {
        let _lock = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let mut demo = ThemeStudioDemo::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::new(80, 24, &mut pool);
        demo.view(&mut frame, Rect::new(0, 0, 80, 24));

        let presets = demo.layout_presets.get();
        demo.preset_index = 1;

        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            x: presets.x + 1,
            y: presets.y + 1,
            modifiers: Modifiers::NONE,
        });
        demo.update(&event);
        // After right-click, preset should be applied (focus changes to Presets)
        assert_eq!(demo.focus, Focus::Presets);
    }

    #[test]
    fn keybindings_include_mouse_hints() {
        let demo = ThemeStudioDemo::new();
        let bindings = demo.keybindings();
        assert!(bindings.iter().any(|e| e.key.contains("Click")));
        assert!(bindings.iter().any(|e| e.key.contains("Scroll")));
        assert!(bindings.iter().any(|e| e.key.contains("Right-click")));
    }

    #[test]
    fn update_ignores_key_release() {
        let mut demo = ThemeStudioDemo::new();
        let initial_preset = demo.preset_index;
        let release_event = Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Release,
        });
        demo.update(&release_event);
        assert_eq!(
            demo.preset_index, initial_preset,
            "Key release should be ignored"
        );
    }

    #[test]
    fn unhandled_keys_do_not_panic() {
        let mut demo = ThemeStudioDemo::new();
        // Various keys that shouldn't be handled
        demo.update(&press(KeyCode::F(1)));
        demo.update(&press(KeyCode::Home));
        demo.update(&press(KeyCode::End));
        demo.update(&press(KeyCode::PageUp));
        demo.update(&press(KeyCode::Char('x')));
        demo.update(&press(KeyCode::Char('z')));
        // No panic = success
    }
}

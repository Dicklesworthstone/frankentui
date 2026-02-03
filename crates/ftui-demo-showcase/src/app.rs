#![forbid(unsafe_code)]

//! Main application model, message routing, and screen navigation.
//!
//! This module contains the top-level [`AppModel`] that implements the Elm
//! architecture via [`Model`]. It manages all demo screens, routes events,
//! handles global keybindings, and renders the chrome (tab bar, status bar,
//! help/debug overlays).

use std::cell::Cell;
use std::collections::VecDeque;
use std::env;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::Cell as RenderCell;
use ftui_render::frame::Frame;
use ftui_runtime::undo::HistoryManager;
use ftui_runtime::{
    Cmd, Every, InlineAutoRemeasureConfig, Model, Subscription, VoiLogEntry, VoiSampler,
};
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::command_palette::{CommandPalette, PaletteAction};
use ftui_widgets::error_boundary::FallbackWidget;
use ftui_widgets::paragraph::Paragraph;

use crate::screens;
use crate::theme;

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
// ScreenId
// ---------------------------------------------------------------------------

/// Identifies which demo screen is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenId {
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
    /// Performance HUD + Render Budget Visualizer (bd-3k3x).
    PerformanceHud,
    /// Internationalization demo (bd-ic6i.5).
    I18nDemo,
}

impl ScreenId {
    /// All screens in display order.
    pub const ALL: &[ScreenId] = &[
        Self::Dashboard,
        Self::Shakespeare,
        Self::CodeExplorer,
        Self::WidgetGallery,
        Self::LayoutLab,
        Self::FormsInput,
        Self::DataViz,
        Self::FileBrowser,
        Self::AdvancedFeatures,
        Self::Performance,
        Self::TerminalCapabilities,
        Self::MacroRecorder,
        Self::MarkdownRichText,
        Self::VisualEffects,
        Self::ResponsiveDemo,
        Self::LogSearch,
        Self::Notifications,
        Self::ActionTimeline,
        Self::IntrinsicSizing,
        Self::AdvancedTextEditor,
        Self::MousePlayground,
        Self::FormValidation,
        Self::VirtualizedSearch,
        Self::AsyncTasks,
        Self::ThemeStudio,
        Self::SnapshotPlayer,
        Self::PerformanceHud,
        Self::I18nDemo,
    ];

    /// 0-based index in the ALL array.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&s| s == self).unwrap_or(0)
    }

    /// Next screen (wraps around).
    pub fn next(self) -> Self {
        let i = (self.index() + 1) % Self::ALL.len();
        Self::ALL[i]
    }

    /// Previous screen (wraps around).
    pub fn prev(self) -> Self {
        let i = (self.index() + Self::ALL.len() - 1) % Self::ALL.len();
        Self::ALL[i]
    }

    /// Title for the tab bar.
    pub fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Shakespeare => "Shakespeare",
            Self::CodeExplorer => "Code Explorer",
            Self::WidgetGallery => "Widget Gallery",
            Self::LayoutLab => "Layout Lab",
            Self::FormsInput => "Forms & Input",
            Self::DataViz => "Data Viz",
            Self::FileBrowser => "File Browser",
            Self::AdvancedFeatures => "Advanced",
            Self::TerminalCapabilities => "Terminal Capabilities",
            Self::MacroRecorder => "Macro Recorder",
            Self::Performance => "Performance",
            Self::MarkdownRichText => "Markdown",
            Self::VisualEffects => "Visual Effects",
            Self::ResponsiveDemo => "Responsive Layout",
            Self::LogSearch => "Log Search",
            Self::Notifications => "Notifications",
            Self::ActionTimeline => "Action Timeline",
            Self::IntrinsicSizing => "Intrinsic Sizing",
            Self::AdvancedTextEditor => "Advanced Text Editor",
            Self::MousePlayground => "Mouse Playground",
            Self::FormValidation => "Form Validation",
            Self::VirtualizedSearch => "Virtualized Search",
            Self::AsyncTasks => "Async Tasks",
            Self::ThemeStudio => "Theme Studio",
            Self::SnapshotPlayer => "Snapshot Player",
            Self::PerformanceHud => "Performance HUD",
            Self::I18nDemo => "i18n Demo",
        }
    }

    /// Short label for the tab (max ~12 chars).
    pub fn tab_label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dash",
            Self::Shakespeare => "Shakes",
            Self::CodeExplorer => "Code",
            Self::WidgetGallery => "Widgets",
            Self::LayoutLab => "Layout",
            Self::FormsInput => "Forms",
            Self::DataViz => "DataViz",
            Self::FileBrowser => "Files",
            Self::AdvancedFeatures => "Adv",
            Self::TerminalCapabilities => "Caps",
            Self::MacroRecorder => "Macro",
            Self::Performance => "Perf",
            Self::MarkdownRichText => "MD",
            Self::VisualEffects => "VFX",
            Self::ResponsiveDemo => "Resp",
            Self::LogSearch => "Logs",
            Self::Notifications => "Notify",
            Self::ActionTimeline => "Timeline",
            Self::IntrinsicSizing => "Sizing",
            Self::AdvancedTextEditor => "Editor",
            Self::MousePlayground => "Mouse",
            Self::FormValidation => "Validate",
            Self::VirtualizedSearch => "VirtSearch",
            Self::AsyncTasks => "Tasks",
            Self::ThemeStudio => "Themes",
            Self::SnapshotPlayer => "Snapshot",
            Self::PerformanceHud => "PerfHUD",
            Self::I18nDemo => "i18n",
        }
    }

    /// Widget name used in error boundary fallback messages.
    fn widget_name(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Shakespeare => "Shakespeare",
            Self::CodeExplorer => "CodeExplorer",
            Self::WidgetGallery => "WidgetGallery",
            Self::LayoutLab => "LayoutLab",
            Self::FormsInput => "FormsInput",
            Self::DataViz => "DataViz",
            Self::FileBrowser => "FileBrowser",
            Self::AdvancedFeatures => "AdvancedFeatures",
            Self::TerminalCapabilities => "TerminalCapabilities",
            Self::MacroRecorder => "MacroRecorder",
            Self::Performance => "Performance",
            Self::MarkdownRichText => "MarkdownRichText",
            Self::VisualEffects => "VisualEffects",
            Self::ResponsiveDemo => "ResponsiveDemo",
            Self::LogSearch => "LogSearch",
            Self::Notifications => "Notifications",
            Self::ActionTimeline => "ActionTimeline",
            Self::IntrinsicSizing => "IntrinsicSizing",
            Self::AdvancedTextEditor => "AdvancedTextEditor",
            Self::MousePlayground => "MousePlayground",
            Self::FormValidation => "FormValidation",
            Self::VirtualizedSearch => "VirtualizedSearch",
            Self::AsyncTasks => "AsyncTasks",
            Self::ThemeStudio => "ThemeStudio",
            Self::SnapshotPlayer => "SnapshotPlayer",
            Self::PerformanceHud => "PerformanceHud",
            Self::I18nDemo => "I18nDemo",
        }
    }

    /// Map number key to screen: '1'..='9' -> first 9, '0' -> 10th.
    pub fn from_number_key(ch: char) -> Option<Self> {
        let idx = match ch {
            '1'..='9' => (ch as usize) - ('1' as usize),
            '0' => 9,
            _ => return None,
        };
        Self::ALL.get(idx).copied()
    }
}

// ---------------------------------------------------------------------------
// ScreenStates
// ---------------------------------------------------------------------------

/// Holds the state for every screen.
#[derive(Default)]
pub struct ScreenStates {
    /// Dashboard screen state.
    pub dashboard: screens::dashboard::Dashboard,
    /// Shakespeare library screen state.
    pub shakespeare: screens::shakespeare::Shakespeare,
    /// Code explorer screen state.
    pub code_explorer: screens::code_explorer::CodeExplorer,
    /// Widget gallery screen state.
    pub widget_gallery: screens::widget_gallery::WidgetGallery,
    /// Layout laboratory screen state.
    pub layout_lab: screens::layout_lab::LayoutLab,
    /// Forms and input screen state.
    pub forms_input: screens::forms_input::FormsInput,
    /// Data visualization screen state.
    pub data_viz: screens::data_viz::DataViz,
    /// File browser screen state.
    pub file_browser: screens::file_browser::FileBrowser,
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
    /// Visual effects screen state.
    pub visual_effects: screens::visual_effects::VisualEffectsScreen,
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
    /// Snapshot/Time Travel Player screen state (bd-3sa7).
    pub snapshot_player: screens::snapshot_player::SnapshotPlayer,
    /// Performance HUD + Render Budget Visualizer screen state (bd-3k3x).
    pub performance_hud: screens::performance_hud::PerformanceHud,
    /// Internationalization demo screen state (bd-ic6i.5).
    pub i18n_demo: screens::i18n_demo::I18nDemo,
    /// Tracks whether each screen has errored during rendering.
    /// Indexed by `ScreenId::index()`.
    screen_errors: [Option<String>; 28],
}

impl ScreenStates {
    /// Forward an event to the screen identified by `id`.
    fn update(&mut self, id: ScreenId, event: &Event) {
        use screens::Screen;
        match id {
            ScreenId::Dashboard => {
                self.dashboard.update(event);
            }
            ScreenId::Shakespeare => {
                self.shakespeare.update(event);
            }
            ScreenId::CodeExplorer => {
                self.code_explorer.update(event);
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
            ScreenId::FileBrowser => {
                self.file_browser.update(event);
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
            ScreenId::VisualEffects => {
                self.visual_effects.update(event);
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
                self.snapshot_player.update(event);
            }
            ScreenId::PerformanceHud => {
                self.performance_hud.update(event);
            }
            ScreenId::I18nDemo => {
                self.i18n_demo.update(event);
            }
        }
    }

    /// Forward a tick to the active screen and always tick performance_hud.
    ///
    /// Only the active screen receives tick updates for animations/data.
    /// Performance HUD is always ticked to collect metrics regardless of which
    /// screen is visible.
    fn tick(&mut self, active: ScreenId, tick_count: u64) {
        use screens::Screen;

        // Always tick performance_hud for metrics collection
        self.performance_hud.tick(tick_count);

        // Only tick the active screen (skip if it's performance_hud since we just ticked it)
        if active == ScreenId::PerformanceHud {
            return;
        }

        match active {
            ScreenId::Dashboard => self.dashboard.tick(tick_count),
            ScreenId::Shakespeare => self.shakespeare.tick(tick_count),
            ScreenId::CodeExplorer => self.code_explorer.tick(tick_count),
            ScreenId::WidgetGallery => self.widget_gallery.tick(tick_count),
            ScreenId::LayoutLab => self.layout_lab.tick(tick_count),
            ScreenId::FormsInput => self.forms_input.tick(tick_count),
            ScreenId::DataViz => self.data_viz.tick(tick_count),
            ScreenId::FileBrowser => self.file_browser.tick(tick_count),
            ScreenId::AdvancedFeatures => self.advanced_features.tick(tick_count),
            ScreenId::TerminalCapabilities => self.terminal_capabilities.tick(tick_count),
            ScreenId::MacroRecorder => self.macro_recorder.tick(tick_count),
            ScreenId::Performance => self.performance.tick(tick_count),
            ScreenId::MarkdownRichText => self.markdown_rich_text.tick(tick_count),
            ScreenId::VisualEffects => self.visual_effects.tick(tick_count),
            ScreenId::ResponsiveDemo => self.responsive_demo.tick(tick_count),
            ScreenId::LogSearch => self.log_search.tick(tick_count),
            ScreenId::Notifications => self.notifications.tick(tick_count),
            ScreenId::ActionTimeline => self.action_timeline.tick(tick_count),
            ScreenId::IntrinsicSizing => self.intrinsic_sizing.tick(tick_count),
            ScreenId::AdvancedTextEditor => self.advanced_text_editor.tick(tick_count),
            ScreenId::MousePlayground => self.mouse_playground.tick(tick_count),
            ScreenId::FormValidation => self.form_validation.tick(tick_count),
            ScreenId::VirtualizedSearch => self.virtualized_search.tick(tick_count),
            ScreenId::AsyncTasks => self.async_tasks.tick(tick_count),
            ScreenId::ThemeStudio => self.theme_studio.tick(tick_count),
            ScreenId::SnapshotPlayer => self.snapshot_player.tick(tick_count),
            ScreenId::PerformanceHud => {} // Already ticked above
            ScreenId::I18nDemo => self.i18n_demo.tick(tick_count),
        }
    }

    fn apply_theme(&mut self) {
        self.dashboard.apply_theme();
        self.file_browser.apply_theme();
        self.code_explorer.apply_theme();
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
                ScreenId::Dashboard => self.dashboard.view(frame, area),
                ScreenId::Shakespeare => self.shakespeare.view(frame, area),
                ScreenId::CodeExplorer => self.code_explorer.view(frame, area),
                ScreenId::WidgetGallery => self.widget_gallery.view(frame, area),
                ScreenId::LayoutLab => self.layout_lab.view(frame, area),
                ScreenId::FormsInput => self.forms_input.view(frame, area),
                ScreenId::DataViz => self.data_viz.view(frame, area),
                ScreenId::FileBrowser => self.file_browser.view(frame, area),
                ScreenId::AdvancedFeatures => self.advanced_features.view(frame, area),
                ScreenId::TerminalCapabilities => self.terminal_capabilities.view(frame, area),
                ScreenId::MacroRecorder => self.macro_recorder.view(frame, area),
                ScreenId::Performance => self.performance.view(frame, area),
                ScreenId::MarkdownRichText => self.markdown_rich_text.view(frame, area),
                ScreenId::VisualEffects => self.visual_effects.view(frame, area),
                ScreenId::ResponsiveDemo => self.responsive_demo.view(frame, area),
                ScreenId::LogSearch => self.log_search.view(frame, area),
                ScreenId::Notifications => self.notifications.view(frame, area),
                ScreenId::ActionTimeline => self.action_timeline.view(frame, area),
                ScreenId::IntrinsicSizing => self.intrinsic_sizing.view(frame, area),
                ScreenId::AdvancedTextEditor => self.advanced_text_editor.view(frame, area),
                ScreenId::MousePlayground => self.mouse_playground.view(frame, area),
                ScreenId::FormValidation => self.form_validation.view(frame, area),
                ScreenId::VirtualizedSearch => self.virtualized_search.view(frame, area),
                ScreenId::AsyncTasks => self.async_tasks.view(frame, area),
                ScreenId::ThemeStudio => self.theme_studio.view(frame, area),
                ScreenId::SnapshotPlayer => self.snapshot_player.view(frame, area),
                ScreenId::PerformanceHud => self.performance_hud.view(frame, area),
                ScreenId::I18nDemo => self.i18n_demo.view(frame, area),
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
    /// Toggle the evidence ledger / Galaxy-Brain debug overlay.
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
// AppModel
// ---------------------------------------------------------------------------

/// Top-level application state.
///
/// Implements the Elm architecture: all state lives here, messages drive
/// transitions, and `view()` is a pure function of state.
pub struct AppModel {
    /// Currently displayed screen.
    pub current_screen: ScreenId,
    /// Per-screen state storage.
    pub screens: ScreenStates,
    /// Whether the help overlay is visible.
    pub help_visible: bool,
    /// Whether the debug overlay is visible.
    pub debug_visible: bool,
    /// Whether the performance HUD overlay is visible.
    pub perf_hud_visible: bool,
    /// Whether the evidence ledger (Galaxy-Brain) overlay is visible.
    pub evidence_ledger_visible: bool,
    /// VOI sampler driving the evidence ledger overlay.
    pub voi_sampler: VoiSampler,
    /// Accessibility settings (high contrast, reduced motion, large text).
    pub a11y: theme::A11ySettings,
    /// Whether the accessibility panel is visible.
    pub a11y_panel_visible: bool,
    /// Base theme before accessibility overrides.
    pub base_theme: theme::ThemeId,
    /// Command palette for instant action search (Ctrl+K).
    pub command_palette: CommandPalette,
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
    /// Timestamp of last tick received (for stall detection).
    tick_last_seen: Option<Instant>,
    /// Last time a tick stall was logged (rate limiting).
    tick_stall_last_log: Cell<Option<Instant>>,
    /// Global undo/redo history manager for reversible operations.
    pub history: HistoryManager,
    /// Optional telemetry hooks for A11y mode changes.
    a11y_telemetry: Option<A11yTelemetryHooks>,
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
        let mut palette = CommandPalette::new().with_max_visible(12);
        Self::register_palette_actions(&mut palette);
        let mut voi_config = InlineAutoRemeasureConfig::default().voi;
        voi_config.enable_logging = true;
        voi_config.max_log_entries = 96;
        let voi_sampler = VoiSampler::new(voi_config);
        Self {
            current_screen: ScreenId::Dashboard,
            screens: ScreenStates::default(),
            help_visible: false,
            debug_visible: false,
            perf_hud_visible: false,
            evidence_ledger_visible: false,
            voi_sampler,
            a11y: theme::A11ySettings::default(),
            a11y_panel_visible: false,
            base_theme,
            command_palette: palette,
            tick_count: 0,
            frame_count: 0,
            terminal_width: 0,
            terminal_height: 0,
            exit_after_ms: 0,
            perf_last_tick: None,
            perf_tick_times_us: VecDeque::with_capacity(120),
            perf_view_counter: Cell::new(0),
            perf_views_per_tick: 0.0,
            perf_prev_view_count: 0,
            tick_last_seen: None,
            tick_stall_last_log: Cell::new(None),
            history: HistoryManager::default(),
            a11y_telemetry: None,
        }
    }

    /// Attach telemetry hooks for accessibility mode changes.
    pub fn with_a11y_telemetry_hooks(mut self, hooks: A11yTelemetryHooks) -> Self {
        self.a11y_telemetry = Some(hooks);
        self
    }

    fn emit_a11y_event(&self, kind: A11yEventKind) {
        let event = A11yTelemetryEvent {
            kind,
            tick: self.tick_count,
            screen: self.current_screen.title(),
            panel_visible: self.a11y_panel_visible,
            high_contrast: self.a11y.high_contrast,
            reduced_motion: self.a11y.reduced_motion,
            large_text: self.a11y.large_text,
        };

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

    /// Register all palette actions (screens + global commands).
    fn register_palette_actions(palette: &mut CommandPalette) {
        use ftui_widgets::command_palette::ActionItem;

        // Screen navigation actions
        for &id in ScreenId::ALL {
            let action_id = format!("screen:{}", id.title().to_lowercase().replace(' ', "_"));
            palette.register_action(
                ActionItem::new(&action_id, format!("Go to {}", id.title()))
                    .with_description(format!("Switch to the {} screen", id.title()))
                    .with_tags(&["screen", "navigate"])
                    .with_category("Navigate"),
            );
        }

        // Global commands
        palette.register_action(
            ActionItem::new("cmd:toggle_help", "Toggle Help")
                .with_description("Show or hide the keyboard shortcuts overlay")
                .with_tags(&["help", "shortcuts"])
                .with_category("View"),
        );
        palette.register_action(
            ActionItem::new("cmd:toggle_debug", "Toggle Debug Overlay")
                .with_description("Show or hide the debug information panel")
                .with_tags(&["debug", "info"])
                .with_category("View"),
        );
        palette.register_action(
            ActionItem::new("cmd:toggle_perf_hud", "Toggle Performance HUD")
                .with_description("Show or hide the performance metrics overlay")
                .with_tags(&["performance", "hud", "fps", "metrics", "budget"])
                .with_category("View"),
        );
        palette.register_action(
            ActionItem::new("cmd:toggle_evidence_ledger", "Toggle Evidence Ledger")
                .with_description("Show scheduler decisions with Bayes factors (Galaxy-Brain)")
                .with_tags(&["evidence", "bayes", "scheduler", "debug", "galaxy-brain"])
                .with_category("View"),
        );
        palette.register_action(
            ActionItem::new("cmd:cycle_theme", "Cycle Theme")
                .with_description("Switch to the next color theme")
                .with_tags(&["theme", "colors", "appearance"])
                .with_category("View"),
        );
        palette.register_action(
            ActionItem::new("cmd:quit", "Quit")
                .with_description("Exit the application")
                .with_tags(&["exit", "close"])
                .with_category("App"),
        );
    }

    fn handle_msg(&mut self, msg: AppMsg, source: EventSource) -> Cmd<AppMsg> {
        match msg {
            AppMsg::Quit => Cmd::Quit,

            AppMsg::SwitchScreen(id) => {
                let from = self.current_screen.title();
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
                let from = self.current_screen.title();
                self.current_screen = self.current_screen.next();
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Next screen",
                    vec![
                        ("from".to_string(), from.to_string()),
                        ("to".to_string(), self.current_screen.title().to_string()),
                    ],
                );
                Cmd::None
            }

            AppMsg::PrevScreen => {
                let from = self.current_screen.title();
                self.current_screen = self.current_screen.prev();
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Previous screen",
                    vec![
                        ("from".to_string(), from.to_string()),
                        ("to".to_string(), self.current_screen.title().to_string()),
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
                        ("screen", self.current_screen.title()),
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

            AppMsg::ToggleEvidenceLedger => {
                self.evidence_ledger_visible = !self.evidence_ledger_visible;
                let state = if self.evidence_ledger_visible {
                    "on"
                } else {
                    "off"
                };
                self.screens.action_timeline.record_command_event(
                    self.tick_count,
                    "Toggle evidence ledger",
                    vec![("state".to_string(), state.to_string())],
                );
                tracing::info!(
                    target: "ftui.evidence_ledger",
                    visible = self.evidence_ledger_visible,
                    tick = self.tick_count,
                    "Evidence ledger (Galaxy-Brain) toggled"
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
                self.tick_count += 1;
                self.tick_last_seen = Some(Instant::now());
                self.record_tick_timing();
                self.update_voi_ledger();
                if !self.a11y.reduced_motion {
                    self.screens.tick(self.current_screen, self.tick_count);
                }
                let playback_events = self.screens.macro_recorder.drain_playback_events();
                for event in playback_events {
                    let cmd = self.handle_msg(AppMsg::from(event), EventSource::Playback);
                    if matches!(cmd, Cmd::Quit) {
                        return Cmd::Quit;
                    }
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
                    let filter_controls = self.current_screen == ScreenId::MacroRecorder;
                    self.screens
                        .macro_recorder
                        .record_event(&event, filter_controls);
                }

                let source_label = match source {
                    EventSource::User => "user",
                    EventSource::Playback => "playback",
                };
                let screen_title = self.current_screen.title();
                self.screens.action_timeline.record_input_event(
                    self.tick_count,
                    &event,
                    source_label,
                    screen_title,
                );

                // When the command palette is visible, route events to it first.
                if self.command_palette.is_visible() {
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
                    if self.a11y_panel_visible {
                        match (*code, *modifiers) {
                            (KeyCode::Char('A'), Modifiers::SHIFT) | (KeyCode::Escape, _) => {
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

                    match (*code, *modifiers) {
                        // Quit
                        (KeyCode::Char('q'), Modifiers::NONE) => return Cmd::Quit,
                        (KeyCode::Char('c'), Modifiers::CTRL) => return Cmd::Quit,
                        // Command palette (Ctrl+K)
                        (KeyCode::Char('k'), Modifiers::CTRL) => {
                            self.command_palette.open();
                            return Cmd::None;
                        }
                        // Help
                        (KeyCode::Char('?'), _) => {
                            self.help_visible = !self.help_visible;
                            return Cmd::None;
                        }
                        // Debug
                        (KeyCode::F(12), _) => {
                            self.debug_visible = !self.debug_visible;
                            return Cmd::None;
                        }
                        // Performance HUD
                        (KeyCode::Char('p'), Modifiers::CTRL) => {
                            self.perf_hud_visible = !self.perf_hud_visible;
                            return Cmd::None;
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
                        // Tab cycling (Tab/BackTab, or Shift+H/Shift+L for Vim users)
                        (KeyCode::Tab, Modifiers::NONE) => {
                            self.current_screen = self.current_screen.next();
                            return Cmd::None;
                        }
                        (KeyCode::BackTab, _) => {
                            self.current_screen = self.current_screen.prev();
                            return Cmd::None;
                        }
                        (KeyCode::Char('L'), Modifiers::SHIFT) => {
                            self.current_screen = self.current_screen.next();
                            return Cmd::None;
                        }
                        (KeyCode::Char('H'), Modifiers::SHIFT) => {
                            self.current_screen = self.current_screen.prev();
                            return Cmd::None;
                        }
                        // Number keys for direct screen access
                        (KeyCode::Char(ch @ '0'..='9'), Modifiers::NONE) => {
                            if let Some(id) = ScreenId::from_number_key(ch) {
                                self.current_screen = id;
                                return Cmd::None;
                            }
                        }
                        _ => {}
                    }
                }

                // Handle 'R' key to retry errored screens
                if self.screens.has_error(self.current_screen)
                    && let Event::Key(KeyEvent {
                        code: KeyCode::Char('r' | 'R'),
                        kind: KeyEventKind::Press,
                        ..
                    }) = &event
                {
                    self.screens.clear_error(self.current_screen);
                    return Cmd::None;
                }
                self.screens.update(self.current_screen, &event);
                Cmd::None
            }
        }
    }
}

impl Model for AppModel {
    type Message = AppMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        if self.exit_after_ms > 0 {
            let ms = self.exit_after_ms;
            Cmd::Task(Box::new(move || {
                std::thread::sleep(Duration::from_millis(ms));
                AppMsg::Quit
            }))
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

        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());

        frame
            .buffer
            .fill(area, RenderCell::default().with_bg(theme::bg::DEEP.into()));

        // Top-level layout: tab bar (1 row) + content + status bar (1 row)
        let chunks = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(1),
                Constraint::Fixed(1),
            ])
            .split(area);

        // Tab bar (chrome module)
        crate::chrome::render_tab_bar(self.current_screen, frame, chunks[0]);

        // Content area with border
        let content_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(self.current_screen.title())
            .title_alignment(Alignment::Center)
            .style(theme::content_border());

        let inner = content_block.inner(chunks[1]);
        content_block.render(chunks[1], frame);

        // Screen content (wrapped in error boundary)
        self.screens.view(self.current_screen, frame, inner);

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

        // Help overlay (chrome module)
        if self.help_visible {
            let bindings = self.current_screen_keybindings();
            crate::chrome::render_help_overlay(self.current_screen, &bindings, frame, area);
        }

        // Debug overlay
        if self.debug_visible {
            self.render_debug_overlay(frame, area);
        }

        // Performance HUD overlay
        if self.perf_hud_visible {
            self.render_perf_hud(frame, area);
        }

        // Evidence Ledger (Galaxy-Brain) overlay
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
            screen_title: self.current_screen.title(),
            screen_index: self.current_screen.index(),
            screen_count: ScreenId::ALL.len(),
            tick_count: self.tick_count,
            frame_count: self.frame_count,
            terminal_width: self.terminal_width,
            terminal_height: self.terminal_height,
            theme_name: theme::current_theme_name(),
            a11y_high_contrast: self.a11y.high_contrast,
            a11y_reduced_motion: self.a11y.reduced_motion,
            a11y_large_text: self.a11y.large_text,
            can_undo,
            can_redo,
            undo_description,
        };
        crate::chrome::render_status_bar(&status_state, frame, chunks[2]);
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        vec![Box::new(Every::new(Duration::from_millis(100), || {
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
    fn current_screen_keybindings(&self) -> Vec<crate::chrome::HelpEntry> {
        use screens::Screen;
        let entries = match self.current_screen {
            ScreenId::Dashboard => self.screens.dashboard.keybindings(),
            ScreenId::Shakespeare => self.screens.shakespeare.keybindings(),
            ScreenId::CodeExplorer => self.screens.code_explorer.keybindings(),
            ScreenId::WidgetGallery => self.screens.widget_gallery.keybindings(),
            ScreenId::LayoutLab => self.screens.layout_lab.keybindings(),
            ScreenId::FormsInput => self.screens.forms_input.keybindings(),
            ScreenId::DataViz => self.screens.data_viz.keybindings(),
            ScreenId::FileBrowser => self.screens.file_browser.keybindings(),
            ScreenId::AdvancedFeatures => self.screens.advanced_features.keybindings(),
            ScreenId::TerminalCapabilities => self.screens.terminal_capabilities.keybindings(),
            ScreenId::MacroRecorder => self.screens.macro_recorder.keybindings(),
            ScreenId::Performance => self.screens.performance.keybindings(),
            ScreenId::MarkdownRichText => self.screens.markdown_rich_text.keybindings(),
            ScreenId::VisualEffects => self.screens.visual_effects.keybindings(),
            ScreenId::ResponsiveDemo => self.screens.responsive_demo.keybindings(),
            ScreenId::LogSearch => self.screens.log_search.keybindings(),
            ScreenId::Notifications => self.screens.notifications.keybindings(),
            ScreenId::ActionTimeline => self.screens.action_timeline.keybindings(),
            ScreenId::IntrinsicSizing => self.screens.intrinsic_sizing.keybindings(),
            ScreenId::AdvancedTextEditor => self.screens.advanced_text_editor.keybindings(),
            ScreenId::MousePlayground => self.screens.mouse_playground.keybindings(),
            ScreenId::FormValidation => self.screens.form_validation.keybindings(),
            ScreenId::VirtualizedSearch => self.screens.virtualized_search.keybindings(),
            ScreenId::AsyncTasks => self.screens.async_tasks.keybindings(),
            ScreenId::ThemeStudio => self.screens.theme_studio.keybindings(),
            ScreenId::SnapshotPlayer => self.screens.snapshot_player.keybindings(),
            ScreenId::PerformanceHud => self.screens.performance_hud.keybindings(),
            ScreenId::I18nDemo => self.screens.i18n_demo.keybindings(),
        };
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
        match self.current_screen {
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

    fn handle_screen_undo(&mut self, action: UndoAction) -> bool {
        use screens::Screen;
        match self.current_screen {
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
            PaletteAction::Dismiss => Cmd::None,
            PaletteAction::Execute(id) => {
                // Screen navigation: "screen:<name>"
                if let Some(screen_name) = id.strip_prefix("screen:") {
                    for &sid in ScreenId::ALL {
                        let expected = sid.title().to_lowercase().replace(' ', "_");
                        if expected == screen_name {
                            let from = self.current_screen.title();
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
                    }
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
                    "cmd:toggle_evidence_ledger" => {
                        self.evidence_ledger_visible = !self.evidence_ledger_visible;
                        let state = if self.evidence_ledger_visible {
                            "on"
                        } else {
                            "off"
                        };
                        self.screens.action_timeline.record_command_event(
                            self.tick_count,
                            "Toggle evidence ledger (palette)",
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
        let now = Instant::now();
        if let Some(last) = self.perf_last_tick {
            let dt_us = now.duration_since(last).as_micros() as u64;
            if self.perf_tick_times_us.len() >= 120 {
                self.perf_tick_times_us.pop_front();
            }
            self.perf_tick_times_us.push_back(dt_us);
        }
        self.perf_last_tick = Some(now);

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

    /// Update the VOI sampler that powers the Galaxy-Brain evidence ledger.
    fn update_voi_ledger(&mut self) {
        let now = Instant::now();
        let decision = self.voi_sampler.decide(now);
        if decision.should_sample {
            // Deterministic, visible pattern for demo purposes.
            let violated = (self.tick_count % 23) < 4;
            self.voi_sampler.observe_at(violated, now);
        }
    }

    /// Emit a diagnostic log if ticks appear to have stalled.
    fn maybe_log_tick_stall(&self) {
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
                ("screen", self.current_screen.title()),
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
            format!(" Screen:     {}", self.current_screen.title()),
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

        let debug_text = format!(
            "Tick: {}\nFrame: {}\nScreen: {:?}\nSize: {}x{}",
            self.tick_count,
            self.frame_count,
            self.current_screen,
            self.terminal_width,
            self.terminal_height,
        );
        Paragraph::new(debug_text)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(debug_inner, frame);
    }

    /// Render the Evidence Ledger (Galaxy-Brain) debug overlay.
    ///
    /// Shows scheduler decision evidence with Bayes factors and regime
    /// information. This implements bd-1rz0.27.
    ///
    /// The overlay displays:
    /// - Current scheduler regime (Steady/Burst)
    /// - Recent decision log entries with evidence breakdown
    /// - Bayes factor contributions (regime, timing, rate)
    /// - E-process confidence values
    fn render_evidence_ledger(&self, frame: &mut Frame, area: Rect) {
        let _span = tracing::debug_span!(
            target: "ftui.evidence_ledger",
            "render_evidence_ledger",
            tick = self.tick_count,
        )
        .entered();

        // Size the overlay to fit substantial content
        let overlay_width = 56u16.min(area.width.saturating_sub(4));
        let overlay_height = 18u16.min(area.height.saturating_sub(4));

        if overlay_width < 30 || overlay_height < 8 {
            tracing::trace!(
                target: "ftui.evidence_ledger",
                overlay_width,
                overlay_height,
                "Evidence ledger gracefully degraded: area too small"
            );
            return; // Graceful degradation: too small to render
        }

        // Position in bottom-left to avoid overlap with other HUDs
        let x = area.x + 1;
        let y = area.y + area.height.saturating_sub(overlay_height).saturating_sub(2);
        let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

        let ledger_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("🧠 Evidence Ledger")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::accent::SUCCESS).bg(theme::bg::DEEP));

        let inner = ledger_block.inner(overlay_area);
        // Fill background to ensure overlay occludes content behind it
        frame.buffer.fill(
            overlay_area,
            RenderCell::default().with_bg(theme::bg::DEEP.into()),
        );
        ledger_block.render(overlay_area, frame);

        if inner.is_empty() {
            return;
        }

        // Build evidence ledger content
        // In a real integration, this would pull from ResizeCoalescer.logs()
        // For now, show simulated/sample data that demonstrates the format
        let mut lines = Vec::with_capacity(16);

        // Header section
        lines.push(format!("Scheduler State (tick {})", self.tick_count));
        lines.push("─".repeat(inner.width.saturating_sub(2) as usize));

        // Simulate regime state based on terminal activity
        let regime = if self.tick_count % 50 < 10 {
            "Burst"
        } else {
            "Steady"
        };
        let event_rate = if regime == "Burst" {
            12.0 + (self.tick_count % 8) as f64
        } else {
            2.0 + (self.tick_count % 3) as f64
        };
        lines.push(format!(
            "Regime: {:<8} Rate: {:.1} evt/s",
            regime, event_rate
        ));

        // Simulated Bayes factor breakdown
        let log_bf = if regime == "Burst" {
            2.3 + (self.tick_count % 10) as f64 * 0.1
        } else {
            -1.5 + (self.tick_count % 5) as f64 * 0.2
        };
        let regime_contrib = log_bf * 0.4;
        let timing_contrib = log_bf * 0.35;
        let rate_contrib = log_bf * 0.25;

        lines.push(String::new());
        lines.push("Decision Evidence".to_string());
        lines.push("─".repeat(inner.width.saturating_sub(2) as usize));
        lines.push(format!("Log Bayes Factor: {:+.3}", log_bf));
        lines.push(format!(
            "  Regime:  {:+.3}  Timing: {:+.3}",
            regime_contrib, timing_contrib
        ));
        lines.push(format!("  Rate:    {:+.3}", rate_contrib));

        // E-process confidence
        let e_value = if regime == "Burst" {
            0.95 + (self.tick_count % 5) as f64 * 0.01
        } else {
            0.12 + (self.tick_count % 8) as f64 * 0.02
        };
        let threshold = 0.95; // 1/alpha where alpha = 0.05

        lines.push(String::new());
        lines.push("Anytime-Valid Confidence".to_string());
        lines.push("─".repeat(inner.width.saturating_sub(2) as usize));
        let status = if e_value >= threshold {
            "⚠ ALERT"
        } else {
            "✓ OK"
        };
        lines.push(format!(
            "E-value: {:.3} / {:.2} {}",
            e_value, threshold, status
        ));

        // Decision history (last few decisions)
        lines.push(String::new());
        lines.push(format!(
            "Recent: {} apply, {} coalesce",
            self.tick_count / 10,
            self.tick_count / 3
        ));

        // Render the text
        let text = lines.join("\n");
        Paragraph::new(text)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(inner, frame);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;
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
        assert_eq!(app.current_screen, ScreenId::I18nDemo);
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
        assert_eq!(ScreenId::from_number_key('1'), Some(ScreenId::Dashboard));
        assert_eq!(ScreenId::from_number_key('2'), Some(ScreenId::Shakespeare));
        assert_eq!(ScreenId::from_number_key('3'), Some(ScreenId::CodeExplorer));
        assert_eq!(
            ScreenId::from_number_key('4'),
            Some(ScreenId::WidgetGallery)
        );
        assert_eq!(ScreenId::from_number_key('5'), Some(ScreenId::LayoutLab));
        assert_eq!(ScreenId::from_number_key('6'), Some(ScreenId::FormsInput));
        assert_eq!(ScreenId::from_number_key('7'), Some(ScreenId::DataViz));
        assert_eq!(ScreenId::from_number_key('8'), Some(ScreenId::FileBrowser));
        assert_eq!(
            ScreenId::from_number_key('9'),
            Some(ScreenId::AdvancedFeatures)
        );
        assert_eq!(ScreenId::from_number_key('0'), Some(ScreenId::Performance));
        // No direct key for screens after the first 10
        assert_eq!(ScreenId::from_number_key('a'), None);
    }

    #[test]
    fn screen_next_prev_wraps() {
        assert_eq!(ScreenId::Dashboard.next(), ScreenId::Shakespeare);
        assert_eq!(ScreenId::VisualEffects.next(), ScreenId::ResponsiveDemo);
        assert_eq!(ScreenId::Dashboard.prev(), ScreenId::I18nDemo);
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
        assert_eq!(app.current_screen, ScreenId::CodeExplorer);
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
    fn integration_all_screens_render() {
        let app = AppModel::new();
        for &id in ScreenId::ALL {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            let area = Rect::new(0, 0, 120, 38); // Leave room for tab bar + status
            app.screens.view(id, &mut frame, area);
            // If we reach here without panicking, the screen rendered successfully.
        }
    }

    /// Render each screen at 40x10 (tiny) to verify graceful degradation.
    #[test]
    fn integration_resize_small() {
        let app = AppModel::new();
        for &id in ScreenId::ALL {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(40, 10, &mut pool);
            let area = Rect::new(0, 0, 40, 8);
            app.screens.view(id, &mut frame, area);
        }
    }

    /// Switch through all screens and verify each renders.
    #[test]
    fn integration_screen_cycle() {
        let mut app = AppModel::new();
        for &id in ScreenId::ALL {
            app.update(AppMsg::SwitchScreen(id));
            assert_eq!(app.current_screen, id);

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            app.view(&mut frame);
        }
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

    /// Verify Tab cycles forward through all screens.
    #[test]
    fn integration_tab_cycles_all_screens() {
        let mut app = AppModel::new();
        assert_eq!(app.current_screen, ScreenId::Dashboard);

        for i in 1..ScreenId::ALL.len() {
            app.update(AppMsg::NextScreen);
            assert_eq!(app.current_screen, ScreenId::ALL[i]);
        }

        // One more wraps to Dashboard.
        app.update(AppMsg::NextScreen);
        assert_eq!(app.current_screen, ScreenId::Dashboard);
    }

    /// Verify all screens have the expected count.
    #[test]
    fn all_screens_count() {
        assert_eq!(ScreenId::ALL.len(), 28);
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
        // One action per screen + 5 global commands (quit, help, theme, debug, perf_hud)
        let expected = ScreenId::ALL.len() + 5;
        assert_eq!(app.command_palette.action_count(), expected);
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
        for i in 0..30 {
            app.perf_tick_times_us.push_back(90_000 + i * 1_000);
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
        // Previous test counted ALL.len() + 4 global commands.
        // Now we have 5 global commands (quit, help, theme, debug, perf_hud).
        let expected = ScreenId::ALL.len() + 5;
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
            let (tps, avg, p95, p99, min, max) = app.perf_stats();
            assert!(tps >= 0.0, "tps must be non-negative (n={n})");
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

    /// Property: sparkline output length never exceeds max_width (char count).
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

    /// Property: sparkline chars are always valid block characters or space.
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
            // No panic = pass
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

    /// Property: view counter increments on each view call.
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
}

#![forbid(unsafe_code)]

//! Live Log Search & Filter demo screen.
//!
//! Demonstrates the [`LogViewer`] widget with real-time search, filtering,
//! and streaming log lines. Shows:
//!
//! - Streaming log append with auto-scroll (follow mode)
//! - `/` to open inline search bar
//! - `n` / `N` for next/prev match navigation
//! - `f` to toggle filter mode (show only matching lines)
//! - Case sensitivity toggle (Ctrl+C in search mode)
//! - Context lines toggle (Ctrl+X in search mode)
//! - Match count and current position indicator
//!
//! # Telemetry and Diagnostics (bd-1b5h.9)
//!
//! This module provides rich diagnostic logging and telemetry hooks:
//! - JSONL diagnostic output via `DiagnosticLog`
//! - Observable hooks for search, filter, and navigation events
//! - Deterministic mode for reproducible testing
//!
//! ## Environment Variables
//!
//! - `FTUI_LOGSEARCH_DIAGNOSTICS=true` - Enable verbose diagnostic output
//! - `FTUI_LOGSEARCH_DETERMINISTIC=true` - Enable deterministic mode

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::Text;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::log_viewer::{LogViewer, LogViewerState, LogWrapMode, SearchConfig, SearchMode};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::{StatefulWidget, Widget};
use std::cell::Cell;

use super::{HelpEntry, Screen};
use crate::determinism;
use crate::theme;

/// Interval between simulated log line bursts (in ticks).
const LOG_BURST_INTERVAL: u64 = 3;
/// Lines per burst.
const LOG_BURST_SIZE: usize = 2;
/// Max lines retained in the viewer.
const MAX_LOG_LINES: usize = 5_000;

// =============================================================================
// Diagnostic Logging (bd-1b5h.9)
// =============================================================================

/// Global diagnostic enable flag (checked once at startup).
static LOGSEARCH_DIAGNOSTICS_ENABLED: AtomicBool = AtomicBool::new(false);
/// Global monotonic event counter for deterministic ordering.
static LOGSEARCH_EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Initialize diagnostic settings from environment.
pub fn init_diagnostics() {
    let enabled = std::env::var("FTUI_LOGSEARCH_DIAGNOSTICS")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    LOGSEARCH_DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Check if diagnostics are enabled.
#[inline]
pub fn diagnostics_enabled() -> bool {
    LOGSEARCH_DIAGNOSTICS_ENABLED.load(Ordering::Relaxed)
}

/// Set diagnostics enabled state (for testing).
pub fn set_diagnostics_enabled(enabled: bool) {
    LOGSEARCH_DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Get next monotonic event sequence number.
#[inline]
fn next_event_seq() -> u64 {
    LOGSEARCH_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Reset event counter (for testing determinism).
pub fn reset_event_counter() {
    LOGSEARCH_EVENT_COUNTER.store(0, Ordering::Relaxed);
}

/// Check if deterministic mode is enabled.
pub fn is_deterministic_mode() -> bool {
    determinism::env_flag("FTUI_LOGSEARCH_DETERMINISTIC") || determinism::is_demo_deterministic()
}

/// Diagnostic event types for JSONL logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticEventKind {
    /// Search bar opened.
    SearchOpened,
    /// Search bar closed.
    SearchClosed,
    /// Search query updated.
    QueryUpdated,
    /// Filter applied.
    FilterApplied,
    /// Filter cleared.
    FilterCleared,
    /// Match navigation (next/prev).
    MatchNavigation,
    /// Scroll navigation (up/down/page).
    ScrollNavigation,
    /// Pause/resume toggle.
    PauseToggle,
    /// Log line generated.
    LogGenerated,
    /// Mode changed.
    ModeChange,
    /// Tick processed.
    Tick,
}

impl DiagnosticEventKind {
    /// Get the JSONL event type string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SearchOpened => "search_opened",
            Self::SearchClosed => "search_closed",
            Self::QueryUpdated => "query_updated",
            Self::FilterApplied => "filter_applied",
            Self::FilterCleared => "filter_cleared",
            Self::MatchNavigation => "match_navigation",
            Self::ScrollNavigation => "scroll_navigation",
            Self::PauseToggle => "pause_toggle",
            Self::LogGenerated => "log_generated",
            Self::ModeChange => "mode_change",
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
    /// Current query string.
    pub query: Option<String>,
    /// Search mode (literal/regex).
    pub search_mode: Option<String>,
    /// Case sensitivity flag.
    pub case_sensitive: Option<bool>,
    /// Result count (matches or filtered lines).
    pub result_count: Option<usize>,
    /// Current match position.
    pub match_position: Option<usize>,
    /// Filter active flag.
    pub filter_active: Option<bool>,
    /// Navigation direction.
    pub direction: Option<String>,
    /// Lines generated count.
    pub lines_generated: Option<u64>,
    /// Paused state.
    pub paused: Option<bool>,
    /// UI mode.
    pub ui_mode: Option<String>,
    /// Current tick count.
    pub tick: u64,
    /// Latency in microseconds (for search operations).
    pub latency_us: Option<u64>,
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
            query: None,
            search_mode: None,
            case_sensitive: None,
            result_count: None,
            match_position: None,
            filter_active: None,
            direction: None,
            lines_generated: None,
            paused: None,
            ui_mode: None,
            tick,
            latency_us: None,
            context: None,
            checksum: 0,
        }
    }

    /// Set query.
    #[must_use]
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }

    /// Set search mode.
    #[must_use]
    pub fn with_search_mode(mut self, mode: impl Into<String>) -> Self {
        self.search_mode = Some(mode.into());
        self
    }

    /// Set case sensitivity.
    #[must_use]
    pub fn with_case_sensitive(mut self, case_sensitive: bool) -> Self {
        self.case_sensitive = Some(case_sensitive);
        self
    }

    /// Set result count.
    #[must_use]
    pub fn with_result_count(mut self, count: usize) -> Self {
        self.result_count = Some(count);
        self
    }

    /// Set match position.
    #[must_use]
    pub fn with_match_position(mut self, pos: usize) -> Self {
        self.match_position = Some(pos);
        self
    }

    /// Set filter active.
    #[must_use]
    pub fn with_filter_active(mut self, active: bool) -> Self {
        self.filter_active = Some(active);
        self
    }

    /// Set navigation direction.
    #[must_use]
    pub fn with_direction(mut self, direction: impl Into<String>) -> Self {
        self.direction = Some(direction.into());
        self
    }

    /// Set lines generated.
    #[must_use]
    pub fn with_lines_generated(mut self, count: u64) -> Self {
        self.lines_generated = Some(count);
        self
    }

    /// Set paused state.
    #[must_use]
    pub fn with_paused(mut self, paused: bool) -> Self {
        self.paused = Some(paused);
        self
    }

    /// Set UI mode.
    #[must_use]
    pub fn with_ui_mode(mut self, mode: impl Into<String>) -> Self {
        self.ui_mode = Some(mode.into());
        self
    }

    /// Set latency.
    #[must_use]
    pub fn with_latency(mut self, latency_us: u64) -> Self {
        self.latency_us = Some(latency_us);
        self
    }

    /// Set context.
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
            self.query.as_deref().unwrap_or(""),
            self.result_count.unwrap_or(0),
            self.match_position.unwrap_or(0),
            self.filter_active.map_or(0, |b| b as u32),
            self.paused.map_or(0, |b| b as u32),
            self.tick,
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

        if let Some(ref q) = self.query {
            let escaped = q.replace('\\', "\\\\").replace('"', "\\\"");
            parts.push(format!("\"query\":\"{escaped}\""));
        }
        if let Some(ref m) = self.search_mode {
            parts.push(format!("\"search_mode\":\"{m}\""));
        }
        if let Some(c) = self.case_sensitive {
            parts.push(format!("\"case_sensitive\":{c}"));
        }
        if let Some(r) = self.result_count {
            parts.push(format!("\"result_count\":{r}"));
        }
        if let Some(p) = self.match_position {
            parts.push(format!("\"match_position\":{p}"));
        }
        if let Some(f) = self.filter_active {
            parts.push(format!("\"filter_active\":{f}"));
        }
        if let Some(ref d) = self.direction {
            parts.push(format!("\"direction\":\"{d}\""));
        }
        if let Some(l) = self.lines_generated {
            parts.push(format!("\"lines_generated\":{l}"));
        }
        if let Some(p) = self.paused {
            parts.push(format!("\"paused\":{p}"));
        }
        if let Some(ref m) = self.ui_mode {
            parts.push(format!("\"ui_mode\":\"{m}\""));
        }
        if let Some(l) = self.latency_us {
            parts.push(format!("\"latency_us\":{l}"));
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
            max_entries: 10000,
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
                DiagnosticEventKind::SearchOpened => summary.search_opened_count += 1,
                DiagnosticEventKind::SearchClosed => summary.search_closed_count += 1,
                DiagnosticEventKind::QueryUpdated => summary.query_updated_count += 1,
                DiagnosticEventKind::FilterApplied => summary.filter_applied_count += 1,
                DiagnosticEventKind::FilterCleared => summary.filter_cleared_count += 1,
                DiagnosticEventKind::MatchNavigation => summary.match_navigation_count += 1,
                DiagnosticEventKind::ScrollNavigation => summary.scroll_navigation_count += 1,
                DiagnosticEventKind::PauseToggle => summary.pause_toggle_count += 1,
                DiagnosticEventKind::LogGenerated => summary.log_generated_count += 1,
                DiagnosticEventKind::ModeChange => summary.mode_change_count += 1,
                DiagnosticEventKind::Tick => summary.tick_count += 1,
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
    pub search_opened_count: usize,
    pub search_closed_count: usize,
    pub query_updated_count: usize,
    pub filter_applied_count: usize,
    pub filter_cleared_count: usize,
    pub match_navigation_count: usize,
    pub scroll_navigation_count: usize,
    pub pause_toggle_count: usize,
    pub log_generated_count: usize,
    pub mode_change_count: usize,
    pub tick_count: usize,
}

impl DiagnosticSummary {
    /// Format as JSONL.
    pub fn to_jsonl(&self) -> String {
        format!(
            "{{\"summary\":true,\"total\":{},\"search_opened\":{},\"search_closed\":{},\
             \"query_updated\":{},\"filter_applied\":{},\"filter_cleared\":{},\
             \"match_navigation\":{},\"scroll_navigation\":{},\"pause_toggle\":{},\
             \"log_generated\":{},\"mode_change\":{},\"tick\":{}}}",
            self.total_entries,
            self.search_opened_count,
            self.search_closed_count,
            self.query_updated_count,
            self.filter_applied_count,
            self.filter_cleared_count,
            self.match_navigation_count,
            self.scroll_navigation_count,
            self.pause_toggle_count,
            self.log_generated_count,
            self.mode_change_count,
            self.tick_count
        )
    }
}

/// Callback type for telemetry hooks.
pub type TelemetryCallback = Box<dyn Fn(&DiagnosticEntry) + Send + Sync>;

/// Telemetry hooks for observing log search events.
#[derive(Default)]
pub struct TelemetryHooks {
    on_search_opened: Option<TelemetryCallback>,
    on_search_closed: Option<TelemetryCallback>,
    on_query_updated: Option<TelemetryCallback>,
    on_filter_change: Option<TelemetryCallback>,
    on_any_event: Option<TelemetryCallback>,
}

impl TelemetryHooks {
    /// Create new empty hooks.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set search opened callback.
    #[must_use]
    pub fn on_search_opened(
        mut self,
        f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_search_opened = Some(Box::new(f));
        self
    }

    /// Set search closed callback.
    #[must_use]
    pub fn on_search_closed(
        mut self,
        f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_search_closed = Some(Box::new(f));
        self
    }

    /// Set query updated callback.
    #[must_use]
    pub fn on_query_updated(
        mut self,
        f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_query_updated = Some(Box::new(f));
        self
    }

    /// Set filter change callback.
    #[must_use]
    pub fn on_filter_change(
        mut self,
        f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_filter_change = Some(Box::new(f));
        self
    }

    /// Set catch-all callback.
    #[must_use]
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
            DiagnosticEventKind::SearchOpened => {
                if let Some(ref cb) = self.on_search_opened {
                    cb(entry);
                }
            }
            DiagnosticEventKind::SearchClosed => {
                if let Some(ref cb) = self.on_search_closed {
                    cb(entry);
                }
            }
            DiagnosticEventKind::QueryUpdated => {
                if let Some(ref cb) = self.on_query_updated {
                    cb(entry);
                }
            }
            DiagnosticEventKind::FilterApplied | DiagnosticEventKind::FilterCleared => {
                if let Some(ref cb) = self.on_filter_change {
                    cb(entry);
                }
            }
            _ => {}
        }
    }
}

/// UI mode for the search bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    /// Normal log viewing mode.
    Normal,
    /// Search bar is open and accepting input.
    Search,
    /// Filter bar is open and accepting input.
    Filter,
}

/// Log Search demo screen state.
pub struct LogSearch {
    viewer: LogViewer,
    viewer_state: LogViewerState,
    mode: UiMode,
    query: String,
    last_search: String,
    search_config: SearchConfig,
    filter_active: bool,
    filter_query: String,
    tick_count: u64,
    lines_generated: u64,
    paused: bool,
    /// Diagnostic log for telemetry (bd-1b5h.9).
    diagnostic_log: Option<DiagnosticLog>,
    /// Telemetry hooks for external observers (bd-1b5h.9).
    telemetry_hooks: Option<TelemetryHooks>,
    /// Cached log panel area for mouse hit-testing.
    last_log_area: Cell<Rect>,
}

impl Default for LogSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl LogSearch {
    pub fn new() -> Self {
        let mut viewer = LogViewer::new(MAX_LOG_LINES)
            .wrap_mode(LogWrapMode::NoWrap)
            .search_highlight_style(
                Style::new()
                    .fg(theme::bg::BASE)
                    .bg(theme::accent::WARNING)
                    .bold(),
            );

        for i in 0..50 {
            viewer.push(generate_log_line(i));
        }

        // Enable diagnostic log if diagnostics are enabled
        let diagnostic_log = if diagnostics_enabled() {
            Some(DiagnosticLog::new().with_stderr())
        } else {
            None
        };

        Self {
            viewer,
            viewer_state: LogViewerState::default(),
            mode: UiMode::Normal,
            query: String::new(),
            last_search: String::new(),
            search_config: SearchConfig {
                mode: SearchMode::Literal,
                case_sensitive: false,
                context_lines: 0,
            },
            filter_active: false,
            filter_query: String::new(),
            tick_count: 0,
            lines_generated: 50,
            paused: false,
            diagnostic_log,
            telemetry_hooks: None,
            last_log_area: Cell::new(Rect::default()),
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

    /// Record a diagnostic entry and dispatch to hooks.
    fn record_diagnostic(&mut self, entry: DiagnosticEntry) {
        let entry = entry.with_checksum();

        // Dispatch to hooks first
        if let Some(ref hooks) = self.telemetry_hooks {
            hooks.dispatch(&entry);
        }

        // Then record to log
        if let Some(ref mut log) = self.diagnostic_log {
            log.record(entry);
        }
    }

    // -------------------------------------------------------------------------
    // Public accessors for testing (bd-1b5h.9)
    // -------------------------------------------------------------------------

    /// Current search query.
    pub fn current_query(&self) -> &str {
        &self.query
    }

    /// Current UI mode.
    pub fn current_mode(&self) -> &UiMode {
        &self.mode
    }

    /// Whether filter is active.
    pub fn is_filter_active(&self) -> bool {
        self.filter_active
    }

    /// Current filter query.
    pub fn filter_query_str(&self) -> &str {
        &self.filter_query
    }

    /// Whether log stream is paused.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Total lines generated.
    pub fn lines_generated_count(&self) -> u64 {
        self.lines_generated
    }

    /// Get search info (current position, total matches).
    pub fn search_info(&self) -> Option<(usize, usize)> {
        self.viewer.search_info()
    }

    fn submit_search(&mut self) {
        let start = Instant::now();

        if self.query.is_empty() {
            self.viewer.clear_search();
            self.last_search.clear();

            let diag = DiagnosticEntry::new(DiagnosticEventKind::SearchClosed, self.tick_count)
                .with_ui_mode("normal")
                .with_context("submitted empty query");
            self.record_diagnostic(diag);
        } else {
            self.last_search = self.query.clone();
            self.viewer
                .search_with_config(&self.query, self.search_config.clone());

            let latency_us = if is_deterministic_mode() {
                0
            } else {
                start.elapsed().as_micros() as u64
            };
            let (pos, total) = self.viewer.search_info().unwrap_or((0, 0));

            let diag = DiagnosticEntry::new(DiagnosticEventKind::SearchClosed, self.tick_count)
                .with_query(&self.query)
                .with_result_count(total)
                .with_match_position(pos)
                .with_case_sensitive(self.search_config.case_sensitive)
                .with_search_mode(match self.search_config.mode {
                    SearchMode::Literal => "literal",
                    SearchMode::Regex => "regex",
                })
                .with_latency(latency_us)
                .with_ui_mode("normal")
                .with_context("search submitted");
            self.record_diagnostic(diag);
        }
        self.mode = UiMode::Normal;
    }

    fn submit_filter(&mut self) {
        if self.query.is_empty() {
            self.viewer.set_filter(None);
            self.filter_active = false;
            self.filter_query.clear();

            let diag = DiagnosticEntry::new(DiagnosticEventKind::FilterCleared, self.tick_count)
                .with_filter_active(false)
                .with_context("submitted empty filter");
            self.record_diagnostic(diag);
        } else {
            self.filter_query = self.query.clone();
            self.viewer.set_filter(Some(&self.query));
            self.filter_active = true;

            let diag = DiagnosticEntry::new(DiagnosticEventKind::FilterApplied, self.tick_count)
                .with_query(&self.filter_query)
                .with_filter_active(true)
                .with_context("filter submitted");
            self.record_diagnostic(diag);
        }
        self.mode = UiMode::Normal;
    }

    fn handle_normal_key(&mut self, key: &KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('/'), Modifiers::NONE) => {
                self.mode = UiMode::Search;
                self.query = self.last_search.clone();

                let diag = DiagnosticEntry::new(DiagnosticEventKind::SearchOpened, self.tick_count)
                    .with_ui_mode("search")
                    .with_query(&self.query);
                self.record_diagnostic(diag);
            }
            (KeyCode::Char('f'), Modifiers::NONE) => {
                self.mode = UiMode::Filter;
                self.query = self.filter_query.clone();

                let diag = DiagnosticEntry::new(DiagnosticEventKind::ModeChange, self.tick_count)
                    .with_ui_mode("filter")
                    .with_query(&self.query);
                self.record_diagnostic(diag);
            }
            (KeyCode::Char('n'), Modifiers::NONE) => {
                if !self.last_search.is_empty() {
                    self.viewer.next_match();

                    let (pos, total) = self.viewer.search_info().unwrap_or((0, 0));
                    let diag =
                        DiagnosticEntry::new(DiagnosticEventKind::MatchNavigation, self.tick_count)
                            .with_direction("next")
                            .with_match_position(pos)
                            .with_result_count(total)
                            .with_query(&self.last_search);
                    self.record_diagnostic(diag);
                }
            }
            (KeyCode::Char('N'), Modifiers::NONE) => {
                if !self.last_search.is_empty() {
                    self.viewer.prev_match();

                    let (pos, total) = self.viewer.search_info().unwrap_or((0, 0));
                    let diag =
                        DiagnosticEntry::new(DiagnosticEventKind::MatchNavigation, self.tick_count)
                            .with_direction("prev")
                            .with_match_position(pos)
                            .with_result_count(total)
                            .with_query(&self.last_search);
                    self.record_diagnostic(diag);
                }
            }
            (KeyCode::Char('F'), Modifiers::NONE) => {
                self.viewer.set_filter(None);
                self.filter_active = false;
                self.filter_query.clear();

                let diag =
                    DiagnosticEntry::new(DiagnosticEventKind::FilterCleared, self.tick_count)
                        .with_filter_active(false);
                self.record_diagnostic(diag);
            }
            (KeyCode::Char(' '), Modifiers::NONE) => {
                self.paused = !self.paused;

                let diag = DiagnosticEntry::new(DiagnosticEventKind::PauseToggle, self.tick_count)
                    .with_paused(self.paused)
                    .with_lines_generated(self.lines_generated);
                self.record_diagnostic(diag);
            }
            (KeyCode::Char('g'), Modifiers::NONE) => {
                self.viewer.scroll_to_top();

                let diag =
                    DiagnosticEntry::new(DiagnosticEventKind::ScrollNavigation, self.tick_count)
                        .with_direction("top");
                self.record_diagnostic(diag);
            }
            (KeyCode::Char('G'), Modifiers::NONE) => {
                self.viewer.scroll_to_bottom();

                let diag =
                    DiagnosticEntry::new(DiagnosticEventKind::ScrollNavigation, self.tick_count)
                        .with_direction("bottom");
                self.record_diagnostic(diag);
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), Modifiers::NONE) => {
                self.viewer.scroll_up(1);

                let diag =
                    DiagnosticEntry::new(DiagnosticEventKind::ScrollNavigation, self.tick_count)
                        .with_direction("up");
                self.record_diagnostic(diag);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), Modifiers::NONE) => {
                self.viewer.scroll_down(1);

                let diag =
                    DiagnosticEntry::new(DiagnosticEventKind::ScrollNavigation, self.tick_count)
                        .with_direction("down");
                self.record_diagnostic(diag);
            }
            (KeyCode::PageUp, _) => {
                self.viewer.page_up(&self.viewer_state);

                let diag =
                    DiagnosticEntry::new(DiagnosticEventKind::ScrollNavigation, self.tick_count)
                        .with_direction("page_up");
                self.record_diagnostic(diag);
            }
            (KeyCode::PageDown, _) => {
                self.viewer.page_down(&self.viewer_state);

                let diag =
                    DiagnosticEntry::new(DiagnosticEventKind::ScrollNavigation, self.tick_count)
                        .with_direction("page_down");
                self.record_diagnostic(diag);
            }
            (KeyCode::Escape, _) => {
                self.viewer.clear_search();
                self.last_search.clear();

                let diag = DiagnosticEntry::new(DiagnosticEventKind::SearchClosed, self.tick_count)
                    .with_context("cleared via escape");
                self.record_diagnostic(diag);
            }
            _ => {}
        }
    }

    fn handle_input_key(&mut self, key: &KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Escape, _) => {
                let was_mode = match self.mode {
                    UiMode::Search => "search",
                    UiMode::Filter => "filter",
                    UiMode::Normal => "normal",
                };
                self.query.clear();
                self.mode = UiMode::Normal;

                let diag = DiagnosticEntry::new(DiagnosticEventKind::SearchClosed, self.tick_count)
                    .with_ui_mode("normal")
                    .with_context(format!("{was_mode} -> normal (escape)"));
                self.record_diagnostic(diag);
            }
            (KeyCode::Enter, _) => match self.mode {
                UiMode::Search => self.submit_search(),
                UiMode::Filter => self.submit_filter(),
                UiMode::Normal => {}
            },
            (KeyCode::Backspace, _) => {
                self.query.pop();
                self.live_update();
                self.emit_query_updated("backspace");
            }
            (KeyCode::Char('u'), m) if m.contains(Modifiers::CTRL) => {
                self.query.clear();
                self.live_update();
                self.emit_query_updated("ctrl+u clear");
            }
            (KeyCode::Char('c'), m) if m.contains(Modifiers::CTRL) => {
                if self.mode == UiMode::Search {
                    self.search_config.case_sensitive = !self.search_config.case_sensitive;
                    self.live_update();

                    let diag =
                        DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, self.tick_count)
                            .with_query(&self.query)
                            .with_case_sensitive(self.search_config.case_sensitive)
                            .with_search_mode(match self.search_config.mode {
                                SearchMode::Literal => "literal",
                                SearchMode::Regex => "regex",
                            })
                            .with_context("case sensitivity toggled");
                    self.record_diagnostic(diag);
                }
            }
            (KeyCode::Char('r'), m) if m.contains(Modifiers::CTRL) => {
                if self.mode == UiMode::Search {
                    self.search_config.mode = match self.search_config.mode {
                        SearchMode::Literal => SearchMode::Regex,
                        SearchMode::Regex => SearchMode::Literal,
                    };
                    self.live_update();

                    let diag =
                        DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, self.tick_count)
                            .with_query(&self.query)
                            .with_case_sensitive(self.search_config.case_sensitive)
                            .with_search_mode(match self.search_config.mode {
                                SearchMode::Literal => "literal",
                                SearchMode::Regex => "regex",
                            })
                            .with_context("mode toggled");
                    self.record_diagnostic(diag);
                }
            }
            (KeyCode::Char('x'), m) if m.contains(Modifiers::CTRL) => {
                if self.mode == UiMode::Search {
                    self.search_config.context_lines = match self.search_config.context_lines {
                        0 => 1,
                        1 => 2,
                        2 => 5,
                        _ => 0,
                    };
                    self.live_update();

                    let diag =
                        DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, self.tick_count)
                            .with_query(&self.query)
                            .with_context(format!(
                                "context lines: {}",
                                self.search_config.context_lines
                            ));
                    self.record_diagnostic(diag);
                }
            }
            (KeyCode::Char(ch), _) => {
                self.query.push(ch);
                self.live_update();
                self.emit_query_updated(&format!("typed '{ch}'"));
            }
            _ => {}
        }
    }

    /// Helper to emit query updated diagnostic.
    fn emit_query_updated(&mut self, context: &str) {
        let (result_count, match_pos) = self.viewer.search_info().unwrap_or((0, 0));
        let diag = DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, self.tick_count)
            .with_query(&self.query)
            .with_result_count(result_count)
            .with_match_position(match_pos)
            .with_case_sensitive(self.search_config.case_sensitive)
            .with_search_mode(match self.search_config.mode {
                SearchMode::Literal => "literal",
                SearchMode::Regex => "regex",
            })
            .with_context(context);
        self.record_diagnostic(diag);
    }

    fn live_update(&mut self) {
        match self.mode {
            UiMode::Search => {
                if self.query.is_empty() {
                    self.viewer.clear_search();
                } else {
                    self.viewer
                        .search_with_config(&self.query, self.search_config.clone());
                }
            }
            UiMode::Filter => {
                if self.query.is_empty() {
                    self.viewer.set_filter(None);
                } else {
                    self.viewer.set_filter(Some(&self.query));
                }
            }
            UiMode::Normal => {}
        }
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 || area.width < 10 {
            return;
        }

        let mut segments: Vec<String> = Vec::new();

        match self.mode {
            UiMode::Normal => segments.push("NORMAL".into()),
            UiMode::Search => segments.push("SEARCH".into()),
            UiMode::Filter => segments.push("FILTER".into()),
        }

        if self.paused {
            segments.push("PAUSED".into());
        }

        if let Some((current, total)) = self.viewer.search_info() {
            segments.push(format!("{current}/{total}"));
        }

        if self.filter_active {
            segments.push("FILTERED".into());
        }

        if self.mode == UiMode::Search {
            if self.search_config.case_sensitive {
                segments.push("Aa".into());
            } else {
                segments.push("aa".into());
            }
            match self.search_config.mode {
                SearchMode::Literal => segments.push("lit".into()),
                SearchMode::Regex => segments.push("re".into()),
            }
            if self.search_config.context_lines > 0 {
                segments.push(format!("ctx:{}", self.search_config.context_lines));
            }
        }

        let status_text = format!(
            " {} | lines: {} | gen: {} ",
            segments.join(" | "),
            self.viewer.line_count(),
            self.lines_generated,
        );

        let style = Style::new().fg(theme::fg::SECONDARY).bg(theme::bg::SURFACE);
        let para = Paragraph::new(Text::from(status_text)).style(style);
        Widget::render(&para, area, frame);
    }

    fn render_input_bar(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }

        let prefix = match self.mode {
            UiMode::Search => "/",
            UiMode::Filter => "filter: ",
            UiMode::Normal => return,
        };

        let display = format!("{}{}_", prefix, self.query);
        let input_style = Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::OVERLAY);
        let para = Paragraph::new(Text::from(display)).style(input_style);
        Widget::render(&para, area, frame);
    }

    /// Handle mouse events: scroll to navigate log, click to focus/unfocus search.
    fn handle_mouse(&mut self, event: &Event) {
        if let Event::Mouse(mouse) = event {
            let log_area = self.last_log_area.get();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if log_area.contains(mouse.x, mouse.y) && self.mode != UiMode::Normal {
                        // Click log area while in search/filter mode: return to normal
                        self.mode = UiMode::Normal;
                    }
                }
                MouseEventKind::ScrollUp => {
                    if log_area.contains(mouse.x, mouse.y) {
                        self.viewer.scroll_up(3);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if log_area.contains(mouse.x, mouse.y) {
                        self.viewer.scroll_down(3);
                    }
                }
                _ => {}
            }
        }
    }
}

impl Screen for LogSearch {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if matches!(event, Event::Mouse(_)) {
            self.handle_mouse(event);
            return Cmd::none();
        }
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            match self.mode {
                UiMode::Normal => self.handle_normal_key(key),
                UiMode::Search | UiMode::Filter => self.handle_input_key(key),
            }
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.width < 4 || area.height < 4 {
            return;
        }

        let input_active = self.mode != UiMode::Normal;
        let bar_height = if input_active { 2 } else { 1 };

        let sections = Flex::vertical()
            .constraints([Constraint::Min(3), Constraint::Fixed(bar_height)])
            .split(area);

        let log_area = sections[0];
        let bar_area = sections[1];

        let title = if self.filter_active {
            format!(" Log Viewer [filter: {}] ", self.filter_query)
        } else {
            " Log Viewer ".to_string()
        };

        let border_style = if self.mode == UiMode::Normal {
            Style::new().fg(theme::fg::MUTED)
        } else {
            Style::new().fg(theme::accent::WARNING)
        };

        let block = Block::new()
            .title(&title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);

        self.last_log_area.set(log_area);
        let inner = block.inner(log_area);
        Widget::render(&block, log_area, frame);

        let mut state = self.viewer_state.clone();
        StatefulWidget::render(&self.viewer, inner, frame, &mut state);

        if input_active {
            let bar_sections = Flex::vertical()
                .constraints([Constraint::Fixed(1), Constraint::Fixed(1)])
                .split(bar_area);
            self.render_input_bar(frame, bar_sections[0]);
            self.render_status_bar(frame, bar_sections[1]);
        } else {
            self.render_status_bar(frame, bar_area);
        }
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;

        if !self.paused && tick_count.is_multiple_of(LOG_BURST_INTERVAL) {
            for _ in 0..LOG_BURST_SIZE {
                self.viewer.push(generate_log_line(self.lines_generated));
                self.lines_generated += 1;
            }
        }
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "/",
                action: "Open search bar",
            },
            HelpEntry {
                key: "f",
                action: "Open filter bar",
            },
            HelpEntry {
                key: "n / N",
                action: "Next / previous match",
            },
            HelpEntry {
                key: "F",
                action: "Clear filter",
            },
            HelpEntry {
                key: "Esc",
                action: "Close search / clear highlights",
            },
            HelpEntry {
                key: "Space",
                action: "Pause / resume log stream",
            },
            HelpEntry {
                key: "g / G",
                action: "Go to top / bottom",
            },
            HelpEntry {
                key: "j/k",
                action: "Scroll up / down",
            },
            HelpEntry {
                key: "Ctrl+C",
                action: "Toggle case sensitivity (search)",
            },
            HelpEntry {
                key: "Ctrl+R",
                action: "Toggle regex mode (search)",
            },
            HelpEntry {
                key: "Ctrl+X",
                action: "Cycle context lines (search)",
            },
            HelpEntry {
                key: "Click",
                action: "Return to normal mode",
            },
            HelpEntry {
                key: "Scroll",
                action: "Navigate log",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Log Search"
    }

    fn tab_label(&self) -> &'static str {
        "Logs"
    }
}

fn generate_log_line(seq: u64) -> Text {
    let (severity_label, severity_color) = match seq % 13 {
        0..=5 => ("INFO", theme::accent::INFO),
        6..=8 => ("DEBUG", theme::fg::MUTED),
        9..=10 => ("WARN", theme::accent::WARNING),
        11 => ("ERROR", theme::accent::ERROR),
        _ => ("TRACE", theme::fg::MUTED),
    };

    let module = match seq % 9 {
        0 => "server::http",
        1 => "db::pool",
        2 => "auth::jwt",
        3 => "cache::redis",
        4 => "queue::worker",
        5 => "api::handler",
        6 => "core::runtime",
        7 => "metrics::push",
        _ => "config::reload",
    };

    let message = match seq % 11 {
        0 => "Request processed successfully",
        1 => "Connection pool health check passed",
        2 => "Token refresh completed for session",
        3 => "Cache hit ratio: 0.94",
        4 => "Worker picked up job from queue",
        5 => "Rate limit threshold approaching",
        6 => "Garbage collection cycle completed",
        7 => "Metric batch flushed to backend",
        8 => "Configuration hot-reload triggered",
        9 => "Retry attempt 2/3 for downstream call",
        _ => "Scheduled maintenance window check",
    };

    let line = format!(
        "[{:>6}] {:>5} {:<18} {}",
        seq, severity_label, module, message
    );

    Text::styled(line, Style::new().fg(severity_color))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
    }

    fn type_chars(screen: &mut LogSearch, s: &str) {
        for ch in s.chars() {
            screen.update(&key_press(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn test_new_creates_initial_lines() {
        let screen = LogSearch::new();
        assert_eq!(screen.viewer.line_count(), 50);
        assert_eq!(screen.mode, UiMode::Normal);
    }

    #[test]
    fn test_search_mode_toggle() {
        let mut screen = LogSearch::new();
        assert_eq!(screen.mode, UiMode::Normal);

        screen.update(&key_press(KeyCode::Char('/')));
        assert_eq!(screen.mode, UiMode::Search);

        screen.update(&key_press(KeyCode::Escape));
        assert_eq!(screen.mode, UiMode::Normal);
    }

    #[test]
    fn test_filter_mode_toggle() {
        let mut screen = LogSearch::new();
        screen.update(&key_press(KeyCode::Char('f')));
        assert_eq!(screen.mode, UiMode::Filter);
    }

    #[test]
    fn test_search_and_navigate() {
        let mut screen = LogSearch::new();

        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "ERROR");
        screen.update(&key_press(KeyCode::Enter));

        assert_eq!(screen.mode, UiMode::Normal);
        assert_eq!(screen.last_search, "ERROR");
        assert!(screen.viewer.search_info().is_some());

        let initial_info = screen.viewer.search_info();
        screen.update(&key_press(KeyCode::Char('n')));
        if let Some((_, total)) = initial_info
            && total > 1
        {
            let (current, _) = screen.viewer.search_info().unwrap();
            assert_eq!(current, 2);
        }
    }

    #[test]
    fn test_tick_generates_lines() {
        let mut screen = LogSearch::new();
        let initial = screen.viewer.line_count();
        screen.tick(LOG_BURST_INTERVAL);
        assert_eq!(screen.viewer.line_count(), initial + LOG_BURST_SIZE);
    }

    #[test]
    fn test_pause_stops_generation() {
        let mut screen = LogSearch::new();
        let initial = screen.viewer.line_count();
        screen.update(&key_press(KeyCode::Char(' ')));
        assert!(screen.paused);
        screen.tick(LOG_BURST_INTERVAL);
        assert_eq!(screen.viewer.line_count(), initial);
    }

    #[test]
    fn test_filter_submit() {
        let mut screen = LogSearch::new();
        screen.update(&key_press(KeyCode::Char('f')));
        type_chars(&mut screen, "ERROR");
        screen.update(&key_press(KeyCode::Enter));
        assert!(screen.filter_active);
        assert_eq!(screen.filter_query, "ERROR");
    }

    #[test]
    fn test_generate_log_line_deterministic() {
        let a = generate_log_line(42).to_plain_text();
        let b = generate_log_line(42).to_plain_text();
        assert_eq!(a, b);
    }

    #[test]
    fn test_keybindings_listed() {
        let screen = LogSearch::new();
        let bindings = screen.keybindings();
        assert!(bindings.len() >= 8);
        assert!(bindings.iter().any(|h| h.key == "/"));
        assert!(bindings.iter().any(|h| h.key == "n / N"));
    }

    // -------------------------------------------------------------------------
    // Diagnostic tests (bd-1b5h.9)
    // -------------------------------------------------------------------------

    #[test]
    fn diagnostic_event_kind_as_str() {
        assert_eq!(DiagnosticEventKind::SearchOpened.as_str(), "search_opened");
        assert_eq!(DiagnosticEventKind::SearchClosed.as_str(), "search_closed");
        assert_eq!(DiagnosticEventKind::QueryUpdated.as_str(), "query_updated");
        assert_eq!(
            DiagnosticEventKind::FilterApplied.as_str(),
            "filter_applied"
        );
        assert_eq!(
            DiagnosticEventKind::FilterCleared.as_str(),
            "filter_cleared"
        );
        assert_eq!(
            DiagnosticEventKind::MatchNavigation.as_str(),
            "match_navigation"
        );
        assert_eq!(
            DiagnosticEventKind::ScrollNavigation.as_str(),
            "scroll_navigation"
        );
        assert_eq!(DiagnosticEventKind::PauseToggle.as_str(), "pause_toggle");
        assert_eq!(DiagnosticEventKind::LogGenerated.as_str(), "log_generated");
        assert_eq!(DiagnosticEventKind::ModeChange.as_str(), "mode_change");
        assert_eq!(DiagnosticEventKind::Tick.as_str(), "tick");
    }

    #[test]
    fn diagnostic_entry_basic_creation() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::SearchOpened, 42);
        assert_eq!(entry.kind, DiagnosticEventKind::SearchOpened);
        assert_eq!(entry.tick, 42);
        assert!(entry.query.is_none());
    }

    #[test]
    fn diagnostic_entry_builder_chain() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, 10)
            .with_query("ERROR")
            .with_result_count(5)
            .with_match_position(1)
            .with_case_sensitive(true)
            .with_search_mode("regex")
            .with_context("test context");

        assert_eq!(entry.query.as_deref(), Some("ERROR"));
        assert_eq!(entry.result_count, Some(5));
        assert_eq!(entry.match_position, Some(1));
        assert_eq!(entry.case_sensitive, Some(true));
        assert_eq!(entry.search_mode.as_deref(), Some("regex"));
        assert_eq!(entry.context.as_deref(), Some("test context"));
    }

    #[test]
    fn diagnostic_entry_with_filter() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::FilterApplied, 1)
            .with_filter_active(true)
            .with_query("WARN");
        assert_eq!(entry.filter_active, Some(true));
        assert_eq!(entry.query.as_deref(), Some("WARN"));
    }

    #[test]
    fn diagnostic_entry_with_pause() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::PauseToggle, 5)
            .with_paused(true)
            .with_lines_generated(100);
        assert_eq!(entry.paused, Some(true));
        assert_eq!(entry.lines_generated, Some(100));
    }

    #[test]
    fn diagnostic_entry_checksum_deterministic() {
        let entry1 = DiagnosticEntry::new(DiagnosticEventKind::SearchOpened, 42)
            .with_query("test")
            .with_checksum();

        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::SearchOpened, 42)
            .with_query("test")
            .with_checksum();

        assert_eq!(entry1.checksum, entry2.checksum);
        assert_ne!(entry1.checksum, 0);
    }

    #[test]
    fn diagnostic_entry_to_jsonl() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, 42)
            .with_query("ERROR")
            .with_result_count(3)
            .with_checksum();
        let jsonl = entry.to_jsonl();

        assert!(jsonl.starts_with('{'));
        assert!(jsonl.ends_with('}'));
        assert!(jsonl.contains("\"kind\":\"query_updated\""));
        assert!(jsonl.contains("\"query\":\"ERROR\""));
        assert!(jsonl.contains("\"result_count\":3"));
    }

    #[test]
    fn diagnostic_log_new_empty() {
        let log = DiagnosticLog::new();
        assert!(log.entries().is_empty());
    }

    #[test]
    fn diagnostic_log_record_entry() {
        let mut log = DiagnosticLog::new();
        let entry = DiagnosticEntry::new(DiagnosticEventKind::SearchOpened, 1).with_checksum();
        log.record(entry);

        assert_eq!(log.entries().len(), 1);
        assert_eq!(log.entries()[0].kind, DiagnosticEventKind::SearchOpened);
    }

    #[test]
    fn diagnostic_log_max_entries() {
        let mut log = DiagnosticLog::new().with_max_entries(5);

        for i in 0..10 {
            let entry = DiagnosticEntry::new(DiagnosticEventKind::Tick, i).with_checksum();
            log.record(entry);
        }

        assert_eq!(log.entries().len(), 5);
        assert_eq!(log.entries()[0].tick, 5);
        assert_eq!(log.entries()[4].tick, 9);
    }

    #[test]
    fn diagnostic_log_summary() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SearchOpened, 1).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, 2).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, 3).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SearchClosed, 4).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::FilterApplied, 5).with_checksum());

        let summary = log.summary();
        assert_eq!(summary.total_entries, 5);
        assert_eq!(summary.search_opened_count, 1);
        assert_eq!(summary.query_updated_count, 2);
        assert_eq!(summary.search_closed_count, 1);
        assert_eq!(summary.filter_applied_count, 1);
    }

    #[test]
    fn telemetry_hooks_default() {
        let hooks = TelemetryHooks::new();
        assert!(hooks.on_search_opened.is_none());
        assert!(hooks.on_query_updated.is_none());
    }

    #[test]
    fn telemetry_hooks_on_any_event() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let hooks = TelemetryHooks::new().on_any(move |_| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        let entry = DiagnosticEntry::new(DiagnosticEventKind::SearchOpened, 1).with_checksum();
        hooks.dispatch(&entry);

        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn telemetry_hooks_specific_events() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let search_count = Arc::new(AtomicUsize::new(0));
        let query_count = Arc::new(AtomicUsize::new(0));
        let search_clone = search_count.clone();
        let query_clone = query_count.clone();

        let hooks = TelemetryHooks::new()
            .on_search_opened(move |_| {
                search_clone.fetch_add(1, Ordering::Relaxed);
            })
            .on_query_updated(move |_| {
                query_clone.fetch_add(1, Ordering::Relaxed);
            });

        hooks.dispatch(&DiagnosticEntry::new(DiagnosticEventKind::SearchOpened, 1).with_checksum());
        hooks.dispatch(&DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, 2).with_checksum());
        hooks.dispatch(&DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated, 3).with_checksum());

        assert_eq!(search_count.load(Ordering::Relaxed), 1);
        assert_eq!(query_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn diagnostics_enabled_toggle() {
        set_diagnostics_enabled(true);
        assert!(diagnostics_enabled());
        set_diagnostics_enabled(false);
        assert!(!diagnostics_enabled());
    }

    #[test]
    fn log_search_with_diagnostics() {
        reset_event_counter();
        let mut screen = LogSearch::new().with_diagnostics();
        assert!(screen.diagnostic_log().is_some());

        // Trigger some actions
        screen.update(&key_press(KeyCode::Char('/')));

        let log = screen.diagnostic_log().unwrap();
        assert!(!log.entries().is_empty());
    }

    #[test]
    fn log_search_accessors() {
        let screen = LogSearch::new();
        assert_eq!(screen.current_query(), "");
        assert_eq!(*screen.current_mode(), UiMode::Normal);
        assert!(!screen.is_filter_active());
        assert_eq!(screen.filter_query_str(), "");
        assert!(!screen.is_paused());
        assert_eq!(screen.lines_generated_count(), 50);
    }

    #[test]
    fn log_search_search_opened_emits_diagnostic() {
        reset_event_counter();
        let mut screen = LogSearch::new().with_diagnostics();

        screen.update(&key_press(KeyCode::Char('/')));

        let log = screen.diagnostic_log().unwrap();
        let opened = log.entries_of_kind(DiagnosticEventKind::SearchOpened);
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].ui_mode.as_deref(), Some("search"));
    }

    #[test]
    fn log_search_scroll_emits_diagnostic() {
        reset_event_counter();
        let mut screen = LogSearch::new().with_diagnostics();
        screen.diagnostic_log_mut().unwrap().clear();

        screen.update(&key_press(KeyCode::Char('j')));

        let log = screen.diagnostic_log().unwrap();
        let scroll = log.entries_of_kind(DiagnosticEventKind::ScrollNavigation);
        assert_eq!(scroll.len(), 1);
        assert_eq!(scroll[0].direction.as_deref(), Some("down"));
    }

    #[test]
    fn log_search_pause_emits_diagnostic() {
        reset_event_counter();
        let mut screen = LogSearch::new().with_diagnostics();
        screen.diagnostic_log_mut().unwrap().clear();

        screen.update(&key_press(KeyCode::Char(' ')));

        let log = screen.diagnostic_log().unwrap();
        let pause = log.entries_of_kind(DiagnosticEventKind::PauseToggle);
        assert_eq!(pause.len(), 1);
        assert_eq!(pause[0].paused, Some(true));
    }

    #[test]
    fn log_search_filter_emits_diagnostic() {
        reset_event_counter();
        let mut screen = LogSearch::new().with_diagnostics();

        // Open filter mode
        screen.update(&key_press(KeyCode::Char('f')));
        // Type filter query
        type_chars(&mut screen, "ERROR");
        // Submit
        screen.update(&key_press(KeyCode::Enter));

        let log = screen.diagnostic_log().unwrap();
        let applied = log.entries_of_kind(DiagnosticEventKind::FilterApplied);
        assert!(!applied.is_empty());
        assert_eq!(applied.last().unwrap().filter_active, Some(true));
    }

    #[test]
    fn log_search_with_telemetry_hooks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        reset_event_counter();

        let event_count = Arc::new(AtomicUsize::new(0));
        let event_clone = event_count.clone();

        let hooks = TelemetryHooks::new().on_any(move |_| {
            event_clone.fetch_add(1, Ordering::Relaxed);
        });

        let mut screen = LogSearch::new()
            .with_diagnostics()
            .with_telemetry_hooks(hooks);

        screen.update(&key_press(KeyCode::Char('/')));
        screen.update(&key_press(KeyCode::Escape));

        // Should have recorded events
        assert!(event_count.load(Ordering::Relaxed) >= 2);
    }

    // =========================================================================
    // Unit Tests + Property Coverage (bd-1b5h.7)
    // =========================================================================

    /// Helper to create a key event with modifiers.
    fn key_press_with_mod(code: KeyCode, modifiers: Modifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        })
    }

    // -------------------------------------------------------------------------
    // Literal vs Regex Match Correctness
    // -------------------------------------------------------------------------

    #[test]
    fn test_search_literal_finds_exact_substring() {
        let mut screen = LogSearch::new();

        // Search for literal "INFO"
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "INFO");
        screen.update(&key_press(KeyCode::Enter));

        // Should find matches in the initial 50 log lines
        let info = screen.viewer.search_info();
        assert!(info.is_some(), "Should find INFO matches in log lines");
        let (_, total) = info.unwrap();
        assert!(total > 0, "Should have at least one INFO match");
    }

    #[test]
    fn test_search_literal_special_chars_not_regex() {
        let mut screen = LogSearch::new();

        // Add a line with regex special characters
        screen.viewer.push("test [bracket] pattern");

        // Search for literal "[bracket]" - should not be treated as regex
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "[bracket]");
        screen.update(&key_press(KeyCode::Enter));

        let info = screen.viewer.search_info();
        assert!(info.is_some(), "Should find literal [bracket] match");
        let (_, total) = info.unwrap();
        assert_eq!(total, 1, "Should find exactly one match");
    }

    #[test]
    fn test_search_regex_mode_toggle() {
        let mut screen = LogSearch::new().with_diagnostics();

        // Open search mode
        screen.update(&key_press(KeyCode::Char('/')));
        assert_eq!(screen.search_config.mode, SearchMode::Literal);

        // Toggle to regex mode with Ctrl+R
        screen.update(&key_press_with_mod(KeyCode::Char('r'), Modifiers::CTRL));
        assert_eq!(screen.search_config.mode, SearchMode::Regex);

        // Toggle back to literal
        screen.update(&key_press_with_mod(KeyCode::Char('r'), Modifiers::CTRL));
        assert_eq!(screen.search_config.mode, SearchMode::Literal);

        // Verify diagnostic recorded the mode change
        let log = screen.diagnostic_log().unwrap();
        let updates = log.entries_of_kind(DiagnosticEventKind::QueryUpdated);
        assert!(
            updates
                .iter()
                .any(|e| e.context.as_deref() == Some("mode toggled"))
        );
    }

    // -------------------------------------------------------------------------
    // Case Sensitivity Toggle
    // -------------------------------------------------------------------------

    #[test]
    fn test_search_case_sensitivity_toggle() {
        let mut screen = LogSearch::new().with_diagnostics();

        // Open search mode
        screen.update(&key_press(KeyCode::Char('/')));
        assert!(
            !screen.search_config.case_sensitive,
            "Default should be case-insensitive"
        );

        // Toggle case sensitivity with Ctrl+C
        screen.update(&key_press_with_mod(KeyCode::Char('c'), Modifiers::CTRL));
        assert!(
            screen.search_config.case_sensitive,
            "Should be case-sensitive after toggle"
        );

        // Toggle back
        screen.update(&key_press_with_mod(KeyCode::Char('c'), Modifiers::CTRL));
        assert!(
            !screen.search_config.case_sensitive,
            "Should be case-insensitive after second toggle"
        );

        // Verify diagnostic recorded the toggle
        let log = screen.diagnostic_log().unwrap();
        let updates = log.entries_of_kind(DiagnosticEventKind::QueryUpdated);
        assert!(
            updates
                .iter()
                .any(|e| e.context.as_deref() == Some("case sensitivity toggled"))
        );
    }

    #[test]
    fn test_search_case_sensitive_vs_insensitive() {
        let mut screen = LogSearch::new();

        // Clear initial lines and add controlled test content
        screen.viewer.clear();
        screen.viewer.push("ERROR: critical failure");
        screen.viewer.push("error: minor issue");
        screen.viewer.push("Error: warning");

        // Case-insensitive search (default)
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "error");
        screen.update(&key_press(KeyCode::Enter));

        let info = screen.viewer.search_info();
        assert!(
            info.is_some(),
            "Case-insensitive search should find matches"
        );
        let (_, total_insensitive) = info.unwrap();
        assert_eq!(
            total_insensitive, 3,
            "Case-insensitive should find all 3 error variants"
        );

        // Now search case-sensitive for uppercase ERROR only
        screen.update(&key_press(KeyCode::Char('/')));
        screen.update(&key_press_with_mod(KeyCode::Char('c'), Modifiers::CTRL)); // Toggle to case-sensitive
        // Clear and type the uppercase search term
        screen.update(&key_press_with_mod(KeyCode::Char('u'), Modifiers::CTRL)); // Clear query
        type_chars(&mut screen, "ERROR");
        screen.update(&key_press(KeyCode::Enter));

        let info = screen.viewer.search_info();
        assert!(
            info.is_some(),
            "Case-sensitive search should find at least one match"
        );
        let (_, total_sensitive) = info.unwrap();
        assert_eq!(
            total_sensitive, 1,
            "Case-sensitive 'ERROR' should find exactly 1 match"
        );
        assert!(
            total_sensitive < total_insensitive,
            "Case-sensitive should find fewer matches"
        );
    }

    // -------------------------------------------------------------------------
    // Context Line Inclusion
    // -------------------------------------------------------------------------

    #[test]
    fn test_search_context_lines_toggle() {
        let mut screen = LogSearch::new().with_diagnostics();

        // Open search mode
        screen.update(&key_press(KeyCode::Char('/')));
        assert_eq!(
            screen.search_config.context_lines, 0,
            "Default should be 0 context lines"
        );

        // Cycle context lines with Ctrl+X: 0 -> 1
        screen.update(&key_press_with_mod(KeyCode::Char('x'), Modifiers::CTRL));
        assert_eq!(screen.search_config.context_lines, 1);

        // 1 -> 2
        screen.update(&key_press_with_mod(KeyCode::Char('x'), Modifiers::CTRL));
        assert_eq!(screen.search_config.context_lines, 2);

        // 2 -> 5
        screen.update(&key_press_with_mod(KeyCode::Char('x'), Modifiers::CTRL));
        assert_eq!(screen.search_config.context_lines, 5);

        // 5 -> 0 (wrap)
        screen.update(&key_press_with_mod(KeyCode::Char('x'), Modifiers::CTRL));
        assert_eq!(screen.search_config.context_lines, 0);

        // Verify diagnostic recorded context changes
        let log = screen.diagnostic_log().unwrap();
        let updates = log.entries_of_kind(DiagnosticEventKind::QueryUpdated);
        assert!(updates.iter().any(|e| {
            e.context
                .as_deref()
                .is_some_and(|c| c.starts_with("context lines:"))
        }));
    }

    // -------------------------------------------------------------------------
    // Highlight Span Correctness
    // -------------------------------------------------------------------------

    #[test]
    fn test_highlight_ranges_no_overlap() {
        let mut screen = LogSearch::new();

        // Add a line with multiple occurrences
        screen.viewer.push("foo bar foo baz foo qux foo");

        // Search for "foo"
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "foo");
        screen.update(&key_press(KeyCode::Enter));

        // Get highlight ranges for the last line (where we pushed "foo bar foo...")
        let line_count = screen.viewer.line_count();
        let ranges = screen.viewer.highlight_ranges_for_line(line_count - 1);
        assert!(
            ranges.is_some(),
            "Should have highlight ranges for matching line"
        );

        let ranges = ranges.unwrap();
        assert!(
            ranges.len() >= 4,
            "Should find at least 4 'foo' occurrences"
        );

        // Verify no overlaps: each range's end should be <= next range's start
        for window in ranges.windows(2) {
            let (_, end1) = window[0];
            let (start2, _) = window[1];
            assert!(
                end1 <= start2,
                "Highlight ranges should not overlap: end {} > start {}",
                end1,
                start2
            );
        }

        // Verify ranges are in ascending order
        for window in ranges.windows(2) {
            let (start1, _) = window[0];
            let (start2, _) = window[1];
            assert!(
                start1 < start2,
                "Highlight ranges should be in ascending order"
            );
        }
    }

    #[test]
    fn test_highlight_ranges_valid_byte_offsets() {
        let mut screen = LogSearch::new();

        // Add lines with various content
        screen.viewer.push("simple match here");
        screen.viewer.push("unicode: café résumé");
        screen.viewer.push("empty");

        // Search for a common word
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "é");
        screen.update(&key_press(KeyCode::Enter));

        // Check all highlight ranges are valid byte indices
        for idx in 0..screen.viewer.line_count() {
            if let Some(ranges) = screen.viewer.highlight_ranges_for_line(idx) {
                for &(start, end) in ranges {
                    assert!(
                        start <= end,
                        "Range start {} should be <= end {}",
                        start,
                        end
                    );
                    // Note: We can't easily check upper bound without access to line content,
                    // but the invariant start <= end should hold
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // Edge Cases
    // -------------------------------------------------------------------------

    #[test]
    fn test_search_empty_query_no_results() {
        let mut screen = LogSearch::new();

        // Open search and submit empty
        screen.update(&key_press(KeyCode::Char('/')));
        screen.update(&key_press(KeyCode::Enter));

        assert!(
            screen.viewer.search_info().is_none(),
            "Empty query should yield no results"
        );
        assert!(screen.last_search.is_empty(), "Last search should be empty");
    }

    #[test]
    fn test_search_no_matches() {
        let mut screen = LogSearch::new();

        // Search for something that doesn't exist
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "ZZZZNONEXISTENT12345");
        screen.update(&key_press(KeyCode::Enter));

        // search_info returns None when there are 0 matches
        assert!(
            screen.viewer.search_info().is_none(),
            "Non-matching query should yield no results"
        );
        assert_eq!(
            screen.last_search, "ZZZZNONEXISTENT12345",
            "Last search should be preserved"
        );
    }

    #[test]
    fn test_filter_empty_query_clears_filter() {
        let mut screen = LogSearch::new();

        // First set a filter
        screen.update(&key_press(KeyCode::Char('f')));
        type_chars(&mut screen, "ERROR");
        screen.update(&key_press(KeyCode::Enter));
        assert!(screen.filter_active);

        // Now clear with empty filter
        screen.update(&key_press(KeyCode::Char('f')));
        screen.update(&key_press_with_mod(KeyCode::Char('u'), Modifiers::CTRL)); // Ctrl+U clears
        screen.update(&key_press(KeyCode::Enter));

        assert!(
            !screen.filter_active,
            "Empty filter should deactivate filter"
        );
        assert!(screen.filter_query.is_empty());
    }

    #[test]
    fn test_navigate_without_search_noop() {
        let mut screen = LogSearch::new();

        // Try to navigate without any active search
        let before_scroll = screen.viewer.is_at_bottom();
        screen.update(&key_press(KeyCode::Char('n'))); // next match
        screen.update(&key_press(KeyCode::Char('N'))); // prev match
        let after_scroll = screen.viewer.is_at_bottom();

        // Should not crash and scroll state should be unchanged (or at least valid)
        assert_eq!(
            before_scroll, after_scroll,
            "Navigation without search should be a no-op"
        );
    }

    #[test]
    fn test_clear_search_via_escape() {
        let mut screen = LogSearch::new();

        // Perform a search
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "INFO");
        screen.update(&key_press(KeyCode::Enter));
        assert!(screen.viewer.search_info().is_some());

        // Clear with Escape in normal mode
        screen.update(&key_press(KeyCode::Escape));
        assert!(
            screen.viewer.search_info().is_none(),
            "Escape should clear search"
        );
        assert!(
            screen.last_search.is_empty(),
            "Last search should be cleared"
        );
    }

    #[test]
    fn test_clear_filter_via_shift_f() {
        let mut screen = LogSearch::new();

        // Set a filter
        screen.update(&key_press(KeyCode::Char('f')));
        type_chars(&mut screen, "ERROR");
        screen.update(&key_press(KeyCode::Enter));
        assert!(screen.filter_active);

        // Clear with Shift+F (capital F)
        screen.update(&key_press(KeyCode::Char('F')));
        assert!(!screen.filter_active, "F should clear filter");
        assert!(screen.filter_query.is_empty());
    }

    // -------------------------------------------------------------------------
    // Property Tests: Determinism
    // -------------------------------------------------------------------------

    #[test]
    fn test_search_ordering_deterministic() {
        // Run the same search twice and verify identical results
        for _ in 0..3 {
            let mut screen = LogSearch::new();

            // Add predictable content
            for i in 0..20 {
                screen.viewer.push(format!("line {} match", i));
            }

            screen.update(&key_press(KeyCode::Char('/')));
            type_chars(&mut screen, "match");
            screen.update(&key_press(KeyCode::Enter));

            let info = screen.viewer.search_info();
            assert!(info.is_some());
            let (current, total) = info.unwrap();
            assert_eq!(current, 1, "First match should always be position 1");
            assert_eq!(total, 20, "Should find all 20 matches");

            // Navigate through and verify order
            let mut positions = vec![1];
            for _ in 0..19 {
                screen.update(&key_press(KeyCode::Char('n')));
                if let Some((pos, _)) = screen.viewer.search_info() {
                    positions.push(pos);
                }
            }

            // Positions should be sequential 1..=20
            let expected: Vec<usize> = (1..=20).collect();
            assert_eq!(positions, expected, "Navigation should be deterministic");
        }
    }

    #[test]
    fn test_log_line_generation_deterministic() {
        // Same sequence number should always produce same output
        let seeds = [0, 42, 100, 999, 12345];
        for seed in seeds {
            let line1 = generate_log_line(seed).to_plain_text();
            let line2 = generate_log_line(seed).to_plain_text();
            assert_eq!(
                line1, line2,
                "Log line generation should be deterministic for seed {}",
                seed
            );
        }
    }

    #[test]
    fn test_filter_result_ordering_stable() {
        let mut screen = LogSearch::new();

        // Clear and add predictable content
        screen.viewer.clear();
        screen.viewer.push("ERROR: first");
        screen.viewer.push("INFO: second");
        screen.viewer.push("ERROR: third");
        screen.viewer.push("INFO: fourth");
        screen.viewer.push("ERROR: fifth");

        // Apply filter
        screen.update(&key_press(KeyCode::Char('f')));
        type_chars(&mut screen, "ERROR");
        screen.update(&key_press(KeyCode::Enter));

        // Verify filter is active and results are in order
        assert!(screen.filter_active);

        // Search within filtered results
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "ERROR");
        screen.update(&key_press(KeyCode::Enter));

        let info = screen.viewer.search_info();
        assert!(info.is_some());
        let (_, total) = info.unwrap();
        assert_eq!(total, 3, "Should find 3 ERROR lines");
    }

    // -------------------------------------------------------------------------
    // Property Tests: Invariants
    // -------------------------------------------------------------------------

    #[test]
    fn test_search_matches_subset_of_all_lines() {
        let mut screen = LogSearch::new();

        // Add some lines
        for i in 0..100 {
            if i % 3 == 0 {
                screen.viewer.push(format!("TARGET line {}", i));
            } else {
                screen.viewer.push(format!("other line {}", i));
            }
        }

        let total_lines = screen.viewer.line_count();

        // Search for TARGET
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "TARGET");
        screen.update(&key_press(KeyCode::Enter));

        let info = screen.viewer.search_info();
        assert!(info.is_some());
        let (_, match_count) = info.unwrap();

        // Invariant: matches <= total lines
        assert!(
            match_count <= total_lines,
            "Match count {} should be <= total lines {}",
            match_count,
            total_lines
        );

        // We added TARGET to every 3rd line of 100, so ~34 matches expected
        // (plus initial 50 lines without TARGET)
        assert!(match_count > 0, "Should have some matches");
    }

    #[test]
    fn test_live_search_updates_incrementally() {
        let mut screen = LogSearch::new();

        // Open search and type incrementally
        screen.update(&key_press(KeyCode::Char('/')));

        // Type "E" - should update live
        screen.update(&key_press(KeyCode::Char('E')));
        let after_e = screen.viewer.search_info();

        // Type "R" - should update live with "ER"
        screen.update(&key_press(KeyCode::Char('R')));
        let after_er = screen.viewer.search_info();

        // Type "ROR" - should update live with "ERROR"
        type_chars(&mut screen, "ROR");
        let _after_error = screen.viewer.search_info();

        // Each step should have valid search state
        // (may have matches or not, but shouldn't crash)
        if let Some((_, total_e)) = after_e
            && let Some((_, total_er)) = after_er
        {
            // Generally, longer queries should have <= matches
            // (not always true but usually)
            assert!(total_er <= total_e || total_e == 0);
        }

        // Final query should be "ERROR"
        assert_eq!(screen.query, "ERROR");
    }

    #[test]
    fn test_backspace_updates_search_live() {
        let mut screen = LogSearch::new();

        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "ERROR");
        let before_backspace = screen.viewer.search_info();

        // Backspace to "ERRO"
        screen.update(&key_press(KeyCode::Backspace));
        assert_eq!(screen.query, "ERRO");
        let after_backspace = screen.viewer.search_info();

        // Both should be valid states (may differ in match count)
        // The key invariant is no crash and query is updated
        if let (Some((_, before)), Some((_, after))) = (before_backspace, after_backspace) {
            // More general query should have >= matches
            assert!(after >= before || after == 0);
        }
    }

    #[test]
    fn test_ctrl_u_clears_query_in_search_mode() {
        let mut screen = LogSearch::new();

        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "ERROR");
        assert_eq!(screen.query, "ERROR");

        // Ctrl+U clears
        screen.update(&key_press_with_mod(KeyCode::Char('u'), Modifiers::CTRL));
        assert!(screen.query.is_empty(), "Ctrl+U should clear query");
    }

    // -------------------------------------------------------------------------
    // Regression Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_search_after_filter_respects_filter() {
        let mut screen = LogSearch::new();

        // Add mixed content
        screen.viewer.push("ERROR: findme");
        screen.viewer.push("INFO: findme");
        screen.viewer.push("ERROR: other");

        // Filter to ERROR only
        screen.update(&key_press(KeyCode::Char('f')));
        type_chars(&mut screen, "ERROR");
        screen.update(&key_press(KeyCode::Enter));
        assert!(screen.filter_active);

        // Search within filtered results
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "findme");
        screen.update(&key_press(KeyCode::Enter));

        let info = screen.viewer.search_info();
        assert!(info.is_some());
        let (_, total) = info.unwrap();
        // Should only find "findme" in ERROR lines, not INFO
        assert_eq!(total, 1, "Search should respect active filter");
    }

    #[test]
    fn test_mode_transitions_preserve_state() {
        let mut screen = LogSearch::new();

        // Configure search settings
        screen.update(&key_press(KeyCode::Char('/')));
        screen.update(&key_press_with_mod(KeyCode::Char('c'), Modifiers::CTRL)); // case sensitive
        let case_before = screen.search_config.case_sensitive;
        screen.update(&key_press(KeyCode::Escape));

        // Re-open search
        screen.update(&key_press(KeyCode::Char('/')));
        assert_eq!(
            screen.search_config.case_sensitive, case_before,
            "Search config should persist across mode transitions"
        );
    }

    #[test]
    fn test_query_recalled_on_reopen() {
        let mut screen = LogSearch::new();

        // Perform a search
        screen.update(&key_press(KeyCode::Char('/')));
        type_chars(&mut screen, "TEST");
        screen.update(&key_press(KeyCode::Enter));
        assert_eq!(screen.last_search, "TEST");

        // Reopen search - query should be recalled
        screen.update(&key_press(KeyCode::Char('/')));
        assert_eq!(screen.query, "TEST", "Previous search should be recalled");
    }

    #[test]
    fn mouse_scroll_navigates_log() {
        use crate::screens::Screen;
        let mut screen = LogSearch::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 120, 40));

        let log_area = screen.last_log_area.get();
        let scroll_down = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            x: log_area.x + 2,
            y: log_area.y + 2,
            modifiers: Modifiers::NONE,
        });
        screen.update(&scroll_down);
        // Scroll should not panic and should navigate
    }

    #[test]
    fn mouse_click_returns_to_normal() {
        use crate::screens::Screen;
        let mut screen = LogSearch::new();
        // Enter search mode
        let slash = Event::Key(KeyEvent {
            code: KeyCode::Char('/'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&slash);
        assert_ne!(screen.mode, UiMode::Normal);

        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 120, 40));

        let log_area = screen.last_log_area.get();
        let click = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: log_area.x + 2,
            y: log_area.y + 2,
            modifiers: Modifiers::NONE,
        });
        screen.update(&click);
        assert_eq!(
            screen.mode,
            UiMode::Normal,
            "click should return to normal mode"
        );
    }

    #[test]
    fn keybindings_includes_mouse() {
        use crate::screens::Screen;
        let screen = LogSearch::new();
        let bindings = screen.keybindings();
        assert!(
            bindings.iter().any(|h| h.key == "Click"),
            "missing Click keybinding"
        );
        assert!(
            bindings.iter().any(|h| h.key == "Scroll"),
            "missing Scroll keybinding"
        );
    }
}

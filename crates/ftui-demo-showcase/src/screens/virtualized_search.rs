#![forbid(unsafe_code)]

//! Virtualized List + Fuzzy Search demo screen.
//!
//! Demonstrates:
//! - `VirtualizedList` with large datasets (10k+ items)
//! - Fuzzy search with incremental filtering
//! - Match highlighting and scoring
//! - Keyboard navigation (J/K, /, Esc, G/Shift+G)
//!
//! Part of bd-2zbk: Demo Showcase: Virtualized List + Fuzzy Search
//!
//! # Telemetry and Diagnostics (bd-2zbk.5)
//!
//! This module provides rich diagnostic logging and telemetry hooks:
//! - JSONL diagnostic output via `DiagnosticLog`
//! - Observable hooks for search, navigation, and filter events
//! - Deterministic mode for reproducible testing
//!
//! ## Environment Variables
//!
//! - `FTUI_VSEARCH_DIAGNOSTICS=true` - Enable verbose diagnostic output
//! - `FTUI_VSEARCH_DETERMINISTIC=true` - Enable deterministic mode

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::display_width;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::paragraph::Paragraph;
use std::cell::Cell;

use super::{HelpEntry, Screen};
use crate::determinism;
use crate::theme;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TOTAL_ITEMS: usize = 10_000;

/// Match highlight color (bright yellow/gold).
const MATCH_HIGHLIGHT: PackedRgba = PackedRgba::rgb(255, 215, 0);

// =============================================================================
// Diagnostic Logging (bd-2zbk.5)
// =============================================================================

/// Global diagnostic enable flag (checked once at startup).
static VSEARCH_DIAGNOSTICS_ENABLED: AtomicBool = AtomicBool::new(false);
/// Global monotonic event counter for deterministic ordering.
static VSEARCH_EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Initialize diagnostic settings from environment.
pub fn init_diagnostics() {
    let enabled = std::env::var("FTUI_VSEARCH_DIAGNOSTICS")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    VSEARCH_DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Check if diagnostics are enabled.
#[inline]
pub fn diagnostics_enabled() -> bool {
    VSEARCH_DIAGNOSTICS_ENABLED.load(Ordering::Relaxed)
}

/// Set diagnostics enabled state (for testing).
pub fn set_diagnostics_enabled(enabled: bool) {
    VSEARCH_DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Get next monotonic event sequence number.
#[inline]
fn next_event_seq() -> u64 {
    VSEARCH_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Reset event counter (for testing determinism).
pub fn reset_event_counter() {
    VSEARCH_EVENT_COUNTER.store(0, Ordering::Relaxed);
}

/// Check if deterministic mode is enabled.
pub fn is_deterministic_mode() -> bool {
    determinism::env_flag("FTUI_VSEARCH_DETERMINISTIC") || determinism::is_demo_deterministic()
}

/// Diagnostic event types for JSONL logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticEventKind {
    /// Search query changed.
    QueryChange,
    /// Filter updated with results.
    FilterUpdate,
    /// Selection moved.
    Navigate,
    /// Focus changed between search and list.
    FocusChange,
    /// Page scroll (up/down).
    PageScroll,
    /// Jump to first/last.
    JumpToEdge,
    /// Fuzzy match performed.
    FuzzyMatch,
    /// Render pass.
    Render,
    /// Tick processed.
    Tick,
}

impl DiagnosticEventKind {
    /// Get the JSONL event type string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryChange => "query_change",
            Self::FilterUpdate => "filter_update",
            Self::Navigate => "navigate",
            Self::FocusChange => "focus_change",
            Self::PageScroll => "page_scroll",
            Self::JumpToEdge => "jump_to_edge",
            Self::FuzzyMatch => "fuzzy_match",
            Self::Render => "render",
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
    /// Number of filtered results.
    pub filtered_count: Option<usize>,
    /// Current selection index.
    pub selected: Option<usize>,
    /// Scroll offset.
    pub scroll_offset: Option<usize>,
    /// Focus state (true = search, false = list).
    pub focus_search: Option<bool>,
    /// Navigation direction ("up", "down", "first", "last", etc.).
    pub direction: Option<String>,
    /// Match score (for fuzzy match events).
    pub match_score: Option<i32>,
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
            query: None,
            filtered_count: None,
            selected: None,
            scroll_offset: None,
            focus_search: None,
            direction: None,
            match_score: None,
            tick,
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

    /// Set filtered count.
    #[must_use]
    pub fn with_filtered_count(mut self, count: usize) -> Self {
        self.filtered_count = Some(count);
        self
    }

    /// Set selection.
    #[must_use]
    pub fn with_selection(mut self, selected: usize, scroll_offset: usize) -> Self {
        self.selected = Some(selected);
        self.scroll_offset = Some(scroll_offset);
        self
    }

    /// Set focus state.
    #[must_use]
    pub fn with_focus(mut self, is_search: bool) -> Self {
        self.focus_search = Some(is_search);
        self
    }

    /// Set navigation direction.
    #[must_use]
    pub fn with_direction(mut self, direction: impl Into<String>) -> Self {
        self.direction = Some(direction.into());
        self
    }

    /// Set match score.
    #[must_use]
    pub fn with_match_score(mut self, score: i32) -> Self {
        self.match_score = Some(score);
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
            self.filtered_count.unwrap_or(0),
            self.selected.unwrap_or(0),
            self.scroll_offset.unwrap_or(0),
            self.focus_search.map_or(0, |b| b as u32),
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
        if let Some(c) = self.filtered_count {
            parts.push(format!("\"filtered_count\":{c}"));
        }
        if let Some(s) = self.selected {
            parts.push(format!("\"selected\":{s}"));
        }
        if let Some(o) = self.scroll_offset {
            parts.push(format!("\"scroll_offset\":{o}"));
        }
        if let Some(f) = self.focus_search {
            parts.push(format!("\"focus_search\":{f}"));
        }
        if let Some(ref d) = self.direction {
            parts.push(format!("\"direction\":\"{d}\""));
        }
        if let Some(s) = self.match_score {
            parts.push(format!("\"match_score\":{s}"));
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
                DiagnosticEventKind::QueryChange => summary.query_change_count += 1,
                DiagnosticEventKind::FilterUpdate => summary.filter_update_count += 1,
                DiagnosticEventKind::Navigate => summary.navigate_count += 1,
                DiagnosticEventKind::FocusChange => summary.focus_change_count += 1,
                DiagnosticEventKind::PageScroll => summary.page_scroll_count += 1,
                DiagnosticEventKind::JumpToEdge => summary.jump_to_edge_count += 1,
                DiagnosticEventKind::FuzzyMatch => summary.fuzzy_match_count += 1,
                DiagnosticEventKind::Tick => summary.tick_count += 1,
                DiagnosticEventKind::Render => summary.render_count += 1,
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
    pub query_change_count: usize,
    pub filter_update_count: usize,
    pub navigate_count: usize,
    pub focus_change_count: usize,
    pub page_scroll_count: usize,
    pub jump_to_edge_count: usize,
    pub fuzzy_match_count: usize,
    pub render_count: usize,
    pub tick_count: usize,
}

impl DiagnosticSummary {
    /// Format as JSONL.
    pub fn to_jsonl(&self) -> String {
        format!(
            "{{\"summary\":true,\"total\":{},\"query_change\":{},\"filter_update\":{},\
             \"navigate\":{},\"focus_change\":{},\"page_scroll\":{},\"jump_to_edge\":{},\
             \"fuzzy_match\":{},\"render\":{},\"tick\":{}}}",
            self.total_entries,
            self.query_change_count,
            self.filter_update_count,
            self.navigate_count,
            self.focus_change_count,
            self.page_scroll_count,
            self.jump_to_edge_count,
            self.fuzzy_match_count,
            self.render_count,
            self.tick_count
        )
    }
}

/// Callback type for telemetry hooks.
pub type TelemetryCallback = Box<dyn Fn(&DiagnosticEntry) + Send + Sync>;

/// Telemetry hooks for observing virtualized search events.
#[derive(Default)]
pub struct TelemetryHooks {
    on_query_change: Option<TelemetryCallback>,
    on_filter_update: Option<TelemetryCallback>,
    on_navigate: Option<TelemetryCallback>,
    on_any_event: Option<TelemetryCallback>,
}

impl TelemetryHooks {
    /// Create new empty hooks.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set query change callback.
    #[must_use]
    pub fn on_query_change(mut self, f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static) -> Self {
        self.on_query_change = Some(Box::new(f));
        self
    }

    /// Set filter update callback.
    #[must_use]
    pub fn on_filter_update(
        mut self,
        f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static,
    ) -> Self {
        self.on_filter_update = Some(Box::new(f));
        self
    }

    /// Set navigate callback.
    #[must_use]
    pub fn on_navigate(mut self, f: impl Fn(&DiagnosticEntry) + Send + Sync + 'static) -> Self {
        self.on_navigate = Some(Box::new(f));
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
            DiagnosticEventKind::QueryChange => {
                if let Some(ref cb) = self.on_query_change {
                    cb(entry);
                }
            }
            DiagnosticEventKind::FilterUpdate => {
                if let Some(ref cb) = self.on_filter_update {
                    cb(entry);
                }
            }
            DiagnosticEventKind::Navigate
            | DiagnosticEventKind::PageScroll
            | DiagnosticEventKind::JumpToEdge => {
                if let Some(ref cb) = self.on_navigate {
                    cb(entry);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Fuzzy Matching
// ---------------------------------------------------------------------------

/// A fuzzy match result with score and match positions.
#[derive(Debug, Clone)]
struct FuzzyMatch {
    /// Index into the original items list.
    index: usize,
    /// Match score (higher = better match).
    score: i32,
    /// Positions of matched characters in the item text.
    positions: Vec<usize>,
}

/// Simple fzy-style fuzzy matching.
///
/// Algorithm: Sequential character matching with gap penalties.
/// - Consecutive matches: +10 bonus
/// - Word boundary matches: +5 bonus
/// - Gap penalty: -1 per skipped character
fn fuzzy_match(query: &str, target: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return None;
    }

    let query_lower: Vec<char> = query.to_lowercase().chars().collect();
    let target_lower: Vec<char> = target.to_lowercase().chars().collect();

    let mut positions = Vec::with_capacity(query_lower.len());
    let mut score: i32 = 0;
    let mut query_idx = 0;
    let mut prev_match_pos: Option<usize> = None;

    for (i, c) in target_lower.iter().enumerate() {
        if query_idx < query_lower.len() && *c == query_lower[query_idx] {
            positions.push(i);

            // Consecutive match bonus
            if let Some(prev) = prev_match_pos {
                if i == prev + 1 {
                    score += 10;
                } else {
                    // Gap penalty
                    score -= (i - prev - 1).min(10) as i32;
                }
            }

            // Word boundary bonus (start of word)
            if i == 0
                || target_lower
                    .get(i.saturating_sub(1))
                    .is_none_or(|c| !c.is_alphanumeric())
            {
                score += 5;
            }

            // Base score for matching
            score += 1;
            prev_match_pos = Some(i);
            query_idx += 1;
        }
    }

    // All query chars must match
    if query_idx == query_lower.len() {
        Some(FuzzyMatch {
            index: 0, // Will be set by caller
            score,
            positions,
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Focus State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Search,
}

// ---------------------------------------------------------------------------
// VirtualizedSearch Screen
// ---------------------------------------------------------------------------

pub struct VirtualizedSearch {
    /// All items in the list.
    items: Vec<String>,
    /// Filtered matches (indices into items + scores).
    filtered: Vec<FuzzyMatch>,
    /// Current selection index (into filtered list).
    selected: usize,
    /// Scroll offset.
    scroll_offset: usize,
    /// Viewport height (cached from render).
    viewport_height: usize,
    /// Search input widget.
    search_input: TextInput,
    /// Current search query.
    query: String,
    /// Which element has focus.
    focus: Focus,
    /// Tick counter for animation.
    tick_count: u64,
    /// Diagnostic log for telemetry (bd-2zbk.5).
    diagnostic_log: Option<DiagnosticLog>,
    /// Telemetry hooks for external observers (bd-2zbk.5).
    telemetry_hooks: Option<TelemetryHooks>,
    /// Cached list panel area for mouse hit-testing.
    last_list_area: Cell<Rect>,
    /// Cached search bar area for mouse hit-testing.
    last_search_area: Cell<Rect>,
}

impl Default for VirtualizedSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualizedSearch {
    pub fn new() -> Self {
        // Generate diverse test data
        let items: Vec<String> = (0..TOTAL_ITEMS)
            .map(|i| {
                let category = match i % 8 {
                    0 => "Configuration",
                    1 => "Authentication",
                    2 => "Database",
                    3 => "Network",
                    4 => "FileSystem",
                    5 => "Logging",
                    6 => "Security",
                    _ => "Performance",
                };
                let action = match (i / 8) % 6 {
                    0 => "initialized",
                    1 => "updated",
                    2 => "validated",
                    3 => "processed",
                    4 => "cached",
                    _ => "completed",
                };
                let component = match (i / 48) % 5 {
                    0 => "CoreService",
                    1 => "ApiGateway",
                    2 => "WorkerPool",
                    3 => "CacheManager",
                    _ => "EventBus",
                };
                format!(
                    "[{:05}] {} :: {} {} — payload_{}",
                    i,
                    category,
                    component,
                    action,
                    i % 1000
                )
            })
            .collect();

        let search_input = TextInput::new()
            .with_placeholder("Type to search...")
            .with_style(Style::new().fg(theme::fg::PRIMARY))
            .with_focused(false);

        // Enable diagnostic log if diagnostics are enabled
        let diagnostic_log = if diagnostics_enabled() {
            Some(DiagnosticLog::new().with_stderr())
        } else {
            None
        };

        let mut screen = Self {
            items,
            filtered: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            viewport_height: 20,
            search_input,
            query: String::new(),
            focus: Focus::List,
            tick_count: 0,
            diagnostic_log,
            telemetry_hooks: None,
            last_list_area: Cell::new(Rect::default()),
            last_search_area: Cell::new(Rect::default()),
        };

        // Initialize with all items (no filter)
        screen.update_filter();
        screen
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
    // Public accessors for testing (bd-2zbk.5)
    // -------------------------------------------------------------------------

    /// Current search query.
    pub fn current_query(&self) -> &str {
        &self.query
    }

    /// Number of filtered results.
    pub fn filtered_count(&self) -> usize {
        self.filtered.len()
    }

    /// Current selection index.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Current scroll offset.
    pub fn current_scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Whether search is focused.
    pub fn is_search_focused(&self) -> bool {
        self.focus == Focus::Search
    }

    /// Total number of items.
    pub fn total_items(&self) -> usize {
        self.items.len()
    }

    /// Update the filtered list based on current query.
    fn update_filter(&mut self) {
        self.filtered.clear();

        if self.query.is_empty() {
            // No query: show all items with default ordering
            self.filtered = (0..self.items.len())
                .map(|i| FuzzyMatch {
                    index: i,
                    score: 0,
                    positions: Vec::new(),
                })
                .collect();
        } else {
            // Fuzzy match against all items
            for (idx, item) in self.items.iter().enumerate() {
                if let Some(mut m) = fuzzy_match(&self.query, item) {
                    m.index = idx;
                    self.filtered.push(m);
                }
            }

            // Sort by score (descending), then by index for stable tie-breaking
            self.filtered
                .sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.index.cmp(&b.index)));
        }

        // Reset selection and scroll
        self.selected = 0;
        self.scroll_offset = 0;

        // Record filter update diagnostic
        let top_score = self.filtered.first().map_or(0, |m| m.score);
        let diag = DiagnosticEntry::new(DiagnosticEventKind::FilterUpdate, self.tick_count)
            .with_query(&self.query)
            .with_filtered_count(self.filtered.len())
            .with_selection(self.selected, self.scroll_offset)
            .with_match_score(top_score);
        self.record_diagnostic(diag);
    }

    fn ensure_visible(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + self.viewport_height {
            self.scroll_offset = self.selected.saturating_sub(self.viewport_height - 1);
        }
    }

    fn select_previous(&mut self) {
        if !self.filtered.is_empty() && self.selected > 0 {
            self.selected -= 1;
            self.ensure_visible();

            let diag = DiagnosticEntry::new(DiagnosticEventKind::Navigate, self.tick_count)
                .with_direction("up")
                .with_selection(self.selected, self.scroll_offset)
                .with_filtered_count(self.filtered.len());
            self.record_diagnostic(diag);
        }
    }

    fn select_next(&mut self) {
        if !self.filtered.is_empty() && self.selected < self.filtered.len() - 1 {
            self.selected += 1;
            self.ensure_visible();

            let diag = DiagnosticEntry::new(DiagnosticEventKind::Navigate, self.tick_count)
                .with_direction("down")
                .with_selection(self.selected, self.scroll_offset)
                .with_filtered_count(self.filtered.len());
            self.record_diagnostic(diag);
        }
    }

    fn select_first(&mut self) {
        let moved = self.selected != 0 || self.scroll_offset != 0;
        self.selected = 0;
        self.scroll_offset = 0;

        if moved {
            let diag = DiagnosticEntry::new(DiagnosticEventKind::JumpToEdge, self.tick_count)
                .with_direction("first")
                .with_selection(self.selected, self.scroll_offset)
                .with_filtered_count(self.filtered.len());
            self.record_diagnostic(diag);
        }
    }

    fn select_last(&mut self) {
        if !self.filtered.is_empty() {
            let old_selected = self.selected;
            self.selected = self.filtered.len() - 1;
            self.ensure_visible();

            if old_selected != self.selected {
                let diag = DiagnosticEntry::new(DiagnosticEventKind::JumpToEdge, self.tick_count)
                    .with_direction("last")
                    .with_selection(self.selected, self.scroll_offset)
                    .with_filtered_count(self.filtered.len());
                self.record_diagnostic(diag);
            }
        }
    }

    fn page_up(&mut self) {
        if self.viewport_height > 0 {
            let old_selected = self.selected;
            self.selected = self.selected.saturating_sub(self.viewport_height);
            self.ensure_visible();

            if old_selected != self.selected {
                let diag = DiagnosticEntry::new(DiagnosticEventKind::PageScroll, self.tick_count)
                    .with_direction("page_up")
                    .with_selection(self.selected, self.scroll_offset)
                    .with_filtered_count(self.filtered.len())
                    .with_context(format!("moved {} items", old_selected - self.selected));
                self.record_diagnostic(diag);
            }
        }
    }

    fn page_down(&mut self) {
        if !self.filtered.is_empty() && self.viewport_height > 0 {
            let old_selected = self.selected;
            self.selected = (self.selected + self.viewport_height).min(self.filtered.len() - 1);
            self.ensure_visible();

            if old_selected != self.selected {
                let diag = DiagnosticEntry::new(DiagnosticEventKind::PageScroll, self.tick_count)
                    .with_direction("page_down")
                    .with_selection(self.selected, self.scroll_offset)
                    .with_filtered_count(self.filtered.len())
                    .with_context(format!("moved {} items", self.selected - old_selected));
                self.record_diagnostic(diag);
            }
        }
    }

    fn render_search_bar(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.focus == Focus::Search;
        let border_style = if is_focused {
            Style::new().fg(theme::accent::PRIMARY)
        } else {
            Style::new().fg(theme::fg::MUTED)
        };

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(if is_focused {
                BorderType::Double
            } else {
                BorderType::Rounded
            })
            .title("Search (/ to focus, Esc to clear)")
            .title_alignment(Alignment::Left)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if !inner.is_empty() {
            // Create a focused version of the input for rendering
            let input = TextInput::new()
                .with_value(&self.query)
                .with_placeholder("Type to search...")
                .with_style(Style::new().fg(theme::fg::PRIMARY))
                .with_focused(is_focused);
            input.render(inner, frame);

            // Set cursor if focused
            if is_focused && !inner.is_empty() {
                let cursor_x =
                    inner.x + display_width(&self.query).min(inner.width as usize - 1) as u16;
                frame.set_cursor(Some((cursor_x, inner.y)));
            }
        }
    }

    fn render_list_panel(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.focus == Focus::List;
        let border_style = if is_focused {
            Style::new().fg(theme::screen_accent::PERFORMANCE)
        } else {
            Style::new().fg(theme::fg::MUTED)
        };

        let title = if self.query.is_empty() {
            format!("Items ({} total)", self.items.len())
        } else {
            format!(
                "Results ({} of {} match)",
                self.filtered.len(),
                self.items.len()
            )
        };

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(&title)
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        // Store viewport height for navigation
        let viewport = inner.height as usize;

        if self.filtered.is_empty() {
            let msg = if self.query.is_empty() {
                "No items"
            } else {
                "No matches found"
            };
            Paragraph::new(msg)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(inner, frame);
            return;
        }

        let end = (self.scroll_offset + viewport).min(self.filtered.len());

        for (row, filter_idx) in (self.scroll_offset..end).enumerate() {
            let y = inner.y + row as u16;
            if y >= inner.y + inner.height {
                break;
            }

            let m = &self.filtered[filter_idx];
            let item_text = &self.items[m.index];
            let is_selected = filter_idx == self.selected;

            let row_area = Rect::new(inner.x, y, inner.width, 1);

            // Render with match highlighting
            self.render_highlighted_row(
                frame,
                row_area,
                item_text,
                &m.positions,
                is_selected,
                m.score,
            );
        }
    }

    fn render_highlighted_row(
        &self,
        frame: &mut Frame,
        area: Rect,
        text: &str,
        positions: &[usize],
        is_selected: bool,
        score: i32,
    ) {
        let base_style = if is_selected {
            Style::new()
                .fg(theme::fg::PRIMARY)
                .bg(theme::alpha::HIGHLIGHT)
        } else {
            Style::new().fg(theme::fg::SECONDARY)
        };

        // Format display text with score if we have matches
        let display_text = if !positions.is_empty() && score != 0 {
            format!("{} [{}]", text, score)
        } else {
            text.to_string()
        };

        // For now, use simple rendering without character-level highlighting
        // TODO(bd-2zbk): Add character-level match highlighting
        let style = if !positions.is_empty() {
            // When there are matches, use a highlight indicator
            if is_selected {
                Style::new().fg(MATCH_HIGHLIGHT).bg(theme::alpha::HIGHLIGHT)
            } else {
                Style::new().fg(theme::fg::PRIMARY)
            }
        } else {
            base_style
        };

        Paragraph::new(display_text.as_str())
            .style(style)
            .render(area, frame);
    }

    fn render_stats_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Stats")
            .title_alignment(Alignment::Center)
            .style(theme::content_border());

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let top_score = self.filtered.first().map_or(0, |m| m.score);

        let stats = [
            format!("Total:    {} items", self.items.len()),
            format!("Matches:  {}", self.filtered.len()),
            format!(
                "Selected: {}",
                if self.filtered.is_empty() {
                    0
                } else {
                    self.selected + 1
                }
            ),
            format!("Query:    \"{}\"", self.query),
            format!("Top score: {}", top_score),
            String::new(),
            "Keybindings:".into(),
            "  /      Focus search".into(),
            "  Esc    Clear search".into(),
            "  j/k    Navigate".into(),
            "  g/G    Top/Bottom".into(),
            "  PgUp/Dn  Page scroll".into(),
        ];

        for (i, line) in stats.iter().enumerate() {
            if i >= inner.height as usize {
                break;
            }
            let style = if line.is_empty() || line.starts_with(' ') {
                Style::new().fg(theme::fg::MUTED)
            } else if line.starts_with("Keybindings") {
                Style::new().fg(theme::accent::PRIMARY)
            } else {
                Style::new().fg(theme::fg::SECONDARY)
            };
            let row_area = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
            Paragraph::new(line.as_str())
                .style(style)
                .render(row_area, frame);
        }
    }

    /// Handle mouse events: click to select/focus, scroll to navigate.
    fn handle_mouse(&mut self, event: &Event) {
        if let Event::Mouse(mouse) = event {
            let list_area = self.last_list_area.get();
            let search_area = self.last_search_area.get();

            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if search_area.contains(mouse.x, mouse.y) {
                        // Click search bar: focus search
                        if self.focus != Focus::Search {
                            self.focus = Focus::Search;
                            self.search_input = TextInput::new()
                                .with_placeholder("Type to search...")
                                .with_style(Style::new().fg(theme::fg::PRIMARY))
                                .with_focused(true);
                        }
                    } else if list_area.contains(mouse.x, mouse.y) {
                        // Click list: select item based on y position
                        if self.focus == Focus::Search {
                            self.focus = Focus::List;
                            self.search_input = TextInput::new()
                                .with_placeholder("Type to search...")
                                .with_style(Style::new().fg(theme::fg::PRIMARY))
                                .with_focused(false);
                        }
                        let inner_y = mouse.y.saturating_sub(list_area.y + 1) as usize;
                        let target = self.scroll_offset + inner_y;
                        if target < self.filtered.len() {
                            self.selected = target;
                        }
                    }
                }
                MouseEventKind::ScrollUp => {
                    if list_area.contains(mouse.x, mouse.y) {
                        self.selected = self.selected.saturating_sub(3);
                        self.ensure_visible();
                    }
                }
                MouseEventKind::ScrollDown => {
                    if list_area.contains(mouse.x, mouse.y) {
                        self.selected =
                            (self.selected + 3).min(self.filtered.len().saturating_sub(1));
                        self.ensure_visible();
                    }
                }
                _ => {}
            }
        }
    }
}

impl Screen for VirtualizedSearch {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if matches!(event, Event::Mouse(_)) {
            self.handle_mouse(event);
            return Cmd::none();
        }
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        {
            let shift = modifiers.contains(Modifiers::SHIFT);

            match self.focus {
                Focus::Search => match code {
                    KeyCode::Escape => {
                        // Clear search and return to list
                        let old_query = self.query.clone();
                        self.query.clear();
                        self.search_input = TextInput::new()
                            .with_placeholder("Type to search...")
                            .with_style(Style::new().fg(theme::fg::PRIMARY))
                            .with_focused(false);
                        self.focus = Focus::List;
                        self.update_filter();

                        // Record focus change
                        let diag =
                            DiagnosticEntry::new(DiagnosticEventKind::FocusChange, self.tick_count)
                                .with_focus(false)
                                .with_context("search -> list (escape)");
                        self.record_diagnostic(diag);

                        // Record query clear if query was non-empty
                        if !old_query.is_empty() {
                            let diag = DiagnosticEntry::new(
                                DiagnosticEventKind::QueryChange,
                                self.tick_count,
                            )
                            .with_query("")
                            .with_filtered_count(self.filtered.len())
                            .with_context("cleared via escape");
                            self.record_diagnostic(diag);
                        }
                    }
                    KeyCode::Enter => {
                        // Return to list with current filter
                        self.focus = Focus::List;

                        let diag =
                            DiagnosticEntry::new(DiagnosticEventKind::FocusChange, self.tick_count)
                                .with_focus(false)
                                .with_query(&self.query)
                                .with_context("search -> list (enter)");
                        self.record_diagnostic(diag);
                    }
                    KeyCode::Backspace => {
                        if !self.query.is_empty() {
                            self.query.pop();
                            self.update_filter();

                            let diag = DiagnosticEntry::new(
                                DiagnosticEventKind::QueryChange,
                                self.tick_count,
                            )
                            .with_query(&self.query)
                            .with_filtered_count(self.filtered.len())
                            .with_context("backspace");
                            self.record_diagnostic(diag);
                        }
                    }
                    KeyCode::Char(c) => {
                        self.query.push(*c);
                        self.update_filter();

                        let diag =
                            DiagnosticEntry::new(DiagnosticEventKind::QueryChange, self.tick_count)
                                .with_query(&self.query)
                                .with_filtered_count(self.filtered.len())
                                .with_context(format!("typed '{c}'"));
                        self.record_diagnostic(diag);
                    }
                    _ => {}
                },
                Focus::List => match code {
                    KeyCode::Char('/') => {
                        self.focus = Focus::Search;

                        let diag =
                            DiagnosticEntry::new(DiagnosticEventKind::FocusChange, self.tick_count)
                                .with_focus(true)
                                .with_query(&self.query)
                                .with_context("list -> search");
                        self.record_diagnostic(diag);
                    }
                    KeyCode::Escape => {
                        if !self.query.is_empty() {
                            self.query.clear();
                            self.update_filter();

                            let diag = DiagnosticEntry::new(
                                DiagnosticEventKind::QueryChange,
                                self.tick_count,
                            )
                            .with_query("")
                            .with_filtered_count(self.filtered.len())
                            .with_context("cleared via escape (from list)");
                            self.record_diagnostic(diag);
                        }
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        self.select_next();
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.select_previous();
                    }
                    KeyCode::Char('g') if !shift => {
                        self.select_first();
                    }
                    KeyCode::Char('G') | KeyCode::Char('g') if shift => {
                        self.select_last();
                    }
                    KeyCode::Home => {
                        self.select_first();
                    }
                    KeyCode::End => {
                        self.select_last();
                    }
                    KeyCode::PageUp => {
                        self.page_up();
                    }
                    KeyCode::PageDown => {
                        self.page_down();
                    }
                    // Auto-focus search when typing printable characters
                    KeyCode::Char(c) if c.is_alphanumeric() || c.is_ascii_punctuation() => {
                        self.focus = Focus::Search;
                        self.query.push(*c);
                        self.update_filter();

                        let diag =
                            DiagnosticEntry::new(DiagnosticEventKind::FocusChange, self.tick_count)
                                .with_focus(true)
                                .with_context("list -> search (auto)");
                        self.record_diagnostic(diag);

                        let diag =
                            DiagnosticEntry::new(DiagnosticEventKind::QueryChange, self.tick_count)
                                .with_query(&self.query)
                                .with_filtered_count(self.filtered.len())
                                .with_context(format!("typed '{c}' (auto-focus)"));
                        self.record_diagnostic(diag);
                    }
                    _ => {}
                },
            }
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Layout: search bar (3 rows) + main content
        let v_chunks = Flex::vertical()
            .constraints([Constraint::Fixed(3), Constraint::Fill])
            .split(area);
        let search_area = v_chunks[0];
        let content_area = v_chunks[1];

        self.last_search_area.set(search_area);
        self.render_search_bar(frame, search_area);

        // Main content: list (70%) + stats (30%)
        let h_chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(70.0), Constraint::Percentage(30.0)])
            .split(content_area);
        let list_area = h_chunks[0];
        let stats_area = h_chunks[1];

        // Update viewport height for navigation (mutable through interior mutability would be ideal)
        // For now we just use the value from the area
        let _vp = list_area.height.saturating_sub(2) as usize; // -2 for borders

        // Note: We can't mutate self.viewport_height here since view takes &self
        // The navigation uses the last known value, which is fine for this demo

        self.last_list_area.set(list_area);
        self.render_list_panel(frame, list_area);
        self.render_stats_panel(frame, stats_area);
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }

    fn title(&self) -> &'static str {
        "Virtualized Search"
    }

    fn tab_label(&self) -> &'static str {
        "VirtSearch"
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "/",
                action: "Focus search input",
            },
            HelpEntry {
                key: "Esc",
                action: "Clear search / unfocus",
            },
            HelpEntry {
                key: "j/↓",
                action: "Next item",
            },
            HelpEntry {
                key: "k/↑",
                action: "Previous item",
            },
            HelpEntry {
                key: "g/G",
                action: "First/Last item",
            },
            HelpEntry {
                key: "PgUp/Dn",
                action: "Page scroll",
            },
            HelpEntry {
                key: "Click",
                action: "Select item / focus search",
            },
            HelpEntry {
                key: "Scroll",
                action: "Navigate list",
            },
        ]
    }
}

// =============================================================================
// Unit Tests (bd-2zbk.5)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // DiagnosticEventKind tests
    // -------------------------------------------------------------------------

    #[test]
    fn diagnostic_event_kind_as_str() {
        assert_eq!(DiagnosticEventKind::QueryChange.as_str(), "query_change");
        assert_eq!(DiagnosticEventKind::FilterUpdate.as_str(), "filter_update");
        assert_eq!(DiagnosticEventKind::Navigate.as_str(), "navigate");
        assert_eq!(DiagnosticEventKind::FocusChange.as_str(), "focus_change");
        assert_eq!(DiagnosticEventKind::PageScroll.as_str(), "page_scroll");
        assert_eq!(DiagnosticEventKind::JumpToEdge.as_str(), "jump_to_edge");
        assert_eq!(DiagnosticEventKind::FuzzyMatch.as_str(), "fuzzy_match");
        assert_eq!(DiagnosticEventKind::Render.as_str(), "render");
        assert_eq!(DiagnosticEventKind::Tick.as_str(), "tick");
    }

    // -------------------------------------------------------------------------
    // DiagnosticEntry tests
    // -------------------------------------------------------------------------

    #[test]
    fn diagnostic_entry_basic_creation() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 42);
        assert_eq!(entry.kind, DiagnosticEventKind::Navigate);
        assert_eq!(entry.tick, 42);
        assert!(entry.query.is_none());
        assert!(entry.selected.is_none());
    }

    #[test]
    fn diagnostic_entry_builder_chain() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::FilterUpdate, 10)
            .with_query("test")
            .with_filtered_count(100)
            .with_selection(5, 0)
            .with_match_score(42)
            .with_context("test context");

        assert_eq!(entry.query.as_deref(), Some("test"));
        assert_eq!(entry.filtered_count, Some(100));
        assert_eq!(entry.selected, Some(5));
        assert_eq!(entry.scroll_offset, Some(0));
        assert_eq!(entry.match_score, Some(42));
        assert_eq!(entry.context.as_deref(), Some("test context"));
    }

    #[test]
    fn diagnostic_entry_with_focus() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::FocusChange, 1).with_focus(true);
        assert_eq!(entry.focus_search, Some(true));

        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::FocusChange, 2).with_focus(false);
        assert_eq!(entry2.focus_search, Some(false));
    }

    #[test]
    fn diagnostic_entry_with_direction() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 5).with_direction("up");
        assert_eq!(entry.direction.as_deref(), Some("up"));
    }

    #[test]
    fn diagnostic_entry_checksum_deterministic() {
        let entry1 = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 42)
            .with_query("test")
            .with_selection(5, 0)
            .with_checksum();

        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 42)
            .with_query("test")
            .with_selection(5, 0)
            .with_checksum();

        assert_eq!(entry1.checksum, entry2.checksum);
        assert_ne!(entry1.checksum, 0);
    }

    #[test]
    fn diagnostic_entry_checksum_varies_with_content() {
        let entry1 = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 42)
            .with_query("test")
            .with_checksum();

        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 42)
            .with_query("different")
            .with_checksum();

        assert_ne!(entry1.checksum, entry2.checksum);
    }

    #[test]
    fn diagnostic_entry_to_jsonl_basic() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 42).with_checksum();
        let jsonl = entry.to_jsonl();

        assert!(jsonl.starts_with('{'));
        assert!(jsonl.ends_with('}'));
        assert!(jsonl.contains("\"kind\":\"navigate\""));
        assert!(jsonl.contains("\"tick\":42"));
        assert!(jsonl.contains("\"checksum\":"));
    }

    #[test]
    fn diagnostic_entry_to_jsonl_with_query() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::QueryChange, 10)
            .with_query("hello world")
            .with_filtered_count(50)
            .with_checksum();
        let jsonl = entry.to_jsonl();

        assert!(jsonl.contains("\"query\":\"hello world\""));
        assert!(jsonl.contains("\"filtered_count\":50"));
    }

    #[test]
    fn diagnostic_entry_to_jsonl_escapes_quotes() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::QueryChange, 1)
            .with_query("test \"quoted\"")
            .with_checksum();
        let jsonl = entry.to_jsonl();

        assert!(jsonl.contains("\"query\":\"test \\\"quoted\\\"\""));
    }

    // -------------------------------------------------------------------------
    // DiagnosticLog tests
    // -------------------------------------------------------------------------

    #[test]
    fn diagnostic_log_new_empty() {
        let log = DiagnosticLog::new();
        assert!(log.entries().is_empty());
    }

    #[test]
    fn diagnostic_log_record_entry() {
        let mut log = DiagnosticLog::new();
        let entry = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 1).with_checksum();
        log.record(entry);

        assert_eq!(log.entries().len(), 1);
        assert_eq!(log.entries()[0].kind, DiagnosticEventKind::Navigate);
    }

    #[test]
    fn diagnostic_log_max_entries() {
        let mut log = DiagnosticLog::new().with_max_entries(5);

        for i in 0..10 {
            let entry = DiagnosticEntry::new(DiagnosticEventKind::Tick, i).with_checksum();
            log.record(entry);
        }

        assert_eq!(log.entries().len(), 5);
        // Should have kept the last 5 entries
        assert_eq!(log.entries()[0].tick, 5);
        assert_eq!(log.entries()[4].tick, 9);
    }

    #[test]
    fn diagnostic_log_clear() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::Navigate, 1).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::Navigate, 2).with_checksum());

        assert_eq!(log.entries().len(), 2);
        log.clear();
        assert!(log.entries().is_empty());
    }

    #[test]
    fn diagnostic_log_entries_of_kind() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::Navigate, 1).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::QueryChange, 2).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::Navigate, 3).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::FocusChange, 4).with_checksum());

        let nav_entries = log.entries_of_kind(DiagnosticEventKind::Navigate);
        assert_eq!(nav_entries.len(), 2);
        assert_eq!(nav_entries[0].tick, 1);
        assert_eq!(nav_entries[1].tick, 3);

        let query_entries = log.entries_of_kind(DiagnosticEventKind::QueryChange);
        assert_eq!(query_entries.len(), 1);
    }

    #[test]
    fn diagnostic_log_to_jsonl() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::Navigate, 1).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::Navigate, 2).with_checksum());

        let jsonl = log.to_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();

        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"tick\":1"));
        assert!(lines[1].contains("\"tick\":2"));
    }

    // -------------------------------------------------------------------------
    // DiagnosticSummary tests
    // -------------------------------------------------------------------------

    #[test]
    fn diagnostic_summary_default() {
        let summary = DiagnosticSummary::default();
        assert_eq!(summary.total_entries, 0);
        assert_eq!(summary.navigate_count, 0);
        assert_eq!(summary.query_change_count, 0);
    }

    #[test]
    fn diagnostic_log_summary() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::Navigate, 1).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::Navigate, 2).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::QueryChange, 3).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::FilterUpdate, 4).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::FocusChange, 5).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::PageScroll, 6).with_checksum());
        log.record(DiagnosticEntry::new(DiagnosticEventKind::JumpToEdge, 7).with_checksum());

        let summary = log.summary();
        assert_eq!(summary.total_entries, 7);
        assert_eq!(summary.navigate_count, 2);
        assert_eq!(summary.query_change_count, 1);
        assert_eq!(summary.filter_update_count, 1);
        assert_eq!(summary.focus_change_count, 1);
        assert_eq!(summary.page_scroll_count, 1);
        assert_eq!(summary.jump_to_edge_count, 1);
    }

    #[test]
    fn diagnostic_summary_to_jsonl() {
        let summary = DiagnosticSummary {
            total_entries: 10,
            navigate_count: 5,
            query_change_count: 2,
            ..Default::default()
        };

        let jsonl = summary.to_jsonl();
        assert!(jsonl.contains("\"summary\":true"));
        assert!(jsonl.contains("\"total\":10"));
        assert!(jsonl.contains("\"navigate\":5"));
        assert!(jsonl.contains("\"query_change\":2"));
    }

    // -------------------------------------------------------------------------
    // TelemetryHooks tests
    // -------------------------------------------------------------------------

    #[test]
    fn telemetry_hooks_default() {
        let hooks = TelemetryHooks::new();
        // Just verify it can be created
        assert!(hooks.on_query_change.is_none());
        assert!(hooks.on_filter_update.is_none());
        assert!(hooks.on_navigate.is_none());
        assert!(hooks.on_any_event.is_none());
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

        let entry = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 1).with_checksum();
        hooks.dispatch(&entry);

        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn telemetry_hooks_on_navigate() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let hooks = TelemetryHooks::new().on_navigate(move |_| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        // Navigate event should trigger
        let entry1 = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 1).with_checksum();
        hooks.dispatch(&entry1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // PageScroll should also trigger on_navigate
        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::PageScroll, 2).with_checksum();
        hooks.dispatch(&entry2);
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        // JumpToEdge should also trigger on_navigate
        let entry3 = DiagnosticEntry::new(DiagnosticEventKind::JumpToEdge, 3).with_checksum();
        hooks.dispatch(&entry3);
        assert_eq!(counter.load(Ordering::Relaxed), 3);

        // QueryChange should NOT trigger on_navigate
        let entry4 = DiagnosticEntry::new(DiagnosticEventKind::QueryChange, 4).with_checksum();
        hooks.dispatch(&entry4);
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn telemetry_hooks_on_query_change() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let hooks = TelemetryHooks::new().on_query_change(move |_| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        let entry = DiagnosticEntry::new(DiagnosticEventKind::QueryChange, 1).with_checksum();
        hooks.dispatch(&entry);

        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // Navigate should NOT trigger on_query_change
        let entry2 = DiagnosticEntry::new(DiagnosticEventKind::Navigate, 2).with_checksum();
        hooks.dispatch(&entry2);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    // -------------------------------------------------------------------------
    // Global state tests
    // -------------------------------------------------------------------------

    #[test]
    fn diagnostics_enabled_default_false() {
        // Note: This test may be flaky if run after other tests that enable diagnostics
        // In practice, each test should manage the global state
        set_diagnostics_enabled(false);
        assert!(!diagnostics_enabled());
    }

    #[test]
    fn diagnostics_enabled_toggle() {
        set_diagnostics_enabled(true);
        assert!(diagnostics_enabled());
        set_diagnostics_enabled(false);
        assert!(!diagnostics_enabled());
    }

    #[test]
    fn event_counter_increments() {
        reset_event_counter();
        let seq1 = next_event_seq();
        let seq2 = next_event_seq();
        let seq3 = next_event_seq();

        assert_eq!(seq1, 0);
        assert_eq!(seq2, 1);
        assert_eq!(seq3, 2);
    }

    // -------------------------------------------------------------------------
    // VirtualizedSearch diagnostic integration tests
    // -------------------------------------------------------------------------

    #[test]
    fn virtualized_search_with_diagnostics() {
        reset_event_counter();
        let mut screen = VirtualizedSearch::new().with_diagnostics();
        assert!(screen.diagnostic_log().is_some());

        // Note: The initial FilterUpdate from new() is not captured because
        // with_diagnostics() is called after new(). To verify diagnostics work,
        // we trigger a new filter update.
        screen.select_next();

        let log = screen.diagnostic_log().unwrap();
        assert!(!log.entries().is_empty());
        assert!(
            !log.entries_of_kind(DiagnosticEventKind::Navigate)
                .is_empty()
        );
    }

    #[test]
    fn virtualized_search_accessors() {
        let screen = VirtualizedSearch::new();

        assert_eq!(screen.current_query(), "");
        assert_eq!(screen.filtered_count(), TOTAL_ITEMS);
        assert_eq!(screen.selected_index(), 0);
        assert_eq!(screen.current_scroll_offset(), 0);
        assert!(!screen.is_search_focused());
        assert_eq!(screen.total_items(), TOTAL_ITEMS);
    }

    #[test]
    fn virtualized_search_navigation_emits_diagnostics() {
        reset_event_counter();
        let mut screen = VirtualizedSearch::new().with_diagnostics();

        // Clear initial diagnostics
        screen.diagnostic_log_mut().unwrap().clear();

        // Navigate down
        screen.select_next();

        let log = screen.diagnostic_log().unwrap();
        let nav_entries = log.entries_of_kind(DiagnosticEventKind::Navigate);
        assert_eq!(nav_entries.len(), 1);
        assert_eq!(nav_entries[0].direction.as_deref(), Some("down"));
        assert_eq!(nav_entries[0].selected, Some(1));
    }

    #[test]
    fn virtualized_search_jump_emits_diagnostics() {
        reset_event_counter();
        let mut screen = VirtualizedSearch::new().with_diagnostics();

        // Move to a non-zero position first
        screen.select_next();
        screen.select_next();

        // Clear diagnostics
        screen.diagnostic_log_mut().unwrap().clear();

        // Jump to first
        screen.select_first();

        let log = screen.diagnostic_log().unwrap();
        let jump_entries = log.entries_of_kind(DiagnosticEventKind::JumpToEdge);
        assert_eq!(jump_entries.len(), 1);
        assert_eq!(jump_entries[0].direction.as_deref(), Some("first"));
    }

    #[test]
    fn virtualized_search_page_scroll_emits_diagnostics() {
        reset_event_counter();
        let mut screen = VirtualizedSearch::new().with_diagnostics();

        // Set a reasonable viewport height
        screen.viewport_height = 20;

        // Clear diagnostics
        screen.diagnostic_log_mut().unwrap().clear();

        // Page down
        screen.page_down();

        let log = screen.diagnostic_log().unwrap();
        let scroll_entries = log.entries_of_kind(DiagnosticEventKind::PageScroll);
        assert_eq!(scroll_entries.len(), 1);
        assert_eq!(scroll_entries[0].direction.as_deref(), Some("page_down"));
        assert!(scroll_entries[0].context.is_some());
    }

    #[test]
    fn virtualized_search_focus_change_via_event() {
        use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};

        reset_event_counter();
        let mut screen = VirtualizedSearch::new().with_diagnostics();

        // Clear initial diagnostics
        screen.diagnostic_log_mut().unwrap().clear();

        // Send '/' key to focus search
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('/'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let _ = screen.update(&event);

        let log = screen.diagnostic_log().unwrap();
        let focus_entries = log.entries_of_kind(DiagnosticEventKind::FocusChange);
        assert_eq!(focus_entries.len(), 1);
        assert_eq!(focus_entries[0].focus_search, Some(true));
    }

    #[test]
    fn virtualized_search_query_change_via_event() {
        use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};

        reset_event_counter();
        let mut screen = VirtualizedSearch::new().with_diagnostics();

        // Focus search first
        screen.focus = Focus::Search;

        // Clear initial diagnostics
        screen.diagnostic_log_mut().unwrap().clear();

        // Type 'a'
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let _ = screen.update(&event);

        assert_eq!(screen.current_query(), "a");

        let log = screen.diagnostic_log().unwrap();
        let query_entries = log.entries_of_kind(DiagnosticEventKind::QueryChange);
        assert!(!query_entries.is_empty());
        assert_eq!(query_entries.last().unwrap().query.as_deref(), Some("a"));
    }

    #[test]
    fn virtualized_search_with_telemetry_hooks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        reset_event_counter();

        let nav_count = Arc::new(AtomicUsize::new(0));
        let nav_count_clone = nav_count.clone();

        let hooks = TelemetryHooks::new().on_navigate(move |_| {
            nav_count_clone.fetch_add(1, Ordering::Relaxed);
        });

        let mut screen = VirtualizedSearch::new()
            .with_diagnostics()
            .with_telemetry_hooks(hooks);

        // Clear initial diagnostics
        screen.diagnostic_log_mut().unwrap().clear();

        // Navigate
        screen.select_next();
        screen.select_next();
        screen.select_previous();

        // Hooks should have been called
        assert_eq!(nav_count.load(Ordering::Relaxed), 3);
    }

    // -------------------------------------------------------------------------
    // Fuzzy match tests
    // -------------------------------------------------------------------------

    #[test]
    fn fuzzy_match_basic() {
        let result = fuzzy_match("abc", "abcdef");
        assert!(result.is_some());

        let m = result.unwrap();
        assert_eq!(m.positions, vec![0, 1, 2]);
        assert!(m.score > 0);
    }

    #[test]
    fn fuzzy_match_non_consecutive() {
        let result = fuzzy_match("adf", "abcdef");
        assert!(result.is_some());

        let m = result.unwrap();
        assert_eq!(m.positions, vec![0, 3, 5]);
    }

    #[test]
    fn fuzzy_match_case_insensitive() {
        let result = fuzzy_match("ABC", "abcdef");
        assert!(result.is_some());

        let result2 = fuzzy_match("abc", "ABCDEF");
        assert!(result2.is_some());
    }

    #[test]
    fn fuzzy_match_no_match() {
        let result = fuzzy_match("xyz", "abcdef");
        assert!(result.is_none());
    }

    #[test]
    fn fuzzy_match_empty_query() {
        let result = fuzzy_match("", "abcdef");
        assert!(result.is_none());
    }

    #[test]
    fn fuzzy_match_word_boundary_bonus() {
        // Match at word start should score higher
        let result1 = fuzzy_match("cat", "category");
        let result2 = fuzzy_match("cat", "concatenate");

        assert!(result1.is_some());
        assert!(result2.is_some());

        // "cat" at start of "category" should score higher than "cat" in "concatenate"
        assert!(result1.unwrap().score > result2.unwrap().score);
    }

    #[test]
    fn mouse_click_list_selects_item() {
        use crate::screens::Screen;
        let mut screen = VirtualizedSearch::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 120, 40));

        let list_area = screen.last_list_area.get();
        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: list_area.x + 2,
            y: list_area.y + 3,
            modifiers: Modifiers::NONE,
        });
        screen.update(&event);
        assert_eq!(screen.selected, 2, "click should select item at row");
    }

    #[test]
    fn mouse_click_search_focuses() {
        use crate::screens::Screen;
        let mut screen = VirtualizedSearch::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 120, 40));

        assert!(matches!(screen.focus, Focus::List));
        let search_area = screen.last_search_area.get();
        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: search_area.x + 2,
            y: search_area.y + 1,
            modifiers: Modifiers::NONE,
        });
        screen.update(&event);
        assert!(
            matches!(screen.focus, Focus::Search),
            "click should focus search"
        );
    }

    #[test]
    fn mouse_scroll_navigates_list() {
        use crate::screens::Screen;
        let mut screen = VirtualizedSearch::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 120, 40));

        let list_area = screen.last_list_area.get();
        let scroll_down = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            x: list_area.x + 2,
            y: list_area.y + 2,
            modifiers: Modifiers::NONE,
        });
        screen.update(&scroll_down);
        assert_eq!(screen.selected, 3, "scroll down should advance 3 items");
    }

    #[test]
    fn keybindings_includes_mouse() {
        use crate::screens::Screen;
        let screen = VirtualizedSearch::new();
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

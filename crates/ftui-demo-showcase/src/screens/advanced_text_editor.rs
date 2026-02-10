#![forbid(unsafe_code)]

//! Advanced Text Editor screen — multi-line editor with search/replace.
//!
//! Demonstrates:
//! - `TextArea` with line numbers, selection highlighting
//! - Search/replace functionality with match navigation
//! - Cursor position and selection length tracking
//! - Undo/redo integration
//!
//! # Telemetry and Diagnostics (bd-12o8.5)
//!
//! This module provides rich diagnostic logging and telemetry hooks:
//! - JSONL diagnostic output via `DiagnosticLog`
//! - Observable hooks for search, replace, undo/redo, and text edit events
//! - Deterministic mode for reproducible testing
//!
//! ## Environment Variables
//!
//! - `FTUI_TEXTEDITOR_DIAGNOSTICS=true` - Enable verbose diagnostic output
//! - `FTUI_TEXTEDITOR_DETERMINISTIC=true` - Enable deterministic mode

use std::cell::Cell;
use std::collections::VecDeque;
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
use ftui_text::grapheme_count;
use ftui_text::search::{SearchResult, search_ascii_case_insensitive};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::textarea::TextArea;

use super::{HelpEntry, Screen};
use crate::determinism;
use crate::theme;

const UNDO_HISTORY_LIMIT: usize = 64;

// =============================================================================
// Diagnostic Logging (bd-12o8.5)
// =============================================================================

/// Global diagnostic enable flag (checked once at startup).
static TEXTEDITOR_DIAGNOSTICS_ENABLED: AtomicBool = AtomicBool::new(false);
/// Global monotonic event counter for deterministic ordering.
static TEXTEDITOR_EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Initialize diagnostic settings from environment.
pub fn init_diagnostics() {
    let enabled = std::env::var("FTUI_TEXTEDITOR_DIAGNOSTICS")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    TEXTEDITOR_DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Check if diagnostics are enabled.
#[inline]
pub fn diagnostics_enabled() -> bool {
    TEXTEDITOR_DIAGNOSTICS_ENABLED.load(Ordering::Relaxed)
}

/// Set diagnostics enabled state (for testing).
pub fn set_diagnostics_enabled(enabled: bool) {
    TEXTEDITOR_DIAGNOSTICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Get next monotonic event sequence number.
#[inline]
fn next_event_seq() -> u64 {
    TEXTEDITOR_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Reset event counter (for testing determinism).
pub fn reset_event_counter() {
    TEXTEDITOR_EVENT_COUNTER.store(0, Ordering::Relaxed);
}

/// Check if deterministic mode is enabled.
pub fn is_deterministic_mode() -> bool {
    determinism::env_flag("FTUI_TEXTEDITOR_DETERMINISTIC") || determinism::is_demo_deterministic()
}

/// Diagnostic event types for JSONL logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticEventKind {
    /// Search panel opened.
    SearchOpened,
    /// Search panel closed.
    SearchClosed,
    /// Search query updated.
    QueryUpdated,
    /// Match navigation (next/prev).
    MatchNavigation,
    /// Single replace performed.
    ReplacePerformed,
    /// Replace all performed.
    ReplaceAllPerformed,
    /// Undo performed.
    UndoPerformed,
    /// Redo performed.
    RedoPerformed,
    /// Text edited (character inserted/deleted).
    TextEdited,
    /// Focus changed between panels.
    FocusChanged,
    /// Undo history panel toggled.
    HistoryPanelToggled,
    /// Selection cleared.
    SelectionCleared,
}

impl DiagnosticEventKind {
    /// Get the JSONL event type string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SearchOpened => "search_opened",
            Self::SearchClosed => "search_closed",
            Self::QueryUpdated => "query_updated",
            Self::MatchNavigation => "match_navigation",
            Self::ReplacePerformed => "replace_performed",
            Self::ReplaceAllPerformed => "replace_all_performed",
            Self::UndoPerformed => "undo_performed",
            Self::RedoPerformed => "redo_performed",
            Self::TextEdited => "text_edited",
            Self::FocusChanged => "focus_changed",
            Self::HistoryPanelToggled => "history_panel_toggled",
            Self::SelectionCleared => "selection_cleared",
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
    /// Current search query.
    pub query: Option<String>,
    /// Current replacement text.
    pub replacement: Option<String>,
    /// Match count.
    pub match_count: Option<usize>,
    /// Current match position (1-based).
    pub match_position: Option<usize>,
    /// Replace count (for replace all).
    pub replace_count: Option<usize>,
    /// Undo stack depth.
    pub undo_depth: Option<usize>,
    /// Redo stack depth.
    pub redo_depth: Option<usize>,
    /// Current focus panel.
    pub focus: Option<String>,
    /// Navigation direction.
    pub direction: Option<String>,
    /// Panel visibility state.
    pub panel_visible: Option<bool>,
    /// Cursor line (0-based).
    pub cursor_line: Option<usize>,
    /// Cursor column (0-based).
    pub cursor_col: Option<usize>,
    /// Selection length in chars.
    pub selection_len: Option<usize>,
    /// Text length in chars.
    pub text_len: Option<usize>,
    /// Additional context.
    pub context: Option<String>,
    /// Checksum for determinism verification.
    pub checksum: u64,
}

impl DiagnosticEntry {
    /// Create a new diagnostic entry with current timestamp.
    pub fn new(kind: DiagnosticEventKind) -> Self {
        let timestamp_us = if is_deterministic_mode() {
            next_event_seq() * 1000
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
            replacement: None,
            match_count: None,
            match_position: None,
            replace_count: None,
            undo_depth: None,
            redo_depth: None,
            focus: None,
            direction: None,
            panel_visible: None,
            cursor_line: None,
            cursor_col: None,
            selection_len: None,
            text_len: None,
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

    /// Set replacement.
    #[must_use]
    pub fn with_replacement(mut self, replacement: impl Into<String>) -> Self {
        self.replacement = Some(replacement.into());
        self
    }

    /// Set match count.
    #[must_use]
    pub fn with_match_count(mut self, count: usize) -> Self {
        self.match_count = Some(count);
        self
    }

    /// Set match position.
    #[must_use]
    pub fn with_match_position(mut self, pos: usize) -> Self {
        self.match_position = Some(pos);
        self
    }

    /// Set replace count.
    #[must_use]
    pub fn with_replace_count(mut self, count: usize) -> Self {
        self.replace_count = Some(count);
        self
    }

    /// Set undo depth.
    #[must_use]
    pub fn with_undo_depth(mut self, depth: usize) -> Self {
        self.undo_depth = Some(depth);
        self
    }

    /// Set redo depth.
    #[must_use]
    pub fn with_redo_depth(mut self, depth: usize) -> Self {
        self.redo_depth = Some(depth);
        self
    }

    /// Set focus.
    #[must_use]
    pub fn with_focus(mut self, focus: impl Into<String>) -> Self {
        self.focus = Some(focus.into());
        self
    }

    /// Set direction.
    #[must_use]
    pub fn with_direction(mut self, direction: impl Into<String>) -> Self {
        self.direction = Some(direction.into());
        self
    }

    /// Set panel visibility.
    #[must_use]
    pub fn with_panel_visible(mut self, visible: bool) -> Self {
        self.panel_visible = Some(visible);
        self
    }

    /// Set cursor position.
    #[must_use]
    pub fn with_cursor(mut self, line: usize, col: usize) -> Self {
        self.cursor_line = Some(line);
        self.cursor_col = Some(col);
        self
    }

    /// Set selection length.
    #[must_use]
    pub fn with_selection_len(mut self, len: usize) -> Self {
        self.selection_len = Some(len);
        self
    }

    /// Set text length.
    #[must_use]
    pub fn with_text_len(mut self, len: usize) -> Self {
        self.text_len = Some(len);
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
            self.match_count.unwrap_or(0),
            self.match_position.unwrap_or(0),
            self.undo_depth.unwrap_or(0),
            self.redo_depth.unwrap_or(0),
            self.focus.as_deref().unwrap_or(""),
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
        ];

        if let Some(ref q) = self.query {
            let escaped = q.replace('\\', "\\\\").replace('"', "\\\"");
            parts.push(format!("\"query\":\"{escaped}\""));
        }
        if let Some(ref r) = self.replacement {
            let escaped = r.replace('\\', "\\\\").replace('"', "\\\"");
            parts.push(format!("\"replacement\":\"{escaped}\""));
        }
        if let Some(c) = self.match_count {
            parts.push(format!("\"match_count\":{c}"));
        }
        if let Some(p) = self.match_position {
            parts.push(format!("\"match_position\":{p}"));
        }
        if let Some(c) = self.replace_count {
            parts.push(format!("\"replace_count\":{c}"));
        }
        if let Some(d) = self.undo_depth {
            parts.push(format!("\"undo_depth\":{d}"));
        }
        if let Some(d) = self.redo_depth {
            parts.push(format!("\"redo_depth\":{d}"));
        }
        if let Some(ref f) = self.focus {
            parts.push(format!("\"focus\":\"{f}\""));
        }
        if let Some(ref d) = self.direction {
            parts.push(format!("\"direction\":\"{d}\""));
        }
        if let Some(v) = self.panel_visible {
            parts.push(format!("\"panel_visible\":{v}"));
        }
        if let Some(l) = self.cursor_line {
            parts.push(format!("\"cursor_line\":{l}"));
        }
        if let Some(c) = self.cursor_col {
            parts.push(format!("\"cursor_col\":{c}"));
        }
        if let Some(l) = self.selection_len {
            parts.push(format!("\"selection_len\":{l}"));
        }
        if let Some(l) = self.text_len {
            parts.push(format!("\"text_len\":{l}"));
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
                DiagnosticEventKind::MatchNavigation => summary.match_navigation_count += 1,
                DiagnosticEventKind::ReplacePerformed => summary.replace_performed_count += 1,
                DiagnosticEventKind::ReplaceAllPerformed => {
                    summary.replace_all_performed_count += 1
                }
                DiagnosticEventKind::UndoPerformed => summary.undo_performed_count += 1,
                DiagnosticEventKind::RedoPerformed => summary.redo_performed_count += 1,
                DiagnosticEventKind::TextEdited => summary.text_edited_count += 1,
                DiagnosticEventKind::FocusChanged => summary.focus_changed_count += 1,
                DiagnosticEventKind::HistoryPanelToggled => {
                    summary.history_panel_toggled_count += 1
                }
                DiagnosticEventKind::SelectionCleared => summary.selection_cleared_count += 1,
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
    pub match_navigation_count: usize,
    pub replace_performed_count: usize,
    pub replace_all_performed_count: usize,
    pub undo_performed_count: usize,
    pub redo_performed_count: usize,
    pub text_edited_count: usize,
    pub focus_changed_count: usize,
    pub history_panel_toggled_count: usize,
    pub selection_cleared_count: usize,
}

impl DiagnosticSummary {
    /// Format as JSONL.
    pub fn to_jsonl(&self) -> String {
        format!(
            "{{\"summary\":true,\"total\":{},\"search_opened\":{},\"search_closed\":{},\
             \"query_updated\":{},\"match_navigation\":{},\"replace_performed\":{},\
             \"replace_all_performed\":{},\"undo_performed\":{},\"redo_performed\":{},\
             \"text_edited\":{},\"focus_changed\":{},\"history_panel_toggled\":{},\
             \"selection_cleared\":{}}}",
            self.total_entries,
            self.search_opened_count,
            self.search_closed_count,
            self.query_updated_count,
            self.match_navigation_count,
            self.replace_performed_count,
            self.replace_all_performed_count,
            self.undo_performed_count,
            self.redo_performed_count,
            self.text_edited_count,
            self.focus_changed_count,
            self.history_panel_toggled_count,
            self.selection_cleared_count
        )
    }
}

// =============================================================================
// Editor Implementation
// =============================================================================

#[derive(Debug, Clone, Copy)]
struct KeyChord {
    code: KeyCode,
    modifiers: Modifiers,
}

impl KeyChord {
    const fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self { code, modifiers }
    }

    fn matches(self, code: KeyCode, modifiers: Modifiers) -> bool {
        self.code == code && self.modifiers == modifiers
    }
}

#[derive(Debug, Clone, Copy)]
struct UndoKeybindings {
    undo: KeyChord,
    redo_primary: KeyChord,
    redo_secondary: KeyChord,
}

impl Default for UndoKeybindings {
    fn default() -> Self {
        Self {
            undo: KeyChord::new(KeyCode::Char('z'), Modifiers::CTRL),
            redo_primary: KeyChord::new(KeyCode::Char('y'), Modifiers::CTRL),
            redo_secondary: KeyChord::new(KeyCode::Char('Z'), Modifiers::CTRL | Modifiers::SHIFT),
        }
    }
}

/// Focus state for the editor screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// Main text editor.
    Editor,
    /// Search query input.
    Search,
    /// Replace text input.
    Replace,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Editor => Self::Search,
            Self::Search => Self::Replace,
            Self::Replace => Self::Editor,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Editor => Self::Replace,
            Self::Search => Self::Editor,
            Self::Replace => Self::Search,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Search => "search",
            Self::Replace => "replace",
        }
    }
}

/// Advanced Text Editor demo screen.
pub struct AdvancedTextEditor {
    /// Main text editor.
    editor: TextArea,
    /// Search query input.
    search_input: TextInput,
    /// Replace text input.
    replace_input: TextInput,
    /// Which panel has focus.
    focus: Focus,
    /// Whether the search/replace panel is visible.
    search_visible: bool,
    /// Cached search results.
    search_results: Vec<SearchResult>,
    /// Current match index (0-based, None if no matches).
    current_match: Option<usize>,
    /// Status message displayed at the bottom.
    status: String,
    /// Undo history (most recent at the back).
    undo_stack: VecDeque<String>,
    /// Redo history (most recent at the back).
    redo_stack: VecDeque<String>,
    /// Whether the undo history panel is visible.
    undo_panel_visible: bool,
    /// Undo/redo keybindings.
    undo_keys: UndoKeybindings,
    /// Diagnostic log for telemetry (bd-12o8.5).
    diagnostic_log: DiagnosticLog,
    /// Editor panel hit rect for mouse focus.
    layout_editor: Cell<Rect>,
    /// Undo panel hit rect for mouse focus.
    layout_undo: Cell<Rect>,
    /// Search panel hit rect for mouse focus.
    layout_search_panel: Cell<Rect>,
    /// Search input hit rect for mouse focus.
    layout_search_input: Cell<Rect>,
    /// Replace input hit rect for mouse focus.
    layout_replace_input: Cell<Rect>,
}

impl Default for AdvancedTextEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl AdvancedTextEditor {
    /// Create a new Advanced Text Editor screen.
    pub fn new() -> Self {
        let sample_text = r#"Welcome to the Advanced Text Editor!

This is a demonstration of FrankenTUI's text editing capabilities.
You can edit text, select regions, search, and replace.

Features:
- Multi-line editing with line numbers
- Selection with Shift+Arrow keys
- Undo (Ctrl+Z) and Redo (Ctrl+Y)
- Search (Ctrl+F) with next/prev match
- Replace (Ctrl+H) single or all matches

Try editing this text or loading your own content.

Unicode support:
- Emoji: 🎉 🚀 ✨
- CJK: 你好世界
- Accented: café résumé naïve

The editor uses a rope data structure internally for efficient
operations on large buffers, with grapheme-aware cursor movement
and proper Unicode handling throughout.
"#;

        let editor = TextArea::new()
            .with_text(sample_text)
            .with_line_numbers(true)
            .with_focus(true)
            .with_placeholder("Start typing...");

        let search_input = TextInput::new()
            .with_placeholder("Search...")
            .with_focused(false);

        let replace_input = TextInput::new()
            .with_placeholder("Replace with...")
            .with_focused(false);

        // Create diagnostic log (writes to stderr if env var enabled)
        let diagnostic_log = if diagnostics_enabled() {
            DiagnosticLog::new().with_stderr()
        } else {
            DiagnosticLog::new()
        };

        Self {
            editor,
            search_input,
            replace_input,
            focus: Focus::Editor,
            search_visible: false,
            search_results: Vec::new(),
            current_match: None,
            status: "Ready | Ctrl+F: Search | Ctrl+H: Replace | ?: Help".into(),
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            undo_panel_visible: false,
            undo_keys: UndoKeybindings::default(),
            diagnostic_log,
            layout_editor: Cell::new(Rect::default()),
            layout_undo: Cell::new(Rect::default()),
            layout_search_panel: Cell::new(Rect::default()),
            layout_search_input: Cell::new(Rect::default()),
            layout_replace_input: Cell::new(Rect::default()),
        }
    }

    /// Configure undo/redo keybindings (customization support).
    #[allow(dead_code)]
    fn with_undo_keybindings(mut self, bindings: UndoKeybindings) -> Self {
        self.undo_keys = bindings;
        self
    }

    /// Get the diagnostic log (for testing/inspection).
    pub fn diagnostic_log(&self) -> &DiagnosticLog {
        &self.diagnostic_log
    }

    /// Get mutable diagnostic log.
    pub fn diagnostic_log_mut(&mut self) -> &mut DiagnosticLog {
        &mut self.diagnostic_log
    }

    /// Get the current focus panel name (for testing).
    pub fn focus_panel(&self) -> &'static str {
        self.focus.as_str()
    }

    /// Whether the search panel is currently visible (for testing).
    pub fn is_search_visible(&self) -> bool {
        self.search_visible
    }

    /// Log a diagnostic event if diagnostics are enabled.
    fn log_event(&mut self, entry: DiagnosticEntry) {
        if diagnostics_enabled() {
            self.diagnostic_log.record(entry.with_checksum());
        }
    }

    /// Apply the current theme to all widgets.
    pub fn apply_theme(&mut self) {
        let input_style = Style::new()
            .fg(theme::fg::PRIMARY)
            .bg(theme::alpha::SURFACE);
        let placeholder_style = Style::new().fg(theme::fg::MUTED);

        self.editor = self
            .editor
            .clone()
            .with_style(
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::SURFACE),
            )
            .with_cursor_line_style(Style::new().bg(theme::alpha::HIGHLIGHT))
            .with_selection_style(
                Style::new()
                    .bg(theme::alpha::HIGHLIGHT)
                    .fg(theme::fg::PRIMARY),
            );

        self.search_input = self
            .search_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style);

        self.replace_input = self
            .replace_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style);
    }

    /// Update focus states for all widgets.
    fn update_focus_states(&mut self) {
        self.editor.set_focused(self.focus == Focus::Editor);
        self.search_input.set_focused(self.focus == Focus::Search);
        self.replace_input.set_focused(self.focus == Focus::Replace);
    }

    fn clear_layout_rects(&self) {
        self.layout_editor.set(Rect::default());
        self.layout_undo.set(Rect::default());
        self.layout_search_panel.set(Rect::default());
        self.layout_search_input.set(Rect::default());
        self.layout_replace_input.set(Rect::default());
    }

    fn focus_from_point(&mut self, x: u16, y: u16) -> bool {
        let search_input = self.layout_search_input.get();
        let replace_input = self.layout_replace_input.get();
        let search_panel = self.layout_search_panel.get();
        let editor = self.layout_editor.get();
        let undo = self.layout_undo.get();

        let target = if search_input.contains(x, y) {
            Some(Focus::Search)
        } else if replace_input.contains(x, y) {
            Some(Focus::Replace)
        } else if search_panel.contains(x, y) {
            Some(Focus::Search)
        } else if editor.contains(x, y) || undo.contains(x, y) {
            Some(Focus::Editor)
        } else {
            None
        };

        let Some(target) = target else {
            return false;
        };

        if target == self.focus {
            return false;
        }

        let old_focus = self.focus;
        self.focus = target;
        self.update_focus_states();

        let entry = DiagnosticEntry::new(DiagnosticEventKind::FocusChanged)
            .with_focus(self.focus.as_str())
            .with_context(format!("from {} via mouse", old_focus.as_str()));
        self.log_event(entry);

        true
    }

    /// Perform a search with the current query.
    fn do_search(&mut self) {
        let query = self.search_input.value().to_string();
        if query.is_empty() {
            self.search_results.clear();
            self.current_match = None;
            self.update_status();
            return;
        }

        let text = self.editor.text();
        self.search_results = search_ascii_case_insensitive(&text, &query);

        // Log query update
        let entry = DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated)
            .with_query(&query)
            .with_match_count(self.search_results.len());
        self.log_event(entry);

        if self.search_results.is_empty() {
            self.current_match = None;
        } else {
            // Find the match closest to the cursor
            let cursor_byte = self.cursor_to_byte_offset();
            let closest = self
                .search_results
                .iter()
                .enumerate()
                .min_by_key(|(_, r)| r.range.start.abs_diff(cursor_byte))
                .map(|(i, _)| i);
            self.current_match = closest;
            self.jump_to_current_match();
        }
        self.update_status();
    }

    /// Calculate the byte offset for the current cursor position.
    fn cursor_to_byte_offset(&self) -> usize {
        let cursor = self.editor.cursor();
        let text = self.editor.text();
        let mut byte_offset = 0;
        for (line_idx, line) in text.lines().enumerate() {
            if line_idx == cursor.line {
                // Count chars up to cursor.grapheme (approximation for ASCII search)
                for (c_idx, c) in line.chars().enumerate() {
                    if c_idx >= cursor.grapheme {
                        break;
                    }
                    byte_offset += c.len_utf8();
                }
                break;
            }
            byte_offset += line.len() + 1; // +1 for newline
        }
        byte_offset
    }

    /// Jump the cursor to the current match.
    fn jump_to_current_match(&mut self) {
        let Some(idx) = self.current_match else {
            return;
        };
        let Some(result) = self.search_results.get(idx) else {
            return;
        };

        // Convert byte offset to line/column position
        let text = self.editor.text();
        let target_byte = result.range.start;
        let mut line = 0;
        let mut column = 0;
        let mut byte = 0;

        for (line_idx, line_text) in text.lines().enumerate() {
            let line_end = byte + line_text.len();
            if target_byte <= line_end {
                line = line_idx;
                // Count chars within this line up to target byte
                let offset_in_line = target_byte.saturating_sub(byte);
                column = line_text[..offset_in_line.min(line_text.len())]
                    .chars()
                    .count();
                break;
            }
            byte = line_end + 1; // +1 for newline
        }

        // Set cursor position
        self.editor
            .editor_mut()
            .set_cursor(ftui_text::CursorPosition::new(line, column, column));
    }

    /// Move to the next search match.
    fn next_match(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        let idx = self.current_match.unwrap_or(0);
        self.current_match = Some((idx + 1) % self.search_results.len());
        self.jump_to_current_match();
        self.update_status();

        // Log navigation
        let entry = DiagnosticEntry::new(DiagnosticEventKind::MatchNavigation)
            .with_direction("next")
            .with_match_position(self.current_match.map_or(0, |i| i + 1))
            .with_match_count(self.search_results.len());
        self.log_event(entry);
    }

    /// Move to the previous search match.
    fn prev_match(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        let idx = self.current_match.unwrap_or(0);
        self.current_match = Some(idx.checked_sub(1).unwrap_or(self.search_results.len() - 1));
        self.jump_to_current_match();
        self.update_status();

        // Log navigation
        let entry = DiagnosticEntry::new(DiagnosticEventKind::MatchNavigation)
            .with_direction("prev")
            .with_match_position(self.current_match.map_or(0, |i| i + 1))
            .with_match_count(self.search_results.len());
        self.log_event(entry);
    }

    /// Replace the current match with the replacement text.
    fn replace_current(&mut self) {
        let Some(idx) = self.current_match else {
            return;
        };
        let Some(result) = self.search_results.get(idx).cloned() else {
            return;
        };

        let replacement = self.replace_input.value().to_string();
        let text = self.editor.text();

        // Build new text with replacement
        let new_text = format!(
            "{}{}{}",
            &text[..result.range.start],
            replacement,
            &text[result.range.end..]
        );

        // Log replace
        let entry = DiagnosticEntry::new(DiagnosticEventKind::ReplacePerformed)
            .with_query(self.search_input.value())
            .with_replacement(&replacement)
            .with_match_position(idx + 1)
            .with_text_len(grapheme_count(&new_text));
        self.log_event(entry);

        self.editor.set_text(&new_text);
        self.do_search(); // Re-run search
        self.update_status();
    }

    /// Replace all matches with the replacement text.
    fn replace_all(&mut self) {
        if self.search_results.is_empty() {
            return;
        }

        let query = self.search_input.value().to_string();
        if query.is_empty() {
            return;
        }

        let replacement = self.replace_input.value().to_string();
        let text = self.editor.text();

        // Replace from end to start to preserve byte offsets
        let mut new_text = text.clone();
        for result in self.search_results.iter().rev() {
            new_text = format!(
                "{}{}{}",
                &new_text[..result.range.start],
                replacement,
                &new_text[result.range.end..]
            );
        }

        let count = self.search_results.len();

        // Log replace all
        let entry = DiagnosticEntry::new(DiagnosticEventKind::ReplaceAllPerformed)
            .with_query(&query)
            .with_replacement(&replacement)
            .with_replace_count(count)
            .with_text_len(grapheme_count(&new_text));
        self.log_event(entry);

        self.editor.set_text(&new_text);
        self.search_results.clear();
        self.current_match = None;
        self.status = format!("Replaced {count} occurrence(s)");
    }

    /// Update the status line.
    fn update_status(&mut self) {
        let cursor = self.editor.cursor();
        let selection_len = self
            .editor
            .selected_text()
            .as_deref()
            .map(grapheme_count)
            .unwrap_or(0);

        let match_info = if !self.search_results.is_empty() {
            let current = self.current_match.map_or(0, |i| i + 1);
            let total = self.search_results.len();
            format!(" | Match {current}/{total}")
        } else if self.search_visible && !self.search_input.value().is_empty() {
            " | No matches".to_string()
        } else {
            String::new()
        };

        let sel_info = if selection_len > 0 {
            format!(" | Sel: {selection_len}")
        } else {
            String::new()
        };

        let undo_info = format!(
            "Undo:{} Redo:{}",
            self.undo_stack.len(),
            self.redo_stack.len()
        );
        let history_hint = if self.undo_panel_visible {
            "Ctrl+U: Hide history"
        } else {
            "Ctrl+U: Show history"
        };

        self.status = format!(
            "Ln {}, Col {}{}{} | {} | {}",
            cursor.line + 1,
            cursor.grapheme + 1,
            sel_info,
            match_info,
            undo_info,
            history_hint
        );
    }

    /// Render the main editor panel.
    fn render_editor_panel(&self, frame: &mut Frame, area: Rect) {
        self.layout_editor.set(area);
        let focused = self.focus == Focus::Editor;
        let border_style = theme::panel_border_style(focused, theme::screen_accent::FORMS_INPUT);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Editor")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        Widget::render(&self.editor, inner, frame);
    }

    fn render_undo_panel(&self, frame: &mut Frame, area: Rect) {
        self.layout_undo.set(area);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Undo History ")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::accent::INFO));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("Undo ({})", self.undo_stack.len()));
        for entry in self.undo_stack.iter().rev().take(6) {
            lines.push(format!("  • {entry}"));
        }

        lines.push(String::new());
        lines.push(format!("Redo ({})", self.redo_stack.len()));
        for entry in self.redo_stack.iter().rev().take(6) {
            lines.push(format!("  • {entry}"));
        }

        Paragraph::new(lines.join("\n"))
            .style(theme::body())
            .render(inner, frame);
    }

    /// Render the search/replace panel.
    fn render_search_panel(&self, frame: &mut Frame, area: Rect) {
        if !self.search_visible || area.height < 4 {
            return;
        }

        self.layout_search_panel.set(area);
        self.layout_search_input.set(Rect::default());
        self.layout_replace_input.set(Rect::default());

        let focused = self.focus == Focus::Search || self.focus == Focus::Replace;
        let border_style = theme::panel_border_style(focused, theme::screen_accent::FORMS_INPUT);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Search / Replace")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
            ])
            .split(inner);

        // Search row
        if !rows[0].is_empty() {
            let cols = Flex::horizontal()
                .constraints([Constraint::Fixed(10), Constraint::Min(1)])
                .split(rows[0]);
            Paragraph::new("Search:")
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(cols[0], frame);
            Widget::render(&self.search_input, cols[1], frame);
            self.layout_search_input.set(cols[1]);
        }

        // Replace row
        if rows.len() > 1 && !rows[1].is_empty() {
            let cols = Flex::horizontal()
                .constraints([Constraint::Fixed(10), Constraint::Min(1)])
                .split(rows[1]);
            Paragraph::new("Replace:")
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(cols[0], frame);
            Widget::render(&self.replace_input, cols[1], frame);
            self.layout_replace_input.set(cols[1]);
        }

        // Buttons row
        if rows.len() > 2 && !rows[2].is_empty() {
            let match_info = if !self.search_results.is_empty() {
                let current = self.current_match.map_or(0, |i| i + 1);
                format!(
                    "{}/{} | Enter: Next | Shift+Enter: Prev | Ctrl+R: Replace | Ctrl+A: All",
                    current,
                    self.search_results.len()
                )
            } else {
                "Type to search | Enter: Next | Esc: Close".to_string()
            };
            Paragraph::new(match_info)
                .style(theme::muted())
                .render(rows[2], frame);
        }
    }

    fn record_undo(&mut self, description: &str) {
        self.undo_stack.push_back(description.to_string());
        self.redo_stack.clear();

        while self.undo_stack.len() > UNDO_HISTORY_LIMIT {
            self.undo_stack.pop_front();
        }
    }

    fn perform_undo(&mut self) {
        if self.undo_stack.pop_back().is_some() {
            self.editor.undo();
            self.redo_stack.push_back("Redo edit".to_string());

            // Log undo
            let entry = DiagnosticEntry::new(DiagnosticEventKind::UndoPerformed)
                .with_undo_depth(self.undo_stack.len())
                .with_redo_depth(self.redo_stack.len());
            self.log_event(entry);
        }
        self.update_status();
    }

    fn perform_redo(&mut self) {
        if self.redo_stack.pop_back().is_some() {
            self.editor.redo();
            self.undo_stack.push_back("Edit text".to_string());

            // Log redo
            let entry = DiagnosticEntry::new(DiagnosticEventKind::RedoPerformed)
                .with_undo_depth(self.undo_stack.len())
                .with_redo_depth(self.redo_stack.len());
            self.log_event(entry);
        }
        self.update_status();
    }
}

impl Screen for AdvancedTextEditor {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.focus_from_point(mouse.x, mouse.y)
        {
            return Cmd::None;
        }

        // Handle focus switching with Ctrl+Arrow
        if let Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
            && self.search_visible
        {
            let old_focus = self.focus;
            self.focus = self.focus.next();
            self.update_focus_states();

            // Log focus change
            let entry = DiagnosticEntry::new(DiagnosticEventKind::FocusChanged)
                .with_focus(self.focus.as_str())
                .with_context(format!("from {} via Ctrl+Right", old_focus.as_str()));
            self.log_event(entry);

            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Left,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
            && self.search_visible
        {
            let old_focus = self.focus;
            self.focus = self.focus.prev();
            self.update_focus_states();

            // Log focus change
            let entry = DiagnosticEntry::new(DiagnosticEventKind::FocusChanged)
                .with_focus(self.focus.as_str())
                .with_context(format!("from {} via Ctrl+Left", old_focus.as_str()));
            self.log_event(entry);

            return Cmd::None;
        }

        // Tab / Shift+Tab focus cycling when search panel is visible
        if let Event::Key(KeyEvent {
            code: KeyCode::Tab,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && self.search_visible
        {
            let old_focus = self.focus;
            if modifiers.contains(Modifiers::SHIFT) {
                self.focus = self.focus.prev();
            } else {
                self.focus = self.focus.next();
            }
            self.update_focus_states();

            // Log focus change
            let direction = if modifiers.contains(Modifiers::SHIFT) {
                "Shift+Tab"
            } else {
                "Tab"
            };
            let entry = DiagnosticEntry::new(DiagnosticEventKind::FocusChanged)
                .with_focus(self.focus.as_str())
                .with_context(format!("from {} via {direction}", old_focus.as_str()));
            self.log_event(entry);

            return Cmd::None;
        }

        // Global shortcuts
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            if self.undo_keys.undo.matches(*code, *modifiers) {
                self.perform_undo();
                return Cmd::None;
            }
            if self.undo_keys.redo_primary.matches(*code, *modifiers)
                || self.undo_keys.redo_secondary.matches(*code, *modifiers)
            {
                self.perform_redo();
                return Cmd::None;
            }

            let ctrl = modifiers.contains(Modifiers::CTRL);
            let shift = modifiers.contains(Modifiers::SHIFT);

            match (*code, ctrl, shift) {
                // Ctrl+U: Toggle undo history panel
                (KeyCode::Char('u'), true, false) => {
                    self.undo_panel_visible = !self.undo_panel_visible;
                    self.update_status();

                    // Log history panel toggle
                    let entry = DiagnosticEntry::new(DiagnosticEventKind::HistoryPanelToggled)
                        .with_panel_visible(self.undo_panel_visible);
                    self.log_event(entry);

                    return Cmd::None;
                }
                // Ctrl+F: Toggle search panel and focus search input
                (KeyCode::Char('f'), true, false) => {
                    self.search_visible = true;
                    self.focus = Focus::Search;
                    self.update_focus_states();

                    // Log search opened
                    let entry = DiagnosticEntry::new(DiagnosticEventKind::SearchOpened)
                        .with_focus("search")
                        .with_panel_visible(true);
                    self.log_event(entry);

                    return Cmd::None;
                }
                // Ctrl+H: Toggle replace panel
                (KeyCode::Char('h'), true, false) => {
                    self.search_visible = true;
                    self.focus = Focus::Replace;
                    self.update_focus_states();

                    // Log search opened (replace mode)
                    let entry = DiagnosticEntry::new(DiagnosticEventKind::SearchOpened)
                        .with_focus("replace")
                        .with_panel_visible(true);
                    self.log_event(entry);

                    return Cmd::None;
                }
                // Escape: Close search panel if open, or clear selection
                (KeyCode::Escape, false, false) => {
                    if self.search_visible {
                        self.search_visible = false;
                        self.focus = Focus::Editor;
                        self.update_focus_states();

                        // Log search closed
                        let entry = DiagnosticEntry::new(DiagnosticEventKind::SearchClosed)
                            .with_focus("editor")
                            .with_panel_visible(false);
                        self.log_event(entry);
                    } else {
                        self.editor.clear_selection();

                        // Log selection cleared
                        let entry = DiagnosticEntry::new(DiagnosticEventKind::SelectionCleared);
                        self.log_event(entry);
                    }
                    self.update_status();
                    return Cmd::None;
                }
                // F3 or Ctrl+G: Next match
                (KeyCode::F(3), false, false) | (KeyCode::Char('g'), true, false) => {
                    self.next_match();
                    return Cmd::None;
                }
                // Shift+F3 or Ctrl+Shift+G: Previous match
                (KeyCode::F(3), false, true) | (KeyCode::Char('G'), true, true) => {
                    self.prev_match();
                    return Cmd::None;
                }
                _ => {}
            }
        }

        // Route events to focused widget
        match self.focus {
            Focus::Editor => {
                let before = self.editor.text();
                self.editor.handle_event(event);
                let after = self.editor.text();
                if before != after {
                    self.record_undo("Edit text");

                    // Log text edit
                    let cursor = self.editor.cursor();
                    let entry = DiagnosticEntry::new(DiagnosticEventKind::TextEdited)
                        .with_text_len(grapheme_count(&after))
                        .with_cursor(cursor.line, cursor.grapheme)
                        .with_undo_depth(self.undo_stack.len());
                    self.log_event(entry);
                }
                self.update_status();
            }
            Focus::Search => {
                // Handle Enter for next/prev match
                if let Event::Key(KeyEvent {
                    code: KeyCode::Enter,
                    modifiers,
                    kind: KeyEventKind::Press,
                    ..
                }) = event
                {
                    if modifiers.contains(Modifiers::SHIFT) {
                        self.prev_match();
                    } else {
                        self.do_search();
                        self.next_match();
                    }
                    return Cmd::None;
                }
                self.search_input.handle_event(event);
                self.do_search();
            }
            Focus::Replace => {
                // Handle Ctrl+R for replace current, Ctrl+A for replace all
                if let Event::Key(KeyEvent {
                    code,
                    modifiers,
                    kind: KeyEventKind::Press,
                    ..
                }) = event
                {
                    let ctrl = modifiers.contains(Modifiers::CTRL);
                    match (*code, ctrl) {
                        (KeyCode::Char('r'), true) => {
                            self.replace_current();
                            return Cmd::None;
                        }
                        (KeyCode::Char('a'), true) => {
                            self.replace_all();
                            return Cmd::None;
                        }
                        (KeyCode::Enter, false) => {
                            self.replace_current();
                            return Cmd::None;
                        }
                        _ => {}
                    }
                }
                self.replace_input.handle_event(event);
            }
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        self.clear_layout_rects();

        // Layout: editor + optional search panel + status bar
        let main_height = if self.search_visible {
            area.height.saturating_sub(6) // 5 for search panel + 1 for status
        } else {
            area.height.saturating_sub(1) // 1 for status
        };

        let chunks = if self.search_visible {
            Flex::vertical()
                .constraints([
                    Constraint::Fixed(main_height),
                    Constraint::Fixed(5),
                    Constraint::Fixed(1),
                ])
                .split(area)
        } else {
            Flex::vertical()
                .constraints([Constraint::Fixed(main_height), Constraint::Fixed(1)])
                .split(area)
        };

        // Editor panel (optionally split with undo history)
        if self.undo_panel_visible {
            let cols = Flex::horizontal()
                .constraints([Constraint::Min(50), Constraint::Fixed(28)])
                .split(chunks[0]);
            self.render_editor_panel(frame, cols[0]);
            self.render_undo_panel(frame, cols[1]);
        } else {
            self.render_editor_panel(frame, chunks[0]);
        }

        // Search panel (if visible)
        if self.search_visible && chunks.len() > 2 {
            self.render_search_panel(frame, chunks[1]);
        }

        // Status bar
        let status_idx = chunks.len() - 1;
        Paragraph::new(&*self.status)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(chunks[status_idx], frame);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Ctrl+F",
                action: "Search",
            },
            HelpEntry {
                key: "Ctrl+H",
                action: "Replace",
            },
            HelpEntry {
                key: "Ctrl+G / F3",
                action: "Next match",
            },
            HelpEntry {
                key: "Ctrl+Shift+G / Shift+F3",
                action: "Previous match",
            },
            HelpEntry {
                key: "Enter (search)",
                action: "Find next",
            },
            HelpEntry {
                key: "Shift+Enter (search)",
                action: "Find previous",
            },
            HelpEntry {
                key: "Ctrl+Z",
                action: "Undo",
            },
            HelpEntry {
                key: "Ctrl+Y / Ctrl+Shift+Z",
                action: "Redo",
            },
            HelpEntry {
                key: "Ctrl+U",
                action: "Toggle history panel",
            },
            HelpEntry {
                key: "Shift+Arrow",
                action: "Select text",
            },
            HelpEntry {
                key: "Ctrl+A",
                action: "Select all / Replace all",
            },
            HelpEntry {
                key: "Ctrl+R / Enter (replace)",
                action: "Replace current",
            },
            HelpEntry {
                key: "Tab / Shift+Tab",
                action: "Cycle focus (search open)",
            },
            HelpEntry {
                key: "Ctrl+Left/Right",
                action: "Cycle focus (search open)",
            },
            HelpEntry {
                key: "Mouse",
                action: "Click to focus panels",
            },
            HelpEntry {
                key: "Esc",
                action: "Close search / Clear selection",
            },
        ]
    }

    fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn next_undo_description(&self) -> Option<&str> {
        self.undo_stack.back().map(String::as_str)
    }

    fn undo(&mut self) -> bool {
        self.perform_undo();
        true
    }

    fn redo(&mut self) -> bool {
        self.perform_redo();
        true
    }

    fn consumes_text_input(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Advanced Text Editor"
    }

    fn tab_label(&self) -> &'static str {
        "Editor"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::MouseEvent;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::empty(),
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

    fn mouse_event(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent::new(kind, x, y))
    }

    #[test]
    fn initial_state() {
        let screen = AdvancedTextEditor::new();
        assert_eq!(screen.focus, Focus::Editor);
        assert!(!screen.search_visible);
        assert_eq!(screen.title(), "Advanced Text Editor");
        assert_eq!(screen.tab_label(), "Editor");
    }

    #[test]
    fn ctrl_f_opens_search() {
        let mut screen = AdvancedTextEditor::new();
        assert!(!screen.search_visible);

        screen.update(&ctrl_press(KeyCode::Char('f')));
        assert!(screen.search_visible);
        assert_eq!(screen.focus, Focus::Search);
    }

    #[test]
    fn ctrl_h_opens_replace() {
        let mut screen = AdvancedTextEditor::new();
        screen.update(&ctrl_press(KeyCode::Char('h')));
        assert!(screen.search_visible);
        assert_eq!(screen.focus, Focus::Replace);
    }

    #[test]
    fn escape_closes_search() {
        let mut screen = AdvancedTextEditor::new();
        screen.update(&ctrl_press(KeyCode::Char('f')));
        assert!(screen.search_visible);

        screen.update(&press(KeyCode::Escape));
        assert!(!screen.search_visible);
        assert_eq!(screen.focus, Focus::Editor);
    }

    #[test]
    fn ctrl_u_toggles_history_panel() {
        let mut screen = AdvancedTextEditor::new();
        assert!(!screen.undo_panel_visible);

        screen.update(&ctrl_press(KeyCode::Char('u')));
        assert!(screen.undo_panel_visible);

        screen.update(&ctrl_press(KeyCode::Char('u')));
        assert!(!screen.undo_panel_visible);
    }

    #[test]
    fn focus_cycles_with_ctrl_arrows() {
        let mut screen = AdvancedTextEditor::new();
        screen.search_visible = true;
        screen.focus = Focus::Editor;
        screen.update_focus_states();

        // Ctrl+Right cycles forward
        screen.update(&Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        }));
        assert_eq!(screen.focus, Focus::Search);

        screen.update(&Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        }));
        assert_eq!(screen.focus, Focus::Replace);

        screen.update(&Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        }));
        assert_eq!(screen.focus, Focus::Editor);
    }

    #[test]
    fn mouse_click_focuses_panels() {
        let mut screen = AdvancedTextEditor::new();
        screen.search_visible = true;
        screen.update_focus_states();

        screen.layout_editor.set(Rect::new(0, 0, 40, 10));
        screen.layout_search_panel.set(Rect::new(0, 10, 40, 3));
        screen.layout_search_input.set(Rect::new(12, 10, 20, 1));
        screen.layout_replace_input.set(Rect::new(12, 11, 20, 1));

        screen.update(&mouse_event(
            MouseEventKind::Down(MouseButton::Left),
            15,
            10,
        ));
        assert_eq!(screen.focus, Focus::Search);

        screen.update(&mouse_event(
            MouseEventKind::Down(MouseButton::Left),
            15,
            11,
        ));
        assert_eq!(screen.focus, Focus::Replace);

        screen.update(&mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        assert_eq!(screen.focus, Focus::Editor);
    }

    #[test]
    fn undo_redo_updates_history() {
        let mut screen = AdvancedTextEditor::new();
        assert_eq!(screen.undo_stack.len(), 0);
        assert_eq!(screen.redo_stack.len(), 0);

        screen.update(&press(KeyCode::Char('a')));
        assert_eq!(screen.undo_stack.len(), 1);
        assert_eq!(screen.redo_stack.len(), 0);

        screen.update(&ctrl_press(KeyCode::Char('z')));
        assert_eq!(screen.undo_stack.len(), 0);
        assert_eq!(screen.redo_stack.len(), 1);

        screen.update(&ctrl_press(KeyCode::Char('y')));
        assert_eq!(screen.undo_stack.len(), 1);
        assert_eq!(screen.redo_stack.len(), 0);
    }

    #[test]
    fn editor_receives_text() {
        let mut screen = AdvancedTextEditor::new();
        let initial_len = screen.editor.text().len();

        screen.update(&press(KeyCode::Char('X')));
        // Text should change (character inserted)
        let new_len = screen.editor.text().len();
        assert!(new_len != initial_len || screen.editor.text().contains('X'));
    }

    #[test]
    fn search_finds_matches() {
        let mut screen = AdvancedTextEditor::new();
        screen.editor.set_text("hello world hello");
        screen.search_visible = true;
        screen.focus = Focus::Search;
        screen.update_focus_states();

        // Type search query
        for ch in "hello".chars() {
            screen.update(&press(KeyCode::Char(ch)));
        }

        assert_eq!(screen.search_results.len(), 2);
        assert!(screen.current_match.is_some());
    }

    #[test]
    fn keybindings_non_empty() {
        let screen = AdvancedTextEditor::new();
        assert!(!screen.keybindings().is_empty());
    }

    #[test]
    fn default_impl() {
        let screen = AdvancedTextEditor::default();
        assert!(!screen.editor.is_empty());
    }

    #[test]
    fn render_without_panic() {
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = AdvancedTextEditor::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        screen.view(&mut frame, area);
    }

    #[test]
    fn render_with_search_visible() {
        use ftui_render::grapheme_pool::GraphemePool;
        let mut screen = AdvancedTextEditor::new();
        screen.search_visible = true;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        screen.view(&mut frame, area);
    }

    #[test]
    fn replace_all_works() {
        let mut screen = AdvancedTextEditor::new();
        screen.editor.set_text("foo bar foo baz foo");

        // Set up search
        screen.search_visible = true;
        screen.focus = Focus::Search;
        for ch in "foo".chars() {
            screen.search_input.handle_event(&press(KeyCode::Char(ch)));
        }
        screen.do_search();
        assert_eq!(screen.search_results.len(), 3);

        // Set up replacement
        screen.focus = Focus::Replace;
        for ch in "XXX".chars() {
            screen.replace_input.handle_event(&press(KeyCode::Char(ch)));
        }

        // Replace all
        screen.replace_all();
        assert_eq!(screen.editor.text(), "XXX bar XXX baz XXX");
    }

    // =============================================================================
    // Diagnostic Logging Tests (bd-12o8.5)
    // =============================================================================

    #[test]
    fn diagnostic_entry_jsonl_format() {
        reset_event_counter();
        let entry = DiagnosticEntry::new(DiagnosticEventKind::SearchOpened)
            .with_query("test")
            .with_match_count(5)
            .with_focus("search")
            .with_checksum();

        let jsonl = entry.to_jsonl();
        assert!(jsonl.contains("\"kind\":\"search_opened\""));
        assert!(jsonl.contains("\"query\":\"test\""));
        assert!(jsonl.contains("\"match_count\":5"));
        assert!(jsonl.contains("\"focus\":\"search\""));
        assert!(jsonl.contains("\"checksum\":"));
    }

    #[test]
    fn diagnostic_log_records_entries() {
        let mut log = DiagnosticLog::new();
        assert!(log.entries().is_empty());

        log.record(DiagnosticEntry::new(DiagnosticEventKind::SearchOpened));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SearchClosed));

        assert_eq!(log.entries().len(), 3);
        assert_eq!(
            log.entries_of_kind(DiagnosticEventKind::SearchOpened).len(),
            1
        );
        assert_eq!(
            log.entries_of_kind(DiagnosticEventKind::QueryUpdated).len(),
            1
        );
    }

    #[test]
    fn diagnostic_summary_counts() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SearchOpened));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::MatchNavigation));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::ReplacePerformed));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::UndoPerformed));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::UndoPerformed));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::UndoPerformed));

        let summary = log.summary();
        assert_eq!(summary.total_entries, 8);
        assert_eq!(summary.search_opened_count, 1);
        assert_eq!(summary.query_updated_count, 2);
        assert_eq!(summary.match_navigation_count, 1);
        assert_eq!(summary.replace_performed_count, 1);
        assert_eq!(summary.undo_performed_count, 3);
    }

    #[test]
    fn diagnostic_log_to_jsonl() {
        reset_event_counter();
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::TextEdited).with_text_len(100));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::UndoPerformed).with_undo_depth(5));

        let jsonl = log.to_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("text_edited"));
        assert!(lines[1].contains("undo_performed"));
    }

    #[test]
    fn diagnostic_log_max_entries() {
        let mut log = DiagnosticLog::new().with_max_entries(3);

        for _ in 0..5 {
            log.record(DiagnosticEntry::new(DiagnosticEventKind::TextEdited));
        }

        assert_eq!(log.entries().len(), 3);
    }

    #[test]
    fn diagnostic_log_clear() {
        let mut log = DiagnosticLog::new();
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SearchOpened));
        log.record(DiagnosticEntry::new(DiagnosticEventKind::SearchClosed));
        assert_eq!(log.entries().len(), 2);

        log.clear();
        assert!(log.entries().is_empty());
    }

    #[test]
    fn diagnostic_entry_escapes_special_chars() {
        let entry = DiagnosticEntry::new(DiagnosticEventKind::QueryUpdated)
            .with_query("test \"quoted\" \\backslash")
            .with_context("context with \"quotes\"");

        let jsonl = entry.to_jsonl();
        assert!(jsonl.contains("\\\"quoted\\\""));
        assert!(jsonl.contains("\\\\backslash"));
    }

    #[test]
    fn editor_has_diagnostic_log() {
        let screen = AdvancedTextEditor::new();
        // Should have an empty diagnostic log by default (diagnostics disabled)
        assert!(screen.diagnostic_log().entries().is_empty());
    }

    #[test]
    fn diagnostic_summary_jsonl_format() {
        let summary = DiagnosticSummary {
            total_entries: 10,
            search_opened_count: 2,
            search_closed_count: 2,
            query_updated_count: 3,
            match_navigation_count: 1,
            replace_performed_count: 1,
            replace_all_performed_count: 0,
            undo_performed_count: 1,
            redo_performed_count: 0,
            text_edited_count: 0,
            focus_changed_count: 0,
            history_panel_toggled_count: 0,
            selection_cleared_count: 0,
        };

        let jsonl = summary.to_jsonl();
        assert!(jsonl.contains("\"summary\":true"));
        assert!(jsonl.contains("\"total\":10"));
        assert!(jsonl.contains("\"search_opened\":2"));
        assert!(jsonl.contains("\"query_updated\":3"));
    }
}

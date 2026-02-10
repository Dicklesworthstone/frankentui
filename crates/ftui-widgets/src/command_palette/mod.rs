#![forbid(unsafe_code)]

//! Command Palette widget for instant action search.
//!
//! This module provides a fuzzy-search command palette with:
//! - Bayesian match scoring with evidence ledger
//! - Incremental scoring with query-prefix pruning
//! - Word-start, prefix, substring, and fuzzy matching
//! - Conformal rank confidence for tie-break stability
//! - Match position tracking for highlighting
//!
//! # Usage
//!
//! ```ignore
//! let mut palette = CommandPalette::new();
//! palette.register("Open File", Some("Open a file from disk"), &["file", "open"]);
//! palette.register("Save File", Some("Save current file"), &["file", "save"]);
//! palette.open(); // Show the palette
//!
//! // In your update loop, handle events:
//! if let Some(action) = palette.handle_event(event) {
//!     match action {
//!         PaletteAction::Execute(id) => { /* run the action */ }
//!         PaletteAction::Dismiss => { /* palette was closed */ }
//!     }
//! }
//!
//! // Render as a Widget
//! palette.render(area, &mut frame);
//! ```
//!
//! # Submodules
//!
//! - [`scorer`]: Bayesian fuzzy matcher with explainable scoring

pub mod scorer;

pub use scorer::{
    BayesianScorer, ConformalRanker, EvidenceKind, EvidenceLedger, IncrementalScorer,
    IncrementalStats, MatchResult, MatchType, RankConfidence, RankStability, RankedItem,
    RankedResults, RankingSummary,
};

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, CellAttrs, CellContent, PackedRgba, StyleFlags as CellStyleFlags};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::{display_width, grapheme_width, graphemes};

use crate::Widget;

#[cfg(feature = "tracing")]
use std::time::Instant;
#[cfg(feature = "tracing")]
use tracing::{debug, info};

#[cfg(feature = "tracing")]
const TELEMETRY_TARGET: &str = "ftui_widgets::command_palette";

#[cfg(feature = "tracing")]
fn emit_palette_opened(action_count: usize, result_count: usize) {
    info!(
        target: TELEMETRY_TARGET,
        event = "palette_opened",
        action_count,
        result_count
    );
}

#[cfg(feature = "tracing")]
fn emit_palette_query_updated(query: &str, match_count: usize, latency_ms: u128) {
    info!(
        target: TELEMETRY_TARGET,
        event = "palette_query_updated",
        query_len = query.len(),
        match_count,
        latency_ms
    );
    if tracing::enabled!(target: TELEMETRY_TARGET, tracing::Level::DEBUG) {
        debug!(
            target: TELEMETRY_TARGET,
            event = "palette_query_text",
            query
        );
    }
}

#[cfg(feature = "tracing")]
fn emit_palette_action_executed(action_id: &str, latency_ms: Option<u128>) {
    if let Some(latency_ms) = latency_ms {
        info!(
            target: TELEMETRY_TARGET,
            event = "palette_action_executed",
            action_id,
            latency_ms
        );
    } else {
        info!(
            target: TELEMETRY_TARGET,
            event = "palette_action_executed",
            action_id
        );
    }
}

#[cfg(feature = "tracing")]
fn emit_palette_closed(reason: PaletteCloseReason) {
    info!(
        target: TELEMETRY_TARGET,
        event = "palette_closed",
        reason = reason.as_str()
    );
}

// ---------------------------------------------------------------------------
// Action Item
// ---------------------------------------------------------------------------

/// A single action that can be invoked from the command palette.
#[derive(Debug, Clone)]
pub struct ActionItem {
    /// Unique identifier for this action.
    pub id: String,
    /// Display title (searched by the scorer).
    pub title: String,
    /// Optional description shown below the title.
    pub description: Option<String>,
    /// Tags for boosting search relevance.
    pub tags: Vec<String>,
    /// Category for visual grouping (e.g., "Git", "File", "View").
    pub category: Option<String>,
}

impl ActionItem {
    /// Create a new action item.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            tags: Vec::new(),
            category: None,
        }
    }

    /// Set description (builder).
    #[must_use]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set tags (builder).
    #[must_use]
    pub fn with_tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|s| (*s).to_string()).collect();
        self
    }

    /// Set category (builder).
    #[must_use]
    pub fn with_category(mut self, cat: impl Into<String>) -> Self {
        self.category = Some(cat.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Palette Action
// ---------------------------------------------------------------------------

/// Action returned from event handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    /// User selected an action to execute (contains the action ID).
    Execute(String),
    /// User dismissed the palette (Esc).
    Dismiss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteCloseReason {
    Dismiss,
    Execute,
    Toggle,
    Programmatic,
}

impl PaletteCloseReason {
    #[cfg(feature = "tracing")]
    const fn as_str(self) -> &'static str {
        match self {
            Self::Dismiss => "dismiss",
            Self::Execute => "execute",
            Self::Toggle => "toggle",
            Self::Programmatic => "programmatic",
        }
    }
}

fn compute_word_starts(title_lower: &str) -> Vec<usize> {
    let bytes = title_lower.as_bytes();
    title_lower
        .char_indices()
        .filter_map(|(i, _)| {
            let is_word_start = i == 0 || {
                let prev = bytes.get(i.saturating_sub(1)).copied().unwrap_or(b' ');
                prev == b' ' || prev == b'-' || prev == b'_'
            };
            is_word_start.then_some(i)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Palette Style
// ---------------------------------------------------------------------------

/// Visual styling for the command palette.
#[derive(Debug, Clone)]
pub struct PaletteStyle {
    /// Border style.
    pub border: Style,
    /// Query input style.
    pub input: Style,
    /// Normal result item style.
    pub item: Style,
    /// Selected/highlighted result item style.
    pub item_selected: Style,
    /// Match highlight style (for matched characters).
    pub match_highlight: Style,
    /// Description text style.
    pub description: Style,
    /// Category badge style.
    pub category: Style,
    /// Empty state / hint text style.
    pub hint: Style,
}

impl Default for PaletteStyle {
    fn default() -> Self {
        // Colors chosen for WCAG AA contrast ratios against bg(30,30,40):
        // - item (190,190,200) on bg(30,30,40) ≈ 9.5:1 (AAA)
        // - selected (255,255,255) on bg(50,50,75) ≈ 8.8:1 (AAA)
        // - highlight (255,210,60) on bg(30,30,40) ≈ 11:1 (AAA)
        // - description (140,140,160) on bg(30,30,40) ≈ 5.2:1 (AA)
        Self {
            border: Style::new().fg(PackedRgba::rgb(100, 100, 120)),
            input: Style::new().fg(PackedRgba::rgb(220, 220, 230)),
            item: Style::new().fg(PackedRgba::rgb(190, 190, 200)),
            item_selected: Style::new()
                .fg(PackedRgba::rgb(255, 255, 255))
                .bg(PackedRgba::rgb(50, 50, 75)),
            match_highlight: Style::new().fg(PackedRgba::rgb(255, 210, 60)),
            description: Style::new().fg(PackedRgba::rgb(140, 140, 160)),
            category: Style::new().fg(PackedRgba::rgb(100, 180, 255)),
            hint: Style::new().fg(PackedRgba::rgb(100, 100, 120)),
        }
    }
}

// ---------------------------------------------------------------------------
// Scored Item (internal)
// ---------------------------------------------------------------------------

/// Internal: a scored result with corpus index.
#[derive(Debug)]
struct ScoredItem {
    /// Index into the actions vec.
    action_index: usize,
    /// Match result from scorer.
    result: MatchResult,
}

// ---------------------------------------------------------------------------
// Public Result View
// ---------------------------------------------------------------------------

/// Read-only view of a scored palette item.
#[derive(Debug, Clone, Copy)]
pub struct PaletteMatch<'a> {
    /// Action metadata.
    pub action: &'a ActionItem,
    /// Match result (score, match type, evidence).
    pub result: &'a MatchResult,
}

// ---------------------------------------------------------------------------
// Match Filter
// ---------------------------------------------------------------------------

/// Optional match-type filter for palette results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchFilter {
    /// Show all matches.
    All,
    /// Exact match only.
    Exact,
    /// Prefix match only.
    Prefix,
    /// Word-start match only.
    WordStart,
    /// Substring match only.
    Substring,
    /// Fuzzy match only.
    Fuzzy,
}

impl MatchFilter {
    fn allows(self, match_type: MatchType) -> bool {
        matches!(
            (self, match_type),
            (Self::All, _)
                | (Self::Exact, MatchType::Exact)
                | (Self::Prefix, MatchType::Prefix)
                | (Self::WordStart, MatchType::WordStart)
                | (Self::Substring, MatchType::Substring)
                | (Self::Fuzzy, MatchType::Fuzzy)
        )
    }
}

// ---------------------------------------------------------------------------
// Command Palette Widget
// ---------------------------------------------------------------------------

/// Command palette widget for instant action search.
///
/// Provides a fuzzy-search overlay with keyboard navigation, match highlighting,
/// and incremental scoring for responsive keystroke handling.
///
/// # Invariants
///
/// 1. `selected` is always < `filtered.len()` (or 0 when empty).
/// 2. Results are sorted by descending score with stable tie-breaking.
/// 3. Query changes trigger incremental re-scoring (not full rescan)
///    when the query extends the previous one.
#[derive(Debug)]
pub struct CommandPalette {
    /// Registered actions.
    actions: Vec<ActionItem>,
    /// Cached titles for scoring (avoids per-keystroke Vec allocation).
    titles_cache: Vec<String>,
    /// Cached lowercased titles for scoring.
    titles_lower: Vec<String>,
    /// Cached word-start positions for each lowercased title.
    titles_word_starts: Vec<Vec<usize>>,
    /// Current query text.
    query: String,
    /// Cursor position in the query (byte offset for simplicity).
    cursor: usize,
    /// Currently selected index in filtered results.
    selected: usize,
    /// Scroll offset for visible window.
    scroll_offset: usize,
    /// Whether the palette is visible.
    visible: bool,
    /// Visual styling.
    style: PaletteStyle,
    /// Incremental scorer for fast keystroke handling.
    scorer: IncrementalScorer,
    /// Current filtered results.
    filtered: Vec<ScoredItem>,
    /// Optional match-type filter.
    match_filter: MatchFilter,
    /// Generation counter for corpus invalidation.
    generation: u64,
    /// Maximum visible results.
    max_visible: usize,
    /// Telemetry timing anchor (only when tracing feature is enabled).
    #[cfg(feature = "tracing")]
    opened_at: Option<Instant>,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    /// Create a new empty command palette.
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            titles_cache: Vec::new(),
            titles_lower: Vec::new(),
            titles_word_starts: Vec::new(),
            query: String::new(),
            cursor: 0,
            selected: 0,
            scroll_offset: 0,
            visible: false,
            style: PaletteStyle::default(),
            scorer: IncrementalScorer::new(),
            filtered: Vec::new(),
            match_filter: MatchFilter::All,
            generation: 0,
            max_visible: 10,
            #[cfg(feature = "tracing")]
            opened_at: None,
        }
    }

    /// Set the visual style (builder).
    #[must_use]
    pub fn with_style(mut self, style: PaletteStyle) -> Self {
        self.style = style;
        self
    }

    /// Set max visible results (builder).
    #[must_use]
    pub fn with_max_visible(mut self, n: usize) -> Self {
        self.max_visible = n;
        self
    }

    /// Enable or disable evidence tracking for match results.
    pub fn enable_evidence_tracking(&mut self, enabled: bool) {
        self.scorer = if enabled {
            IncrementalScorer::with_scorer(BayesianScorer::new())
        } else {
            IncrementalScorer::new()
        };
        self.update_filtered(false);
    }

    // --- Action Registration ---

    fn push_title_cache_into(
        titles_cache: &mut Vec<String>,
        titles_lower: &mut Vec<String>,
        titles_word_starts: &mut Vec<Vec<usize>>,
        title: &str,
    ) {
        titles_cache.push(title.to_string());
        let lower = title.to_lowercase();
        titles_word_starts.push(compute_word_starts(&lower));
        titles_lower.push(lower);
    }

    fn push_title_cache(&mut self, title: &str) {
        Self::push_title_cache_into(
            &mut self.titles_cache,
            &mut self.titles_lower,
            &mut self.titles_word_starts,
            title,
        );
    }

    fn rebuild_title_cache(&mut self) {
        self.titles_cache.clear();
        self.titles_lower.clear();
        self.titles_word_starts.clear();

        self.titles_cache.reserve(self.actions.len());
        self.titles_lower.reserve(self.actions.len());
        self.titles_word_starts.reserve(self.actions.len());

        let titles_cache = &mut self.titles_cache;
        let titles_lower = &mut self.titles_lower;
        let titles_word_starts = &mut self.titles_word_starts;
        for action in &self.actions {
            Self::push_title_cache_into(
                titles_cache,
                titles_lower,
                titles_word_starts,
                &action.title,
            );
        }
    }

    /// Register a new action.
    pub fn register(
        &mut self,
        title: impl Into<String>,
        description: Option<&str>,
        tags: &[&str],
    ) -> &mut Self {
        let title = title.into();
        let id = title.to_lowercase().replace(' ', "_");
        let mut item = ActionItem::new(id, title);
        if let Some(desc) = description {
            item.description = Some(desc.to_string());
        }
        item.tags = tags.iter().map(|s| (*s).to_string()).collect();
        self.push_title_cache(&item.title);
        self.actions.push(item);
        self.generation = self.generation.wrapping_add(1);
        self
    }

    /// Register an action item directly.
    pub fn register_action(&mut self, action: ActionItem) -> &mut Self {
        self.push_title_cache(&action.title);
        self.actions.push(action);
        self.generation = self.generation.wrapping_add(1);
        self
    }

    /// Replace all actions with a new list.
    ///
    /// This resets caches and refreshes the filtered results.
    pub fn replace_actions(&mut self, actions: Vec<ActionItem>) {
        self.actions = actions;
        self.rebuild_title_cache();
        self.generation = self.generation.wrapping_add(1);
        self.scorer.invalidate();
        self.selected = 0;
        self.scroll_offset = 0;
        self.update_filtered(false);
    }

    /// Clear all registered actions.
    pub fn clear_actions(&mut self) {
        self.replace_actions(Vec::new());
    }

    /// Number of registered actions.
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }

    // --- Visibility ---

    /// Open the palette (show it and focus the query input).
    pub fn open(&mut self) {
        self.visible = true;
        self.query.clear();
        self.cursor = 0;
        self.selected = 0;
        self.scroll_offset = 0;
        self.scorer.invalidate();
        #[cfg(feature = "tracing")]
        {
            self.opened_at = Some(Instant::now());
        }
        self.update_filtered(false);
        #[cfg(feature = "tracing")]
        #[cfg(test)]
        tracing::callsite::rebuild_interest_cache();
        #[cfg(feature = "tracing")]
        emit_palette_opened(self.actions.len(), self.filtered.len());
    }

    /// Close the palette.
    pub fn close(&mut self) {
        self.close_with_reason(PaletteCloseReason::Programmatic);
    }

    /// Toggle visibility.
    pub fn toggle(&mut self) {
        if self.visible {
            self.close_with_reason(PaletteCloseReason::Toggle);
        } else {
            self.open();
        }
    }

    /// Whether the palette is currently visible.
    #[inline]
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    // --- Query Access ---

    /// Current query string.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replace the query string and re-run filtering.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.cursor = self.query.len();
        self.selected = 0;
        self.scroll_offset = 0;
        self.scorer.invalidate();
        self.update_filtered(false);
    }

    /// Number of filtered results.
    pub fn result_count(&self) -> usize {
        self.filtered.len()
    }

    /// Currently selected index.
    #[inline]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Get the currently selected action, if any.
    #[must_use = "use the selected action (if any)"]
    pub fn selected_action(&self) -> Option<&ActionItem> {
        self.filtered
            .get(self.selected)
            .map(|si| &self.actions[si.action_index])
    }

    /// Read-only access to the selected match (action + result).
    #[must_use = "use the selected match (if any)"]
    pub fn selected_match(&self) -> Option<PaletteMatch<'_>> {
        self.filtered.get(self.selected).map(|si| PaletteMatch {
            action: &self.actions[si.action_index],
            result: &si.result,
        })
    }

    /// Iterate over the current filtered results.
    pub fn results(&self) -> impl Iterator<Item = PaletteMatch<'_>> {
        self.filtered.iter().map(|si| PaletteMatch {
            action: &self.actions[si.action_index],
            result: &si.result,
        })
    }

    /// Set a match-type filter and refresh results.
    pub fn set_match_filter(&mut self, filter: MatchFilter) {
        if self.match_filter == filter {
            return;
        }
        self.match_filter = filter;
        self.selected = 0;
        self.scroll_offset = 0;
        self.update_filtered(false);
    }

    /// Get scorer statistics for diagnostics.
    pub fn scorer_stats(&self) -> &IncrementalStats {
        self.scorer.stats()
    }

    // --- Event Handling ---

    /// Handle an input event. Returns a `PaletteAction` if the user executed
    /// or dismissed the palette.
    ///
    /// Returns `None` if the event was consumed but no action was triggered,
    /// or if the palette is not visible.
    #[must_use = "use the returned action (if any) to execute or dismiss"]
    pub fn handle_event(&mut self, event: &Event) -> Option<PaletteAction> {
        if !self.visible {
            // Check for open shortcut (Ctrl+P)
            if let Event::Key(KeyEvent {
                code: KeyCode::Char('p'),
                modifiers,
                kind: KeyEventKind::Press,
            }) = event
                && modifiers.contains(Modifiers::CTRL)
            {
                self.open();
            }
            return None;
        }

        match event {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press,
            }) => self.handle_key(*code, *modifiers),
            _ => None,
        }
    }

    /// Handle a key press while the palette is open.
    fn handle_key(&mut self, code: KeyCode, modifiers: Modifiers) -> Option<PaletteAction> {
        match code {
            KeyCode::Escape => {
                self.close_with_reason(PaletteCloseReason::Dismiss);
                return Some(PaletteAction::Dismiss);
            }

            KeyCode::Enter => {
                if let Some(si) = self.filtered.get(self.selected) {
                    let id = self.actions[si.action_index].id.clone();
                    #[cfg(feature = "tracing")]
                    {
                        let latency_ms = self.opened_at.map(|start| start.elapsed().as_millis());
                        #[cfg(test)]
                        tracing::callsite::rebuild_interest_cache();
                        emit_palette_action_executed(&id, latency_ms);
                    }
                    self.close_with_reason(PaletteCloseReason::Execute);
                    return Some(PaletteAction::Execute(id));
                }
            }

            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.adjust_scroll();
                }
            }

            KeyCode::Down => {
                if !self.filtered.is_empty() && self.selected < self.filtered.len() - 1 {
                    self.selected += 1;
                    self.adjust_scroll();
                }
            }

            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(self.max_visible);
                self.adjust_scroll();
            }

            KeyCode::PageDown => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + self.max_visible).min(self.filtered.len() - 1);
                    self.adjust_scroll();
                }
            }

            KeyCode::Home => {
                self.selected = 0;
                self.scroll_offset = 0;
            }

            KeyCode::End => {
                if !self.filtered.is_empty() {
                    self.selected = self.filtered.len() - 1;
                    self.adjust_scroll();
                }
            }

            KeyCode::Backspace => {
                if !self.query.is_empty() {
                    // Remove last character
                    self.query.pop();
                    self.cursor = self.query.len();
                    self.selected = 0;
                    self.scroll_offset = 0;
                    self.update_filtered(true);
                }
            }

            KeyCode::Char(c) => {
                if modifiers.contains(Modifiers::CTRL) {
                    // Ctrl+A: select all (move cursor to start)
                    if c == 'a' {
                        self.cursor = 0;
                    }
                    // Ctrl+U: clear query
                    if c == 'u' {
                        self.query.clear();
                        self.cursor = 0;
                        self.selected = 0;
                        self.scroll_offset = 0;
                        self.update_filtered(true);
                    }
                } else {
                    self.query.push(c);
                    self.cursor = self.query.len();
                    self.selected = 0;
                    self.scroll_offset = 0;
                    self.update_filtered(true);
                }
            }

            _ => {}
        }

        None
    }

    /// Re-score the corpus against the current query.
    fn update_filtered(&mut self, _emit_telemetry: bool) {
        #[cfg(feature = "tracing")]
        // Don't gate on `tracing::enabled!` here: callsite interest can be cached across
        // dynamic subscribers (tests), which may suppress telemetry unexpectedly.
        // Emitting the event is already cheap when disabled.
        let start = _emit_telemetry.then(Instant::now);

        if self.titles_cache.len() != self.actions.len()
            || self.titles_lower.len() != self.actions.len()
            || self.titles_word_starts.len() != self.actions.len()
        {
            self.rebuild_title_cache();
        }

        let results = self.scorer.score_corpus_with_lowered_and_words(
            &self.query,
            &self.titles_cache,
            &self.titles_lower,
            &self.titles_word_starts,
            Some(self.generation),
        );

        self.filtered = results
            .into_iter()
            .filter(|(_, result)| self.match_filter.allows(result.match_type))
            .map(|(idx, result)| ScoredItem {
                action_index: idx,
                result,
            })
            .collect();

        // Clamp selection.
        if !self.filtered.is_empty() {
            self.selected = self.selected.min(self.filtered.len() - 1);
        } else {
            self.selected = 0;
        }

        #[cfg(feature = "tracing")]
        if let Some(start) = start {
            #[cfg(test)]
            tracing::callsite::rebuild_interest_cache();
            emit_palette_query_updated(
                &self.query,
                self.filtered.len(),
                start.elapsed().as_millis(),
            );
        }
    }

    fn close_with_reason(&mut self, _reason: PaletteCloseReason) {
        self.visible = false;
        self.query.clear();
        self.cursor = 0;
        self.filtered.clear();
        #[cfg(feature = "tracing")]
        {
            self.opened_at = None;
            #[cfg(test)]
            tracing::callsite::rebuild_interest_cache();
            emit_palette_closed(_reason);
        }
    }

    /// Adjust scroll_offset to keep selected item visible.
    fn adjust_scroll(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.max_visible {
            self.scroll_offset = self.selected + 1 - self.max_visible;
        }
    }
}

// ---------------------------------------------------------------------------
// Widget Implementation
// ---------------------------------------------------------------------------

impl Widget for CommandPalette {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if !self.visible || area.width < 10 || area.height < 5 {
            return;
        }

        // Calculate palette dimensions: centered, ~60% width, height based on results.
        let palette_width = (area.width * 3 / 5).max(30).min(area.width - 2);
        let result_rows = self.filtered.len().min(self.max_visible);
        // +3 for: border top, query line, border bottom. +1 if empty hint.
        let palette_height = (result_rows as u16 + 3)
            .max(5)
            .min(area.height.saturating_sub(2));
        let palette_x = area.x + (area.width.saturating_sub(palette_width)) / 2;
        let palette_y = area.y + area.height / 6; // ~1/6 from top

        let palette_area = Rect::new(palette_x, palette_y, palette_width, palette_height);

        // Clear the palette area.
        self.clear_area(palette_area, frame);

        // Draw border.
        self.draw_border(palette_area, frame);

        // Draw query input line.
        let input_area = Rect::new(
            palette_area.x + 2,
            palette_area.y + 1,
            palette_area.width.saturating_sub(4),
            1,
        );
        self.draw_query_input(input_area, frame);

        // Draw results list.
        let results_y = palette_area.y + 2;
        let results_height = palette_area.height.saturating_sub(3);
        let results_area = Rect::new(
            palette_area.x + 1,
            results_y,
            palette_area.width.saturating_sub(2),
            results_height,
        );
        self.draw_results(results_area, frame);

        // Position cursor in query input.
        // Calculate visual cursor position from byte offset by computing display width
        // of the text up to the cursor position.
        let cursor_visual_pos = display_width(&self.query[..self.cursor.min(self.query.len())]);
        let cursor_x = input_area.x + cursor_visual_pos.min(input_area.width as usize) as u16;
        frame.cursor_position = Some((cursor_x, input_area.y));
        frame.cursor_visible = true;
    }

    fn is_essential(&self) -> bool {
        true
    }
}

impl CommandPalette {
    /// Clear the palette area with a background color.
    fn clear_area(&self, area: Rect, frame: &mut Frame) {
        let bg = PackedRgba::rgb(30, 30, 40);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = frame.buffer.get_mut(x, y) {
                    *cell = Cell::from_char(' ');
                    cell.bg = bg;
                }
            }
        }
    }

    /// Draw a simple border around the palette.
    fn draw_border(&self, area: Rect, frame: &mut Frame) {
        let border_fg = self
            .style
            .border
            .fg
            .unwrap_or(PackedRgba::rgb(100, 100, 120));
        let bg = PackedRgba::rgb(30, 30, 40);

        // Top border with title.
        if let Some(cell) = frame.buffer.get_mut(area.x, area.y) {
            cell.content = CellContent::from_char('┌');
            cell.fg = border_fg;
            cell.bg = bg;
        }
        for x in (area.x + 1)..area.right().saturating_sub(1) {
            if let Some(cell) = frame.buffer.get_mut(x, area.y) {
                cell.content = CellContent::from_char('─');
                cell.fg = border_fg;
                cell.bg = bg;
            }
        }
        if area.width > 1
            && let Some(cell) = frame.buffer.get_mut(area.right() - 1, area.y)
        {
            cell.content = CellContent::from_char('┐');
            cell.fg = border_fg;
            cell.bg = bg;
        }

        // Title "Command Palette" in top border.
        let title = " Command Palette ";
        let title_width = display_width(title).min(area.width as usize);
        let title_x = area.x + (area.width.saturating_sub(title_width as u16)) / 2;
        let title_style = Style::new().fg(PackedRgba::rgb(200, 200, 220)).bg(bg);
        crate::draw_text_span(frame, title_x, area.y, title, title_style, area.right());

        // Side borders.
        for y in (area.y + 1)..area.bottom().saturating_sub(1) {
            if let Some(cell) = frame.buffer.get_mut(area.x, y) {
                cell.content = CellContent::from_char('│');
                cell.fg = border_fg;
                cell.bg = bg;
            }
            if area.width > 1
                && let Some(cell) = frame.buffer.get_mut(area.right() - 1, y)
            {
                cell.content = CellContent::from_char('│');
                cell.fg = border_fg;
                cell.bg = bg;
            }
        }

        // Bottom border.
        if area.height > 1 {
            let by = area.bottom() - 1;
            if let Some(cell) = frame.buffer.get_mut(area.x, by) {
                cell.content = CellContent::from_char('└');
                cell.fg = border_fg;
                cell.bg = bg;
            }
            for x in (area.x + 1)..area.right().saturating_sub(1) {
                if let Some(cell) = frame.buffer.get_mut(x, by) {
                    cell.content = CellContent::from_char('─');
                    cell.fg = border_fg;
                    cell.bg = bg;
                }
            }
            if area.width > 1
                && let Some(cell) = frame.buffer.get_mut(area.right() - 1, by)
            {
                cell.content = CellContent::from_char('┘');
                cell.fg = border_fg;
                cell.bg = bg;
            }
        }
    }

    /// Draw the query input line with prompt.
    fn draw_query_input(&self, area: Rect, frame: &mut Frame) {
        let input_fg = self
            .style
            .input
            .fg
            .unwrap_or(PackedRgba::rgb(220, 220, 230));
        let bg = PackedRgba::rgb(30, 30, 40);
        let prompt_fg = PackedRgba::rgb(100, 180, 255);

        // Draw ">" prompt.
        if let Some(cell) = frame.buffer.get_mut(area.x.saturating_sub(1), area.y) {
            cell.content = CellContent::from_char('>');
            cell.fg = prompt_fg;
            cell.bg = bg;
        }

        // Draw query text.
        if self.query.is_empty() {
            // Placeholder.
            let hint = "Type to search...";
            let hint_fg = self.style.hint.fg.unwrap_or(PackedRgba::rgb(100, 100, 120));
            for (i, ch) in hint.chars().enumerate() {
                let x = area.x + i as u16;
                if x >= area.right() {
                    break;
                }
                if let Some(cell) = frame.buffer.get_mut(x, area.y) {
                    cell.content = CellContent::from_char(ch);
                    cell.fg = hint_fg;
                    cell.bg = bg;
                }
            }
        } else {
            // Render query text with proper grapheme/width handling.
            let mut col = area.x;
            for grapheme in graphemes(&self.query) {
                let w = grapheme_width(grapheme);
                if w == 0 {
                    continue;
                }
                if col >= area.right() {
                    break;
                }
                if col.saturating_add(w as u16) > area.right() {
                    break;
                }
                let content = if w > 1 || grapheme.chars().count() > 1 {
                    let id = frame.intern_with_width(grapheme, w as u8);
                    CellContent::from_grapheme(id)
                } else if let Some(ch) = grapheme.chars().next() {
                    CellContent::from_char(ch)
                } else {
                    continue;
                };
                if let Some(cell) = frame.buffer.get_mut(col, area.y) {
                    cell.content = content;
                    cell.fg = input_fg;
                    cell.bg = bg;
                }
                col = col.saturating_add(w as u16);
            }
        }
    }

    /// Draw the filtered results list.
    fn draw_results(&self, area: Rect, frame: &mut Frame) {
        if self.filtered.is_empty() {
            // Empty state.
            let msg = if self.query.is_empty() {
                "No actions registered"
            } else {
                "No results"
            };
            let hint_fg = self.style.hint.fg.unwrap_or(PackedRgba::rgb(100, 100, 120));
            let bg = PackedRgba::rgb(30, 30, 40);
            for (i, ch) in msg.chars().enumerate() {
                let x = area.x + 1 + i as u16;
                if x >= area.right() {
                    break;
                }
                if let Some(cell) = frame.buffer.get_mut(x, area.y) {
                    cell.content = CellContent::from_char(ch);
                    cell.fg = hint_fg;
                    cell.bg = bg;
                }
            }
            return;
        }

        let item_fg = self.style.item.fg.unwrap_or(PackedRgba::rgb(180, 180, 190));
        let selected_fg = self
            .style
            .item_selected
            .fg
            .unwrap_or(PackedRgba::rgb(255, 255, 255));
        let selected_bg = self
            .style
            .item_selected
            .bg
            .unwrap_or(PackedRgba::rgb(60, 60, 80));
        let highlight_fg = self
            .style
            .match_highlight
            .fg
            .unwrap_or(PackedRgba::rgb(255, 200, 50));
        let desc_fg = self
            .style
            .description
            .fg
            .unwrap_or(PackedRgba::rgb(120, 120, 140));
        let cat_fg = self
            .style
            .category
            .fg
            .unwrap_or(PackedRgba::rgb(100, 180, 255));
        let bg = PackedRgba::rgb(30, 30, 40);

        let visible_end = (self.scroll_offset + area.height as usize).min(self.filtered.len());

        for (row_idx, si) in self.filtered[self.scroll_offset..visible_end]
            .iter()
            .enumerate()
        {
            let y = area.y + row_idx as u16;
            if y >= area.bottom() {
                break;
            }

            let action = &self.actions[si.action_index];
            let is_selected = (self.scroll_offset + row_idx) == self.selected;

            let row_fg = if is_selected { selected_fg } else { item_fg };
            let row_bg = if is_selected { selected_bg } else { bg };

            // Bold attribute for selected row (accessible without color).
            let row_attrs = if is_selected {
                CellAttrs::new(CellStyleFlags::BOLD, 0)
            } else {
                CellAttrs::default()
            };

            // Clear row.
            for x in area.x..area.right() {
                if let Some(cell) = frame.buffer.get_mut(x, y) {
                    cell.content = CellContent::from_char(' ');
                    cell.fg = row_fg;
                    cell.bg = row_bg;
                    cell.attrs = row_attrs;
                }
            }

            // Selection marker (visible without color — structural indicator).
            let mut col = area.x;
            if is_selected && let Some(cell) = frame.buffer.get_mut(col, y) {
                cell.content = CellContent::from_char('>');
                cell.fg = highlight_fg;
                cell.bg = row_bg;
                cell.attrs = CellAttrs::new(CellStyleFlags::BOLD, 0);
            }
            col += 2;

            // Category badge (if present).
            if let Some(ref cat) = action.category {
                let badge = format!("[{}] ", cat);
                for grapheme in graphemes(&badge) {
                    let w = grapheme_width(grapheme);
                    if w == 0 {
                        continue;
                    }
                    if col >= area.right() || col.saturating_add(w as u16) > area.right() {
                        break;
                    }
                    let content = if w > 1 || grapheme.chars().count() > 1 {
                        let id = frame.intern_with_width(grapheme, w as u8);
                        CellContent::from_grapheme(id)
                    } else if let Some(ch) = grapheme.chars().next() {
                        CellContent::from_char(ch)
                    } else {
                        continue;
                    };
                    let mut cell = Cell::new(content);
                    cell.fg = cat_fg;
                    cell.bg = row_bg;
                    cell.attrs = row_attrs;
                    frame.buffer.set_fast(col, y, cell);
                    col = col.saturating_add(w as u16);
                }
            }

            // Title with match highlighting and ellipsis truncation.
            let title_max_width = area.right().saturating_sub(col) as usize;
            let title_width = display_width(action.title.as_str());
            let needs_ellipsis = title_width > title_max_width && title_max_width > 3;
            let title_display_width = if needs_ellipsis {
                title_max_width.saturating_sub(1) // leave room for '…'
            } else {
                title_max_width
            };

            let mut title_used_width = 0usize;
            let mut char_idx = 0usize;
            let mut match_cursor = 0usize;
            let match_positions = &si.result.match_positions;
            for grapheme in graphemes(action.title.as_str()) {
                let g_chars = grapheme.chars().count();
                let char_end = char_idx + g_chars;
                while match_cursor < match_positions.len()
                    && match_positions[match_cursor] < char_idx
                {
                    match_cursor += 1;
                }
                let is_match = match_cursor < match_positions.len()
                    && match_positions[match_cursor] < char_end;

                let w = grapheme_width(grapheme);
                if w == 0 {
                    char_idx = char_end;
                    continue;
                }
                if title_used_width + w > title_display_width || col >= area.right() {
                    break;
                }
                if col.saturating_add(w as u16) > area.right() {
                    break;
                }

                let content = if w > 1 || grapheme.chars().count() > 1 {
                    let id = frame.intern_with_width(grapheme, w as u8);
                    CellContent::from_grapheme(id)
                } else if let Some(ch) = grapheme.chars().next() {
                    CellContent::from_char(ch)
                } else {
                    char_idx = char_end;
                    continue;
                };

                let mut cell = Cell::new(content);
                cell.fg = if is_match { highlight_fg } else { row_fg };
                cell.bg = row_bg;
                cell.attrs = row_attrs;
                frame.buffer.set_fast(col, y, cell);

                col = col.saturating_add(w as u16);
                title_used_width += w;
                char_idx = char_end;
            }

            // Ellipsis for truncated titles.
            if needs_ellipsis && col < area.right() {
                if let Some(cell) = frame.buffer.get_mut(col, y) {
                    cell.content = CellContent::from_char('\u{2026}'); // …
                    cell.fg = row_fg;
                    cell.bg = row_bg;
                    cell.attrs = row_attrs;
                }
                col += 1;
            }

            // Description (if space allows, with ellipsis truncation).
            if let Some(ref desc) = action.description {
                col += 2; // gap
                let max_desc_width = area.right().saturating_sub(col) as usize;
                if max_desc_width > 5 {
                    let desc_width = display_width(desc.as_str());
                    let desc_needs_ellipsis = desc_width > max_desc_width && max_desc_width > 3;
                    let desc_display_width = if desc_needs_ellipsis {
                        max_desc_width.saturating_sub(1)
                    } else {
                        max_desc_width
                    };

                    let mut desc_used_width = 0usize;
                    for grapheme in graphemes(desc.as_str()) {
                        let w = grapheme_width(grapheme);
                        if w == 0 {
                            continue;
                        }
                        if desc_used_width + w > desc_display_width || col >= area.right() {
                            break;
                        }
                        if col.saturating_add(w as u16) > area.right() {
                            break;
                        }
                        let content = if w > 1 || grapheme.chars().count() > 1 {
                            let id = frame.intern_with_width(grapheme, w as u8);
                            CellContent::from_grapheme(id)
                        } else if let Some(ch) = grapheme.chars().next() {
                            CellContent::from_char(ch)
                        } else {
                            continue;
                        };
                        let mut cell = Cell::new(content);
                        cell.fg = desc_fg;
                        cell.bg = row_bg;
                        cell.attrs = row_attrs;
                        frame.buffer.set_fast(col, y, cell);
                        col = col.saturating_add(w as u16);
                        desc_used_width += w;
                    }

                    if desc_needs_ellipsis
                        && col < area.right()
                        && let Some(cell) = frame.buffer.get_mut(col, y)
                    {
                        cell.content = CellContent::from_char('\u{2026}');
                        cell.fg = desc_fg;
                        cell.bg = row_bg;
                        cell.attrs = row_attrs;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod widget_tests {
    use super::*;

    #[test]
    fn new_palette_is_hidden() {
        let palette = CommandPalette::new();
        assert!(!palette.is_visible());
        assert_eq!(palette.action_count(), 0);
    }

    #[test]
    fn register_actions() {
        let mut palette = CommandPalette::new();
        palette.register("Open File", Some("Open a file"), &["file"]);
        palette.register("Save File", None, &[]);
        assert_eq!(palette.action_count(), 2);
    }

    #[test]
    fn open_shows_all_actions() {
        let mut palette = CommandPalette::new();
        palette.register("Open File", None, &[]);
        palette.register("Save File", None, &[]);
        palette.register("Close Tab", None, &[]);
        palette.open();
        assert!(palette.is_visible());
        assert_eq!(palette.result_count(), 3);
    }

    #[test]
    fn close_hides_palette() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert!(palette.is_visible());
        palette.close();
        assert!(!palette.is_visible());
    }

    #[test]
    fn toggle_visibility() {
        let mut palette = CommandPalette::new();
        palette.toggle();
        assert!(palette.is_visible());
        palette.toggle();
        assert!(!palette.is_visible());
    }

    #[test]
    fn typing_filters_results() {
        let mut palette = CommandPalette::new();
        palette.register("Open File", None, &[]);
        palette.register("Save File", None, &[]);
        palette.register("Git: Commit", None, &[]);
        palette.open();
        assert_eq!(palette.result_count(), 3);

        // Type "git"
        let g = Event::Key(KeyEvent {
            code: KeyCode::Char('g'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let i = Event::Key(KeyEvent {
            code: KeyCode::Char('i'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let t = Event::Key(KeyEvent {
            code: KeyCode::Char('t'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        palette.handle_event(&g);
        palette.handle_event(&i);
        palette.handle_event(&t);

        assert_eq!(palette.query(), "git");
        // Only "Git: Commit" should match well
        assert!(palette.result_count() >= 1);
    }

    #[test]
    fn backspace_removes_character() {
        let mut palette = CommandPalette::new();
        palette.register("Open File", None, &[]);
        palette.open();

        let o = Event::Key(KeyEvent {
            code: KeyCode::Char('o'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let bs = Event::Key(KeyEvent {
            code: KeyCode::Backspace,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        palette.handle_event(&o);
        assert_eq!(palette.query(), "o");
        palette.handle_event(&bs);
        assert_eq!(palette.query(), "");
    }

    #[test]
    fn esc_dismisses_palette() {
        let mut palette = CommandPalette::new();
        palette.open();

        let esc = Event::Key(KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        let result = palette.handle_event(&esc);
        assert_eq!(result, Some(PaletteAction::Dismiss));
        assert!(!palette.is_visible());
    }

    #[test]
    fn enter_executes_selected() {
        let mut palette = CommandPalette::new();
        palette.register("Open File", None, &[]);
        palette.open();

        let enter = Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        let result = palette.handle_event(&enter);
        assert_eq!(result, Some(PaletteAction::Execute("open_file".into())));
    }

    #[test]
    fn arrow_keys_navigate() {
        let mut palette = CommandPalette::new();
        palette.register("A", None, &[]);
        palette.register("B", None, &[]);
        palette.register("C", None, &[]);
        palette.open();

        assert_eq!(palette.selected_index(), 0);

        let down = Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let up = Event::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        palette.handle_event(&down);
        assert_eq!(palette.selected_index(), 1);
        palette.handle_event(&down);
        assert_eq!(palette.selected_index(), 2);
        // Can't go past end
        palette.handle_event(&down);
        assert_eq!(palette.selected_index(), 2);

        palette.handle_event(&up);
        assert_eq!(palette.selected_index(), 1);
        palette.handle_event(&up);
        assert_eq!(palette.selected_index(), 0);
        // Can't go below 0
        palette.handle_event(&up);
        assert_eq!(palette.selected_index(), 0);
    }

    #[test]
    fn home_end_navigation() {
        let mut palette = CommandPalette::new();
        for i in 0..20 {
            palette.register(format!("Action {}", i), None, &[]);
        }
        palette.open();

        let end = Event::Key(KeyEvent {
            code: KeyCode::End,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let home = Event::Key(KeyEvent {
            code: KeyCode::Home,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        palette.handle_event(&end);
        assert_eq!(palette.selected_index(), 19);

        palette.handle_event(&home);
        assert_eq!(palette.selected_index(), 0);
    }

    #[test]
    fn ctrl_u_clears_query() {
        let mut palette = CommandPalette::new();
        palette.register("Open File", None, &[]);
        palette.open();

        let o = Event::Key(KeyEvent {
            code: KeyCode::Char('o'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&o);
        assert_eq!(palette.query(), "o");

        let ctrl_u = Event::Key(KeyEvent {
            code: KeyCode::Char('u'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&ctrl_u);
        assert_eq!(palette.query(), "");
    }

    #[test]
    fn ctrl_p_opens_palette() {
        let mut palette = CommandPalette::new();
        assert!(!palette.is_visible());

        let ctrl_p = Event::Key(KeyEvent {
            code: KeyCode::Char('p'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&ctrl_p);
        assert!(palette.is_visible());
    }

    #[test]
    fn selected_action_returns_correct_item() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.register("Beta", None, &[]);
        palette.open();

        let action = palette.selected_action().unwrap();
        // With empty query, all items shown — first by score (neutral, so first registered)
        assert!(!action.title.is_empty());
    }

    #[test]
    fn register_action_item_directly() {
        let mut palette = CommandPalette::new();
        let item = ActionItem::new("custom_id", "Custom Action")
            .with_description("A custom action")
            .with_tags(&["custom", "test"])
            .with_category("Testing");

        palette.register_action(item);
        assert_eq!(palette.action_count(), 1);
    }

    #[test]
    fn replace_actions_refreshes_results() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.register("Beta", None, &[]);
        palette.open();
        palette.set_query("Beta");
        assert_eq!(
            palette.selected_action().map(|a| a.title.as_str()),
            Some("Beta")
        );

        let actions = vec![
            ActionItem::new("gamma", "Gamma"),
            ActionItem::new("delta", "Delta"),
        ];
        palette.replace_actions(actions);
        palette.set_query("Delta");
        assert_eq!(
            palette.selected_action().map(|a| a.title.as_str()),
            Some("Delta")
        );
    }

    #[test]
    fn clear_actions_resets_results() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.register("Beta", None, &[]);
        palette.open();
        palette.set_query("Alpha");
        assert!(palette.selected_action().is_some());

        palette.clear_actions();
        assert_eq!(palette.action_count(), 0);
        assert!(palette.selected_action().is_none());
    }

    #[test]
    fn set_query_refilters() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.register("Beta", None, &[]);
        palette.open();
        palette.set_query("Alpha");
        assert_eq!(palette.query(), "Alpha");
        assert_eq!(
            palette.selected_action().map(|a| a.title.as_str()),
            Some("Alpha")
        );
        palette.set_query("Beta");
        assert_eq!(palette.query(), "Beta");
        assert_eq!(
            palette.selected_action().map(|a| a.title.as_str()),
            Some("Beta")
        );
    }

    #[test]
    fn events_ignored_when_hidden() {
        let mut palette = CommandPalette::new();
        // Not Ctrl+P, so should be ignored
        let a = Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        assert!(palette.handle_event(&a).is_none());
        assert!(!palette.is_visible());
    }

    // -----------------------------------------------------------------------
    // Accessibility / UX tests (bd-39y4.10)
    // -----------------------------------------------------------------------

    #[test]
    fn selected_row_has_bold_attribute() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.register("Beta", None, &[]);
        palette.open();

        let area = Rect::from_size(60, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 10, &mut pool);
        palette.render(area, &mut frame);

        // The selected row (first result) should have bold cells.
        // Results start at y=2 inside the palette (after border + query).
        let palette_y = area.y + area.height / 6;
        let result_y = palette_y + 2;

        // Check that at least one cell in the first result row is bold
        let mut found_bold = false;
        for x in 0..60u16 {
            if let Some(cell) = frame.buffer.get(x, result_y)
                && cell.attrs.flags().contains(CellStyleFlags::BOLD)
            {
                found_bold = true;
                break;
            }
        }
        assert!(
            found_bold,
            "Selected row should have bold attribute for accessibility"
        );
    }

    #[test]
    fn selection_marker_visible() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();

        let area = Rect::from_size(60, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 10, &mut pool);
        palette.render(area, &mut frame);

        // Find the '>' selection marker in the results area
        let palette_y = area.y + area.height / 6;
        let result_y = palette_y + 2;
        let mut found_marker = false;
        for x in 0..60u16 {
            if let Some(cell) = frame.buffer.get(x, result_y)
                && cell.content.as_char() == Some('>')
            {
                found_marker = true;
                break;
            }
        }
        assert!(
            found_marker,
            "Selection marker '>' should be visible (color-independent indicator)"
        );
    }

    #[test]
    fn long_title_truncated_with_ellipsis() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new().with_max_visible(5);
        palette.register(
            "This Is A Very Long Action Title That Should Be Truncated With Ellipsis",
            None,
            &[],
        );
        palette.open();

        // Render in a narrow area
        let area = Rect::from_size(40, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        palette.render(area, &mut frame);

        // Find the ellipsis character '…' in the results area
        let palette_y = area.y + area.height / 6;
        let result_y = palette_y + 2;
        let mut found_ellipsis = false;
        for x in 0..40u16 {
            if let Some(cell) = frame.buffer.get(x, result_y)
                && cell.content.as_char() == Some('\u{2026}')
            {
                found_ellipsis = true;
                break;
            }
        }
        assert!(
            found_ellipsis,
            "Long titles should be truncated with '…' ellipsis"
        );
    }

    #[test]
    fn keyboard_only_flow_end_to_end() {
        let mut palette = CommandPalette::new();
        palette.register("Open File", Some("Open a file from disk"), &["file"]);
        palette.register("Save File", Some("Save current file"), &["file"]);
        palette.register("Git: Commit", Some("Commit changes"), &["git"]);

        // Step 1: Open with Ctrl+P
        let ctrl_p = Event::Key(KeyEvent {
            code: KeyCode::Char('p'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&ctrl_p);
        assert!(palette.is_visible());
        assert_eq!(palette.result_count(), 3);

        // Step 2: Type query to filter
        for ch in "git".chars() {
            let event = Event::Key(KeyEvent {
                code: KeyCode::Char(ch),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            });
            palette.handle_event(&event);
        }
        assert!(palette.result_count() >= 1);

        // Step 3: Navigate down (in case selected isn't the right one)
        let down = Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&down);

        // Step 4: Navigate back up
        let up = Event::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&up);
        assert_eq!(palette.selected_index(), 0);

        // Step 5: Execute with Enter
        let enter = Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let result = palette.handle_event(&enter);
        assert!(matches!(result, Some(PaletteAction::Execute(_))));
        assert!(!palette.is_visible());
    }

    #[test]
    fn no_focus_trap_esc_always_dismisses() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();

        // Type some query
        for ch in "xyz".chars() {
            let event = Event::Key(KeyEvent {
                code: KeyCode::Char(ch),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            });
            palette.handle_event(&event);
        }
        assert_eq!(palette.result_count(), 0); // no matches

        // Esc should still dismiss even with no results
        let esc = Event::Key(KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let result = palette.handle_event(&esc);
        assert_eq!(result, Some(PaletteAction::Dismiss));
        assert!(!palette.is_visible());
    }

    #[test]
    fn unicode_query_renders_correctly() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        palette.register("Café Menu", None, &["food"]);
        palette.open();
        palette.set_query("café");

        assert_eq!(palette.query(), "café");

        let area = Rect::from_size(60, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 10, &mut pool);
        palette.render(area, &mut frame);

        // The query "café" should be visible in the input area
        // palette_y ≈ 1 (10/6), query line is at palette_y + 1 = 2
        let palette_y = area.y + area.height / 6;
        let input_y = palette_y + 1;

        // Find the query characters in the input row
        let mut found_query_chars = 0;
        for x in 0..60u16 {
            if let Some(cell) = frame.buffer.get(x, input_y)
                && let Some(ch) = cell.content.as_char()
                && "café".contains(ch)
            {
                found_query_chars += 1;
            }
        }
        // Should find at least 3 of the 4 characters (c, a, f, é may be grapheme)
        assert!(
            found_query_chars >= 3,
            "Unicode query should render (found {} chars)",
            found_query_chars
        );
    }

    #[test]
    fn wide_char_query_renders_correctly() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        palette.register("日本語メニュー", None, &["japanese"]);
        palette.open();
        palette.set_query("日本");

        assert_eq!(palette.query(), "日本");

        let area = Rect::from_size(60, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 10, &mut pool);
        palette.render(area, &mut frame);

        // Wide characters should be rendered (each takes 2 columns)
        let palette_y = area.y + area.height / 6;
        let input_y = palette_y + 1;

        // Find grapheme cells in the input row
        let mut found_grapheme = false;
        for x in 0..60u16 {
            if let Some(cell) = frame.buffer.get(x, input_y)
                && cell.content.is_grapheme()
            {
                found_grapheme = true;
                break;
            }
        }
        assert!(
            found_grapheme,
            "Wide character query should render as graphemes"
        );
    }

    #[test]
    fn wcag_aa_contrast_ratios() {
        // Verify the default style colors meet WCAG AA contrast requirements.
        // WCAG AA requires >= 4.5:1 for normal text.
        let style = PaletteStyle::default();
        let bg = PackedRgba::rgb(30, 30, 40);

        // Helper: relative luminance per WCAG 2.0
        fn relative_luminance(color: PackedRgba) -> f64 {
            fn linearize(c: u8) -> f64 {
                let v = c as f64 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            let r = linearize(color.r());
            let g = linearize(color.g());
            let b = linearize(color.b());
            0.2126 * r + 0.7152 * g + 0.0722 * b
        }

        fn contrast_ratio(fg: PackedRgba, bg: PackedRgba) -> f64 {
            let l1 = relative_luminance(fg);
            let l2 = relative_luminance(bg);
            let (lighter, darker) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
            (lighter + 0.05) / (darker + 0.05)
        }

        // Item text on palette background
        let item_fg = style.item.fg.unwrap();
        let item_ratio = contrast_ratio(item_fg, bg);
        assert!(
            item_ratio >= 4.5,
            "Item text contrast {:.1}:1 < 4.5:1 (WCAG AA)",
            item_ratio
        );

        // Selected text on selected background
        let sel_fg = style.item_selected.fg.unwrap();
        let sel_bg = style.item_selected.bg.unwrap();
        let sel_ratio = contrast_ratio(sel_fg, sel_bg);
        assert!(
            sel_ratio >= 4.5,
            "Selected text contrast {:.1}:1 < 4.5:1 (WCAG AA)",
            sel_ratio
        );

        // Match highlight on palette background
        let hl_fg = style.match_highlight.fg.unwrap();
        let hl_ratio = contrast_ratio(hl_fg, bg);
        assert!(
            hl_ratio >= 4.5,
            "Highlight text contrast {:.1}:1 < 4.5:1 (WCAG AA)",
            hl_ratio
        );

        // Description text on palette background
        let desc_fg = style.description.fg.unwrap();
        let desc_ratio = contrast_ratio(desc_fg, bg);
        assert!(
            desc_ratio >= 4.5,
            "Description text contrast {:.1}:1 < 4.5:1 (WCAG AA)",
            desc_ratio
        );
    }

    #[test]
    fn action_item_builder_fields() {
        let item = ActionItem::new("my_id", "My Action")
            .with_description("A description")
            .with_tags(&["tag1", "tag2"])
            .with_category("Category");

        assert_eq!(item.id, "my_id");
        assert_eq!(item.title, "My Action");
        assert_eq!(item.description.as_deref(), Some("A description"));
        assert_eq!(item.tags, vec!["tag1", "tag2"]);
        assert_eq!(item.category.as_deref(), Some("Category"));
    }

    #[test]
    fn action_item_defaults_none() {
        let item = ActionItem::new("id", "title");
        assert!(item.description.is_none());
        assert!(item.tags.is_empty());
        assert!(item.category.is_none());
    }

    #[test]
    fn palette_action_equality() {
        assert_eq!(PaletteAction::Dismiss, PaletteAction::Dismiss);
        assert_eq!(
            PaletteAction::Execute("x".into()),
            PaletteAction::Execute("x".into())
        );
        assert_ne!(PaletteAction::Dismiss, PaletteAction::Execute("x".into()));
    }

    #[test]
    fn match_filter_allows_all() {
        assert!(MatchFilter::All.allows(MatchType::Exact));
        assert!(MatchFilter::All.allows(MatchType::Prefix));
        assert!(MatchFilter::All.allows(MatchType::WordStart));
        assert!(MatchFilter::All.allows(MatchType::Substring));
        assert!(MatchFilter::All.allows(MatchType::Fuzzy));
    }

    #[test]
    fn match_filter_specific_types() {
        assert!(MatchFilter::Exact.allows(MatchType::Exact));
        assert!(!MatchFilter::Exact.allows(MatchType::Fuzzy));
        assert!(MatchFilter::Fuzzy.allows(MatchType::Fuzzy));
        assert!(!MatchFilter::Fuzzy.allows(MatchType::Exact));
    }

    #[test]
    fn palette_default_trait() {
        let palette = CommandPalette::default();
        assert!(!palette.is_visible());
        assert_eq!(palette.action_count(), 0);
        assert_eq!(palette.query(), "");
    }

    #[test]
    fn with_max_visible_builder() {
        let palette = CommandPalette::new().with_max_visible(5);
        // Verify by registering more than 5 items and checking rendering doesn't panic
        let mut palette = palette;
        for i in 0..10 {
            palette.register(format!("Action {i}"), None, &[]);
        }
        palette.open();
        assert_eq!(palette.result_count(), 10);
    }

    #[test]
    fn scorer_stats_accessible() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();
        palette.set_query("a");
        let stats = palette.scorer_stats();
        assert!(stats.full_scans + stats.incremental_scans >= 1);
    }

    #[test]
    fn selected_match_returns_match() {
        let mut palette = CommandPalette::new();
        palette.register("Hello World", None, &[]);
        palette.open();
        palette.set_query("hello");
        let m = palette.selected_match();
        assert!(m.is_some());
        assert_eq!(m.unwrap().action.title, "Hello World");
    }

    #[test]
    fn results_iterator_returns_matches() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.register("Beta", None, &[]);
        palette.open();
        let count = palette.results().count();
        assert_eq!(count, 2);
    }

    #[test]
    fn set_match_filter_narrows_results() {
        let mut palette = CommandPalette::new();
        palette.register("Open File", None, &[]);
        palette.register("Save File", None, &[]);
        palette.open();
        palette.set_query("open");
        let before = palette.result_count();

        // Setting to Exact should narrow or keep results
        palette.set_match_filter(MatchFilter::Exact);
        let after = palette.result_count();
        assert!(after <= before);
    }

    #[test]
    fn enter_with_no_results_returns_none() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();
        palette.set_query("zzzznotfound");
        assert_eq!(palette.result_count(), 0);

        let enter = Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let result = palette.handle_event(&enter);
        assert!(result.is_none());
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn telemetry_emits_in_order() {
        use std::sync::{Arc, Mutex};
        use tracing::Subscriber;
        use tracing_subscriber::Layer;
        use tracing_subscriber::filter::Targets;
        use tracing_subscriber::layer::{Context, SubscriberExt};

        #[derive(Default)]
        struct EventCapture {
            events: Arc<Mutex<Vec<String>>>,
        }

        impl<S> Layer<S> for EventCapture
        where
            S: Subscriber,
        {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                use tracing::field::{Field, Visit};

                struct EventVisitor {
                    name: Option<String>,
                }

                impl Visit for EventVisitor {
                    fn record_str(&mut self, field: &Field, value: &str) {
                        if field.name() == "event" {
                            self.name = Some(value.to_string());
                        }
                    }

                    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                        if field.name() == "event" {
                            let raw = format!("{value:?}");
                            let normalized = raw.trim_matches('\"').to_string();
                            self.name = Some(normalized);
                        }
                    }
                }

                let mut visitor = EventVisitor { name: None };
                event.record(&mut visitor);
                if let Some(name) = visitor.name {
                    self.events
                        .lock()
                        .expect("lock telemetry events")
                        .push(name);
                }
            }
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let capture = EventCapture {
            events: Arc::clone(&events),
        };

        let subscriber = tracing_subscriber::registry()
            .with(capture)
            .with(Targets::new().with_target(TELEMETRY_TARGET, tracing::Level::INFO));
        let _guard = tracing::subscriber::set_default(subscriber);

        // Rebuild interest cache before each step that emits tracing events.
        // Parallel workspace tests can poison the global callsite interest
        // cache between our operations, causing `info!` macros to short-circuit.
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        tracing::callsite::rebuild_interest_cache();
        palette.open();

        let a = Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        tracing::callsite::rebuild_interest_cache();
        palette.handle_event(&a);

        let enter = Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        tracing::callsite::rebuild_interest_cache();
        let _ = palette.handle_event(&enter);
        tracing::callsite::rebuild_interest_cache();
        palette.close();

        let events = events.lock().expect("lock telemetry events");
        let open_idx = events
            .iter()
            .position(|e| e == "palette_opened")
            .expect("palette_opened missing");
        let query_idx = events
            .iter()
            .position(|e| e == "palette_query_updated")
            .expect("palette_query_updated missing");
        let exec_idx = events
            .iter()
            .position(|e| e == "palette_action_executed")
            .expect("palette_action_executed missing");
        let close_idx = events
            .iter()
            .position(|e| e == "palette_closed")
            .expect("palette_closed missing");

        assert!(open_idx < query_idx);
        assert!(query_idx < exec_idx);
        assert!(exec_idx < close_idx);
    }

    // -----------------------------------------------------------------------
    // Edge-case tests (bd-2svld)
    // -----------------------------------------------------------------------

    #[test]
    fn compute_word_starts_empty() {
        let starts = compute_word_starts("");
        assert!(starts.is_empty());
    }

    #[test]
    fn compute_word_starts_single_word() {
        let starts = compute_word_starts("hello");
        assert_eq!(starts, vec![0]);
    }

    #[test]
    fn compute_word_starts_spaces() {
        let starts = compute_word_starts("open file now");
        assert_eq!(starts, vec![0, 5, 10]);
    }

    #[test]
    fn compute_word_starts_hyphen_underscore() {
        let starts = compute_word_starts("git-commit_push");
        // Positions: g=0, c=4, p=11
        assert_eq!(starts, vec![0, 4, 11]);
    }

    #[test]
    fn compute_word_starts_all_separators() {
        let starts = compute_word_starts("- _");
        // '-' at 0 is word start (i==0), ' ' at 1 follows '-' so word start,
        // '_' at 2 follows ' ' so word start
        assert_eq!(starts, vec![0, 1, 2]);
    }

    #[test]
    fn backspace_on_empty_query_is_noop() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();
        assert_eq!(palette.query(), "");

        let bs = Event::Key(KeyEvent {
            code: KeyCode::Backspace,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&bs);
        assert_eq!(palette.query(), "");
        // Results should still show all items
        assert_eq!(palette.result_count(), 1);
    }

    #[test]
    fn ctrl_a_moves_cursor_to_start() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();

        // Type "abc"
        for ch in "abc".chars() {
            let event = Event::Key(KeyEvent {
                code: KeyCode::Char(ch),
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            });
            palette.handle_event(&event);
        }
        assert_eq!(palette.query(), "abc");

        let ctrl_a = Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&ctrl_a);
        // Cursor should move to 0 but query unchanged
        assert_eq!(palette.query(), "abc");
    }

    #[test]
    fn key_release_events_ignored() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();

        let release = Event::Key(KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Release,
        });
        let result = palette.handle_event(&release);
        assert!(result.is_none());
        assert_eq!(palette.query(), "");
    }

    #[test]
    fn resize_event_ignored() {
        let mut palette = CommandPalette::new();
        palette.open();

        let resize = Event::Resize {
            width: 80,
            height: 24,
        };
        let result = palette.handle_event(&resize);
        assert!(result.is_none());
    }

    #[test]
    fn is_essential_returns_true() {
        let palette = CommandPalette::new();
        assert!(palette.is_essential());
    }

    #[test]
    fn render_too_small_area_noop() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();

        // Width < 10
        let area = Rect::new(0, 0, 9, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 20, &mut pool);
        palette.render(area, &mut frame);
        // Should not panic, cursor not set
        assert!(frame.cursor_position.is_none());
    }

    #[test]
    fn render_too_short_area_noop() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();

        // Height < 5
        let area = Rect::new(0, 0, 60, 4);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 10, &mut pool);
        palette.render(area, &mut frame);
        assert!(frame.cursor_position.is_none());
    }

    #[test]
    fn render_hidden_palette_noop() {
        use ftui_render::grapheme_pool::GraphemePool;

        let palette = CommandPalette::new();
        assert!(!palette.is_visible());

        let area = Rect::from_size(60, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 10, &mut pool);
        palette.render(area, &mut frame);
        assert!(frame.cursor_position.is_none());
    }

    #[test]
    fn render_empty_palette_shows_no_actions_hint() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        // No actions registered
        palette.open();

        let area = Rect::from_size(60, 15);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 15, &mut pool);
        palette.render(area, &mut frame);

        // Should show "No actions registered" somewhere in the results area
        let palette_y = area.y + area.height / 6;
        let result_y = palette_y + 2;
        let mut found_n = false;
        for x in 0..60u16 {
            if let Some(cell) = frame.buffer.get(x, result_y)
                && cell.content.as_char() == Some('N')
            {
                found_n = true;
                break;
            }
        }
        assert!(found_n, "Should render 'No actions registered' hint");
    }

    #[test]
    fn render_query_no_results_shows_hint() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();
        palette.set_query("zzzznotfound");
        assert_eq!(palette.result_count(), 0);

        let area = Rect::from_size(60, 15);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 15, &mut pool);
        palette.render(area, &mut frame);

        // Should show "No results"
        let palette_y = area.y + area.height / 6;
        let result_y = palette_y + 2;
        let mut found_n = false;
        for x in 0..60u16 {
            if let Some(cell) = frame.buffer.get(x, result_y)
                && cell.content.as_char() == Some('N')
            {
                found_n = true;
                break;
            }
        }
        assert!(found_n, "Should render 'No results' hint");
    }

    #[test]
    fn render_with_category_badge() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        let item = ActionItem::new("git_commit", "Commit Changes").with_category("Git");
        palette.register_action(item);
        palette.open();

        let area = Rect::from_size(80, 15);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 15, &mut pool);
        palette.render(area, &mut frame);

        // Should render "[Git] " badge - look for '[' in results area
        let palette_y = area.y + area.height / 6;
        let result_y = palette_y + 2;
        let mut found_bracket = false;
        for x in 0..80u16 {
            if let Some(cell) = frame.buffer.get(x, result_y)
                && cell.content.as_char() == Some('[')
            {
                found_bracket = true;
                break;
            }
        }
        assert!(found_bracket, "Should render category badge '[Git]'");
    }

    #[test]
    fn render_with_description_text() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        palette.register("Open File", Some("Opens a file from disk"), &[]);
        palette.open();

        let area = Rect::from_size(80, 15);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 15, &mut pool);
        palette.render(area, &mut frame);

        // Description text should appear after the title
        let palette_y = area.y + area.height / 6;
        let result_y = palette_y + 2;
        let mut found_desc_char = false;
        // Description starts with 'O' in "Opens..."
        for x in 20..80u16 {
            if let Some(cell) = frame.buffer.get(x, result_y)
                && cell.content.as_char() == Some('O')
            {
                found_desc_char = true;
                break;
            }
        }
        assert!(found_desc_char, "Description text should be rendered");
    }

    #[test]
    fn open_resets_previous_state() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.register("Beta", None, &[]);
        palette.open();
        palette.set_query("Alpha");

        // Navigate down
        let down = Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&down);

        // Re-open should reset everything
        palette.open();
        assert_eq!(palette.query(), "");
        assert_eq!(palette.selected_index(), 0);
        assert_eq!(palette.result_count(), 2);
    }

    #[test]
    fn set_match_filter_same_value_is_noop() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();
        palette.set_query("alpha");

        palette.set_match_filter(MatchFilter::All);
        let count1 = palette.result_count();
        // Setting same filter again — should not change anything
        palette.set_match_filter(MatchFilter::All);
        assert_eq!(palette.result_count(), count1);
    }

    #[test]
    fn generation_increments_on_register() {
        let mut palette = CommandPalette::new();
        palette.register("A", None, &[]);
        palette.register("B", None, &[]);
        // Can't read generation directly, but replace_actions also bumps it
        // and invalidates scorer — verify it doesn't panic
        palette.replace_actions(vec![ActionItem::new("c", "C")]);
        palette.open();
        assert_eq!(palette.action_count(), 1);
    }

    #[test]
    fn enable_evidence_tracking_toggle() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();

        palette.enable_evidence_tracking(true);
        palette.set_query("alpha");
        assert!(palette.result_count() >= 1);

        palette.enable_evidence_tracking(false);
        palette.set_query("alpha");
        assert!(palette.result_count() >= 1);
    }

    #[test]
    fn register_chaining() {
        let mut palette = CommandPalette::new();
        palette
            .register("A", None, &[])
            .register("B", None, &[])
            .register("C", Some("desc"), &["tag"]);
        assert_eq!(palette.action_count(), 3);
    }

    #[test]
    fn register_action_chaining() {
        let mut palette = CommandPalette::new();
        palette
            .register_action(ActionItem::new("a", "A"))
            .register_action(ActionItem::new("b", "B"));
        assert_eq!(palette.action_count(), 2);
    }

    #[test]
    fn page_up_down_navigation() {
        let mut palette = CommandPalette::new().with_max_visible(3);
        for i in 0..10 {
            palette.register(format!("Action {i}"), None, &[]);
        }
        palette.open();
        assert_eq!(palette.selected_index(), 0);

        let pgdn = Event::Key(KeyEvent {
            code: KeyCode::PageDown,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&pgdn);
        assert_eq!(palette.selected_index(), 3); // 0 + max_visible

        palette.handle_event(&pgdn);
        assert_eq!(palette.selected_index(), 6);

        palette.handle_event(&pgdn);
        assert_eq!(palette.selected_index(), 9); // clamped to last

        let pgup = Event::Key(KeyEvent {
            code: KeyCode::PageUp,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&pgup);
        assert_eq!(palette.selected_index(), 6); // 9 - 3

        palette.handle_event(&pgup);
        assert_eq!(palette.selected_index(), 3);

        palette.handle_event(&pgup);
        assert_eq!(palette.selected_index(), 0);
    }

    #[test]
    fn page_down_empty_results_is_noop() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert_eq!(palette.result_count(), 0);

        let pgdn = Event::Key(KeyEvent {
            code: KeyCode::PageDown,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&pgdn);
        assert_eq!(palette.selected_index(), 0);
    }

    #[test]
    fn end_empty_results_is_noop() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert_eq!(palette.result_count(), 0);

        let end = Event::Key(KeyEvent {
            code: KeyCode::End,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&end);
        assert_eq!(palette.selected_index(), 0);
    }

    #[test]
    fn down_empty_results_is_noop() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert_eq!(palette.result_count(), 0);

        let down = Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&down);
        assert_eq!(palette.selected_index(), 0);
    }

    #[test]
    fn selected_action_none_when_empty() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert!(palette.selected_action().is_none());
        assert!(palette.selected_match().is_none());
    }

    #[test]
    fn results_iterator_empty() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert_eq!(palette.results().count(), 0);
    }

    #[test]
    fn scroll_adjust_keeps_selection_visible() {
        let mut palette = CommandPalette::new().with_max_visible(3);
        for i in 0..10 {
            palette.register(format!("Action {i}"), None, &[]);
        }
        palette.open();

        let end = Event::Key(KeyEvent {
            code: KeyCode::End,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&end);
        assert_eq!(palette.selected_index(), 9);
        // scroll_offset should have adjusted so item 9 is visible
        // (scroll_offset = selected + 1 - max_visible = 9 + 1 - 3 = 7)

        let home = Event::Key(KeyEvent {
            code: KeyCode::Home,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&home);
        assert_eq!(palette.selected_index(), 0);
    }

    #[test]
    fn action_item_clone() {
        let item = ActionItem::new("id", "Title")
            .with_description("Desc")
            .with_tags(&["a", "b"])
            .with_category("Cat");
        let cloned = item.clone();
        assert_eq!(cloned.id, "id");
        assert_eq!(cloned.title, "Title");
        assert_eq!(cloned.description.as_deref(), Some("Desc"));
        assert_eq!(cloned.tags, vec!["a", "b"]);
        assert_eq!(cloned.category.as_deref(), Some("Cat"));
    }

    #[test]
    fn action_item_debug() {
        let item = ActionItem::new("id", "Title");
        let debug = format!("{:?}", item);
        assert!(debug.contains("ActionItem"));
        assert!(debug.contains("Title"));
    }

    #[test]
    fn palette_action_clone_and_debug() {
        let exec = PaletteAction::Execute("test".into());
        let cloned = exec.clone();
        assert_eq!(exec, cloned);

        let dismiss = PaletteAction::Dismiss;
        let debug = format!("{:?}", dismiss);
        assert!(debug.contains("Dismiss"));
    }

    #[test]
    fn match_filter_traits() {
        // Debug
        let f = MatchFilter::Fuzzy;
        let debug = format!("{:?}", f);
        assert!(debug.contains("Fuzzy"));

        // Clone + Copy
        let f2 = f;
        assert_eq!(f, f2);

        // PartialEq
        assert_eq!(MatchFilter::All, MatchFilter::All);
        assert_ne!(MatchFilter::Exact, MatchFilter::Prefix);
    }

    #[test]
    fn match_filter_specific_allows() {
        assert!(MatchFilter::Prefix.allows(MatchType::Prefix));
        assert!(!MatchFilter::Prefix.allows(MatchType::Exact));
        assert!(!MatchFilter::Prefix.allows(MatchType::Substring));

        assert!(MatchFilter::WordStart.allows(MatchType::WordStart));
        assert!(!MatchFilter::WordStart.allows(MatchType::Fuzzy));

        assert!(MatchFilter::Substring.allows(MatchType::Substring));
        assert!(!MatchFilter::Substring.allows(MatchType::WordStart));
    }

    #[test]
    fn palette_style_default_has_all_colors() {
        let style = PaletteStyle::default();
        assert!(style.border.fg.is_some());
        assert!(style.input.fg.is_some());
        assert!(style.item.fg.is_some());
        assert!(style.item_selected.fg.is_some());
        assert!(style.item_selected.bg.is_some());
        assert!(style.match_highlight.fg.is_some());
        assert!(style.description.fg.is_some());
        assert!(style.category.fg.is_some());
        assert!(style.hint.fg.is_some());
    }

    #[test]
    fn palette_style_debug_and_clone() {
        let style = PaletteStyle::default();
        let debug = format!("{:?}", style);
        assert!(debug.contains("PaletteStyle"));

        let cloned = style.clone();
        // Verify cloned fields match
        assert_eq!(cloned.border.fg, style.border.fg);
    }

    #[test]
    fn with_style_builder() {
        let style = PaletteStyle::default();
        let palette = CommandPalette::new().with_style(style);
        // Should not panic — style applied
        assert!(!palette.is_visible());
    }

    #[test]
    fn command_palette_debug() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        let debug = format!("{:?}", palette);
        assert!(debug.contains("CommandPalette"));
    }

    #[test]
    fn unrecognized_key_returns_none() {
        let mut palette = CommandPalette::new();
        palette.open();

        let tab = Event::Key(KeyEvent {
            code: KeyCode::Tab,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let result = palette.handle_event(&tab);
        assert!(result.is_none());
    }

    #[test]
    fn ctrl_p_when_visible_does_not_reopen() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();
        palette.set_query("test");

        // Ctrl+P while visible should be treated as Ctrl+Char('p')
        let ctrl_p = Event::Key(KeyEvent {
            code: KeyCode::Char('p'),
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&ctrl_p);
        // The visible palette handles Ctrl+P as a Ctrl char, not toggling
        assert!(palette.is_visible());
    }

    #[test]
    fn close_clears_query_and_results() {
        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();
        palette.set_query("alpha");
        assert!(!palette.query().is_empty());
        assert!(palette.result_count() > 0);

        palette.close();
        assert!(!palette.is_visible());
        assert_eq!(palette.query(), "");
        assert_eq!(palette.result_count(), 0);
    }

    #[test]
    fn render_cursor_position_set() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new();
        palette.register("Alpha", None, &[]);
        palette.open();

        let area = Rect::from_size(60, 15);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 15, &mut pool);
        palette.render(area, &mut frame);

        assert!(frame.cursor_position.is_some());
        assert!(frame.cursor_visible);
    }

    #[test]
    fn render_many_items_with_scroll() {
        use ftui_render::grapheme_pool::GraphemePool;

        let mut palette = CommandPalette::new().with_max_visible(3);
        for i in 0..20 {
            palette.register(format!("Action {i}"), None, &[]);
        }
        palette.open();

        // Scroll to bottom
        let end = Event::Key(KeyEvent {
            code: KeyCode::End,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        palette.handle_event(&end);

        let area = Rect::from_size(60, 15);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 15, &mut pool);
        // Should render without panic even when scrolled
        palette.render(area, &mut frame);
        assert!(frame.cursor_position.is_some());
    }
}
mod property_tests;

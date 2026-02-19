#![forbid(unsafe_code)]

//! A scrolling log viewer widget optimized for streaming append-only content.
//!
//! `LogViewer` is THE essential widget for agent harness UIs. It displays streaming
//! logs with scrollback while maintaining UI chrome and handles:
//!
//! - High-frequency log line additions without flicker
//! - Auto-scroll behavior for "follow" mode
//! - Manual scrolling to inspect history
//! - Memory bounds via circular buffer eviction
//! - Substring filtering for log lines
//! - Text search with next/prev match navigation
//!
//! # Architecture
//!
//! LogViewer delegates storage and scroll state to [`Virtualized<Text>`], gaining
//! momentum scrolling, overscan, and page navigation for free. LogViewer adds
//! capacity management (eviction), wrapping, filtering, and search on top.
//!
//! # Example
//! ```ignore
//! use ftui_widgets::log_viewer::{LogViewer, LogViewerState, LogWrapMode};
//! use ftui_text::Text;
//!
//! // Create a viewer with 10,000 line capacity
//! let mut viewer = LogViewer::new(10_000);
//!
//! // Push log lines (styled or plain)
//! viewer.push("Starting process...");
//! viewer.push(Text::styled("ERROR: failed", Style::new().fg(Color::Red)));
//!
//! // Render with state
//! let mut state = LogViewerState::default();
//! viewer.render(area, frame, &mut state);
//! ```

use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::search::{search_ascii_case_insensitive, search_exact};
use ftui_text::{Text, WrapMode, WrapOptions, display_width, wrap_with_options};

use crate::virtualized::Virtualized;
use crate::{StatefulWidget, draw_text_span, draw_text_span_with_link};

/// Line wrapping mode for log lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogWrapMode {
    /// No wrapping, truncate long lines.
    #[default]
    NoWrap,
    /// Wrap at any character boundary.
    CharWrap,
    /// Wrap at word boundaries (Unicode-aware).
    WordWrap,
}

impl From<LogWrapMode> for WrapMode {
    fn from(mode: LogWrapMode) -> Self {
        match mode {
            LogWrapMode::NoWrap => WrapMode::None,
            LogWrapMode::CharWrap => WrapMode::Char,
            LogWrapMode::WordWrap => WrapMode::Word,
        }
    }
}

/// Search mode for log search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchMode {
    /// Plain substring matching.
    #[default]
    Literal,
    /// Regular expression matching (requires `regex-search` feature).
    Regex,
}

/// Search configuration.
#[derive(Clone, Debug)]
pub struct SearchConfig {
    /// Search mode (literal or regex).
    pub mode: SearchMode,
    /// Whether the search is case-sensitive.
    pub case_sensitive: bool,
    /// Number of context lines around matches (0 = only matching lines).
    pub context_lines: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            mode: SearchMode::Literal,
            case_sensitive: true,
            context_lines: 0,
        }
    }
}

/// Search state for text search within the log.
#[derive(Debug, Clone)]
struct SearchState {
    /// The search query string (retained for re-search after eviction).
    query: String,
    /// Lowercase query for case-insensitive search optimization.
    query_lower: Option<String>,
    /// Current search configuration.
    config: SearchConfig,
    /// Indices of matching lines.
    matches: Vec<usize>,
    /// Current match index within the matches vector.
    current: usize,
    /// Per-match-line byte ranges for highlighting. Indexed by position in `matches`.
    highlight_ranges: Vec<Vec<(usize, usize)>>,
    /// Compiled regex pattern (behind feature gate).
    #[cfg(feature = "regex-search")]
    compiled_regex: Option<regex::Regex>,
    /// Indices including context lines around matches (sorted, deduped).
    /// `None` when `config.context_lines == 0`.
    context_expanded: Option<Vec<usize>>,
}

/// Statistics tracking incremental vs full-rescan filter/search operations.
///
/// Useful for monitoring the efficiency of the streaming update path.
/// Reset with [`FilterStats::reset`].
#[derive(Debug, Clone, Default)]
pub struct FilterStats {
    /// Lines checked incrementally (O(1) per push).
    pub incremental_checks: u64,
    /// Lines that matched during incremental checks.
    pub incremental_matches: u64,
    /// Full rescans triggered (e.g., by `set_filter` or `search`).
    pub full_rescans: u64,
    /// Total lines scanned during full rescans.
    pub full_rescan_lines: u64,
    /// Search matches added incrementally on push.
    pub incremental_search_matches: u64,
    /// Lines checked incrementally for search matches.
    pub incremental_search_checks: u64,
}

impl FilterStats {
    /// Reset all counters to zero.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A scrolling log viewer optimized for streaming append-only content.
///
/// Internally uses [`Virtualized<Text>`] for storage and scroll management,
/// adding capacity enforcement, wrapping, filtering, and search on top.
///
/// # Design Rationale
/// - Virtualized handles scroll offset, follow mode, momentum, page navigation
/// - LogViewer adds max_lines eviction (Virtualized has no built-in capacity limit)
/// - Separate scroll semantics: Virtualized uses "offset from top"; LogViewer
///   exposes "follow mode" (newest at bottom) as the default behavior
/// - wrap_mode configurable per-instance for different use cases
/// - Stateful widget pattern for scroll state preservation across renders
#[derive(Debug, Clone)]
pub struct LogViewer {
    /// Virtualized storage with scroll state management.
    virt: Virtualized<Text>,
    /// Maximum lines to retain (memory bound).
    max_lines: usize,
    /// Line wrapping mode.
    wrap_mode: LogWrapMode,
    /// Default style for lines.
    style: Style,
    /// Highlight style for selected/focused line.
    highlight_style: Option<Style>,
    /// Highlight style for search matches within a line.
    search_highlight_style: Option<Style>,
    /// Active filter pattern (plain substring match).
    filter: Option<String>,
    /// Indices of lines matching the filter (None = show all).
    filtered_indices: Option<Vec<usize>>,
    /// Scroll offset within the filtered set (top index of filtered list).
    filtered_scroll_offset: usize,
    /// Active search state.
    search: Option<SearchState>,
    /// Incremental filter/search statistics.
    filter_stats: FilterStats,
}

/// Separate state for StatefulWidget pattern.
#[derive(Debug, Clone, Default)]
pub struct LogViewerState {
    /// Viewport height from last render (for page up/down).
    pub last_viewport_height: u16,
    /// Total visible line count from last render.
    pub last_visible_lines: usize,
    /// Selected line index (for copy/selection features).
    pub selected_line: Option<usize>,
}

impl LogViewer {
    /// Create a new LogViewer with specified max line capacity.
    ///
    /// # Arguments
    /// * `max_lines` - Maximum lines to retain. When exceeded, oldest lines
    ///   are evicted. Recommend 10,000-100,000 for typical agent use cases.
    #[must_use]
    pub fn new(max_lines: usize) -> Self {
        Self {
            virt: Virtualized::new(max_lines).with_follow(true),
            max_lines,
            wrap_mode: LogWrapMode::NoWrap,
            style: Style::default(),
            highlight_style: None,
            search_highlight_style: None,
            filter: None,
            filtered_indices: None,
            filtered_scroll_offset: 0,
            search: None,
            filter_stats: FilterStats::default(),
        }
    }

    /// Set the wrap mode.
    #[must_use]
    pub fn wrap_mode(mut self, mode: LogWrapMode) -> Self {
        self.wrap_mode = mode;
        self
    }

    /// Set the default style for lines.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the highlight style for selected lines.
    #[must_use]
    pub fn highlight_style(mut self, style: Style) -> Self {
        self.highlight_style = Some(style);
        self
    }

    /// Set the highlight style for search matches within lines.
    #[must_use]
    pub fn search_highlight_style(mut self, style: Style) -> Self {
        self.search_highlight_style = Some(style);
        self
    }

    /// Returns the total number of log lines.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.virt.len()
    }

    /// Returns true if there are no log lines.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.virt.is_empty()
    }

    /// Append a single log line.
    ///
    /// # Performance
    /// - O(1) amortized for append
    /// - O(1) for eviction when at capacity
    ///
    /// # Auto-scroll Behavior
    /// If follow mode is enabled, view stays at bottom after push.
    pub fn push(&mut self, line: impl Into<Text>) {
        let follow_filtered = self.filtered_indices.as_ref().is_some_and(|indices| {
            self.is_filtered_at_bottom(indices.len(), self.virt.visible_count())
        });
        let text: Text = line.into();

        // Split multi-line text into individual items for smooth scrolling
        for line in text.into_iter() {
            let item = Text::from_line(line);
            let plain = item.to_plain_text();

            // Incremental filter check: test new line against active filter.
            let filter_matched = if let Some(filter) = self.filter.as_ref() {
                self.filter_stats.incremental_checks += 1;
                let matched = plain.contains(filter.as_str());
                if matched {
                    if let Some(indices) = self.filtered_indices.as_mut() {
                        let idx = self.virt.len();
                        indices.push(idx);
                    }
                    self.filter_stats.incremental_matches += 1;
                }
                matched
            } else {
                false
            };

            // Incremental search check: test new line against active search query.
            // Only add to search matches if (a) there is no filter or (b) the
            // line passed the filter, because search results respect the filter.
            if let Some(ref mut search) = self.search {
                let should_check = self.filter.is_none() || filter_matched;
                if should_check {
                    self.filter_stats.incremental_search_checks += 1;
                    let ranges = find_match_ranges(
                        &plain,
                        &search.query,
                        search.query_lower.as_deref(),
                        &search.config,
                        #[cfg(feature = "regex-search")]
                        search.compiled_regex.as_ref(),
                    );
                    if !ranges.is_empty() {
                        let idx = self.virt.len();
                        search.matches.push(idx);
                        search.highlight_ranges.push(ranges);
                        self.filter_stats.incremental_search_matches += 1;
                    }
                }
            }

            self.virt.push(item);

            // Enforce capacity
            if self.virt.len() > self.max_lines {
                let removed = self.virt.trim_front(self.max_lines);

                // Adjust filtered indices
                if let Some(ref mut indices) = self.filtered_indices {
                    let mut filtered_removed = 0usize;
                    indices.retain_mut(|idx| {
                        if *idx < removed {
                            filtered_removed += 1;
                            false
                        } else {
                            *idx -= removed;
                            true
                        }
                    });
                    if filtered_removed > 0 {
                        self.filtered_scroll_offset =
                            self.filtered_scroll_offset.saturating_sub(filtered_removed);
                    }
                    if indices.is_empty() {
                        self.filtered_scroll_offset = 0;
                    }
                }

                // Adjust search match indices and corresponding highlight_ranges
                if let Some(ref mut search) = self.search {
                    let mut keep = Vec::with_capacity(search.matches.len());
                    let mut new_highlights = Vec::with_capacity(search.highlight_ranges.len());
                    for (i, idx) in search.matches.iter_mut().enumerate() {
                        if *idx < removed {
                            // evicted
                        } else {
                            *idx -= removed;
                            keep.push(*idx);
                            if i < search.highlight_ranges.len() {
                                new_highlights
                                    .push(std::mem::take(&mut search.highlight_ranges[i]));
                            }
                        }
                    }
                    search.matches = keep;
                    search.highlight_ranges = new_highlights;
                    // Clamp current to valid range
                    if !search.matches.is_empty() {
                        search.current = search.current.min(search.matches.len() - 1);
                    }
                    // Recompute context expansion if needed
                    if search.config.context_lines > 0 {
                        search.context_expanded = Some(expand_context(
                            &search.matches,
                            search.config.context_lines,
                            self.virt.len(),
                        ));
                    }
                }
            }

            if follow_filtered
                && let Some(indices) = self.filtered_indices.as_ref()
                && !indices.is_empty()
            {
                self.filtered_scroll_offset = indices.len().saturating_sub(1);
            }
        }
    }

    /// Append multiple lines efficiently.
    pub fn push_many(&mut self, lines: impl IntoIterator<Item = impl Into<Text>>) {
        for line in lines {
            self.push(line);
        }
    }

    /// Scroll up by N lines. Disables follow mode.
    pub fn scroll_up(&mut self, lines: usize) {
        if self.filtered_indices.is_some() {
            self.filtered_scroll_offset = self.filtered_scroll_offset.saturating_sub(lines);
        } else {
            let delta = i32::try_from(lines).unwrap_or(i32::MAX);
            self.virt.scroll(-delta);
        }
    }

    /// Scroll down by N lines. Re-enables follow mode if at bottom.
    pub fn scroll_down(&mut self, lines: usize) {
        if let Some(filtered_total) = self.filtered_indices.as_ref().map(Vec::len) {
            if filtered_total == 0 {
                self.filtered_scroll_offset = 0;
            } else {
                self.filtered_scroll_offset = self.filtered_scroll_offset.saturating_add(lines);
                let max_offset = filtered_total.saturating_sub(1);
                if self.filtered_scroll_offset > max_offset {
                    self.filtered_scroll_offset = max_offset;
                }
            }
        } else {
            let delta = i32::try_from(lines).unwrap_or(i32::MAX);
            self.virt.scroll(delta);
            if self.virt.is_at_bottom() {
                self.virt.set_follow(true);
            }
        }
    }

    /// Jump to top of log history.
    pub fn scroll_to_top(&mut self) {
        if self.filtered_indices.is_some() {
            self.filtered_scroll_offset = 0;
        } else {
            self.virt.scroll_to_top();
        }
    }

    /// Jump to bottom and re-enable follow mode.
    pub fn scroll_to_bottom(&mut self) {
        if let Some(filtered_total) = self.filtered_indices.as_ref().map(Vec::len) {
            if filtered_total == 0 {
                self.filtered_scroll_offset = 0;
            } else if self.virt.visible_count() > 0 {
                self.filtered_scroll_offset =
                    filtered_total.saturating_sub(self.virt.visible_count());
            } else {
                self.filtered_scroll_offset = filtered_total.saturating_sub(1);
            }
        } else {
            self.virt.scroll_to_end();
        }
    }

    /// Page up (scroll by viewport height).
    ///
    /// Uses the visible count tracked by the Virtualized container.
    /// The `state` parameter is accepted for API compatibility.
    pub fn page_up(&mut self, _state: &LogViewerState) {
        if self.filtered_indices.is_some() {
            let lines = _state.last_viewport_height as usize;
            if lines > 0 {
                self.scroll_up(lines);
            }
        } else {
            self.virt.page_up();
        }
    }

    /// Page down (scroll by viewport height).
    ///
    /// Uses the visible count tracked by the Virtualized container.
    /// The `state` parameter is accepted for API compatibility.
    pub fn page_down(&mut self, _state: &LogViewerState) {
        if self.filtered_indices.is_some() {
            let lines = _state.last_viewport_height as usize;
            if lines > 0 {
                self.scroll_down(lines);
            }
        } else {
            self.virt.page_down();
            if self.virt.is_at_bottom() {
                self.virt.set_follow(true);
            }
        }
    }

    /// Check if currently scrolled to the bottom.
    ///
    /// Returns `true` when follow mode is active (even before first render
    /// when the viewport size is unknown).
    #[must_use]
    pub fn is_at_bottom(&self) -> bool {
        if let Some(indices) = self.filtered_indices.as_ref() {
            self.is_filtered_at_bottom(indices.len(), self.virt.visible_count())
        } else {
            self.virt.follow_mode() || self.virt.is_at_bottom()
        }
    }

    /// Total line count in buffer.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.virt.len()
    }

    /// Check if follow mode (auto-scroll) is enabled.
    #[must_use]
    pub fn auto_scroll_enabled(&self) -> bool {
        self.virt.follow_mode()
    }

    /// Set follow mode (auto-scroll) state.
    pub fn set_auto_scroll(&mut self, enabled: bool) {
        self.virt.set_follow(enabled);
    }

    /// Toggle follow mode on/off.
    pub fn toggle_follow(&mut self) {
        let current = self.virt.follow_mode();
        self.virt.set_follow(!current);
    }

    /// Clear all lines.
    pub fn clear(&mut self) {
        self.virt.clear();
        self.filtered_indices = self.filter.as_ref().map(|_| Vec::new());
        self.filtered_scroll_offset = 0;
        self.search = None;
        self.filter_stats.reset();
    }

    /// Get a reference to the incremental filter/search statistics.
    ///
    /// Use this to monitor how often the streaming incremental path is used
    /// versus full rescans.
    #[must_use]
    pub fn filter_stats(&self) -> &FilterStats {
        &self.filter_stats
    }

    /// Get a mutable reference to the filter statistics (for resetting).
    pub fn filter_stats_mut(&mut self) -> &mut FilterStats {
        &mut self.filter_stats
    }

    /// Set a filter pattern (plain substring match).
    ///
    /// Only lines containing the pattern will be shown. Pass `None` to clear.
    pub fn set_filter(&mut self, pattern: Option<&str>) {
        match pattern {
            Some(pat) if !pat.is_empty() => {
                // Full rescan: rebuild filtered indices from all lines.
                self.filter_stats.full_rescans += 1;
                self.filter_stats.full_rescan_lines += self.virt.len() as u64;
                let mut indices = Vec::new();
                for idx in 0..self.virt.len() {
                    if let Some(item) = self.virt.get(idx)
                        && item.to_plain_text().contains(pat)
                    {
                        indices.push(idx);
                    }
                }
                self.filter = Some(pat.to_string());
                self.filtered_indices = Some(indices);
                self.filtered_scroll_offset = if let Some(indices) = self.filtered_indices.as_ref()
                {
                    if indices.is_empty() {
                        0
                    } else if self.virt.follow_mode() || self.virt.is_at_bottom() {
                        indices.len().saturating_sub(1)
                    } else {
                        let scroll_offset = self.virt.scroll_offset();
                        indices.partition_point(|&idx| idx < scroll_offset)
                    }
                } else {
                    0
                };
                self.search = None;
            }
            _ => {
                self.filter = None;
                self.filtered_indices = None;
                self.filtered_scroll_offset = 0;
                self.search = None;
            }
        }
    }

    /// Search for text and return match count.
    ///
    /// Convenience wrapper using default config (literal, case-sensitive, no context).
    /// Sets up search state for navigation with `next_match` / `prev_match`.
    pub fn search(&mut self, query: &str) -> usize {
        self.search_with_config(query, SearchConfig::default())
    }

    /// Search with full configuration (mode, case sensitivity, context lines).
    ///
    /// Returns match count. Sets up state for `next_match` / `prev_match`.
    pub fn search_with_config(&mut self, query: &str, config: SearchConfig) -> usize {
        if query.is_empty() {
            self.search = None;
            return 0;
        }

        // Compile regex if needed
        #[cfg(feature = "regex-search")]
        let compiled_regex = if config.mode == SearchMode::Regex {
            match compile_regex(query, &config) {
                Some(re) => Some(re),
                None => {
                    // Invalid regex — clear search and return 0
                    self.search = None;
                    return 0;
                }
            }
        } else {
            None
        };

        // Pre-compute lowercase query for optimization
        let query_lower = if !config.case_sensitive {
            Some(query.to_ascii_lowercase())
        } else {
            None
        };

        // Full rescan for search matches.
        self.filter_stats.full_rescans += 1;
        let mut matches = Vec::new();
        let mut highlight_ranges = Vec::new();

        let iter: Box<dyn Iterator<Item = usize>> =
            if let Some(indices) = self.filtered_indices.as_ref() {
                self.filter_stats.full_rescan_lines += indices.len() as u64;
                Box::new(indices.iter().copied())
            } else {
                self.filter_stats.full_rescan_lines += self.virt.len() as u64;
                Box::new(0..self.virt.len())
            };

        for idx in iter {
            if let Some(item) = self.virt.get(idx) {
                let plain = item.to_plain_text();
                let ranges = find_match_ranges(
                    &plain,
                    query,
                    query_lower.as_deref(),
                    &config,
                    #[cfg(feature = "regex-search")]
                    compiled_regex.as_ref(),
                );
                if !ranges.is_empty() {
                    matches.push(idx);
                    highlight_ranges.push(ranges);
                }
            }
        }

        let count = matches.len();

        let context_expanded = if config.context_lines > 0 {
            Some(expand_context(
                &matches,
                config.context_lines,
                self.virt.len(),
            ))
        } else {
            None
        };

        self.search = Some(SearchState {
            query: query.to_string(),
            query_lower,
            config,
            matches,
            current: 0,
            highlight_ranges,
            #[cfg(feature = "regex-search")]
            compiled_regex,
            context_expanded,
        });

        // Jump to first match
        if let Some(ref search) = self.search
            && let Some(&idx) = search.matches.first()
        {
            self.scroll_to_match(idx);
        }

        count
    }

    /// Jump to next search match.
    pub fn next_match(&mut self) {
        if let Some(ref mut search) = self.search
            && !search.matches.is_empty()
        {
            search.current = (search.current + 1) % search.matches.len();
            let idx = search.matches[search.current];
            self.scroll_to_match(idx);
        }
    }

    /// Jump to previous search match.
    pub fn prev_match(&mut self) {
        if let Some(ref mut search) = self.search
            && !search.matches.is_empty()
        {
            search.current = if search.current == 0 {
                search.matches.len() - 1
            } else {
                search.current - 1
            };
            let idx = search.matches[search.current];
            self.scroll_to_match(idx);
        }
    }

    /// Clear active search.
    pub fn clear_search(&mut self) {
        self.search = None;
    }

    /// Get current search match info: (current_match_1indexed, total_matches).
    #[must_use]
    pub fn search_info(&self) -> Option<(usize, usize)> {
        self.search.as_ref().and_then(|s| {
            if s.matches.is_empty() {
                None
            } else {
                Some((s.current + 1, s.matches.len()))
            }
        })
    }

    /// Get the highlight byte ranges for a given line index, if any.
    ///
    /// Returns `Some(&[(start, end)])` when the line is a search match.
    #[must_use]
    pub fn highlight_ranges_for_line(&self, line_idx: usize) -> Option<&[(usize, usize)]> {
        let search = self.search.as_ref()?;
        let pos = search.matches.iter().position(|&m| m == line_idx)?;
        search.highlight_ranges.get(pos).map(|v| v.as_slice())
    }

    /// Get the context-expanded line indices, if context lines are configured.
    ///
    /// Returns `None` when no search is active or `context_lines == 0`.
    #[must_use]
    pub fn context_line_indices(&self) -> Option<&[usize]> {
        self.search
            .as_ref()
            .and_then(|s| s.context_expanded.as_deref())
    }

    /// Returns the fraction of recent pushes that matched the active search.
    ///
    /// Useful for callers integrating with an `EProcessThrottle` to decide
    /// when to trigger a full UI refresh vs. deferring.
    /// Returns 0.0 when no search is active or no incremental checks occurred.
    #[must_use]
    pub fn search_match_rate_hint(&self) -> f64 {
        let stats = &self.filter_stats;
        if stats.incremental_search_checks == 0 {
            return 0.0;
        }
        stats.incremental_search_matches as f64 / stats.incremental_search_checks as f64
    }

    /// Render a single line with optional wrapping and search highlighting.
    #[allow(clippy::too_many_arguments)]
    fn render_line(
        &self,
        text: &Text,
        line_idx: usize,
        x: u16,
        y: u16,
        width: u16,
        max_y: u16,
        frame: &mut Frame,
        is_selected: bool,
    ) -> u16 {
        let effective_style = if is_selected {
            self.highlight_style.unwrap_or(self.style)
        } else {
            self.style
        };

        let line = text.lines().first();
        let content = text.to_plain_text();
        let content_width = display_width(&content);
        let hl_ranges = self.highlight_ranges_for_line(line_idx);

        // Handle wrapping
        match self.wrap_mode {
            LogWrapMode::NoWrap => {
                if y < max_y {
                    if let Some(ranges) = hl_ranges.filter(|r| !r.is_empty()) {
                        self.draw_highlighted_line(
                            &content,
                            ranges,
                            x,
                            y,
                            x.saturating_add(width),
                            frame,
                            effective_style,
                        );
                    } else {
                        self.draw_text_line(
                            line,
                            &content,
                            x,
                            y,
                            x.saturating_add(width),
                            frame,
                            effective_style,
                        );
                    }
                }
                1
            }
            LogWrapMode::CharWrap | LogWrapMode::WordWrap => {
                if content_width <= width as usize {
                    if y < max_y {
                        if let Some(ranges) = hl_ranges.filter(|r| !r.is_empty()) {
                            self.draw_highlighted_line(
                                &content,
                                ranges,
                                x,
                                y,
                                x.saturating_add(width),
                                frame,
                                effective_style,
                            );
                        } else {
                            self.draw_text_line(
                                line,
                                &content,
                                x,
                                y,
                                x.saturating_add(width),
                                frame,
                                effective_style,
                            );
                        }
                    }
                    1
                } else {
                    let options = WrapOptions::new(width as usize).mode(self.wrap_mode.into());
                    let wrapped = wrap_with_options(&content, &options);
                    let mut lines_rendered = 0u16;

                    for (i, part) in wrapped.into_iter().enumerate() {
                        let line_y = y.saturating_add(i as u16);
                        if line_y >= max_y {
                            break;
                        }
                        draw_text_span(
                            frame,
                            x,
                            line_y,
                            &part,
                            effective_style,
                            x.saturating_add(width),
                        );
                        lines_rendered += 1;
                    }

                    lines_rendered.max(1)
                }
            }
        }
    }

    /// Draw a line with search match highlights.
    #[allow(clippy::too_many_arguments)]
    fn draw_highlighted_line(
        &self,
        content: &str,
        ranges: &[(usize, usize)],
        x: u16,
        y: u16,
        max_x: u16,
        frame: &mut Frame,
        base_style: Style,
    ) {
        let hl_style = self
            .search_highlight_style
            .unwrap_or_else(|| Style::new().bold().reverse());
        let mut cursor_x = x;
        let mut pos = 0;

        for &(start, end) in ranges {
            let start = start.min(content.len());
            let end = end.min(content.len());
            if start > pos {
                // Draw non-highlighted segment
                cursor_x =
                    draw_text_span(frame, cursor_x, y, &content[pos..start], base_style, max_x);
            }
            if start < end {
                // Draw highlighted segment
                cursor_x =
                    draw_text_span(frame, cursor_x, y, &content[start..end], hl_style, max_x);
            }
            pos = end;
        }
        // Draw trailing non-highlighted text
        if pos < content.len() {
            draw_text_span(frame, cursor_x, y, &content[pos..], base_style, max_x);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_line(
        &self,
        line: Option<&ftui_text::Line>,
        fallback: &str,
        x: u16,
        y: u16,
        max_x: u16,
        frame: &mut Frame,
        base_style: Style,
    ) {
        if let Some(line) = line {
            let mut cursor_x = x;
            for span in line.spans() {
                if cursor_x >= max_x {
                    break;
                }
                let span_style = span
                    .style
                    .map_or(base_style, |style| style.merge(&base_style));
                cursor_x = draw_text_span_with_link(
                    frame,
                    cursor_x,
                    y,
                    span.as_str(),
                    span_style,
                    max_x,
                    span.link.as_deref(),
                );
            }
        } else {
            draw_text_span(frame, x, y, fallback, base_style, max_x);
        }
    }

    fn scroll_to_match(&mut self, idx: usize) {
        if let Some(indices) = self.filtered_indices.as_ref() {
            let position = indices.partition_point(|&v| v < idx);
            self.filtered_scroll_offset = position.min(indices.len().saturating_sub(1));
        } else {
            self.virt.scroll_to(idx);
        }
    }

    fn is_filtered_at_bottom(&self, total: usize, visible_count: usize) -> bool {
        if total == 0 || visible_count == 0 {
            return true;
        }
        self.filtered_scroll_offset >= total.saturating_sub(visible_count)
    }
}

impl StatefulWidget for LogViewer {
    type State = LogViewerState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Keep Virtualized's visible_count in sync even in filtered mode.
        let _ = self.virt.visible_range(area.height);

        // Update state with current viewport info
        state.last_viewport_height = area.height;

        let total_lines = self.virt.len();
        if total_lines == 0 {
            state.last_visible_lines = 0;
            return;
        }

        // Use filtered indices if a filter is active
        let render_indices: Option<&[usize]> = self.filtered_indices.as_deref();

        // Calculate visible range using Virtualized's scroll state
        let visible_count = area.height as usize;

        // Determine which lines to show
        let (start_idx, end_idx, _at_bottom_ignored) = if let Some(indices) = render_indices {
            // Filtered mode: show lines matching the filter
            let filtered_total = indices.len();
            if filtered_total == 0 {
                state.last_visible_lines = 0;
                return;
            }
            // Clamp scroll to filtered set
            let max_offset = filtered_total.saturating_sub(visible_count);
            let offset = self.filtered_scroll_offset.min(max_offset);
            let start = offset;
            let end = (offset + visible_count).min(filtered_total);
            let is_bottom = offset >= max_offset;
            (start, end, is_bottom)
        } else {
            // Unfiltered mode: use Virtualized's range directly
            let range = self.virt.visible_range(area.height);
            (range.start, range.end, self.virt.is_at_bottom())
        };

        let mut y = area.y;
        let mut lines_rendered = 0;
        let mut last_rendered_index = None;

        for display_idx in start_idx..end_idx {
            if y >= area.bottom() {
                break;
            }

            // Resolve to actual line index
            let line_idx = if let Some(indices) = render_indices {
                indices[display_idx]
            } else {
                display_idx
            };

            let Some(line) = self.virt.get(line_idx) else {
                continue;
            };

            let is_selected = state.selected_line == Some(line_idx);

            let lines_used = self.render_line(
                line,
                line_idx,
                area.x,
                y,
                area.width,
                area.bottom(),
                frame,
                is_selected,
            );

            y = y.saturating_add(lines_used);
            lines_rendered += 1;
            last_rendered_index = Some(display_idx);
        }

        state.last_visible_lines = lines_rendered;

        // Correct visible count in Virtualized based on actual wrapped rendering
        self.virt.set_visible_count(lines_rendered);

        // Determine if we are truly at the bottom (rendered the last item)
        let at_bottom = if let Some(indices) = render_indices {
            if let Some(last) = last_rendered_index {
                last >= indices.len().saturating_sub(1)
            } else {
                false
            }
        } else if let Some(last) = last_rendered_index {
            last >= total_lines.saturating_sub(1)
        } else {
            false
        };

        // Render scroll indicator if not at bottom
        if !at_bottom && area.width >= 4 {
            let lines_below = if let Some(indices) = render_indices {
                indices.len().saturating_sub(end_idx)
            } else {
                total_lines.saturating_sub(end_idx)
            };
            let indicator = format!(" {} ", lines_below);
            let indicator_len = display_width(&indicator) as u16;
            if indicator_len < area.width {
                let indicator_x = area.right().saturating_sub(indicator_len);
                let indicator_y = area.bottom().saturating_sub(1);
                draw_text_span(
                    frame,
                    indicator_x,
                    indicator_y,
                    &indicator,
                    Style::new().bold(),
                    area.right(),
                );
            }
        }

        // Render search indicator if active
        if let Some((current, total)) = self.search_info()
            && area.width >= 10
        {
            let search_indicator = format!(" {}/{} ", current, total);
            let ind_len = display_width(&search_indicator) as u16;
            if ind_len < area.width {
                let ind_x = area.x;
                let ind_y = area.bottom().saturating_sub(1);
                draw_text_span(
                    frame,
                    ind_x,
                    ind_y,
                    &search_indicator,
                    Style::new().bold(),
                    ind_x.saturating_add(ind_len),
                );
            }
        }
    }
}

/// Find match ranges using allocation-free case-insensitive search.
fn search_ascii_case_insensitive_ranges(haystack: &str, needle_lower: &str) -> Vec<(usize, usize)> {
    let mut results = Vec::new();
    if needle_lower.is_empty() {
        return results;
    }

    if !haystack.is_ascii() || !needle_lower.is_ascii() {
        return search_ascii_case_insensitive(haystack, needle_lower)
            .into_iter()
            .map(|r| (r.range.start, r.range.end))
            .collect();
    }

    let haystack_bytes = haystack.as_bytes();
    let needle_bytes = needle_lower.as_bytes();
    let needle_len = needle_bytes.len();

    if needle_len > haystack_bytes.len() {
        return results;
    }

    const MAX_WORK: usize = 4096;
    if haystack_bytes.len().saturating_mul(needle_len) > MAX_WORK {
        return search_ascii_case_insensitive(haystack, needle_lower)
            .into_iter()
            .map(|r| (r.range.start, r.range.end))
            .collect();
    }

    let mut i = 0;
    while i <= haystack_bytes.len() - needle_len {
        let mut match_found = true;
        for j in 0..needle_len {
            if haystack_bytes[i + j].to_ascii_lowercase() != needle_bytes[j] {
                match_found = false;
                break;
            }
        }
        if match_found {
            results.push((i, i + needle_len));
            i += needle_len;
        } else {
            i += 1;
        }
    }
    results
}

/// Find match byte ranges within a single line using the given config.
fn find_match_ranges(
    plain: &str,
    query: &str,
    query_lower: Option<&str>,
    config: &SearchConfig,
    #[cfg(feature = "regex-search")] compiled_regex: Option<&regex::Regex>,
) -> Vec<(usize, usize)> {
    match config.mode {
        SearchMode::Literal => {
            if config.case_sensitive {
                search_exact(plain, query)
                    .into_iter()
                    .map(|r| (r.range.start, r.range.end))
                    .collect()
            } else if let Some(lower) = query_lower {
                search_ascii_case_insensitive_ranges(plain, lower)
            } else {
                search_ascii_case_insensitive(plain, query)
                    .into_iter()
                    .map(|r| (r.range.start, r.range.end))
                    .collect()
            }
        }
        SearchMode::Regex => {
            #[cfg(feature = "regex-search")]
            {
                if let Some(re) = compiled_regex {
                    re.find_iter(plain).map(|m| (m.start(), m.end())).collect()
                } else {
                    Vec::new()
                }
            }
            #[cfg(not(feature = "regex-search"))]
            {
                // Without the feature, regex mode is a no-op.
                Vec::new()
            }
        }
    }
}

/// Compile a regex from the query, respecting case sensitivity.
#[cfg(feature = "regex-search")]
fn compile_regex(query: &str, config: &SearchConfig) -> Option<regex::Regex> {
    let pattern = if config.case_sensitive {
        query.to_string()
    } else {
        format!("(?i){}", query)
    };
    regex::Regex::new(&pattern).ok()
}

/// Expand match indices by ±N context lines, dedup and sort.
fn expand_context(matches: &[usize], context_lines: usize, total_lines: usize) -> Vec<usize> {
    let mut expanded = Vec::new();
    for &idx in matches {
        let start = idx.saturating_sub(context_lines);
        let end = (idx + context_lines + 1).min(total_lines);
        for i in start..end {
            expanded.push(i);
        }
    }
    expanded.sort_unstable();
    expanded.dedup();
    expanded
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::StyleFlags as RenderStyleFlags;
    use ftui_render::grapheme_pool::GraphemePool;
    use ftui_style::StyleFlags as TextStyleFlags;

    fn line_text(frame: &Frame, y: u16, width: u16) -> String {
        let mut out = String::with_capacity(width as usize);
        for x in 0..width {
            let ch = frame
                .buffer
                .get(x, y)
                .and_then(|cell| cell.content.as_char())
                .unwrap_or(' ');
            out.push(ch);
        }
        out
    }

    #[test]
    fn test_push_appends_to_end() {
        let mut log = LogViewer::new(100);
        log.push("line 1");
        log.push("line 2");
        assert_eq!(log.line_count(), 2);
    }

    #[test]
    fn test_circular_buffer_eviction() {
        let mut log = LogViewer::new(3);
        log.push("line 1");
        log.push("line 2");
        log.push("line 3");
        log.push("line 4"); // Should evict "line 1"
        assert_eq!(log.line_count(), 3);
    }

    #[test]
    fn test_auto_scroll_stays_at_bottom() {
        let mut log = LogViewer::new(100);
        log.push("line 1");
        assert!(log.is_at_bottom());
        log.push("line 2");
        assert!(log.is_at_bottom());
    }

    #[test]
    fn test_manual_scroll_disables_auto_scroll() {
        let mut log = LogViewer::new(100);
        log.virt.set_visible_count(10);
        for i in 0..50 {
            log.push(format!("line {}", i));
        }
        log.scroll_up(10);
        assert!(!log.auto_scroll_enabled());
        log.push("new line");
        assert!(!log.auto_scroll_enabled()); // Still scrolled up
    }

    #[test]
    fn test_scroll_to_bottom_reengages_auto_scroll() {
        let mut log = LogViewer::new(100);
        log.virt.set_visible_count(10);
        for i in 0..50 {
            log.push(format!("line {}", i));
        }
        log.scroll_up(10);
        log.scroll_to_bottom();
        assert!(log.is_at_bottom());
        assert!(log.auto_scroll_enabled());
    }

    #[test]
    fn test_scroll_down_reengages_at_bottom() {
        let mut log = LogViewer::new(100);
        log.virt.set_visible_count(10);
        for i in 0..50 {
            log.push(format!("line {}", i));
        }
        log.scroll_up(5);
        assert!(!log.auto_scroll_enabled());

        log.scroll_down(5);
        if log.is_at_bottom() {
            assert!(log.auto_scroll_enabled());
        }
    }

    #[test]
    fn test_scroll_to_top() {
        let mut log = LogViewer::new(100);
        for i in 0..50 {
            log.push(format!("line {}", i));
        }
        log.scroll_to_top();
        assert!(!log.auto_scroll_enabled());
    }

    #[test]
    fn test_page_up_down() {
        let mut log = LogViewer::new(100);
        log.virt.set_visible_count(10);
        for i in 0..50 {
            log.push(format!("line {}", i));
        }

        let state = LogViewerState {
            last_viewport_height: 10,
            ..Default::default()
        };

        assert!(log.is_at_bottom());

        log.page_up(&state);
        assert!(!log.is_at_bottom());

        log.page_down(&state);
        // After paging down from near-bottom, should be closer to bottom
    }

    #[test]
    fn test_clear() {
        let mut log = LogViewer::new(100);
        log.push("line 1");
        log.push("line 2");
        log.clear();
        assert_eq!(log.line_count(), 0);
    }

    #[test]
    fn test_push_many() {
        let mut log = LogViewer::new(100);
        log.push_many(["line 1", "line 2", "line 3"]);
        assert_eq!(log.line_count(), 3);
    }

    #[test]
    fn test_render_empty() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let log = LogViewer::new(100);
        let mut state = LogViewerState::default();

        log.render(Rect::new(0, 0, 80, 24), &mut frame, &mut state);

        assert_eq!(state.last_visible_lines, 0);
    }

    #[test]
    fn test_render_some_lines() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 10, &mut pool);
        let mut log = LogViewer::new(100);

        for i in 0..5 {
            log.push(format!("Line {}", i));
        }

        let mut state = LogViewerState::default();
        log.render(Rect::new(0, 0, 80, 10), &mut frame, &mut state);

        assert_eq!(state.last_viewport_height, 10);
        assert_eq!(state.last_visible_lines, 5);
    }

    #[test]
    fn test_toggle_follow() {
        let mut log = LogViewer::new(100);
        assert!(log.auto_scroll_enabled());
        log.toggle_follow();
        assert!(!log.auto_scroll_enabled());
        log.toggle_follow();
        assert!(log.auto_scroll_enabled());
    }

    #[test]
    fn test_filter_shows_matching_lines() {
        let mut log = LogViewer::new(100);
        log.push("INFO: starting");
        log.push("ERROR: something failed");
        log.push("INFO: processing");
        log.push("ERROR: another failure");
        log.push("INFO: done");

        log.set_filter(Some("ERROR"));
        assert_eq!(log.filtered_indices.as_ref().unwrap().len(), 2);

        // Clear filter
        log.set_filter(None);
        assert!(log.filtered_indices.is_none());
    }

    #[test]
    fn test_search_finds_matches() {
        let mut log = LogViewer::new(100);
        log.push("hello world");
        log.push("goodbye world");
        log.push("hello again");

        let count = log.search("hello");
        assert_eq!(count, 2);
        assert_eq!(log.search_info(), Some((1, 2)));
    }

    #[test]
    fn test_search_respects_filter() {
        let mut log = LogViewer::new(100);
        log.push("INFO: ok");
        log.push("ERROR: first");
        log.push("WARN: mid");
        log.push("ERROR: second");

        log.set_filter(Some("ERROR"));
        assert_eq!(log.search("WARN"), 0);
        assert_eq!(log.search("ERROR"), 2);
    }

    #[test]
    fn test_filter_clears_search() {
        let mut log = LogViewer::new(100);
        log.push("alpha");
        log.search("alpha");
        assert!(log.search_info().is_some());

        log.set_filter(Some("alpha"));
        assert!(log.search_info().is_none());
    }

    #[test]
    fn test_search_sets_filtered_scroll_offset() {
        let mut log = LogViewer::new(100);
        log.push("match one");
        log.push("line two");
        log.push("match three");
        log.push("match four");

        log.set_filter(Some("match"));
        log.search("match");

        assert_eq!(log.filtered_scroll_offset, 0);
        log.next_match();
        assert_eq!(log.filtered_scroll_offset, 1);
    }

    #[test]
    fn test_search_next_prev() {
        let mut log = LogViewer::new(100);
        log.push("match A");
        log.push("nothing here");
        log.push("match B");
        log.push("match C");

        log.search("match");
        assert_eq!(log.search_info(), Some((1, 3)));

        log.next_match();
        assert_eq!(log.search_info(), Some((2, 3)));

        log.next_match();
        assert_eq!(log.search_info(), Some((3, 3)));

        log.next_match(); // wraps around
        assert_eq!(log.search_info(), Some((1, 3)));

        log.prev_match(); // wraps back
        assert_eq!(log.search_info(), Some((3, 3)));
    }

    #[test]
    fn test_clear_search() {
        let mut log = LogViewer::new(100);
        log.push("hello");
        log.search("hello");
        assert!(log.search_info().is_some());

        log.clear_search();
        assert!(log.search_info().is_none());
    }

    #[test]
    fn test_filter_with_push() {
        let mut log = LogViewer::new(100);
        log.set_filter(Some("ERROR"));
        log.push("INFO: ok");
        log.push("ERROR: bad");
        log.push("INFO: fine");

        assert_eq!(log.filtered_indices.as_ref().unwrap().len(), 1);
        assert_eq!(log.filtered_indices.as_ref().unwrap()[0], 1);
    }

    #[test]
    fn test_eviction_adjusts_filter_indices() {
        let mut log = LogViewer::new(3);
        log.set_filter(Some("x"));
        log.push("x1");
        log.push("y2");
        log.push("x3");
        // At capacity: indices [0, 2]
        assert_eq!(log.filtered_indices.as_ref().unwrap(), &[0, 2]);

        log.push("y4"); // evicts "x1", indices should adjust
        // After eviction of 1 item: "x3" was at 2, now at 1
        assert_eq!(log.filtered_indices.as_ref().unwrap(), &[1]);
    }

    #[test]
    fn test_filter_scroll_offset_tracks_unfiltered_position() {
        let mut log = LogViewer::new(100);
        for i in 0..20 {
            if i == 2 || i == 10 || i == 15 {
                log.push(format!("match {}", i));
            } else {
                log.push(format!("line {}", i));
            }
        }

        log.virt.scroll_to(12);
        log.set_filter(Some("match"));

        // Matches before index 12 are at 2 and 10 -> offset should be 2.
        assert_eq!(log.filtered_scroll_offset, 2);
    }

    #[test]
    fn test_filtered_scroll_down_moves_within_filtered_list() {
        let mut log = LogViewer::new(100);
        log.push("match one");
        log.push("line two");
        log.push("match three");
        log.push("line four");
        log.push("match five");

        log.set_filter(Some("match"));
        log.scroll_to_top();
        log.scroll_down(1);

        assert_eq!(log.filtered_scroll_offset, 1);
    }

    // -----------------------------------------------------------------------
    // Incremental filter/search tests (bd-1b5h.11)
    // -----------------------------------------------------------------------

    #[test]
    fn test_incremental_filter_on_push_tracks_stats() {
        let mut log = LogViewer::new(100);
        log.set_filter(Some("ERROR"));
        // set_filter triggers one full rescan (on empty log).
        assert_eq!(log.filter_stats().full_rescans, 1);

        log.push("INFO: ok");
        log.push("ERROR: bad");
        log.push("INFO: fine");
        log.push("ERROR: worse");

        // 4 lines pushed with filter active → 4 incremental checks.
        assert_eq!(log.filter_stats().incremental_checks, 4);
        // 2 matched.
        assert_eq!(log.filter_stats().incremental_matches, 2);
        // No additional full rescans.
        assert_eq!(log.filter_stats().full_rescans, 1);
    }

    #[test]
    fn test_incremental_search_on_push() {
        let mut log = LogViewer::new(100);
        log.push("hello world");
        log.push("goodbye world");

        // Full search scan.
        let count = log.search("hello");
        assert_eq!(count, 1);
        assert_eq!(log.filter_stats().full_rescans, 1);

        // Push new lines while search is active → incremental search update.
        log.push("hello again");
        log.push("nothing here");

        // Search matches should include the new "hello again" line.
        assert_eq!(log.search_info(), Some((1, 2)));
        assert_eq!(log.filter_stats().incremental_search_matches, 1);
    }

    #[test]
    fn test_incremental_search_respects_active_filter() {
        let mut log = LogViewer::new(100);
        log.push("ERROR: hello");
        log.push("INFO: hello");

        log.set_filter(Some("ERROR"));
        let count = log.search("hello");
        assert_eq!(count, 1); // Only ERROR line passes filter.

        // Push new lines: only those passing filter should be search-matched.
        log.push("ERROR: hello again");
        log.push("INFO: hello again"); // Doesn't pass filter.

        assert_eq!(log.search_info(), Some((1, 2))); // Original + new ERROR.
        assert_eq!(log.filter_stats().incremental_search_matches, 1);
    }

    #[test]
    fn test_incremental_search_without_filter() {
        let mut log = LogViewer::new(100);
        log.push("first");
        log.search("match");
        assert_eq!(log.search_info(), None); // No matches.

        // Push matching line without any filter active.
        log.push("match found");
        assert_eq!(log.search_info(), Some((1, 1)));
        assert_eq!(log.filter_stats().incremental_search_matches, 1);
    }

    #[test]
    fn test_filter_stats_reset_on_clear() {
        let mut log = LogViewer::new(100);
        log.set_filter(Some("x"));
        log.push("x1");
        log.push("y2");

        assert!(log.filter_stats().incremental_checks > 0);
        log.clear();
        assert_eq!(log.filter_stats().incremental_checks, 0);
        assert_eq!(log.filter_stats().full_rescans, 0);
    }

    #[test]
    fn test_filter_stats_full_rescan_on_filter_change() {
        let mut log = LogViewer::new(100);
        for i in 0..100 {
            log.push(format!("line {}", i));
        }

        log.set_filter(Some("line 5"));
        assert_eq!(log.filter_stats().full_rescans, 1);
        assert_eq!(log.filter_stats().full_rescan_lines, 100);

        log.set_filter(Some("line 9"));
        assert_eq!(log.filter_stats().full_rescans, 2);
        assert_eq!(log.filter_stats().full_rescan_lines, 200);
    }

    #[test]
    fn test_filter_stats_manual_reset() {
        let mut log = LogViewer::new(100);
        log.set_filter(Some("x"));
        log.push("x1");
        assert!(log.filter_stats().incremental_checks > 0);

        log.filter_stats_mut().reset();
        assert_eq!(log.filter_stats().incremental_checks, 0);

        // Subsequent pushes still tracked after reset.
        log.push("x2");
        assert_eq!(log.filter_stats().incremental_checks, 1);
    }

    #[test]
    fn test_incremental_eviction_adjusts_search_matches() {
        let mut log = LogViewer::new(3);
        log.push("match A");
        log.push("no");
        log.push("match B");
        log.search("match");
        assert_eq!(log.search_info(), Some((1, 2)));

        // Push beyond capacity: evicts "match A".
        log.push("match C"); // Incremental search match.

        // "match A" evicted. "match B" index adjusted. "match C" added.
        let search = log.search.as_ref().unwrap();
        assert_eq!(search.matches.len(), 2);
        // All search match indices should be valid (< log.line_count()).
        for &idx in &search.matches {
            assert!(idx < log.line_count(), "Search index {} out of range", idx);
        }
    }

    #[test]
    fn test_no_stats_when_no_filter_or_search() {
        let mut log = LogViewer::new(100);
        log.push("line 1");
        log.push("line 2");

        assert_eq!(log.filter_stats().incremental_checks, 0);
        assert_eq!(log.filter_stats().full_rescans, 0);
        assert_eq!(log.filter_stats().incremental_search_matches, 0);
    }

    #[test]
    fn test_search_full_rescan_counts_lines() {
        let mut log = LogViewer::new(100);
        for i in 0..50 {
            log.push(format!("line {}", i));
        }

        log.search("line 1");
        assert_eq!(log.filter_stats().full_rescans, 1);
        assert_eq!(log.filter_stats().full_rescan_lines, 50);
    }

    #[test]
    fn test_search_full_rescan_on_filtered_counts_filtered_lines() {
        let mut log = LogViewer::new(100);
        for i in 0..50 {
            if i % 2 == 0 {
                log.push(format!("even {}", i));
            } else {
                log.push(format!("odd {}", i));
            }
        }

        log.set_filter(Some("even"));
        let initial_rescans = log.filter_stats().full_rescans;
        let initial_lines = log.filter_stats().full_rescan_lines;

        log.search("even 4");
        assert_eq!(log.filter_stats().full_rescans, initial_rescans + 1);
        // Search scanned only filtered lines (25 even lines).
        assert_eq!(log.filter_stats().full_rescan_lines, initial_lines + 25);
    }

    // -----------------------------------------------------------------------
    // Enhanced search tests (bd-1b5h.2)
    // -----------------------------------------------------------------------

    #[test]
    fn test_search_literal_case_sensitive() {
        let mut log = LogViewer::new(100);
        log.push("Hello World");
        log.push("hello world");
        log.push("HELLO WORLD");

        let config = SearchConfig {
            mode: SearchMode::Literal,
            case_sensitive: true,
            context_lines: 0,
        };
        let count = log.search_with_config("Hello", config);
        assert_eq!(count, 1);
        assert_eq!(log.search_info(), Some((1, 1)));
    }

    #[test]
    fn test_search_literal_case_insensitive() {
        let mut log = LogViewer::new(100);
        log.push("Hello World");
        log.push("hello world");
        log.push("HELLO WORLD");
        log.push("no match here");

        let config = SearchConfig {
            mode: SearchMode::Literal,
            case_sensitive: false,
            context_lines: 0,
        };
        let count = log.search_with_config("hello", config);
        assert_eq!(count, 3);
    }

    #[test]
    fn test_search_ascii_case_insensitive_fast_path_ranges() {
        let mut log = LogViewer::new(100);
        log.push("Alpha beta ALPHA beta alpha");

        let config = SearchConfig {
            mode: SearchMode::Literal,
            case_sensitive: false,
            context_lines: 0,
        };
        let count = log.search_with_config("alpha", config);
        assert_eq!(count, 1);

        let ranges = log.highlight_ranges_for_line(0).expect("match ranges");
        assert_eq!(ranges, &[(0, 5), (11, 16), (22, 27)]);
    }

    #[test]
    fn test_search_unicode_fallback_ranges() {
        let mut log = LogViewer::new(100);
        let line = "café résumé café";
        log.push(line);

        let config = SearchConfig {
            mode: SearchMode::Literal,
            case_sensitive: false,
            context_lines: 0,
        };
        let count = log.search_with_config("café", config);
        assert_eq!(count, 1);

        let expected: Vec<(usize, usize)> = search_exact(line, "café")
            .into_iter()
            .map(|r| (r.range.start, r.range.end))
            .collect();
        let ranges = log.highlight_ranges_for_line(0).expect("match ranges");
        assert_eq!(ranges, expected.as_slice());
    }

    #[test]
    fn test_search_highlight_ranges_stable_after_push() {
        let mut log = LogViewer::new(100);
        log.push("Alpha beta ALPHA beta alpha");

        let config = SearchConfig {
            mode: SearchMode::Literal,
            case_sensitive: false,
            context_lines: 0,
        };
        log.search_with_config("alpha", config);
        let before = log
            .highlight_ranges_for_line(0)
            .expect("match ranges")
            .to_vec();

        log.push("no match here");
        let after = log
            .highlight_ranges_for_line(0)
            .expect("match ranges")
            .to_vec();

        assert_eq!(before, after);
    }

    #[cfg(feature = "regex-search")]
    #[test]
    fn test_search_regex_basic() {
        let mut log = LogViewer::new(100);
        log.push("error: code 42");
        log.push("error: code 99");
        log.push("info: all good");
        log.push("error: code 7");

        let config = SearchConfig {
            mode: SearchMode::Regex,
            case_sensitive: true,
            context_lines: 0,
        };
        let count = log.search_with_config(r"error: code \d+", config);
        assert_eq!(count, 3);
    }

    #[cfg(feature = "regex-search")]
    #[test]
    fn test_search_regex_invalid_pattern() {
        let mut log = LogViewer::new(100);
        log.push("something");

        let config = SearchConfig {
            mode: SearchMode::Regex,
            case_sensitive: true,
            context_lines: 0,
        };
        // Invalid regex (unmatched paren)
        let count = log.search_with_config(r"(unclosed", config);
        assert_eq!(count, 0);
        assert!(log.search_info().is_none());
    }

    #[test]
    fn test_search_highlight_ranges() {
        let mut log = LogViewer::new(100);
        log.push("foo bar foo baz foo");

        let count = log.search("foo");
        assert_eq!(count, 1);

        let ranges = log.highlight_ranges_for_line(0);
        assert!(ranges.is_some());
        let ranges = ranges.unwrap();
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0], (0, 3));
        assert_eq!(ranges[1], (8, 11));
        assert_eq!(ranges[2], (16, 19));
    }

    #[test]
    fn test_search_context_lines() {
        let mut log = LogViewer::new(100);
        for i in 0..10 {
            log.push(format!("line {}", i));
        }

        let config = SearchConfig {
            mode: SearchMode::Literal,
            case_sensitive: true,
            context_lines: 1,
        };
        // "line 5" is at index 5
        let count = log.search_with_config("line 5", config);
        assert_eq!(count, 1);

        let ctx = log.context_line_indices();
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        // Should include lines 4, 5, 6
        assert!(ctx.contains(&4));
        assert!(ctx.contains(&5));
        assert!(ctx.contains(&6));
        assert!(!ctx.contains(&3));
        assert!(!ctx.contains(&7));
    }

    #[test]
    fn test_search_incremental_with_config() {
        let mut log = LogViewer::new(100);
        log.push("Hello World");

        let config = SearchConfig {
            mode: SearchMode::Literal,
            case_sensitive: false,
            context_lines: 0,
        };
        let count = log.search_with_config("hello", config);
        assert_eq!(count, 1);

        // Push new line that matches case-insensitively
        log.push("HELLO again");
        assert_eq!(log.search_info(), Some((1, 2)));

        // Push line that doesn't match
        log.push("goodbye");
        assert_eq!(log.search_info(), Some((1, 2)));
    }

    #[test]
    fn test_search_mode_switch() {
        let mut log = LogViewer::new(100);
        log.push("error 42");
        log.push("error 99");
        log.push("info ok");

        // First search: literal
        let count = log.search("error");
        assert_eq!(count, 2);

        // Switch to case-insensitive
        let config = SearchConfig {
            mode: SearchMode::Literal,
            case_sensitive: false,
            context_lines: 0,
        };
        let count = log.search_with_config("ERROR", config);
        assert_eq!(count, 2);

        // Switch back to case-sensitive — "ERROR" shouldn't match "error"
        let config = SearchConfig {
            mode: SearchMode::Literal,
            case_sensitive: true,
            context_lines: 0,
        };
        let count = log.search_with_config("ERROR", config);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_search_empty_query() {
        let mut log = LogViewer::new(100);
        log.push("something");

        let count = log.search("");
        assert_eq!(count, 0);
        assert!(log.search_info().is_none());

        let config = SearchConfig::default();
        let count = log.search_with_config("", config);
        assert_eq!(count, 0);
        assert!(log.search_info().is_none());
    }

    #[test]
    fn test_highlight_ranges_within_bounds() {
        let mut log = LogViewer::new(100);
        let lines = [
            "short",
            "hello world hello",
            "café résumé café",
            "🌍 emoji 🌍",
            "",
        ];
        for line in &lines {
            log.push(*line);
        }

        log.search("hello");

        // Check all highlight ranges are valid byte ranges
        for match_idx in 0..log.line_count() {
            if let Some(ranges) = log.highlight_ranges_for_line(match_idx)
                && let Some(item) = log.virt.get(match_idx)
            {
                let plain = item.to_plain_text();
                for &(start, end) in ranges {
                    assert!(
                        start <= end,
                        "Invalid range: start={} > end={} on line {}",
                        start,
                        end,
                        match_idx
                    );
                    assert!(
                        end <= plain.len(),
                        "Out of bounds: end={} > len={} on line {}",
                        end,
                        plain.len(),
                        match_idx
                    );
                }
            }
        }
    }

    #[test]
    fn test_search_match_rate_hint() {
        let mut log = LogViewer::new(100);
        log.set_filter(Some("x"));
        log.push("x match");
        log.search("match");
        log.push("x match again");
        log.push("x no");

        // 3 incremental checks, 1 search match
        let rate = log.search_match_rate_hint();
        assert!(rate > 0.0);
        assert!(rate <= 1.0);
    }

    #[test]
    fn test_large_scrollback_eviction_and_scroll_bounds() {
        let mut log = LogViewer::new(1_000);
        log.virt.set_visible_count(25);

        for i in 0..5_000 {
            log.push(format!("line {}", i));
        }

        assert_eq!(log.line_count(), 1_000);

        let first = log.virt.get(0).expect("first line");
        assert_eq!(first.lines()[0].to_plain_text(), "line 4000");

        let last = log
            .virt
            .get(log.line_count().saturating_sub(1))
            .expect("last line");
        assert_eq!(last.lines()[0].to_plain_text(), "line 4999");

        log.scroll_to_top();
        assert!(!log.auto_scroll_enabled());

        log.scroll_down(10_000);
        assert!(log.is_at_bottom());
        assert!(log.auto_scroll_enabled());

        let max_offset = log.line_count().saturating_sub(log.virt.visible_count());
        assert!(log.virt.scroll_offset() <= max_offset);
    }

    #[test]
    fn test_large_scrollback_render_top_and_bottom_lines() {
        let mut log = LogViewer::new(1_000);
        log.virt.set_visible_count(3);
        for i in 0..5_000 {
            log.push(format!("line {}", i));
        }

        let mut pool = GraphemePool::new();
        let mut state = LogViewerState::default();

        log.scroll_to_top();
        let mut frame = Frame::new(20, 3, &mut pool);
        log.render(Rect::new(0, 0, 20, 3), &mut frame, &mut state);
        let top_line = line_text(&frame, 0, 20);
        assert!(
            top_line.trim_end().starts_with("line 4000"),
            "expected top line to start with line 4000, got: {top_line:?}"
        );

        log.scroll_to_bottom();
        let mut frame = Frame::new(20, 3, &mut pool);
        log.render(Rect::new(0, 0, 20, 3), &mut frame, &mut state);
        let bottom_line = line_text(&frame, 2, 20);
        assert!(
            bottom_line.trim_end().starts_with("line 4999"),
            "expected bottom line to start with line 4999, got: {bottom_line:?}"
        );
    }

    #[test]
    fn test_filtered_autoscroll_respects_manual_position() {
        let mut log = LogViewer::new(200);
        log.virt.set_visible_count(2);

        log.push("match 1");
        log.push("skip");
        log.push("match 2");
        log.push("match 3");
        log.push("skip again");
        log.push("match 4");
        log.push("match 5");

        log.set_filter(Some("match"));
        assert!(log.is_at_bottom());

        log.scroll_up(2);
        let offset_before = log.filtered_scroll_offset;
        assert!(!log.is_at_bottom());

        log.push("match 6");
        assert_eq!(log.filtered_scroll_offset, offset_before);

        log.scroll_to_bottom();
        let offset_at_bottom = log.filtered_scroll_offset;
        log.push("match 7");
        assert!(log.filtered_scroll_offset >= offset_at_bottom);
        assert!(log.is_at_bottom());
    }

    #[test]
    fn test_markup_parsing_preserves_spans() {
        let mut log = LogViewer::new(100);
        let text = ftui_text::markup::parse_markup("[bold]Hello[/bold] [fg=red]world[/fg]!")
            .expect("markup parse failed");
        log.push(text);

        let item = log.virt.get(0).expect("log line");
        let line = &item.lines()[0];
        assert_eq!(line.to_plain_text(), "Hello world!");

        let spans = line.spans();
        assert!(spans.iter().any(|span| span.style.is_some()));
        assert!(spans.iter().any(|span| {
            span.style
                .and_then(|style| style.attrs)
                .is_some_and(|attrs| attrs.contains(TextStyleFlags::BOLD))
        }));
    }

    #[test]
    fn test_markup_renders_bold_cells() {
        let mut log = LogViewer::new(10);
        let text = ftui_text::markup::parse_markup("[bold]Hello[/bold] world")
            .expect("markup parse failed");
        log.push(text);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(16, 1, &mut pool);
        let mut state = LogViewerState::default();
        log.render(Rect::new(0, 0, 16, 1), &mut frame, &mut state);

        let rendered = line_text(&frame, 0, 16);
        assert!(rendered.trim_end().starts_with("Hello world"));
        for x in 0..5 {
            let cell = frame.buffer.get(x, 0).expect("cell");
            assert!(
                cell.attrs.has_flag(RenderStyleFlags::BOLD),
                "expected bold at x={x}, attrs={:?}",
                cell.attrs.flags()
            );
        }
    }

    #[test]
    fn test_toggle_follow_disables_autoscroll_on_push() {
        let mut log = LogViewer::new(100);
        log.virt.set_visible_count(3);
        for i in 0..5 {
            log.push(format!("line {}", i));
        }
        assert!(log.is_at_bottom());

        log.toggle_follow();
        assert!(!log.auto_scroll_enabled());

        log.push("new line");
        assert!(!log.auto_scroll_enabled());
        assert!(!log.is_at_bottom());
    }

    #[test]
    fn test_search_match_rate_hint_ratio() {
        let mut log = LogViewer::new(100);
        assert_eq!(log.search_match_rate_hint(), 0.0);

        log.set_filter(Some("ERR"));
        log.search("ERR");

        log.push("ERR one");
        log.push("INFO skip");
        log.push("ERR two");
        log.push("WARN skip");

        assert_eq!(log.filter_stats().incremental_checks, 4);
        assert_eq!(log.filter_stats().incremental_search_checks, 2);
        assert_eq!(log.filter_stats().incremental_search_matches, 2);
        assert_eq!(log.search_match_rate_hint(), 1.0);
    }

    #[test]
    fn test_render_char_wrap_splits_lines() {
        let mut log = LogViewer::new(10).wrap_mode(LogWrapMode::CharWrap);
        log.push("abcdefghij");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        let mut state = LogViewerState::default();
        log.render(Rect::new(0, 0, 5, 3), &mut frame, &mut state);

        assert_eq!(line_text(&frame, 0, 5), "abcde");
        assert_eq!(line_text(&frame, 1, 5), "fghij");
    }

    #[test]
    fn test_render_scroll_indicator_when_not_at_bottom() {
        let mut log = LogViewer::new(100);
        for i in 0..5 {
            log.push(format!("line {}", i));
        }

        log.scroll_to_top();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 2, &mut pool);
        let mut state = LogViewerState::default();
        log.render(Rect::new(0, 0, 10, 2), &mut frame, &mut state);

        let indicator = " 3 ";
        let bottom_line = line_text(&frame, 1, 10);
        assert_eq!(&bottom_line[7..10], indicator);
    }

    #[test]
    fn test_render_search_indicator_when_active() {
        let mut log = LogViewer::new(100);
        for i in 0..5 {
            log.push(format!("line {}", i));
        }
        log.search("line");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 2, &mut pool);
        let mut state = LogViewerState::default();
        log.render(Rect::new(0, 0, 12, 2), &mut frame, &mut state);

        let indicator = " 1/5 ";
        let bottom_line = line_text(&frame, 1, 12);
        assert_eq!(&bottom_line[0..indicator.len()], indicator);
    }

    #[test]
    fn test_search_ascii_case_insensitive_ranges_long_needle() {
        let ranges = search_ascii_case_insensitive_ranges("hi", "hello");
        assert!(ranges.is_empty());
    }

    #[test]
    fn test_search_ascii_case_insensitive_ranges_large_work_fallback() {
        let mut haystack = "a".repeat(500);
        haystack.push_str("HELLO");
        haystack.push_str(&"b".repeat(500));

        let ranges = search_ascii_case_insensitive_ranges(&haystack, "hello");
        assert_eq!(ranges, vec![(500, 505)]);
    }
}

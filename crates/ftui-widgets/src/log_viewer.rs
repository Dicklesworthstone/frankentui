#![forbid(unsafe_code)]

//! A scrolling log viewer widget optimized for streaming append-only content.
//!
//! `LogViewer` is THE essential widget for agent harness UIs. It displays streaming
//! logs with scrollback while maintaining UI chrome and handles:
//!
//! - High-frequency log line additions without flicker
//! - Auto-scroll behavior for "follow" mode
//! - Manual scrolling to inspect history
//! - Memory bounds via circular buffer
//!
//! # Example
//! ```ignore
//! use ftui_widgets::log_viewer::{LogViewer, LogViewerState, WrapMode};
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

use std::collections::VecDeque;

use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::{Text, WrapMode, WrapOptions, display_width, wrap_with_options};

use crate::{StatefulWidget, draw_text_span};

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

/// A scrolling log viewer optimized for streaming append-only content.
///
/// # Design Rationale
/// - VecDeque for O(1) push/pop at both ends (circular buffer eviction)
/// - Separate scroll_offset from auto_scroll flag for manual override
/// - wrap_mode configurable per-instance for different use cases
/// - Stateful widget pattern for scroll state preservation across renders
#[derive(Debug, Clone)]
pub struct LogViewer {
    /// Log lines stored as styled Text (supports colors, hyperlinks).
    lines: VecDeque<Text>,
    /// Maximum lines to retain (memory bound).
    max_lines: usize,
    /// Current scroll offset from bottom (0 = bottom).
    scroll_offset: usize,
    /// Auto-scroll enabled (re-engages when scrolled to bottom).
    auto_scroll: bool,
    /// Line wrapping mode.
    wrap_mode: LogWrapMode,
    /// Default style for lines.
    style: Style,
    /// Highlight style for selected/focused line.
    highlight_style: Option<Style>,
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
            lines: VecDeque::with_capacity(max_lines.min(1024)),
            max_lines,
            scroll_offset: 0,
            auto_scroll: true,
            wrap_mode: LogWrapMode::NoWrap,
            style: Style::default(),
            highlight_style: None,
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

    /// Append a single log line.
    ///
    /// # Performance
    /// - O(1) amortized for append
    /// - O(1) for eviction when at capacity
    ///
    /// # Auto-scroll Behavior
    /// If auto_scroll is enabled, view stays at bottom after push.
    pub fn push(&mut self, line: impl Into<Text>) {
        // Evict oldest if at capacity
        if self.lines.len() >= self.max_lines {
            self.lines.pop_front();
        }

        self.lines.push_back(line.into());

        // Auto-scroll: keep view at bottom
        if self.auto_scroll {
            self.scroll_offset = 0;
        }
    }

    /// Append multiple lines efficiently.
    pub fn push_many(&mut self, lines: impl IntoIterator<Item = impl Into<Text>>) {
        for line in lines {
            self.push(line);
        }
    }

    /// Scroll up by N lines. Disables auto-scroll.
    pub fn scroll_up(&mut self, lines: usize) {
        self.auto_scroll = false;
        let max_offset = self.lines.len().saturating_sub(1);
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(max_offset);
    }

    /// Scroll down by N lines. Re-enables auto-scroll if at bottom.
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// Jump to top of log history.
    pub fn scroll_to_top(&mut self) {
        self.auto_scroll = false;
        self.scroll_offset = self.lines.len().saturating_sub(1);
    }

    /// Jump to bottom and re-enable auto-scroll.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Page up (scroll by viewport height).
    pub fn page_up(&mut self, state: &LogViewerState) {
        let page_size = state.last_viewport_height.max(1) as usize;
        self.scroll_up(page_size);
    }

    /// Page down (scroll by viewport height).
    pub fn page_down(&mut self, state: &LogViewerState) {
        let page_size = state.last_viewport_height.max(1) as usize;
        self.scroll_down(page_size);
    }

    /// Check if currently scrolled to the bottom.
    #[must_use]
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }

    /// Total line count in buffer.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Check if auto-scroll is enabled.
    #[must_use]
    pub fn auto_scroll_enabled(&self) -> bool {
        self.auto_scroll
    }

    /// Set auto-scroll state.
    pub fn set_auto_scroll(&mut self, enabled: bool) {
        self.auto_scroll = enabled;
        if enabled {
            self.scroll_offset = 0;
        }
    }

    /// Clear all lines.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.scroll_offset = 0;
    }

    /// Render a single line with optional wrapping.
    #[allow(clippy::too_many_arguments)]
    fn render_line(
        &self,
        text: &Text,
        x: u16,
        y: u16,
        width: u16,
        max_y: u16,
        frame: &mut Frame,
        is_selected: bool,
    ) -> u16 {
        // For now, use default style. Text doesn't have a single style() method
        // since it contains multiple spans. Individual span styles are preserved
        // in to_plain_text() rendering.
        let effective_style = if is_selected {
            self.highlight_style.unwrap_or(self.style)
        } else {
            self.style
        };

        let content = text.to_plain_text();
        let content_width = display_width(&content);

        // Handle wrapping
        match self.wrap_mode {
            LogWrapMode::NoWrap => {
                // Truncate if needed
                if y < max_y {
                    draw_text_span(frame, x, y, &content, effective_style, x + width);
                }
                1
            }
            LogWrapMode::CharWrap | LogWrapMode::WordWrap => {
                if content_width <= width as usize {
                    // No wrap needed
                    if y < max_y {
                        draw_text_span(frame, x, y, &content, effective_style, x + width);
                    }
                    1
                } else {
                    // Wrap the line
                    let options = WrapOptions::new(width as usize).mode(self.wrap_mode.into());
                    let wrapped = wrap_with_options(&content, &options);
                    let mut lines_rendered = 0u16;

                    for (i, part) in wrapped.into_iter().enumerate() {
                        let line_y = y.saturating_add(i as u16);
                        if line_y >= max_y {
                            break;
                        }
                        draw_text_span(frame, x, line_y, &part, effective_style, x + width);
                        lines_rendered += 1;
                    }

                    lines_rendered.max(1)
                }
            }
        }
    }
}

impl StatefulWidget for LogViewer {
    type State = LogViewerState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Update state with current viewport info
        state.last_viewport_height = area.height;

        // Calculate visible range (scroll_offset=0 means newest at bottom)
        let visible_count = area.height as usize;
        let total_lines = self.lines.len();

        if total_lines == 0 {
            state.last_visible_lines = 0;
            return;
        }

        let end_idx = total_lines.saturating_sub(self.scroll_offset);
        let start_idx = end_idx.saturating_sub(visible_count);

        // For wrapped lines, we need to be smarter about what's visible
        // For now, use simple line-based calculation
        let mut y = area.y;
        let mut lines_rendered = 0;

        for line_idx in start_idx..end_idx {
            if y >= area.bottom() {
                break;
            }

            let line = &self.lines[line_idx];
            let is_selected = state.selected_line == Some(line_idx);

            let lines_used = self.render_line(
                line,
                area.x,
                y,
                area.width,
                area.bottom(),
                frame,
                is_selected,
            );

            y = y.saturating_add(lines_used);
            lines_rendered += 1;
        }

        state.last_visible_lines = lines_rendered;

        // Render scroll indicator if not at bottom
        if self.scroll_offset > 0 && area.width >= 4 {
            let indicator = format!(" {} ", self.scroll_offset);
            let indicator_len = indicator.len() as u16;
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

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
        for i in 0..50 {
            log.push(format!("line {}", i));
        }
        log.scroll_up(10);
        assert!(!log.is_at_bottom());
        log.push("new line");
        assert!(!log.is_at_bottom()); // Still scrolled up
    }

    #[test]
    fn test_scroll_to_bottom_reengages_auto_scroll() {
        let mut log = LogViewer::new(100);
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
        for i in 0..50 {
            log.push(format!("line {}", i));
        }
        log.scroll_up(5);
        assert!(!log.auto_scroll_enabled());

        log.scroll_down(5);
        assert!(log.is_at_bottom());
        assert!(log.auto_scroll_enabled());
    }

    #[test]
    fn test_scroll_to_top() {
        let mut log = LogViewer::new(100);
        for i in 0..50 {
            log.push(format!("line {}", i));
        }
        log.scroll_to_top();
        assert_eq!(log.scroll_offset, 49); // At top
        assert!(!log.auto_scroll_enabled());
    }

    #[test]
    fn test_page_up_down() {
        let mut log = LogViewer::new(100);
        for i in 0..50 {
            log.push(format!("line {}", i));
        }

        let state = LogViewerState {
            last_viewport_height: 10,
            ..Default::default()
        };

        log.page_up(&state);
        assert_eq!(log.scroll_offset, 10);

        log.page_down(&state);
        assert_eq!(log.scroll_offset, 0);
    }

    #[test]
    fn test_clear() {
        let mut log = LogViewer::new(100);
        log.push("line 1");
        log.push("line 2");
        log.clear();
        assert_eq!(log.line_count(), 0);
        assert_eq!(log.scroll_offset, 0);
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
}

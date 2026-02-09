#![forbid(unsafe_code)]

//! History panel widget for displaying undo/redo command history.
//!
//! Renders a styled list of command descriptions showing the undo/redo history
//! stack. The current position in the history is marked to indicate what will
//! be undone/redone next.
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::history_panel::HistoryPanel;
//!
//! let panel = HistoryPanel::new()
//!     .with_undo_items(&["Insert text", "Delete word"])
//!     .with_redo_items(&["Paste"])
//!     .with_title("History");
//! ```

use crate::{Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::wrap::display_width;

/// A single entry in the history panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Description of the command.
    pub description: String,
    /// Whether this entry is in the undo or redo stack.
    pub is_redo: bool,
}

impl HistoryEntry {
    /// Create a new history entry.
    #[must_use]
    pub fn new(description: impl Into<String>, is_redo: bool) -> Self {
        Self {
            description: description.into(),
            is_redo,
        }
    }
}

/// Display mode for the history panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryPanelMode {
    /// Compact mode: shows only the most recent undo/redo items.
    #[default]
    Compact,
    /// Full mode: shows the complete history stack.
    Full,
}

/// History panel widget that displays undo/redo command history.
///
/// The panel shows commands in chronological order with the current position
/// marked. Commands above the marker can be undone, commands below can be redone.
#[derive(Debug, Clone)]
pub struct HistoryPanel {
    /// Title displayed at the top of the panel.
    title: String,
    /// Entries in the undo stack (oldest first).
    undo_items: Vec<String>,
    /// Entries in the redo stack (oldest first).
    redo_items: Vec<String>,
    /// Display mode.
    mode: HistoryPanelMode,
    /// Maximum items to show in compact mode.
    compact_limit: usize,
    /// Style for the title.
    title_style: Style,
    /// Style for undo items.
    undo_style: Style,
    /// Style for redo items (dimmed, as they are "future" commands).
    redo_style: Style,
    /// Style for the current position marker.
    marker_style: Style,
    /// Style for the panel background.
    bg_style: Style,
    /// Current position marker text.
    marker_text: String,
    /// Undo icon prefix.
    undo_icon: String,
    /// Redo icon prefix.
    redo_icon: String,
}

impl Default for HistoryPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryPanel {
    /// Create a new history panel with no entries.
    #[must_use]
    pub fn new() -> Self {
        Self {
            title: "History".to_string(),
            undo_items: Vec::new(),
            redo_items: Vec::new(),
            mode: HistoryPanelMode::Compact,
            compact_limit: 5,
            title_style: Style::new().bold(),
            undo_style: Style::default(),
            redo_style: Style::new().dim(),
            marker_style: Style::new().bold(),
            bg_style: Style::default(),
            marker_text: "─── current ───".to_string(),
            undo_icon: "↶ ".to_string(),
            redo_icon: "↷ ".to_string(),
        }
    }

    /// Set the panel title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the undo items (descriptions from oldest to newest).
    #[must_use]
    pub fn with_undo_items(mut self, items: &[impl AsRef<str>]) -> Self {
        self.undo_items = items.iter().map(|s| s.as_ref().to_string()).collect();
        self
    }

    /// Set the redo items (descriptions from oldest to newest).
    #[must_use]
    pub fn with_redo_items(mut self, items: &[impl AsRef<str>]) -> Self {
        self.redo_items = items.iter().map(|s| s.as_ref().to_string()).collect();
        self
    }

    /// Set the display mode.
    #[must_use]
    pub fn with_mode(mut self, mode: HistoryPanelMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set the compact mode limit.
    #[must_use]
    pub fn with_compact_limit(mut self, limit: usize) -> Self {
        self.compact_limit = limit;
        self
    }

    /// Set the title style.
    #[must_use]
    pub fn with_title_style(mut self, style: Style) -> Self {
        self.title_style = style;
        self
    }

    /// Set the undo items style.
    #[must_use]
    pub fn with_undo_style(mut self, style: Style) -> Self {
        self.undo_style = style;
        self
    }

    /// Set the redo items style.
    #[must_use]
    pub fn with_redo_style(mut self, style: Style) -> Self {
        self.redo_style = style;
        self
    }

    /// Set the marker style.
    #[must_use]
    pub fn with_marker_style(mut self, style: Style) -> Self {
        self.marker_style = style;
        self
    }

    /// Set the background style.
    #[must_use]
    pub fn with_bg_style(mut self, style: Style) -> Self {
        self.bg_style = style;
        self
    }

    /// Set the marker text.
    #[must_use]
    pub fn with_marker_text(mut self, text: impl Into<String>) -> Self {
        self.marker_text = text.into();
        self
    }

    /// Set the undo icon prefix.
    #[must_use]
    pub fn with_undo_icon(mut self, icon: impl Into<String>) -> Self {
        self.undo_icon = icon.into();
        self
    }

    /// Set the redo icon prefix.
    #[must_use]
    pub fn with_redo_icon(mut self, icon: impl Into<String>) -> Self {
        self.redo_icon = icon.into();
        self
    }

    /// Check if there are any history items.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.undo_items.is_empty() && self.redo_items.is_empty()
    }

    /// Get the total number of items.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.undo_items.len() + self.redo_items.len()
    }

    /// Get the undo stack items.
    #[must_use]
    pub fn undo_items(&self) -> &[String] {
        &self.undo_items
    }

    /// Get the redo stack items.
    #[must_use]
    pub fn redo_items(&self) -> &[String] {
        &self.redo_items
    }

    /// Render the panel content.
    fn render_content(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let max_x = area.right();
        let mut row: u16 = 0;

        // Title
        if row < area.height && !self.title.is_empty() {
            let y = area.y.saturating_add(row);
            draw_text_span(frame, area.x, y, &self.title, self.title_style, max_x);
            row += 1;

            // Blank line after title
            if row < area.height {
                row += 1;
            }
        }

        // Determine which items to show based on mode
        let (undo_to_show, redo_to_show) = match self.mode {
            HistoryPanelMode::Compact => {
                let half_limit = self.compact_limit / 2;
                let undo_start = self.undo_items.len().saturating_sub(half_limit);
                let redo_end = half_limit.min(self.redo_items.len());
                (&self.undo_items[undo_start..], &self.redo_items[..redo_end])
            }
            HistoryPanelMode::Full => (&self.undo_items[..], &self.redo_items[..]),
        };

        // Show ellipsis if there are hidden undo items
        if self.mode == HistoryPanelMode::Compact
            && undo_to_show.len() < self.undo_items.len()
            && row < area.height
        {
            let y = area.y.saturating_add(row);
            let hidden = self.undo_items.len() - undo_to_show.len();
            let text = format!("... ({} more)", hidden);
            draw_text_span(frame, area.x, y, &text, self.redo_style, max_x);
            row += 1;
        }

        // Undo items (oldest first, so they appear top-to-bottom chronologically)
        for desc in undo_to_show {
            if row >= area.height {
                break;
            }
            let y = area.y.saturating_add(row);
            let icon_end =
                draw_text_span(frame, area.x, y, &self.undo_icon, self.undo_style, max_x);
            draw_text_span(frame, icon_end, y, desc, self.undo_style, max_x);
            row += 1;
        }

        // Current position marker
        if row < area.height {
            let y = area.y.saturating_add(row);
            // Center the marker
            let marker_width = display_width(&self.marker_text);
            let available = area.width as usize;
            let pad_left = available.saturating_sub(marker_width) / 2;
            let x = area.x.saturating_add(pad_left as u16);
            draw_text_span(frame, x, y, &self.marker_text, self.marker_style, max_x);
            row += 1;
        }

        // Redo items (these are "future" commands that can be redone)
        for desc in redo_to_show {
            if row >= area.height {
                break;
            }
            let y = area.y.saturating_add(row);
            let icon_end =
                draw_text_span(frame, area.x, y, &self.redo_icon, self.redo_style, max_x);
            draw_text_span(frame, icon_end, y, desc, self.redo_style, max_x);
            row += 1;
        }

        // Show ellipsis if there are hidden redo items
        if self.mode == HistoryPanelMode::Compact
            && redo_to_show.len() < self.redo_items.len()
            && row < area.height
        {
            let y = area.y.saturating_add(row);
            let hidden = self.redo_items.len() - redo_to_show.len();
            let text = format!("... ({} more)", hidden);
            draw_text_span(frame, area.x, y, &text, self.redo_style, max_x);
        }
    }
}

impl Widget for HistoryPanel {
    fn render(&self, area: Rect, frame: &mut Frame) {
        // Fill background if style is set
        if let Some(bg) = self.bg_style.bg {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    if let Some(cell) = frame.buffer.get_mut(x, y) {
                        cell.bg = bg;
                    }
                }
            }
        }

        self.render_content(area, frame);
    }

    fn is_essential(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn new_panel_is_empty() {
        let panel = HistoryPanel::new();
        assert!(panel.is_empty());
        assert_eq!(panel.len(), 0);
    }

    #[test]
    fn with_undo_items() {
        let panel = HistoryPanel::new().with_undo_items(&["Insert text", "Delete word"]);
        assert_eq!(panel.undo_items().len(), 2);
        assert_eq!(panel.undo_items()[0], "Insert text");
        assert_eq!(panel.len(), 2);
    }

    #[test]
    fn with_redo_items() {
        let panel = HistoryPanel::new().with_redo_items(&["Paste"]);
        assert_eq!(panel.redo_items().len(), 1);
        assert_eq!(panel.len(), 1);
    }

    #[test]
    fn with_both_stacks() {
        let panel = HistoryPanel::new()
            .with_undo_items(&["A", "B"])
            .with_redo_items(&["C"]);
        assert!(!panel.is_empty());
        assert_eq!(panel.len(), 3);
    }

    #[test]
    fn with_title() {
        let panel = HistoryPanel::new().with_title("My History");
        assert_eq!(panel.title, "My History");
    }

    #[test]
    fn with_mode() {
        let panel = HistoryPanel::new().with_mode(HistoryPanelMode::Full);
        assert_eq!(panel.mode, HistoryPanelMode::Full);
    }

    #[test]
    fn render_empty() {
        let panel = HistoryPanel::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 10, &mut pool);
        let area = Rect::new(0, 0, 30, 10);
        panel.render(area, &mut frame); // Should not panic
    }

    #[test]
    fn render_with_items() {
        let panel = HistoryPanel::new()
            .with_undo_items(&["Insert text"])
            .with_redo_items(&["Delete word"]);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 10, &mut pool);
        let area = Rect::new(0, 0, 30, 10);
        panel.render(area, &mut frame);

        // Verify title appears
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('H')); // "History"
    }

    #[test]
    fn render_zero_area() {
        let panel = HistoryPanel::new().with_undo_items(&["Test"]);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 10, &mut pool);
        let area = Rect::new(0, 0, 0, 0);
        panel.render(area, &mut frame); // Should not panic
    }

    #[test]
    fn compact_limit() {
        let items: Vec<_> = (0..10).map(|i| format!("Item {}", i)).collect();
        let panel = HistoryPanel::new()
            .with_mode(HistoryPanelMode::Compact)
            .with_compact_limit(4)
            .with_undo_items(&items);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 20, &mut pool);
        let area = Rect::new(0, 0, 30, 20);
        panel.render(area, &mut frame); // Should show only last 2 undo items
    }

    #[test]
    fn is_not_essential() {
        let panel = HistoryPanel::new();
        assert!(!panel.is_essential());
    }

    #[test]
    fn default_impl() {
        let panel = HistoryPanel::default();
        assert!(panel.is_empty());
    }

    #[test]
    fn with_icons() {
        let panel = HistoryPanel::new()
            .with_undo_icon("<< ")
            .with_redo_icon(">> ");
        assert_eq!(panel.undo_icon, "<< ");
        assert_eq!(panel.redo_icon, ">> ");
    }

    #[test]
    fn with_marker_text() {
        let panel = HistoryPanel::new().with_marker_text("=== NOW ===");
        assert_eq!(panel.marker_text, "=== NOW ===");
    }

    #[test]
    fn history_entry_new() {
        let entry = HistoryEntry::new("Delete line", false);
        assert_eq!(entry.description, "Delete line");
        assert!(!entry.is_redo);

        let redo = HistoryEntry::new("Paste", true);
        assert!(redo.is_redo);
    }

    #[test]
    fn history_panel_mode_default_is_compact() {
        assert_eq!(HistoryPanelMode::default(), HistoryPanelMode::Compact);
    }

    #[test]
    fn with_compact_limit_setter() {
        let panel = HistoryPanel::new().with_compact_limit(10);
        assert_eq!(panel.compact_limit, 10);
    }

    #[test]
    fn history_entry_equality() {
        let a = HistoryEntry::new("X", false);
        let b = HistoryEntry::new("X", false);
        let c = HistoryEntry::new("X", true);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn full_mode_renders_all_items() {
        let items: Vec<_> = (0..10).map(|i| format!("Item {i}")).collect();
        let panel = HistoryPanel::new()
            .with_mode(HistoryPanelMode::Full)
            .with_undo_items(&items);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 30, &mut pool);
        let area = Rect::new(0, 0, 30, 30);
        panel.render(area, &mut frame); // Should not panic in full mode
    }
}

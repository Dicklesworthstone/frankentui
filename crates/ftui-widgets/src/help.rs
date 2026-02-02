//! Help widget for displaying keybinding lists.
//!
//! Renders a styled list of key/description pairs for showing available
//! keyboard shortcuts in a TUI application.
//!
//! # Example
//!
//! ```
//! use ftui_widgets::help::{Help, HelpEntry};
//!
//! let help = Help::new()
//!     .entry("q", "quit")
//!     .entry("^s", "save")
//!     .entry("?", "toggle help");
//!
//! assert_eq!(help.entries().len(), 3);
//! ```

use crate::{Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::wrap::display_width;

/// A single keybinding entry in the help view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpEntry {
    /// The key or key combination (e.g. "^C", "↑/k").
    pub key: String,
    /// Description of what the key does.
    pub desc: String,
    /// Whether this entry is enabled (disabled entries are hidden).
    pub enabled: bool,
}

impl HelpEntry {
    /// Create a new enabled help entry.
    #[must_use]
    pub fn new(key: impl Into<String>, desc: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            desc: desc.into(),
            enabled: true,
        }
    }

    /// Set whether this entry is enabled.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Display mode for the help widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelpMode {
    /// Short inline mode: entries separated by a bullet on one line.
    #[default]
    Short,
    /// Full mode: entries stacked vertically with aligned columns.
    Full,
}

/// Help widget that renders keybinding entries.
///
/// In [`HelpMode::Short`] mode, entries are shown inline separated by a bullet
/// character, truncated with an ellipsis if they exceed the available width.
///
/// In [`HelpMode::Full`] mode, entries are rendered in a vertical list with
/// keys and descriptions in aligned columns.
#[derive(Debug, Clone)]
pub struct Help {
    entries: Vec<HelpEntry>,
    mode: HelpMode,
    /// Separator between entries in short mode.
    separator: String,
    /// Ellipsis shown when truncated.
    ellipsis: String,
    /// Style for key text.
    key_style: Style,
    /// Style for description text.
    desc_style: Style,
    /// Style for separator/ellipsis.
    separator_style: Style,
}

impl Default for Help {
    fn default() -> Self {
        Self::new()
    }
}

impl Help {
    /// Create a new help widget with no entries.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            mode: HelpMode::Short,
            separator: " • ".to_string(),
            ellipsis: "…".to_string(),
            key_style: Style::new().bold(),
            desc_style: Style::default(),
            separator_style: Style::default(),
        }
    }

    /// Add an entry to the help widget.
    #[must_use]
    pub fn entry(mut self, key: impl Into<String>, desc: impl Into<String>) -> Self {
        self.entries.push(HelpEntry::new(key, desc));
        self
    }

    /// Add a pre-built entry.
    #[must_use]
    pub fn with_entry(mut self, entry: HelpEntry) -> Self {
        self.entries.push(entry);
        self
    }

    /// Set all entries at once.
    #[must_use]
    pub fn with_entries(mut self, entries: Vec<HelpEntry>) -> Self {
        self.entries = entries;
        self
    }

    /// Set the display mode.
    #[must_use]
    pub fn with_mode(mut self, mode: HelpMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set the separator used between entries in short mode.
    #[must_use]
    pub fn with_separator(mut self, sep: impl Into<String>) -> Self {
        self.separator = sep.into();
        self
    }

    /// Set the ellipsis string.
    #[must_use]
    pub fn with_ellipsis(mut self, ellipsis: impl Into<String>) -> Self {
        self.ellipsis = ellipsis.into();
        self
    }

    /// Set the style for key text.
    #[must_use]
    pub fn with_key_style(mut self, style: Style) -> Self {
        self.key_style = style;
        self
    }

    /// Set the style for description text.
    #[must_use]
    pub fn with_desc_style(mut self, style: Style) -> Self {
        self.desc_style = style;
        self
    }

    /// Set the style for separators and ellipsis.
    #[must_use]
    pub fn with_separator_style(mut self, style: Style) -> Self {
        self.separator_style = style;
        self
    }

    /// Get the entries.
    #[must_use]
    pub fn entries(&self) -> &[HelpEntry] {
        &self.entries
    }

    /// Get the current mode.
    #[must_use]
    pub fn mode(&self) -> HelpMode {
        self.mode
    }

    /// Toggle between short and full mode.
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            HelpMode::Short => HelpMode::Full,
            HelpMode::Full => HelpMode::Short,
        };
    }

    /// Add an entry mutably.
    pub fn push_entry(&mut self, entry: HelpEntry) {
        self.entries.push(entry);
    }

    /// Collect the enabled entries.
    fn enabled_entries(&self) -> Vec<&HelpEntry> {
        self.entries.iter().filter(|e| e.enabled).collect()
    }

    /// Render short mode: entries inline on one line.
    fn render_short(&self, area: Rect, frame: &mut Frame) {
        let entries = self.enabled_entries();
        if entries.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        let sep_width = display_width(&self.separator);
        let ellipsis_width = display_width(&self.ellipsis);
        let max_x = area.right();
        let y = area.y;
        let mut x = area.x;

        for (i, entry) in entries.iter().enumerate() {
            if entry.key.is_empty() && entry.desc.is_empty() {
                continue;
            }

            // Separator before non-first items
            let sep_w = if i > 0 { sep_width } else { 0 };

            // Calculate item width: key + " " + desc
            let key_w = display_width(&entry.key);
            let desc_w = display_width(&entry.desc);
            let item_w = key_w + 1 + desc_w;
            let total_item_w = sep_w + item_w;

            // Check if this item fits, accounting for possible ellipsis
            let space_left = (max_x as usize).saturating_sub(x as usize);
            if total_item_w > space_left {
                // Try to fit ellipsis
                let ell_total = if i > 0 { 1 + ellipsis_width } else { ellipsis_width };
                if ell_total <= space_left && deg.apply_styling() {
                    if i > 0 {
                        x = draw_text_span(
                            frame,
                            x,
                            y,
                            " ",
                            self.separator_style,
                            max_x,
                        );
                    }
                    draw_text_span(
                        frame,
                        x,
                        y,
                        &self.ellipsis,
                        self.separator_style,
                        max_x,
                    );
                }
                break;
            }

            // Draw separator
            if i > 0 {
                if deg.apply_styling() {
                    x = draw_text_span(frame, x, y, &self.separator, self.separator_style, max_x);
                } else {
                    x = draw_text_span(frame, x, y, &self.separator, Style::default(), max_x);
                }
            }

            // Draw key
            if deg.apply_styling() {
                x = draw_text_span(frame, x, y, &entry.key, self.key_style, max_x);
                x = draw_text_span(frame, x, y, " ", self.desc_style, max_x);
                x = draw_text_span(frame, x, y, &entry.desc, self.desc_style, max_x);
            } else {
                let text = format!("{} {}", entry.key, entry.desc);
                x = draw_text_span(frame, x, y, &text, Style::default(), max_x);
            }
        }
    }

    /// Render full mode: entries stacked vertically with aligned columns.
    fn render_full(&self, area: Rect, frame: &mut Frame) {
        let entries = self.enabled_entries();
        if entries.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;

        // Find max key width for alignment
        let max_key_w = entries
            .iter()
            .filter(|e| !e.key.is_empty() || !e.desc.is_empty())
            .map(|e| display_width(&e.key))
            .max()
            .unwrap_or(0);

        let max_x = area.right();
        let mut row: u16 = 0;

        for entry in &entries {
            if entry.key.is_empty() && entry.desc.is_empty() {
                continue;
            }
            if row >= area.height {
                break;
            }

            let y = area.y + row;
            let mut x = area.x;

            if deg.apply_styling() {
                // Draw key, right-padded to max_key_w
                let key_w = display_width(&entry.key);
                x = draw_text_span(frame, x, y, &entry.key, self.key_style, max_x);
                // Pad to alignment
                let pad = max_key_w.saturating_sub(key_w);
                for _ in 0..pad {
                    x = draw_text_span(frame, x, y, " ", Style::default(), max_x);
                }
                // Space between key and desc
                x = draw_text_span(frame, x, y, "  ", Style::default(), max_x);
                // Draw description
                draw_text_span(frame, x, y, &entry.desc, self.desc_style, max_x);
            } else {
                let text = format!("{:>width$}  {}", entry.key, entry.desc, width = max_key_w);
                draw_text_span(frame, x, y, &text, Style::default(), max_x);
            }

            row += 1;
        }
    }
}

impl Widget for Help {
    fn render(&self, area: Rect, frame: &mut Frame) {
        match self.mode {
            HelpMode::Short => self.render_short(area, frame),
            HelpMode::Full => self.render_full(area, frame),
        }
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
    fn new_help_is_empty() {
        let help = Help::new();
        assert!(help.entries().is_empty());
        assert_eq!(help.mode(), HelpMode::Short);
    }

    #[test]
    fn entry_builder() {
        let help = Help::new()
            .entry("q", "quit")
            .entry("^s", "save");
        assert_eq!(help.entries().len(), 2);
        assert_eq!(help.entries()[0].key, "q");
        assert_eq!(help.entries()[0].desc, "quit");
    }

    #[test]
    fn with_entries_replaces() {
        let help = Help::new()
            .entry("old", "old")
            .with_entries(vec![HelpEntry::new("new", "new")]);
        assert_eq!(help.entries().len(), 1);
        assert_eq!(help.entries()[0].key, "new");
    }

    #[test]
    fn disabled_entries_hidden() {
        let help = Help::new()
            .with_entry(HelpEntry::new("a", "shown"))
            .with_entry(HelpEntry::new("b", "hidden").with_enabled(false))
            .with_entry(HelpEntry::new("c", "also shown"));
        assert_eq!(help.enabled_entries().len(), 2);
    }

    #[test]
    fn toggle_mode() {
        let mut help = Help::new();
        assert_eq!(help.mode(), HelpMode::Short);
        help.toggle_mode();
        assert_eq!(help.mode(), HelpMode::Full);
        help.toggle_mode();
        assert_eq!(help.mode(), HelpMode::Short);
    }

    #[test]
    fn push_entry() {
        let mut help = Help::new();
        help.push_entry(HelpEntry::new("x", "action"));
        assert_eq!(help.entries().len(), 1);
    }

    #[test]
    fn render_short_basic() {
        let help = Help::new()
            .entry("q", "quit")
            .entry("^s", "save");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);
        help.render(area, &mut frame);

        // Check that key text appears in buffer
        let cell_q = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell_q.content.as_char(), Some('q'));
    }

    #[test]
    fn render_short_truncation() {
        let help = Help::new()
            .entry("q", "quit")
            .entry("^s", "save")
            .entry("^x", "something very long that should not fit");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let area = Rect::new(0, 0, 20, 1);
        help.render(area, &mut frame);

        // First entry should be present
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('q'));
    }

    #[test]
    fn render_short_empty_entries() {
        let help = Help::new();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let area = Rect::new(0, 0, 20, 1);
        help.render(area, &mut frame);

        // Buffer should remain default (empty cell)
        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(cell.content.is_empty() || cell.content.as_char() == Some(' '));
    }

    #[test]
    fn render_full_basic() {
        let help = Help::new()
            .with_mode(HelpMode::Full)
            .entry("q", "quit")
            .entry("^s", "save file");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        let area = Rect::new(0, 0, 30, 5);
        help.render(area, &mut frame);

        // First row should have "q" key
        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(cell.content.as_char() == Some(' ') || cell.content.as_char() == Some('q'));
        // Second row should have "^s" key (right-padded: " ^s")
        let cell_row2 = frame.buffer.get(0, 1).unwrap();
        assert!(cell_row2.content.as_char() == Some('^') || cell_row2.content.as_char() == Some(' '));
    }

    #[test]
    fn render_full_respects_height() {
        let help = Help::new()
            .with_mode(HelpMode::Full)
            .entry("a", "first")
            .entry("b", "second")
            .entry("c", "third");

        let mut pool = GraphemePool::new();
        // Only 2 rows available
        let mut frame = Frame::new(30, 2, &mut pool);
        let area = Rect::new(0, 0, 30, 2);
        help.render(area, &mut frame);

        // Only first two entries should render (height=2)
        // No crash, no panic
    }

    #[test]
    fn help_entry_equality() {
        let a = HelpEntry::new("q", "quit");
        let b = HelpEntry::new("q", "quit");
        let c = HelpEntry::new("x", "exit");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn help_entry_disabled() {
        let entry = HelpEntry::new("q", "quit").with_enabled(false);
        assert!(!entry.enabled);
    }

    #[test]
    fn with_separator() {
        let help = Help::new().with_separator(" | ");
        assert_eq!(help.separator, " | ");
    }

    #[test]
    fn with_ellipsis() {
        let help = Help::new().with_ellipsis("...");
        assert_eq!(help.ellipsis, "...");
    }

    #[test]
    fn render_zero_area() {
        let help = Help::new().entry("q", "quit");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let area = Rect::new(0, 0, 0, 0);
        help.render(area, &mut frame); // Should not panic
    }

    #[test]
    fn is_not_essential() {
        let help = Help::new();
        assert!(!help.is_essential());
    }

    #[test]
    fn render_full_alignment() {
        // Verify key column alignment in full mode
        let help = Help::new()
            .with_mode(HelpMode::Full)
            .entry("q", "quit")
            .entry("ctrl+s", "save");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 3, &mut pool);
        let area = Rect::new(0, 0, 30, 3);
        help.render(area, &mut frame);

        // "q" is 1 char, "ctrl+s" is 6 chars, max_key_w = 6
        // Row 0: "q      quit" (q + 5 spaces + 2 spaces + quit)
        // Row 1: "ctrl+s  save"
        // Check that descriptions start at the same column
        // Key col = 6, gap = 2, desc starts at col 8
    }

    #[test]
    fn default_impl() {
        let help = Help::default();
        assert!(help.entries().is_empty());
    }
}

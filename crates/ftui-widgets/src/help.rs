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

use crate::{StatefulWidget, Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::budget::DegradationLevel;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_style::StyleFlags;
use ftui_text::wrap::display_width;
use std::hash::{Hash, Hasher};

/// Category for organizing help entries into logical groups.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum HelpCategory {
    /// General/uncategorized keybinding.
    #[default]
    General,
    /// Navigation keys (arrows, page up/down, home/end, etc.).
    Navigation,
    /// Editing actions (cut, copy, paste, undo, redo, etc.).
    Editing,
    /// File operations (save, open, close, new, etc.).
    File,
    /// View/display controls (zoom, scroll, toggle panels, etc.).
    View,
    /// Application-level shortcuts (quit, settings, help, etc.).
    Global,
    /// Custom category with a user-defined label.
    Custom(String),
}

impl HelpCategory {
    /// Return a display label for this category.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::General => "General",
            Self::Navigation => "Navigation",
            Self::Editing => "Editing",
            Self::File => "File",
            Self::View => "View",
            Self::Global => "Global",
            Self::Custom(s) => s,
        }
    }
}

/// A single keybinding entry in the help view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpEntry {
    /// The key or key combination (e.g. "^C", "↑/k").
    pub key: String,
    /// Description of what the key does.
    pub desc: String,
    /// Whether this entry is enabled (disabled entries are hidden).
    pub enabled: bool,
    /// Category for grouping related entries.
    pub category: HelpCategory,
}

impl HelpEntry {
    /// Create a new enabled help entry.
    #[must_use]
    pub fn new(key: impl Into<String>, desc: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            desc: desc.into(),
            enabled: true,
            category: HelpCategory::default(),
        }
    }

    /// Set whether this entry is enabled.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Set the category for this entry.
    #[must_use]
    pub fn with_category(mut self, category: HelpCategory) -> Self {
        self.category = category;
        self
    }
}

/// Display mode for the help widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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

/// Cached render state for [`Help`], enabling incremental layout reuse and
/// dirty-rect updates for keybinding hint panels.
///
/// # Invariants
/// - Layout is reused only when entry count and slot widths remain compatible.
/// - Dirty rects always cover the full prior slot width for changed entries.
/// - Layout rebuilds on any change that could cause reflow.
///
/// # Failure Modes
/// - If a changed entry exceeds its cached slot width, we rebuild the layout.
/// - If enabled entry count changes, we rebuild the layout.
#[derive(Debug, Default)]
pub struct HelpRenderState {
    cache: Option<HelpCache>,
    enabled_indices: Vec<usize>,
    dirty_indices: Vec<usize>,
    dirty_rects: Vec<Rect>,
    stats: HelpCacheStats,
}

/// Cache hit/miss statistics for [`HelpRenderState`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HelpCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub dirty_updates: u64,
    pub layout_rebuilds: u64,
}

impl HelpRenderState {
    /// Return cache statistics.
    #[must_use]
    pub fn stats(&self) -> HelpCacheStats {
        self.stats
    }

    /// Clear recorded dirty rects.
    pub fn clear_dirty_rects(&mut self) {
        self.dirty_rects.clear();
    }

    /// Take dirty rects for logging/inspection.
    #[must_use]
    pub fn take_dirty_rects(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.dirty_rects)
    }

    /// Read dirty rects without clearing.
    #[must_use]
    pub fn dirty_rects(&self) -> &[Rect] {
        &self.dirty_rects
    }

    /// Reset cache stats (useful for perf logging).
    pub fn reset_stats(&mut self) {
        self.stats = HelpCacheStats::default();
    }
}

#[derive(Debug)]
struct HelpCache {
    buffer: Buffer,
    layout: HelpLayout,
    key: LayoutKey,
    entry_hashes: Vec<u64>,
    enabled_count: usize,
}

#[derive(Debug, Clone)]
struct HelpLayout {
    mode: HelpMode,
    width: u16,
    entries: Vec<EntrySlot>,
    ellipsis: Option<EllipsisSlot>,
    max_key_width: usize,
    separator_width: usize,
}

#[derive(Debug, Clone)]
struct EntrySlot {
    x: u16,
    y: u16,
    width: u16,
    key_width: usize,
}

#[derive(Debug, Clone)]
struct EllipsisSlot {
    x: u16,
    width: u16,
    prefix_space: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StyleKey {
    fg: Option<PackedRgba>,
    bg: Option<PackedRgba>,
    attrs: Option<StyleFlags>,
}

impl From<Style> for StyleKey {
    fn from(style: Style) -> Self {
        Self {
            fg: style.fg,
            bg: style.bg,
            attrs: style.attrs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LayoutKey {
    mode: HelpMode,
    width: u16,
    height: u16,
    separator_hash: u64,
    ellipsis_hash: u64,
    key_style: StyleKey,
    desc_style: StyleKey,
    separator_style: StyleKey,
    degradation: DegradationLevel,
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
                let ell_total = if i > 0 {
                    1 + ellipsis_width
                } else {
                    ellipsis_width
                };
                if ell_total <= space_left && deg.apply_styling() {
                    if i > 0 {
                        x = draw_text_span(frame, x, y, " ", self.separator_style, max_x);
                    }
                    draw_text_span(frame, x, y, &self.ellipsis, self.separator_style, max_x);
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

            let y = area.y.saturating_add(row);
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

    fn entry_hash(entry: &HelpEntry) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        entry.key.hash(&mut hasher);
        entry.desc.hash(&mut hasher);
        entry.enabled.hash(&mut hasher);
        entry.category.hash(&mut hasher);
        hasher.finish()
    }

    fn hash_str(value: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    fn layout_key(&self, area: Rect, degradation: DegradationLevel) -> LayoutKey {
        LayoutKey {
            mode: self.mode,
            width: area.width,
            height: area.height,
            separator_hash: Self::hash_str(&self.separator),
            ellipsis_hash: Self::hash_str(&self.ellipsis),
            key_style: StyleKey::from(self.key_style),
            desc_style: StyleKey::from(self.desc_style),
            separator_style: StyleKey::from(self.separator_style),
            degradation,
        }
    }

    fn build_layout(&self, area: Rect) -> HelpLayout {
        match self.mode {
            HelpMode::Short => self.build_short_layout(area),
            HelpMode::Full => self.build_full_layout(area),
        }
    }

    fn build_short_layout(&self, area: Rect) -> HelpLayout {
        let mut entries = Vec::new();
        let mut ellipsis = None;
        let sep_width = display_width(&self.separator);
        let ellipsis_width = display_width(&self.ellipsis);
        let max_x = area.width;
        let mut x: u16 = 0;
        let mut first = true;

        for entry in self
            .entries
            .iter()
            .filter(|e| e.enabled && (!e.key.is_empty() || !e.desc.is_empty()))
        {
            let key_width = display_width(&entry.key);
            let desc_width = display_width(&entry.desc);
            let item_width = key_width + 1 + desc_width;
            let total_width = if first {
                item_width
            } else {
                sep_width + item_width
            };
            let space_left = (max_x as usize).saturating_sub(x as usize);

            if total_width > space_left {
                let ell_total = if first {
                    ellipsis_width
                } else {
                    1 + ellipsis_width
                };
                if ell_total <= space_left {
                    ellipsis = Some(EllipsisSlot {
                        x,
                        width: ell_total as u16,
                        prefix_space: !first,
                    });
                }
                break;
            }

            entries.push(EntrySlot {
                x,
                y: 0,
                width: total_width as u16,
                key_width,
            });
            x = x.saturating_add(total_width as u16);
            first = false;
        }

        HelpLayout {
            mode: HelpMode::Short,
            width: area.width,
            entries,
            ellipsis,
            max_key_width: 0,
            separator_width: sep_width,
        }
    }

    fn build_full_layout(&self, area: Rect) -> HelpLayout {
        let mut max_key_width = 0usize;
        for entry in self
            .entries
            .iter()
            .filter(|e| e.enabled && (!e.key.is_empty() || !e.desc.is_empty()))
        {
            let key_width = display_width(&entry.key);
            max_key_width = max_key_width.max(key_width);
        }

        let mut entries = Vec::new();
        let mut row: u16 = 0;
        for entry in self
            .entries
            .iter()
            .filter(|e| e.enabled && (!e.key.is_empty() || !e.desc.is_empty()))
        {
            if row >= area.height {
                break;
            }
            let key_width = display_width(&entry.key);
            let desc_width = display_width(&entry.desc);
            let entry_width = max_key_width.saturating_add(2).saturating_add(desc_width);
            let slot_width = entry_width.min(area.width as usize) as u16;
            entries.push(EntrySlot {
                x: 0,
                y: row,
                width: slot_width,
                key_width,
            });
            row = row.saturating_add(1);
        }

        HelpLayout {
            mode: HelpMode::Full,
            width: area.width,
            entries,
            ellipsis: None,
            max_key_width,
            separator_width: 0,
        }
    }

    fn render_cached(&self, area: Rect, frame: &mut Frame, layout: &HelpLayout) {
        match layout.mode {
            HelpMode::Short => self.render_short_cached(area, frame, layout),
            HelpMode::Full => self.render_full_cached(area, frame, layout),
        }
    }

    fn render_short_cached(&self, area: Rect, frame: &mut Frame, layout: &HelpLayout) {
        if layout.entries.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        let max_x = area.right();
        let mut enabled_iter = self
            .entries
            .iter()
            .filter(|e| e.enabled && (!e.key.is_empty() || !e.desc.is_empty()));

        for (idx, slot) in layout.entries.iter().enumerate() {
            let Some(entry) = enabled_iter.next() else {
                break;
            };
            let mut x = area.x.saturating_add(slot.x);
            let y = area.y.saturating_add(slot.y);

            if idx > 0 {
                let sep_style = if deg.apply_styling() {
                    self.separator_style
                } else {
                    Style::default()
                };
                x = draw_text_span(frame, x, y, &self.separator, sep_style, max_x);
            }

            let key_style = if deg.apply_styling() {
                self.key_style
            } else {
                Style::default()
            };
            let desc_style = if deg.apply_styling() {
                self.desc_style
            } else {
                Style::default()
            };

            x = draw_text_span(frame, x, y, &entry.key, key_style, max_x);
            x = draw_text_span(frame, x, y, " ", desc_style, max_x);
            draw_text_span(frame, x, y, &entry.desc, desc_style, max_x);
        }

        if let Some(ellipsis) = &layout.ellipsis {
            let y = area.y.saturating_add(0);
            let mut x = area.x.saturating_add(ellipsis.x);
            let ellipsis_style = if deg.apply_styling() {
                self.separator_style
            } else {
                Style::default()
            };
            if ellipsis.prefix_space {
                x = draw_text_span(frame, x, y, " ", ellipsis_style, max_x);
            }
            draw_text_span(frame, x, y, &self.ellipsis, ellipsis_style, max_x);
        }
    }

    fn render_full_cached(&self, area: Rect, frame: &mut Frame, layout: &HelpLayout) {
        if layout.entries.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        let max_x = area.right();

        let mut enabled_iter = self
            .entries
            .iter()
            .filter(|e| e.enabled && (!e.key.is_empty() || !e.desc.is_empty()));

        for slot in layout.entries.iter() {
            let Some(entry) = enabled_iter.next() else {
                break;
            };

            let y = area.y.saturating_add(slot.y);
            let mut x = area.x.saturating_add(slot.x);

            let key_style = if deg.apply_styling() {
                self.key_style
            } else {
                Style::default()
            };
            let desc_style = if deg.apply_styling() {
                self.desc_style
            } else {
                Style::default()
            };

            x = draw_text_span(frame, x, y, &entry.key, key_style, max_x);
            let pad = layout.max_key_width.saturating_sub(slot.key_width);
            for _ in 0..pad {
                x = draw_text_span(frame, x, y, " ", Style::default(), max_x);
            }
            x = draw_text_span(frame, x, y, "  ", Style::default(), max_x);
            draw_text_span(frame, x, y, &entry.desc, desc_style, max_x);
        }
    }

    fn render_short_entry(&self, slot: &EntrySlot, entry: &HelpEntry, frame: &mut Frame) {
        let deg = frame.buffer.degradation;
        let max_x = slot.x.saturating_add(slot.width);

        let rect = Rect::new(slot.x, slot.y, slot.width, 1);
        frame.buffer.fill(rect, Cell::default());

        let mut x = slot.x;
        if slot.x > 0 {
            let sep_style = if deg.apply_styling() {
                self.separator_style
            } else {
                Style::default()
            };
            x = draw_text_span(frame, x, slot.y, &self.separator, sep_style, max_x);
        }

        let key_style = if deg.apply_styling() {
            self.key_style
        } else {
            Style::default()
        };
        let desc_style = if deg.apply_styling() {
            self.desc_style
        } else {
            Style::default()
        };

        x = draw_text_span(frame, x, slot.y, &entry.key, key_style, max_x);
        x = draw_text_span(frame, x, slot.y, " ", desc_style, max_x);
        draw_text_span(frame, x, slot.y, &entry.desc, desc_style, max_x);
    }

    fn render_full_entry(
        &self,
        slot: &EntrySlot,
        entry: &HelpEntry,
        layout: &HelpLayout,
        frame: &mut Frame,
    ) {
        let deg = frame.buffer.degradation;
        let max_x = slot.x.saturating_add(slot.width);

        let rect = Rect::new(slot.x, slot.y, slot.width, 1);
        frame.buffer.fill(rect, Cell::default());

        let mut x = slot.x;
        let key_style = if deg.apply_styling() {
            self.key_style
        } else {
            Style::default()
        };
        let desc_style = if deg.apply_styling() {
            self.desc_style
        } else {
            Style::default()
        };

        x = draw_text_span(frame, x, slot.y, &entry.key, key_style, max_x);
        let pad = layout.max_key_width.saturating_sub(slot.key_width);
        for _ in 0..pad {
            x = draw_text_span(frame, x, slot.y, " ", Style::default(), max_x);
        }
        x = draw_text_span(frame, x, slot.y, "  ", Style::default(), max_x);
        draw_text_span(frame, x, slot.y, &entry.desc, desc_style, max_x);
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

impl StatefulWidget for Help {
    type State = HelpRenderState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut HelpRenderState) {
        if area.is_empty() || area.width == 0 || area.height == 0 {
            state.cache = None;
            return;
        }

        state.dirty_rects.clear();
        state.dirty_indices.clear();

        let layout_key = self.layout_key(area, frame.buffer.degradation);
        let enabled_count = collect_enabled_indices(&self.entries, &mut state.enabled_indices);

        let cache_miss = state
            .cache
            .as_ref()
            .is_none_or(|cache| cache.key != layout_key);

        if cache_miss {
            rebuild_cache(self, area, frame, state, layout_key, enabled_count);
            blit_cache(state.cache.as_ref(), area, frame);
            return;
        }

        let cache = state
            .cache
            .as_mut()
            .expect("cache present after miss check");
        if enabled_count != cache.enabled_count {
            rebuild_cache(self, area, frame, state, layout_key, enabled_count);
            blit_cache(state.cache.as_ref(), area, frame);
            return;
        }

        let mut layout_changed = false;
        let visible_count = cache.layout.entries.len();

        for (pos, entry_idx) in state.enabled_indices.iter().enumerate() {
            let entry = &self.entries[*entry_idx];
            let hash = Help::entry_hash(entry);

            if pos >= cache.entry_hashes.len() {
                layout_changed = true;
                break;
            }

            if hash != cache.entry_hashes[pos] {
                if pos >= visible_count || !entry_fits_slot(entry, pos, &cache.layout) {
                    layout_changed = true;
                    break;
                }
                cache.entry_hashes[pos] = hash;
                state.dirty_indices.push(pos);
            }
        }

        if layout_changed {
            rebuild_cache(self, area, frame, state, layout_key, enabled_count);
            blit_cache(state.cache.as_ref(), area, frame);
            return;
        }

        if state.dirty_indices.is_empty() {
            state.stats.hits += 1;
            blit_cache(state.cache.as_ref(), area, frame);
            return;
        }

        // Partial update: only changed entries are redrawn into the cached buffer.
        state.stats.dirty_updates += 1;

        let cache = state
            .cache
            .as_mut()
            .expect("cache present for dirty update");
        let mut cache_buffer = std::mem::take(&mut cache.buffer);
        cache_buffer.degradation = frame.buffer.degradation;
        {
            let mut cache_frame = Frame {
                buffer: cache_buffer,
                pool: frame.pool,
                links: None,
                hit_grid: None,
                widget_budget: frame.widget_budget.clone(),
                widget_signals: Vec::new(),
                cursor_position: None,
                cursor_visible: true,
                degradation: frame.buffer.degradation,
            };

            for idx in &state.dirty_indices {
                if let Some(entry_idx) = state.enabled_indices.get(*idx)
                    && let Some(slot) = cache.layout.entries.get(*idx)
                {
                    let entry = &self.entries[*entry_idx];
                    match cache.layout.mode {
                        HelpMode::Short => self.render_short_entry(slot, entry, &mut cache_frame),
                        HelpMode::Full => {
                            self.render_full_entry(slot, entry, &cache.layout, &mut cache_frame)
                        }
                    }
                    state
                        .dirty_rects
                        .push(Rect::new(slot.x, slot.y, slot.width, 1));
                }
            }

            cache_buffer = cache_frame.buffer;
        }
        cache.buffer = cache_buffer;

        blit_cache(state.cache.as_ref(), area, frame);
    }
}

fn collect_enabled_indices(entries: &[HelpEntry], out: &mut Vec<usize>) -> usize {
    out.clear();
    for (idx, entry) in entries.iter().enumerate() {
        if entry.enabled && (!entry.key.is_empty() || !entry.desc.is_empty()) {
            out.push(idx);
        }
    }
    out.len()
}

fn entry_fits_slot(entry: &HelpEntry, index: usize, layout: &HelpLayout) -> bool {
    match layout.mode {
        HelpMode::Short => {
            let entry_width = display_width(&entry.key) + 1 + display_width(&entry.desc);
            let slot = match layout.entries.get(index) {
                Some(slot) => slot,
                None => return false,
            };
            let sep_width = layout.separator_width;
            let max_width = if slot.x == 0 {
                slot.width as usize
            } else {
                slot.width.saturating_sub(sep_width as u16) as usize
            };
            entry_width <= max_width
        }
        HelpMode::Full => {
            let key_width = display_width(&entry.key);
            let desc_width = display_width(&entry.desc);
            let entry_width = layout
                .max_key_width
                .saturating_add(2)
                .saturating_add(desc_width);
            let slot = match layout.entries.get(index) {
                Some(slot) => slot,
                None => return false,
            };
            if slot.width == layout.width {
                key_width <= layout.max_key_width
            } else {
                key_width <= layout.max_key_width && entry_width <= slot.width as usize
            }
        }
    }
}

fn rebuild_cache(
    help: &Help,
    area: Rect,
    frame: &mut Frame,
    state: &mut HelpRenderState,
    layout_key: LayoutKey,
    enabled_count: usize,
) {
    state.stats.misses += 1;
    state.stats.layout_rebuilds += 1;

    let layout_area = Rect::new(0, 0, area.width, area.height);
    let layout = help.build_layout(layout_area);

    let mut buffer = Buffer::new(area.width, area.height);
    buffer.degradation = frame.buffer.degradation;
    {
        let mut cache_frame = Frame {
            buffer,
            pool: frame.pool,
            links: None,
            hit_grid: None,
            widget_budget: frame.widget_budget.clone(),
            widget_signals: Vec::new(),
            cursor_position: None,
            cursor_visible: true,
            degradation: frame.buffer.degradation,
        };
        help.render_cached(layout_area, &mut cache_frame, &layout);
        buffer = cache_frame.buffer;
    }

    let mut entry_hashes = Vec::with_capacity(state.enabled_indices.len());
    for idx in &state.enabled_indices {
        entry_hashes.push(Help::entry_hash(&help.entries[*idx]));
    }

    state.cache = Some(HelpCache {
        buffer,
        layout,
        key: layout_key,
        entry_hashes,
        enabled_count,
    });
}

fn blit_cache(cache: Option<&HelpCache>, area: Rect, frame: &mut Frame) {
    let Some(cache) = cache else {
        return;
    };

    for slot in &cache.layout.entries {
        let src = Rect::new(slot.x, slot.y, slot.width, 1);
        frame
            .buffer
            .copy_from(&cache.buffer, src, area.x + slot.x, area.y + slot.y);
    }

    if let Some(ellipsis) = &cache.layout.ellipsis {
        let src = Rect::new(ellipsis.x, 0, ellipsis.width, 1);
        frame
            .buffer
            .copy_from(&cache.buffer, src, area.x + ellipsis.x, area.y);
    }
}

/// Format for displaying key labels in hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KeyFormat {
    /// Plain key display: `q quit`
    #[default]
    Plain,
    /// Bracketed key display: `[q] quit`
    Bracketed,
}

/// A keybinding hints widget with category grouping and context-aware filtering.
///
/// Supports two entry scopes:
/// - **Global**: shortcuts always visible regardless of context.
/// - **Contextual**: shortcuts shown only when [`show_context`](Self::with_show_context)
///   is enabled (typically when a particular widget has focus).
///
/// In [`HelpMode::Full`] mode with categories enabled, entries are grouped
/// under category headers. In [`HelpMode::Short`] mode, entries are rendered
/// inline.
///
/// # Example
///
/// ```
/// use ftui_widgets::help::{KeybindingHints, HelpCategory, KeyFormat};
///
/// let hints = KeybindingHints::new()
///     .with_key_format(KeyFormat::Bracketed)
///     .global_entry("q", "quit")
///     .global_entry_categorized("Tab", "next", HelpCategory::Navigation)
///     .contextual_entry_categorized("^s", "save", HelpCategory::File);
///
/// assert_eq!(hints.global_entries().len(), 2);
/// assert_eq!(hints.contextual_entries().len(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct KeybindingHints {
    global_entries: Vec<HelpEntry>,
    contextual_entries: Vec<HelpEntry>,
    key_format: KeyFormat,
    mode: HelpMode,
    key_style: Style,
    desc_style: Style,
    separator_style: Style,
    category_style: Style,
    separator: String,
    ellipsis: String,
    show_categories: bool,
    show_context: bool,
}

impl Default for KeybindingHints {
    fn default() -> Self {
        Self::new()
    }
}

impl KeybindingHints {
    /// Create a new hints widget with no entries.
    #[must_use]
    pub fn new() -> Self {
        Self {
            global_entries: Vec::new(),
            contextual_entries: Vec::new(),
            key_format: KeyFormat::default(),
            mode: HelpMode::Short,
            key_style: Style::new().bold(),
            desc_style: Style::default(),
            separator_style: Style::default(),
            category_style: Style::new().bold().underline(),
            separator: " • ".to_string(),
            ellipsis: "…".to_string(),
            show_categories: true,
            show_context: false,
        }
    }

    /// Add a global entry (always visible).
    #[must_use]
    pub fn global_entry(mut self, key: impl Into<String>, desc: impl Into<String>) -> Self {
        self.global_entries
            .push(HelpEntry::new(key, desc).with_category(HelpCategory::Global));
        self
    }

    /// Add a global entry with a specific category.
    #[must_use]
    pub fn global_entry_categorized(
        mut self,
        key: impl Into<String>,
        desc: impl Into<String>,
        category: HelpCategory,
    ) -> Self {
        self.global_entries
            .push(HelpEntry::new(key, desc).with_category(category));
        self
    }

    /// Add a contextual entry (shown when context is active).
    #[must_use]
    pub fn contextual_entry(mut self, key: impl Into<String>, desc: impl Into<String>) -> Self {
        self.contextual_entries.push(HelpEntry::new(key, desc));
        self
    }

    /// Add a contextual entry with a specific category.
    #[must_use]
    pub fn contextual_entry_categorized(
        mut self,
        key: impl Into<String>,
        desc: impl Into<String>,
        category: HelpCategory,
    ) -> Self {
        self.contextual_entries
            .push(HelpEntry::new(key, desc).with_category(category));
        self
    }

    /// Add a pre-built global entry.
    #[must_use]
    pub fn with_global_entry(mut self, entry: HelpEntry) -> Self {
        self.global_entries.push(entry);
        self
    }

    /// Add a pre-built contextual entry.
    #[must_use]
    pub fn with_contextual_entry(mut self, entry: HelpEntry) -> Self {
        self.contextual_entries.push(entry);
        self
    }

    /// Set the key display format.
    #[must_use]
    pub fn with_key_format(mut self, format: KeyFormat) -> Self {
        self.key_format = format;
        self
    }

    /// Set the display mode.
    #[must_use]
    pub fn with_mode(mut self, mode: HelpMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set whether contextual entries are shown.
    #[must_use]
    pub fn with_show_context(mut self, show: bool) -> Self {
        self.show_context = show;
        self
    }

    /// Set whether category headers are shown in full mode.
    #[must_use]
    pub fn with_show_categories(mut self, show: bool) -> Self {
        self.show_categories = show;
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

    /// Set the style for separators.
    #[must_use]
    pub fn with_separator_style(mut self, style: Style) -> Self {
        self.separator_style = style;
        self
    }

    /// Set the style for category headers.
    #[must_use]
    pub fn with_category_style(mut self, style: Style) -> Self {
        self.category_style = style;
        self
    }

    /// Set the separator string for short mode.
    #[must_use]
    pub fn with_separator(mut self, sep: impl Into<String>) -> Self {
        self.separator = sep.into();
        self
    }

    /// Get the global entries.
    #[must_use]
    pub fn global_entries(&self) -> &[HelpEntry] {
        &self.global_entries
    }

    /// Get the contextual entries.
    #[must_use]
    pub fn contextual_entries(&self) -> &[HelpEntry] {
        &self.contextual_entries
    }

    /// Get the current mode.
    #[must_use]
    pub fn mode(&self) -> HelpMode {
        self.mode
    }

    /// Get the key format.
    #[must_use]
    pub fn key_format(&self) -> KeyFormat {
        self.key_format
    }

    /// Toggle between short and full mode.
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            HelpMode::Short => HelpMode::Full,
            HelpMode::Full => HelpMode::Short,
        };
    }

    /// Set whether contextual entries are shown (mutable).
    pub fn set_show_context(&mut self, show: bool) {
        self.show_context = show;
    }

    /// Format a key string according to the current key format.
    fn format_key(&self, key: &str) -> String {
        match self.key_format {
            KeyFormat::Plain => key.to_string(),
            KeyFormat::Bracketed => format!("[{key}]"),
        }
    }

    /// Collect visible entries, applying scope filter and key formatting.
    #[must_use]
    pub fn visible_entries(&self) -> Vec<HelpEntry> {
        let mut entries = Vec::new();
        for e in &self.global_entries {
            if e.enabled {
                entries.push(HelpEntry {
                    key: self.format_key(&e.key),
                    desc: e.desc.clone(),
                    enabled: true,
                    category: e.category.clone(),
                });
            }
        }
        if self.show_context {
            for e in &self.contextual_entries {
                if e.enabled {
                    entries.push(HelpEntry {
                        key: self.format_key(&e.key),
                        desc: e.desc.clone(),
                        enabled: true,
                        category: e.category.clone(),
                    });
                }
            }
        }
        entries
    }

    /// Group entries by category, preserving insertion order within each group.
    fn grouped_entries(entries: &[HelpEntry]) -> Vec<(&HelpCategory, Vec<&HelpEntry>)> {
        let mut groups: Vec<(&HelpCategory, Vec<&HelpEntry>)> = Vec::new();
        for entry in entries {
            if let Some(group) = groups.iter_mut().find(|(cat, _)| **cat == entry.category) {
                group.1.push(entry);
            } else {
                groups.push((&entry.category, vec![entry]));
            }
        }
        groups
    }

    /// Render full mode with category headers.
    fn render_full_grouped(&self, entries: &[HelpEntry], area: Rect, frame: &mut Frame) {
        let groups = Self::grouped_entries(entries);
        let deg = frame.buffer.degradation;
        let max_x = area.right();
        let mut y = area.y;

        // Find max key width across all entries for alignment.
        let max_key_w = entries
            .iter()
            .map(|e| display_width(&e.key))
            .max()
            .unwrap_or(0);

        for (i, (cat, group_entries)) in groups.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }

            // Category header
            let cat_style = if deg.apply_styling() {
                self.category_style
            } else {
                Style::default()
            };
            draw_text_span(frame, area.x, y, cat.label(), cat_style, max_x);
            y += 1;

            // Entries in this category
            for entry in group_entries {
                if y >= area.bottom() {
                    break;
                }

                let key_style = if deg.apply_styling() {
                    self.key_style
                } else {
                    Style::default()
                };
                let desc_style = if deg.apply_styling() {
                    self.desc_style
                } else {
                    Style::default()
                };

                let mut x = area.x;
                x = draw_text_span(frame, x, y, &entry.key, key_style, max_x);
                let pad = max_key_w.saturating_sub(display_width(&entry.key));
                for _ in 0..pad {
                    x = draw_text_span(frame, x, y, " ", Style::default(), max_x);
                }
                x = draw_text_span(frame, x, y, "  ", Style::default(), max_x);
                draw_text_span(frame, x, y, &entry.desc, desc_style, max_x);
                y += 1;
            }

            // Blank line between groups (except after last)
            if i + 1 < groups.len() {
                y += 1;
            }
        }
    }
}

impl Widget for KeybindingHints {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let entries = self.visible_entries();
        if entries.is_empty() || area.is_empty() {
            return;
        }

        match self.mode {
            HelpMode::Short => {
                // In short mode, render all entries inline using Help widget.
                let help = Help::new()
                    .with_mode(HelpMode::Short)
                    .with_key_style(self.key_style)
                    .with_desc_style(self.desc_style)
                    .with_separator_style(self.separator_style)
                    .with_separator(self.separator.clone())
                    .with_ellipsis(self.ellipsis.clone())
                    .with_entries(entries);
                Widget::render(&help, area, frame);
            }
            HelpMode::Full => {
                if self.show_categories {
                    self.render_full_grouped(&entries, area, frame);
                } else {
                    let help = Help::new()
                        .with_mode(HelpMode::Full)
                        .with_key_style(self.key_style)
                        .with_desc_style(self.desc_style)
                        .with_entries(entries);
                    Widget::render(&help, area, frame);
                }
            }
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
    use proptest::prelude::*;
    use proptest::string::string_regex;
    use std::time::Instant;

    #[test]
    fn new_help_is_empty() {
        let help = Help::new();
        assert!(help.entries().is_empty());
        assert_eq!(help.mode(), HelpMode::Short);
    }

    #[test]
    fn entry_builder() {
        let help = Help::new().entry("q", "quit").entry("^s", "save");
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
        let help = Help::new().entry("q", "quit").entry("^s", "save");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);
        Widget::render(&help, area, &mut frame);

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
        Widget::render(&help, area, &mut frame);

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
        Widget::render(&help, area, &mut frame);

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
        Widget::render(&help, area, &mut frame);

        // First row should have "q" key
        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(cell.content.as_char() == Some(' ') || cell.content.as_char() == Some('q'));
        // Second row should have "^s" key (right-padded: " ^s")
        let cell_row2 = frame.buffer.get(0, 1).unwrap();
        assert!(
            cell_row2.content.as_char() == Some('^') || cell_row2.content.as_char() == Some(' ')
        );
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
        Widget::render(&help, area, &mut frame);

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
        Widget::render(&help, area, &mut frame); // Should not panic
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
        Widget::render(&help, area, &mut frame);

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

    #[test]
    fn cache_hit_same_hints() {
        let help = Help::new().entry("q", "quit").entry("^s", "save");
        let mut state = HelpRenderState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);

        StatefulWidget::render(&help, area, &mut frame, &mut state);
        let stats_after_first = state.stats();
        StatefulWidget::render(&help, area, &mut frame, &mut state);
        let stats_after_second = state.stats();

        assert!(
            stats_after_second.hits > stats_after_first.hits,
            "Second render should be a cache hit"
        );
        assert!(state.dirty_rects().is_empty(), "No dirty rects on hit");
    }

    #[test]
    fn dirty_rect_only_changes() {
        let mut help = Help::new()
            .with_mode(HelpMode::Full)
            .entry("q", "quit")
            .entry("w", "write")
            .entry("e", "edit");

        let mut state = HelpRenderState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 3, &mut pool);
        let area = Rect::new(0, 0, 40, 3);

        StatefulWidget::render(&help, area, &mut frame, &mut state);

        help.entries[1].desc.clear();
        help.entries[1].desc.push_str("save");

        StatefulWidget::render(&help, area, &mut frame, &mut state);
        let dirty = state.take_dirty_rects();

        assert_eq!(dirty.len(), 1, "Only one row should be dirty");
        assert_eq!(dirty[0].y, 1, "Second entry row should be dirty");
    }

    proptest! {
        #[test]
        fn prop_cache_hits_on_stable_entries(entries in prop::collection::vec(
            (string_regex("[a-z]{1,6}").unwrap(), string_regex("[a-z]{1,10}").unwrap()),
            1..6
        )) {
            let mut help = Help::new();
            for (key, desc) in entries {
                help = help.entry(key, desc);
            }
            let mut state = HelpRenderState::default();
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(80, 1, &mut pool);
            let area = Rect::new(0, 0, 80, 1);

            StatefulWidget::render(&help, area, &mut frame, &mut state);
            let stats_after_first = state.stats();
            StatefulWidget::render(&help, area, &mut frame, &mut state);
            let stats_after_second = state.stats();

            prop_assert!(stats_after_second.hits > stats_after_first.hits);
            prop_assert!(state.dirty_rects().is_empty());
        }
    }

    #[test]
    fn perf_micro_hint_update() {
        let mut help = Help::new()
            .with_mode(HelpMode::Short)
            .entry("^T", "Theme")
            .entry("^C", "Quit")
            .entry("?", "Help")
            .entry("F12", "Debug");

        let mut state = HelpRenderState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 1, &mut pool);
        let area = Rect::new(0, 0, 120, 1);

        StatefulWidget::render(&help, area, &mut frame, &mut state);

        let iterations = 200u32;
        let mut times_us = Vec::with_capacity(iterations as usize);
        for i in 0..iterations {
            let label = if i % 2 == 0 { "Close" } else { "Open" };
            help.entries[1].desc.clear();
            help.entries[1].desc.push_str(label);

            let start = Instant::now();
            StatefulWidget::render(&help, area, &mut frame, &mut state);
            let elapsed = start.elapsed();
            times_us.push(elapsed.as_micros() as u64);
        }

        times_us.sort();
        let len = times_us.len();
        let p50 = times_us[len / 2];
        let p95 = times_us[((len as f64 * 0.95) as usize).min(len.saturating_sub(1))];
        let p99 = times_us[((len as f64 * 0.99) as usize).min(len.saturating_sub(1))];
        let updates_per_sec = 1_000_000u64.checked_div(p50).unwrap_or(0);

        eprintln!(
            "{{\"ts\":\"2026-02-03T00:00:00Z\",\"case\":\"help_hint_update\",\"iterations\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"updates_per_sec\":{},\"hits\":{},\"misses\":{},\"dirty_updates\":{}}}",
            iterations,
            p50,
            p95,
            p99,
            updates_per_sec,
            state.stats().hits,
            state.stats().misses,
            state.stats().dirty_updates
        );

        // Budget: keep p95 under 2ms in CI (500 updates/sec).
        assert!(p95 <= 2000, "p95 too slow: {p95}us");
    }

    // ── HelpCategory tests ─────────────────────────────────────────

    #[test]
    fn help_category_default_is_general() {
        assert_eq!(HelpCategory::default(), HelpCategory::General);
    }

    #[test]
    fn help_category_labels() {
        assert_eq!(HelpCategory::General.label(), "General");
        assert_eq!(HelpCategory::Navigation.label(), "Navigation");
        assert_eq!(HelpCategory::Editing.label(), "Editing");
        assert_eq!(HelpCategory::File.label(), "File");
        assert_eq!(HelpCategory::View.label(), "View");
        assert_eq!(HelpCategory::Global.label(), "Global");
        assert_eq!(
            HelpCategory::Custom("My Section".into()).label(),
            "My Section"
        );
    }

    #[test]
    fn help_entry_with_category() {
        let entry = HelpEntry::new("q", "quit").with_category(HelpCategory::Navigation);
        assert_eq!(entry.category, HelpCategory::Navigation);
    }

    #[test]
    fn help_entry_default_category_is_general() {
        let entry = HelpEntry::new("q", "quit");
        assert_eq!(entry.category, HelpCategory::General);
    }

    #[test]
    fn category_changes_entry_hash() {
        let a = HelpEntry::new("q", "quit");
        let b = HelpEntry::new("q", "quit").with_category(HelpCategory::Navigation);
        assert_ne!(Help::entry_hash(&a), Help::entry_hash(&b));
    }

    // ── KeyFormat tests ────────────────────────────────────────────

    #[test]
    fn key_format_default_is_plain() {
        assert_eq!(KeyFormat::default(), KeyFormat::Plain);
    }

    // ── KeybindingHints tests ──────────────────────────────────────

    #[test]
    fn keybinding_hints_new_is_empty() {
        let hints = KeybindingHints::new();
        assert!(hints.global_entries().is_empty());
        assert!(hints.contextual_entries().is_empty());
        assert_eq!(hints.mode(), HelpMode::Short);
        assert_eq!(hints.key_format(), KeyFormat::Plain);
    }

    #[test]
    fn keybinding_hints_default() {
        let hints = KeybindingHints::default();
        assert!(hints.global_entries().is_empty());
    }

    #[test]
    fn keybinding_hints_global_entry() {
        let hints = KeybindingHints::new()
            .global_entry("q", "quit")
            .global_entry("^s", "save");
        assert_eq!(hints.global_entries().len(), 2);
        assert_eq!(hints.global_entries()[0].key, "q");
        assert_eq!(hints.global_entries()[0].category, HelpCategory::Global);
    }

    #[test]
    fn keybinding_hints_categorized_entries() {
        let hints = KeybindingHints::new()
            .global_entry_categorized("Tab", "next", HelpCategory::Navigation)
            .global_entry_categorized("q", "quit", HelpCategory::Global);
        assert_eq!(hints.global_entries()[0].category, HelpCategory::Navigation);
        assert_eq!(hints.global_entries()[1].category, HelpCategory::Global);
    }

    #[test]
    fn keybinding_hints_contextual_entry() {
        let hints = KeybindingHints::new()
            .contextual_entry("^s", "save")
            .contextual_entry_categorized("^f", "find", HelpCategory::Editing);
        assert_eq!(hints.contextual_entries().len(), 2);
        assert_eq!(
            hints.contextual_entries()[0].category,
            HelpCategory::General
        );
        assert_eq!(
            hints.contextual_entries()[1].category,
            HelpCategory::Editing
        );
    }

    #[test]
    fn keybinding_hints_with_prebuilt_entries() {
        let global = HelpEntry::new("q", "quit").with_category(HelpCategory::Global);
        let ctx = HelpEntry::new("^s", "save").with_category(HelpCategory::File);
        let hints = KeybindingHints::new()
            .with_global_entry(global)
            .with_contextual_entry(ctx);
        assert_eq!(hints.global_entries().len(), 1);
        assert_eq!(hints.contextual_entries().len(), 1);
    }

    #[test]
    fn keybinding_hints_toggle_mode() {
        let mut hints = KeybindingHints::new();
        assert_eq!(hints.mode(), HelpMode::Short);
        hints.toggle_mode();
        assert_eq!(hints.mode(), HelpMode::Full);
        hints.toggle_mode();
        assert_eq!(hints.mode(), HelpMode::Short);
    }

    #[test]
    fn keybinding_hints_set_show_context() {
        let mut hints = KeybindingHints::new()
            .global_entry("q", "quit")
            .contextual_entry("^s", "save");

        // Context off: only global visible
        let visible = hints.visible_entries();
        assert_eq!(visible.len(), 1);

        // Context on: both visible
        hints.set_show_context(true);
        let visible = hints.visible_entries();
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn keybinding_hints_bracketed_format() {
        let hints = KeybindingHints::new()
            .with_key_format(KeyFormat::Bracketed)
            .global_entry("q", "quit");
        let visible = hints.visible_entries();
        assert_eq!(visible[0].key, "[q]");
    }

    #[test]
    fn keybinding_hints_plain_format() {
        let hints = KeybindingHints::new()
            .with_key_format(KeyFormat::Plain)
            .global_entry("q", "quit");
        let visible = hints.visible_entries();
        assert_eq!(visible[0].key, "q");
    }

    #[test]
    fn keybinding_hints_disabled_entries_hidden() {
        let hints = KeybindingHints::new()
            .with_global_entry(HelpEntry::new("a", "shown"))
            .with_global_entry(HelpEntry::new("b", "hidden").with_enabled(false));
        let visible = hints.visible_entries();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].key, "a");
    }

    #[test]
    fn keybinding_hints_grouped_entries() {
        let entries = vec![
            HelpEntry::new("Tab", "next").with_category(HelpCategory::Navigation),
            HelpEntry::new("q", "quit").with_category(HelpCategory::Global),
            HelpEntry::new("S-Tab", "prev").with_category(HelpCategory::Navigation),
        ];
        let groups = KeybindingHints::grouped_entries(&entries);
        assert_eq!(groups.len(), 2);
        assert_eq!(*groups[0].0, HelpCategory::Navigation);
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(*groups[1].0, HelpCategory::Global);
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn keybinding_hints_render_short() {
        let hints = KeybindingHints::new()
            .global_entry("q", "quit")
            .global_entry("^s", "save");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);
        Widget::render(&hints, area, &mut frame);

        // First char should be 'q' (plain format)
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('q'));
    }

    #[test]
    fn keybinding_hints_render_short_bracketed() {
        let hints = KeybindingHints::new()
            .with_key_format(KeyFormat::Bracketed)
            .global_entry("q", "quit");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);
        Widget::render(&hints, area, &mut frame);

        // First char should be '[' (bracketed format)
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('['));
    }

    #[test]
    fn keybinding_hints_render_full_grouped() {
        let hints = KeybindingHints::new()
            .with_mode(HelpMode::Full)
            .with_show_categories(true)
            .global_entry_categorized("Tab", "next", HelpCategory::Navigation)
            .global_entry_categorized("q", "quit", HelpCategory::Global);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);
        Widget::render(&hints, area, &mut frame);

        // Row 0 should contain category header "Navigation"
        let mut row0 = String::new();
        for x in 0..40u16 {
            if let Some(cell) = frame.buffer.get(x, 0)
                && let Some(ch) = cell.content.as_char()
            {
                row0.push(ch);
            }
        }
        assert!(
            row0.contains("Navigation"),
            "First row should be Navigation header: {row0}"
        );
    }

    #[test]
    fn keybinding_hints_render_full_no_categories() {
        let hints = KeybindingHints::new()
            .with_mode(HelpMode::Full)
            .with_show_categories(false)
            .global_entry("q", "quit")
            .global_entry("^s", "save");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 5, &mut pool);
        let area = Rect::new(0, 0, 40, 5);
        // Should not panic
        Widget::render(&hints, area, &mut frame);
    }

    #[test]
    fn keybinding_hints_render_empty() {
        let hints = KeybindingHints::new();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let area = Rect::new(0, 0, 20, 1);
        // Should not panic
        Widget::render(&hints, area, &mut frame);
    }

    #[test]
    fn keybinding_hints_render_zero_area() {
        let hints = KeybindingHints::new().global_entry("q", "quit");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let area = Rect::new(0, 0, 0, 0);
        // Should not panic
        Widget::render(&hints, area, &mut frame);
    }

    #[test]
    fn keybinding_hints_is_not_essential() {
        let hints = KeybindingHints::new();
        assert!(!hints.is_essential());
    }

    // ── Property tests for KeybindingHints ──────────────────────────

    proptest! {
        #[test]
        fn prop_visible_entries_count(
            n_global in 0..5usize,
            n_ctx in 0..5usize,
            show_ctx in proptest::bool::ANY,
        ) {
            let mut hints = KeybindingHints::new().with_show_context(show_ctx);
            for i in 0..n_global {
                hints = hints.global_entry(format!("g{i}"), format!("global {i}"));
            }
            for i in 0..n_ctx {
                hints = hints.contextual_entry(format!("c{i}"), format!("ctx {i}"));
            }
            let visible = hints.visible_entries();
            let expected = if show_ctx { n_global + n_ctx } else { n_global };
            prop_assert_eq!(visible.len(), expected);
        }

        #[test]
        fn prop_bracketed_keys_wrapped(
            keys in prop::collection::vec(string_regex("[a-z]{1,4}").unwrap(), 1..5),
        ) {
            let mut hints = KeybindingHints::new().with_key_format(KeyFormat::Bracketed);
            for key in &keys {
                hints = hints.global_entry(key.clone(), "action");
            }
            let visible = hints.visible_entries();
            for entry in &visible {
                prop_assert!(entry.key.starts_with('['), "Key should start with [: {}", entry.key);
                prop_assert!(entry.key.ends_with(']'), "Key should end with ]: {}", entry.key);
            }
        }

        #[test]
        fn prop_grouped_preserves_count(
            entries in prop::collection::vec(
                (string_regex("[a-z]{1,4}").unwrap(), 0..3u8),
                1..8
            ),
        ) {
            let help_entries: Vec<HelpEntry> = entries.into_iter().map(|(key, cat_idx)| {
                let cat = match cat_idx {
                    0 => HelpCategory::Navigation,
                    1 => HelpCategory::Editing,
                    _ => HelpCategory::Global,
                };
                HelpEntry::new(key, "action").with_category(cat)
            }).collect();

            let total = help_entries.len();
            let groups = KeybindingHints::grouped_entries(&help_entries);
            let grouped_total: usize = groups.iter().map(|(_, v)| v.len()).sum();
            prop_assert_eq!(total, grouped_total, "Grouping should preserve total entry count");
        }

        #[test]
        fn prop_render_no_panic(
            n_global in 0..5usize,
            n_ctx in 0..5usize,
            width in 1..80u16,
            height in 1..20u16,
            show_ctx in proptest::bool::ANY,
            use_full in proptest::bool::ANY,
            use_brackets in proptest::bool::ANY,
            show_cats in proptest::bool::ANY,
        ) {
            let mode = if use_full { HelpMode::Full } else { HelpMode::Short };
            let fmt = if use_brackets { KeyFormat::Bracketed } else { KeyFormat::Plain };
            let mut hints = KeybindingHints::new()
                .with_mode(mode)
                .with_key_format(fmt)
                .with_show_context(show_ctx)
                .with_show_categories(show_cats);

            for i in 0..n_global {
                hints = hints.global_entry(format!("g{i}"), format!("global action {i}"));
            }
            for i in 0..n_ctx {
                hints = hints.contextual_entry(format!("c{i}"), format!("ctx action {i}"));
            }

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(width, height, &mut pool);
            let area = Rect::new(0, 0, width, height);
            Widget::render(&hints, area, &mut frame);
            // No panic = pass
        }
    }

    // ========================================================================
    // Edge-case tests (bd-1noim)
    // ========================================================================

    // ── HelpCategory edge cases ─────────────────────────────────────

    #[test]
    fn help_category_custom_empty_string() {
        let cat = HelpCategory::Custom(String::new());
        assert_eq!(cat.label(), "");
    }

    #[test]
    fn help_category_custom_eq() {
        let a = HelpCategory::Custom("Foo".into());
        let b = HelpCategory::Custom("Foo".into());
        let c = HelpCategory::Custom("Bar".into());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn help_category_clone() {
        let cat = HelpCategory::Navigation;
        let cloned = cat.clone();
        assert_eq!(cat, cloned);
    }

    #[test]
    fn help_category_hash_consistency() {
        use std::collections::hash_map::DefaultHasher;
        let mut h1 = DefaultHasher::new();
        let mut h2 = DefaultHasher::new();
        HelpCategory::File.hash(&mut h1);
        HelpCategory::File.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn help_category_debug_format() {
        let dbg = format!("{:?}", HelpCategory::General);
        assert!(dbg.contains("General"));
        let dbg_custom = format!("{:?}", HelpCategory::Custom("X".into()));
        assert!(dbg_custom.contains("Custom"));
    }

    // ── HelpEntry edge cases ────────────────────────────────────────

    #[test]
    fn help_entry_empty_key_and_desc() {
        let entry = HelpEntry::new("", "");
        assert!(entry.key.is_empty());
        assert!(entry.desc.is_empty());
        assert!(entry.enabled);
    }

    #[test]
    fn help_entry_clone() {
        let entry = HelpEntry::new("q", "quit").with_category(HelpCategory::File);
        let cloned = entry.clone();
        assert_eq!(entry, cloned);
    }

    #[test]
    fn help_entry_debug_format() {
        let entry = HelpEntry::new("^s", "save");
        let dbg = format!("{:?}", entry);
        assert!(dbg.contains("HelpEntry"));
        assert!(dbg.contains("save"));
    }

    // ── HelpMode edge cases ─────────────────────────────────────────

    #[test]
    fn help_mode_default_is_short() {
        assert_eq!(HelpMode::default(), HelpMode::Short);
    }

    #[test]
    fn help_mode_eq_and_hash() {
        use std::collections::hash_map::DefaultHasher;
        assert_eq!(HelpMode::Short, HelpMode::Short);
        assert_ne!(HelpMode::Short, HelpMode::Full);
        let mut h = DefaultHasher::new();
        HelpMode::Full.hash(&mut h);
        // Just verify it doesn't panic
    }

    #[test]
    fn help_mode_copy() {
        let m = HelpMode::Full;
        let m2 = m; // Copy
        assert_eq!(m, m2);
    }

    // ── Help rendering edge cases ───────────────────────────────────

    #[test]
    fn render_short_all_disabled() {
        let help = Help::new()
            .with_entry(HelpEntry::new("a", "first").with_enabled(false))
            .with_entry(HelpEntry::new("b", "second").with_enabled(false));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);
        Widget::render(&help, area, &mut frame);
        // No visible entries, buffer stays default
        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(cell.content.is_empty() || cell.content.as_char() == Some(' '));
    }

    #[test]
    fn render_short_empty_key_desc_entries_skipped() {
        let help = Help::new()
            .with_entry(HelpEntry::new("", ""))
            .entry("q", "quit");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);
        Widget::render(&help, area, &mut frame);
        // The empty entry produces no text but separator logic still fires.
        // Verify 'q' appears somewhere in the rendered row.
        let mut found_q = false;
        for x in 0..40 {
            if let Some(cell) = frame.buffer.get(x, 0)
                && cell.content.as_char() == Some('q')
            {
                found_q = true;
                break;
            }
        }
        assert!(found_q, "'q' should appear in the rendered row");
    }

    #[test]
    fn render_short_width_one() {
        let help = Help::new().entry("q", "quit");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let area = Rect::new(0, 0, 1, 1);
        Widget::render(&help, area, &mut frame);
        // Should not panic; may show ellipsis or partial
    }

    #[test]
    fn render_full_width_one() {
        let help = Help::new().with_mode(HelpMode::Full).entry("q", "quit");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 5, &mut pool);
        let area = Rect::new(0, 0, 1, 5);
        Widget::render(&help, area, &mut frame);
        // Should not panic
    }

    #[test]
    fn render_full_height_one() {
        let help = Help::new()
            .with_mode(HelpMode::Full)
            .entry("a", "first")
            .entry("b", "second")
            .entry("c", "third");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);
        Widget::render(&help, area, &mut frame);
        // Only first entry should render
    }

    #[test]
    fn render_short_single_entry_exact_fit() {
        // "q quit" = 6 chars, area width = 6
        let help = Help::new().entry("q", "quit");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 1, &mut pool);
        let area = Rect::new(0, 0, 6, 1);
        Widget::render(&help, area, &mut frame);
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('q'));
    }

    #[test]
    fn render_short_empty_separator() {
        let help = Help::new()
            .with_separator("")
            .entry("a", "x")
            .entry("b", "y");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);
        Widget::render(&help, area, &mut frame);
        // Both entries render without separator
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('a'));
    }

    // ── Help builder edge cases ─────────────────────────────────────

    #[test]
    fn help_with_mode_full() {
        let help = Help::new().with_mode(HelpMode::Full);
        assert_eq!(help.mode(), HelpMode::Full);
    }

    #[test]
    fn help_clone() {
        let help = Help::new()
            .entry("q", "quit")
            .with_separator(" | ")
            .with_ellipsis("...");
        let cloned = help.clone();
        assert_eq!(cloned.entries().len(), 1);
        assert_eq!(cloned.separator, " | ");
        assert_eq!(cloned.ellipsis, "...");
    }

    #[test]
    fn help_debug_format() {
        let help = Help::new().entry("q", "quit");
        let dbg = format!("{:?}", help);
        assert!(dbg.contains("Help"));
    }

    // ── HelpRenderState edge cases ──────────────────────────────────

    #[test]
    fn help_render_state_default() {
        let state = HelpRenderState::default();
        assert!(state.cache.is_none());
        assert!(state.dirty_rects().is_empty());
        assert_eq!(state.stats().hits, 0);
        assert_eq!(state.stats().misses, 0);
    }

    #[test]
    fn help_render_state_clear_dirty_rects() {
        let mut state = HelpRenderState::default();
        state.dirty_rects.push(Rect::new(0, 0, 10, 1));
        assert_eq!(state.dirty_rects().len(), 1);
        state.clear_dirty_rects();
        assert!(state.dirty_rects().is_empty());
    }

    #[test]
    fn help_render_state_take_dirty_rects() {
        let mut state = HelpRenderState::default();
        state.dirty_rects.push(Rect::new(0, 0, 5, 1));
        state.dirty_rects.push(Rect::new(0, 1, 5, 1));
        let taken = state.take_dirty_rects();
        assert_eq!(taken.len(), 2);
        assert!(state.dirty_rects().is_empty()); // cleared after take
    }

    #[test]
    fn help_render_state_reset_stats() {
        let mut state = HelpRenderState::default();
        state.stats.hits = 42;
        state.stats.misses = 7;
        state.stats.dirty_updates = 3;
        state.stats.layout_rebuilds = 2;
        state.reset_stats();
        assert_eq!(state.stats(), HelpCacheStats::default());
    }

    #[test]
    fn help_cache_stats_default() {
        let stats = HelpCacheStats::default();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.dirty_updates, 0);
        assert_eq!(stats.layout_rebuilds, 0);
    }

    #[test]
    fn help_cache_stats_clone_eq() {
        let a = HelpCacheStats {
            hits: 5,
            misses: 2,
            dirty_updates: 1,
            layout_rebuilds: 3,
        };
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn stateful_render_empty_area_clears_cache() {
        let help = Help::new().entry("q", "quit");
        let mut state = HelpRenderState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 1, &mut pool);
        let area = Rect::new(0, 0, 40, 1);

        // First render populates cache
        StatefulWidget::render(&help, area, &mut frame, &mut state);
        assert!(state.cache.is_some());

        // Render with empty area clears cache
        let empty = Rect::new(0, 0, 0, 0);
        StatefulWidget::render(&help, empty, &mut frame, &mut state);
        assert!(state.cache.is_none());
    }

    #[test]
    fn stateful_render_cache_miss_on_area_change() {
        let help = Help::new().entry("q", "quit").entry("^s", "save");
        let mut state = HelpRenderState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 5, &mut pool);

        StatefulWidget::render(&help, Rect::new(0, 0, 40, 1), &mut frame, &mut state);
        let misses1 = state.stats().misses;

        StatefulWidget::render(&help, Rect::new(0, 0, 60, 1), &mut frame, &mut state);
        let misses2 = state.stats().misses;

        assert!(misses2 > misses1, "Area change should cause cache miss");
    }

    #[test]
    fn stateful_render_cache_miss_on_mode_change() {
        let mut help = Help::new().entry("q", "quit");
        let mut state = HelpRenderState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 5, &mut pool);
        let area = Rect::new(0, 0, 40, 5);

        StatefulWidget::render(&help, area, &mut frame, &mut state);
        let misses1 = state.stats().misses;

        help.toggle_mode();
        StatefulWidget::render(&help, area, &mut frame, &mut state);
        let misses2 = state.stats().misses;

        assert!(misses2 > misses1, "Mode change should cause cache miss");
    }

    #[test]
    fn stateful_render_layout_rebuild_on_enabled_count_change() {
        let mut help = Help::new()
            .entry("q", "quit")
            .entry("^s", "save")
            .entry("^x", "exit");
        let mut state = HelpRenderState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 1, &mut pool);
        let area = Rect::new(0, 0, 80, 1);

        StatefulWidget::render(&help, area, &mut frame, &mut state);
        let rebuilds1 = state.stats().layout_rebuilds;

        // Disable one entry
        help.entries[1].enabled = false;
        StatefulWidget::render(&help, area, &mut frame, &mut state);
        let rebuilds2 = state.stats().layout_rebuilds;

        assert!(
            rebuilds2 > rebuilds1,
            "Enabled count change should trigger layout rebuild"
        );
    }

    // ── KeyFormat edge cases ────────────────────────────────────────

    #[test]
    fn key_format_eq_and_hash() {
        use std::collections::hash_map::DefaultHasher;
        assert_eq!(KeyFormat::Plain, KeyFormat::Plain);
        assert_ne!(KeyFormat::Plain, KeyFormat::Bracketed);
        let mut h = DefaultHasher::new();
        KeyFormat::Bracketed.hash(&mut h);
    }

    #[test]
    fn key_format_copy() {
        let f = KeyFormat::Bracketed;
        let f2 = f;
        assert_eq!(f, f2);
    }

    #[test]
    fn key_format_debug() {
        let dbg = format!("{:?}", KeyFormat::Bracketed);
        assert!(dbg.contains("Bracketed"));
    }

    // ── KeybindingHints edge cases ──────────────────────────────────

    #[test]
    fn keybinding_hints_clone() {
        let hints = KeybindingHints::new()
            .global_entry("q", "quit")
            .contextual_entry("^s", "save");
        let cloned = hints.clone();
        assert_eq!(cloned.global_entries().len(), 1);
        assert_eq!(cloned.contextual_entries().len(), 1);
    }

    #[test]
    fn keybinding_hints_debug() {
        let hints = KeybindingHints::new().global_entry("q", "quit");
        let dbg = format!("{:?}", hints);
        assert!(dbg.contains("KeybindingHints"));
    }

    #[test]
    fn keybinding_hints_with_separator() {
        let hints = KeybindingHints::new().with_separator(" | ");
        assert_eq!(hints.separator, " | ");
    }

    #[test]
    fn keybinding_hints_with_styles() {
        let hints = KeybindingHints::new()
            .with_key_style(Style::new().bold())
            .with_desc_style(Style::default())
            .with_separator_style(Style::default())
            .with_category_style(Style::new().underline());
        // Just verify builder doesn't panic
        assert_eq!(hints.mode(), HelpMode::Short);
    }

    #[test]
    fn keybinding_hints_visible_entries_disabled_contextual() {
        let hints = KeybindingHints::new()
            .with_show_context(true)
            .global_entry("q", "quit")
            .with_contextual_entry(HelpEntry::new("^s", "save").with_enabled(false));
        let visible = hints.visible_entries();
        // Only global "q" visible; disabled contextual "^s" hidden
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].desc, "quit");
    }

    #[test]
    fn keybinding_hints_empty_global_nonempty_ctx_hidden() {
        let hints = KeybindingHints::new()
            .contextual_entry("^s", "save")
            .contextual_entry("^f", "find");
        // Context off by default
        let visible = hints.visible_entries();
        assert!(visible.is_empty());
    }

    #[test]
    fn keybinding_hints_render_full_grouped_height_limit() {
        let hints = KeybindingHints::new()
            .with_mode(HelpMode::Full)
            .with_show_categories(true)
            .global_entry_categorized("a", "first", HelpCategory::Navigation)
            .global_entry_categorized("b", "second", HelpCategory::Navigation)
            .global_entry_categorized("c", "third", HelpCategory::Navigation)
            .global_entry_categorized("d", "fourth", HelpCategory::Global)
            .global_entry_categorized("e", "fifth", HelpCategory::Global);

        let mut pool = GraphemePool::new();
        // Only 3 rows: header + 2 entries, can't fit all
        let mut frame = Frame::new(40, 3, &mut pool);
        let area = Rect::new(0, 0, 40, 3);
        Widget::render(&hints, area, &mut frame);
        // Should not panic; clips to available height
    }

    #[test]
    fn keybinding_hints_render_empty_area() {
        let hints = KeybindingHints::new().global_entry("q", "quit");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Widget::render(&hints, Rect::new(0, 0, 0, 0), &mut frame);
        // Should not panic (is_empty check)
    }

    // ── Help entry_hash edge cases ──────────────────────────────────

    #[test]
    fn entry_hash_differs_for_different_keys() {
        let a = HelpEntry::new("q", "quit");
        let b = HelpEntry::new("x", "quit");
        assert_ne!(Help::entry_hash(&a), Help::entry_hash(&b));
    }

    #[test]
    fn entry_hash_differs_for_different_descs() {
        let a = HelpEntry::new("q", "quit");
        let b = HelpEntry::new("q", "exit");
        assert_ne!(Help::entry_hash(&a), Help::entry_hash(&b));
    }

    #[test]
    fn entry_hash_differs_for_enabled_flag() {
        let a = HelpEntry::new("q", "quit");
        let b = HelpEntry::new("q", "quit").with_enabled(false);
        assert_ne!(Help::entry_hash(&a), Help::entry_hash(&b));
    }

    #[test]
    fn entry_hash_same_for_equal_entries() {
        let a = HelpEntry::new("q", "quit");
        let b = HelpEntry::new("q", "quit");
        assert_eq!(Help::entry_hash(&a), Help::entry_hash(&b));
    }
}

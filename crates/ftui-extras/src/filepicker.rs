#![forbid(unsafe_code)]

//! File picker widget for browsing and selecting files/directories.
//!
//! The widget handles navigation and rendering; the caller provides
//! directory listings (no filesystem access in the widget itself).
//!
//! # Example
//! ```ignore
//! use ftui_extras::filepicker::{FilePicker, FileEntry, FileKind};
//!
//! let entries = vec![
//!     FileEntry::new("..", FileKind::Directory),
//!     FileEntry::new("src", FileKind::Directory),
//!     FileEntry::new("Cargo.toml", FileKind::File),
//! ];
//! let mut picker = FilePicker::new(entries);
//! picker.move_down(); // select "src"
//! assert_eq!(picker.selected_entry().unwrap().name, "src");
//! ```

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;
use unicode_width::UnicodeWidthStr;

/// The kind of a file entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
}

/// A single entry in the file picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Display name (filename, not full path).
    pub name: String,
    /// Kind of entry.
    pub kind: FileKind,
    /// Size in bytes (for files).
    pub size: Option<u64>,
}

impl FileEntry {
    /// Create a new file entry.
    #[must_use]
    pub fn new(name: impl Into<String>, kind: FileKind) -> Self {
        Self {
            name: name.into(),
            kind,
            size: None,
        }
    }

    /// Set the file size.
    #[must_use]
    pub fn with_size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    /// Whether this is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Directory
    }

    /// Icon prefix for display.
    #[must_use]
    pub fn icon(&self) -> &'static str {
        match self.kind {
            FileKind::File => " ",
            FileKind::Directory => "/",
            FileKind::Symlink => "@",
        }
    }
}

/// Styles for the file picker.
#[derive(Debug, Clone, Default)]
pub struct FilePickerStyle {
    /// Style for the selected/cursor line.
    pub selected: Style,
    /// Style for directory entries.
    pub directory: Style,
    /// Style for file entries.
    pub file: Style,
    /// Style for symlink entries.
    pub symlink: Style,
    /// Style for the current path display.
    pub path: Style,
}

/// Configuration for filtering entries.
#[derive(Debug, Clone, Default)]
pub struct FilePickerFilter {
    /// Only show entries with these extensions (empty = show all).
    pub allowed_extensions: Vec<String>,
    /// Whether to show hidden files (starting with '.').
    pub show_hidden: bool,
}

impl FilePickerFilter {
    /// Check if an entry passes the filter.
    #[must_use]
    pub fn matches(&self, entry: &FileEntry) -> bool {
        // Directories always pass
        if entry.is_dir() {
            return true;
        }
        // Hidden files check
        if !self.show_hidden && entry.name.starts_with('.') {
            return false;
        }
        // Extension filter
        if !self.allowed_extensions.is_empty() {
            let ext = entry
                .name
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            return self
                .allowed_extensions
                .iter()
                .any(|a| a.to_ascii_lowercase() == ext);
        }
        true
    }
}

/// File picker state for stateful rendering.
#[derive(Debug, Clone)]
pub struct FilePicker {
    /// All entries in the current directory.
    entries: Vec<FileEntry>,
    /// Currently selected index (into filtered entries).
    selected: usize,
    /// Scroll offset for the visible window.
    scroll_offset: usize,
    /// Current directory path (display only).
    current_path: String,
    /// Filter configuration.
    filter: FilePickerFilter,
    /// Cached filtered indices.
    filtered_indices: Vec<usize>,
    /// Style configuration.
    style: FilePickerStyle,
    /// Visible height (set during render or manually).
    visible_height: usize,
}

impl Default for FilePicker {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl FilePicker {
    /// Create a new file picker with the given entries.
    #[must_use]
    pub fn new(entries: Vec<FileEntry>) -> Self {
        let mut picker = Self {
            entries,
            selected: 0,
            scroll_offset: 0,
            current_path: String::from("/"),
            filter: FilePickerFilter::default(),
            filtered_indices: Vec::new(),
            style: FilePickerStyle::default(),
            visible_height: 20,
        };
        picker.rebuild_filter();
        picker
    }

    /// Set the current path for display.
    pub fn set_path(&mut self, path: impl Into<String>) {
        self.current_path = path.into();
    }

    /// Get the current display path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.current_path
    }

    /// Replace all entries (e.g., when navigating to a new directory).
    pub fn set_entries(&mut self, entries: Vec<FileEntry>) {
        self.entries = entries;
        self.selected = 0;
        self.scroll_offset = 0;
        self.rebuild_filter();
    }

    /// Set the filter configuration.
    pub fn set_filter(&mut self, filter: FilePickerFilter) {
        self.filter = filter;
        self.rebuild_filter();
    }

    /// Set styles.
    pub fn set_style(&mut self, style: FilePickerStyle) {
        self.style = style;
    }

    /// Number of filtered entries.
    #[must_use]
    pub fn filtered_count(&self) -> usize {
        self.filtered_indices.len()
    }

    /// Currently selected index in the filtered list.
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Get the currently selected entry, if any.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&FileEntry> {
        let idx = *self.filtered_indices.get(self.selected)?;
        self.entries.get(idx)
    }

    /// Move selection up by one.
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.ensure_visible();
        }
    }

    /// Move selection down by one.
    pub fn move_down(&mut self) {
        if self.selected + 1 < self.filtered_indices.len() {
            self.selected += 1;
            self.ensure_visible();
        }
    }

    /// Move selection to the first entry.
    pub fn move_to_first(&mut self) {
        self.selected = 0;
        self.scroll_offset = 0;
    }

    /// Move selection to the last entry.
    pub fn move_to_last(&mut self) {
        if !self.filtered_indices.is_empty() {
            self.selected = self.filtered_indices.len() - 1;
            self.ensure_visible();
        }
    }

    /// Page up.
    pub fn page_up(&mut self) {
        if self.visible_height > 1 {
            self.selected = self.selected.saturating_sub(self.visible_height - 1);
            self.ensure_visible();
        }
    }

    /// Page down.
    pub fn page_down(&mut self) {
        if self.visible_height > 1 && !self.filtered_indices.is_empty() {
            self.selected =
                (self.selected + self.visible_height - 1).min(self.filtered_indices.len() - 1);
            self.ensure_visible();
        }
    }

    /// Rebuild the filtered indices cache.
    fn rebuild_filter(&mut self) {
        self.filtered_indices = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.filter.matches(e))
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len().saturating_sub(1);
        }
        self.ensure_visible();
    }

    /// Ensure the selected item is visible within the scroll window.
    fn ensure_visible(&mut self) {
        if self.visible_height == 0 {
            return;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.visible_height {
            self.scroll_offset = self.selected - self.visible_height + 1;
        }
    }

    /// Render the file picker into the given area.
    pub fn render(&mut self, area: Rect, frame: &mut Frame) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let width = area.width as usize;

        // First line: current path
        let path_display = truncate_str(&self.current_path, width);
        draw_line(
            &mut frame.buffer,
            area.x,
            area.y,
            &path_display,
            self.style.path,
            width,
        );

        // Remaining lines: entries
        let entry_area_height = (area.height as usize).saturating_sub(1);
        self.visible_height = entry_area_height;
        self.ensure_visible();

        for row in 0..entry_area_height {
            let idx = self.scroll_offset + row;
            let y = area.y + 1 + row as u16;

            if let Some(&orig_idx) = self.filtered_indices.get(idx) {
                let entry = &self.entries[orig_idx];
                let is_selected = idx == self.selected;

                let prefix = entry.icon();
                let name = &entry.name;
                let display = format!("{prefix}{name}");
                let display = truncate_str(&display, width);

                let style = if is_selected {
                    self.style.selected
                } else {
                    match entry.kind {
                        FileKind::Directory => self.style.directory,
                        FileKind::File => self.style.file,
                        FileKind::Symlink => self.style.symlink,
                    }
                };

                draw_line(&mut frame.buffer, area.x, y, &display, style, width);
            }
        }
    }
}

/// Truncate a string to fit within `max_width` display columns.
fn truncate_str(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        return s.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let target = max_width - 3;
    let mut result = String::new();
    let mut current_width = 0;
    for ch in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + w > target {
            break;
        }
        result.push(ch);
        current_width += w;
    }
    result.push_str("...");
    result
}

/// Draw a single line of text into the buffer, filling remaining width with spaces.
fn draw_line(buffer: &mut Buffer, x: u16, y: u16, text: &str, style: Style, width: usize) {
    let mut col = 0;
    for ch in text.chars() {
        if col >= width {
            break;
        }
        let cell_x = x + col as u16;
        let mut cell = Cell::from_char(ch);
        apply_style(&mut cell, style);
        buffer.set(cell_x, y, cell);
        col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
    }
    // Fill remaining with spaces
    while col < width {
        let cell_x = x + col as u16;
        let mut cell = Cell::from_char(' ');
        apply_style(&mut cell, style);
        buffer.set(cell_x, y, cell);
        col += 1;
    }
}

/// Apply a style to a cell.
fn apply_style(cell: &mut Cell, style: Style) {
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg {
        cell.bg = bg;
    }
    if let Some(attrs) = style.attrs {
        let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
        cell.attrs = cell.attrs.with_flags(cell_flags);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    fn sample_entries() -> Vec<FileEntry> {
        vec![
            FileEntry::new("..", FileKind::Directory),
            FileEntry::new("src", FileKind::Directory),
            FileEntry::new("tests", FileKind::Directory),
            FileEntry::new("Cargo.toml", FileKind::File),
            FileEntry::new("README.md", FileKind::File),
            FileEntry::new(".gitignore", FileKind::File),
        ]
    }

    #[test]
    fn new_picker_selects_first() {
        let picker = FilePicker::new(sample_entries());
        assert_eq!(picker.selected_index(), 0);
        assert_eq!(picker.selected_entry().unwrap().name, "..");
    }

    #[test]
    fn move_down_and_up() {
        let mut picker = FilePicker::new(sample_entries());
        picker.move_down();
        assert_eq!(picker.selected_entry().unwrap().name, "src");
        picker.move_down();
        assert_eq!(picker.selected_entry().unwrap().name, "tests");
        picker.move_up();
        assert_eq!(picker.selected_entry().unwrap().name, "src");
    }

    #[test]
    fn move_up_at_top_is_noop() {
        let mut picker = FilePicker::new(sample_entries());
        picker.move_up();
        assert_eq!(picker.selected_index(), 0);
    }

    #[test]
    fn move_down_at_bottom_is_noop() {
        let mut picker = FilePicker::new(sample_entries());
        for _ in 0..10 {
            picker.move_down();
        }
        assert_eq!(picker.selected_index(), picker.filtered_count() - 1);
    }

    #[test]
    fn move_to_first_and_last() {
        let mut picker = FilePicker::new(sample_entries());
        picker.move_to_last();
        // .gitignore is hidden by default filter, so last visible entry is README.md
        assert_eq!(picker.selected_entry().unwrap().name, "README.md");
        picker.move_to_first();
        assert_eq!(picker.selected_entry().unwrap().name, "..");
    }

    #[test]
    fn set_entries_resets_selection() {
        let mut picker = FilePicker::new(sample_entries());
        picker.move_down();
        picker.move_down();
        assert_eq!(picker.selected_index(), 2);

        picker.set_entries(vec![FileEntry::new("new_file.txt", FileKind::File)]);
        assert_eq!(picker.selected_index(), 0);
        assert_eq!(picker.selected_entry().unwrap().name, "new_file.txt");
    }

    #[test]
    fn empty_picker() {
        let picker = FilePicker::new(Vec::new());
        assert_eq!(picker.filtered_count(), 0);
        assert!(picker.selected_entry().is_none());
    }

    #[test]
    fn filter_by_extension() {
        let mut picker = FilePicker::new(sample_entries());
        picker.set_filter(FilePickerFilter {
            allowed_extensions: vec!["md".into()],
            show_hidden: true,
        });
        // Should show directories + .md files
        let names: Vec<&str> = (0..picker.filtered_count())
            .filter_map(|i| {
                let idx = picker.filtered_indices[i];
                Some(picker.entries[idx].name.as_str())
            })
            .collect();
        assert!(names.contains(&"README.md"));
        assert!(!names.contains(&"Cargo.toml"));
        // Directories always pass
        assert!(names.contains(&"src"));
    }

    #[test]
    fn filter_hidden_files() {
        let mut picker = FilePicker::new(sample_entries());
        picker.set_filter(FilePickerFilter {
            allowed_extensions: vec![],
            show_hidden: false,
        });
        let names: Vec<&str> = (0..picker.filtered_count())
            .filter_map(|i| {
                let idx = picker.filtered_indices[i];
                Some(picker.entries[idx].name.as_str())
            })
            .collect();
        assert!(!names.contains(&".gitignore"));
        assert!(names.contains(&"Cargo.toml"));
    }

    #[test]
    fn filter_show_hidden() {
        let mut picker = FilePicker::new(sample_entries());
        picker.set_filter(FilePickerFilter {
            allowed_extensions: vec![],
            show_hidden: true,
        });
        let has_hidden = (0..picker.filtered_count()).any(|i| {
            let idx = picker.filtered_indices[i];
            picker.entries[idx].name == ".gitignore"
        });
        assert!(has_hidden);
    }

    #[test]
    fn page_up_down() {
        let entries: Vec<FileEntry> = (0..50)
            .map(|i| FileEntry::new(format!("file_{i:03}.txt"), FileKind::File))
            .collect();
        let mut picker = FilePicker::new(entries);
        picker.visible_height = 10;

        picker.page_down();
        assert_eq!(picker.selected_index(), 9);

        picker.page_down();
        assert_eq!(picker.selected_index(), 18);

        picker.page_up();
        assert_eq!(picker.selected_index(), 9);
    }

    #[test]
    fn scroll_offset_follows_selection() {
        let entries: Vec<FileEntry> = (0..30)
            .map(|i| FileEntry::new(format!("file_{i:03}.txt"), FileKind::File))
            .collect();
        let mut picker = FilePicker::new(entries);
        picker.visible_height = 5;

        // Move past visible window
        for _ in 0..7 {
            picker.move_down();
        }
        assert!(picker.scroll_offset > 0);
        assert!(picker.selected >= picker.scroll_offset);
        assert!(picker.selected < picker.scroll_offset + picker.visible_height);
    }

    #[test]
    fn file_entry_icons() {
        assert_eq!(FileEntry::new("f", FileKind::File).icon(), " ");
        assert_eq!(FileEntry::new("d", FileKind::Directory).icon(), "/");
        assert_eq!(FileEntry::new("l", FileKind::Symlink).icon(), "@");
    }

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_truncated() {
        let result = truncate_str("hello world", 8);
        assert!(result.ends_with("..."));
        assert!(result.width() <= 8);
    }

    #[test]
    fn truncate_str_very_narrow() {
        assert_eq!(truncate_str("hello", 3), "...");
        assert_eq!(truncate_str("hello", 2), "..");
        assert_eq!(truncate_str("hello", 1), ".");
    }

    #[test]
    fn render_basic() {
        let mut picker = FilePicker::new(sample_entries());
        picker.set_path("/home/user/project");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::from_size(40, 10);

        picker.render(area, &mut frame);
        // Should not panic and should produce some output
    }

    #[test]
    fn render_zero_area() {
        let mut picker = FilePicker::new(sample_entries());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);

        // Zero height should not panic
        picker.render(Rect::from_size(40, 0), &mut frame);
        // Zero width should not panic
        picker.render(Rect::from_size(0, 10), &mut frame);
    }

    #[test]
    fn render_narrow_width() {
        let mut picker = FilePicker::new(sample_entries());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        let area = Rect::from_size(10, 10);

        picker.render(area, &mut frame);
        // Should not panic with narrow width
    }

    #[test]
    fn set_path_and_get() {
        let mut picker = FilePicker::new(Vec::new());
        picker.set_path("/tmp/test");
        assert_eq!(picker.path(), "/tmp/test");
    }

    #[test]
    fn file_entry_with_size() {
        let entry = FileEntry::new("big.dat", FileKind::File).with_size(1024);
        assert_eq!(entry.size, Some(1024));
    }
}

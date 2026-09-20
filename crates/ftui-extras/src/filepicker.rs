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

use ftui_a11y::node::{A11yNodeInfo, A11yRole};
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, CellContent};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::{display_width, grapheme_width, graphemes};

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
    /// Whether this picker currently owns keyboard focus.
    focused: bool,
    /// Optional stable accessibility identity for this picker.
    accessibility_id: Option<u64>,
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
            focused: false,
            accessibility_id: None,
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

    /// Set whether this picker currently owns keyboard focus.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Set a stable accessibility identity, or clear the override.
    pub fn set_accessibility_id(&mut self, id: Option<u64>) {
        self.accessibility_id = id;
    }

    /// Builder for a stable accessibility identity.
    #[must_use]
    pub fn accessibility_id(mut self, id: u64) -> Self {
        self.accessibility_id = Some(id);
        self
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

    fn accessibility_root_id(&self, area: Rect) -> u64 {
        self.accessibility_id
            .unwrap_or_else(|| A11yNodeInfo::stable_id_for(A11yRole::List, area))
    }

    fn accessibility_entry_id(&self, parent: u64, entry: &FileEntry) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        ("ftui.extras.filepicker.entry", parent, &self.current_path, &entry.name, entry.kind)
            .hash(&mut hasher);
        hasher.finish()
    }

    /// Push semantic nodes for a specific visible entry window.
    ///
    /// This is public because callers such as composite screens may render
    /// richer visual columns themselves while still reusing the picker's
    /// authoritative filtering, selection, identity, and focus semantics.
    pub fn push_accessibility_window(
        &self,
        area: Rect,
        frame: &mut Frame,
        focused: bool,
        scroll_offset: usize,
    ) {
        if !frame.a11y_enabled() || area.is_empty() {
            return;
        }
        let bounds = area.intersection(&frame.buffer.current_scissor());
        if bounds.is_empty() {
            return;
        }
        let root_id = self.accessibility_root_id(area);
        let root = A11yNodeInfo::new(root_id, A11yRole::List, bounds)
            .with_name(format!("Files in {}", self.current_path))
            .with_description(format!("{} entries", self.filtered_indices.len()));
        frame.with_a11y_scope(root, |frame| {
            if self.filtered_indices.is_empty() {
                let empty_bounds = Rect::new(area.x, area.y, area.width, 1)
                    .intersection(&frame.buffer.current_scissor());
                if !empty_bounds.is_empty() {
                    frame.push_a11y(
                        A11yNodeInfo::new(
                            self.accessibility_entry_id(
                                root_id,
                                &FileEntry::new("(empty directory)", FileKind::File),
                            ),
                            A11yRole::Label,
                            empty_bounds,
                        )
                        .with_name("Empty directory"),
                    );
                }
                return;
            }

            let end = (scroll_offset + area.height as usize).min(self.filtered_indices.len());
            for (row, filtered_index) in (scroll_offset..end).enumerate() {
                let Some(&entry_index) = self.filtered_indices.get(filtered_index) else {
                    continue;
                };
                let Some(entry) = self.entries.get(entry_index) else {
                    continue;
                };
                let y = area.y.saturating_add(row as u16);
                let item_bounds = Rect::new(area.x, y, area.width, 1)
                    .intersection(&frame.buffer.current_scissor());
                if item_bounds.is_empty() {
                    continue;
                }
                let kind = match entry.kind {
                    FileKind::Directory => "Directory",
                    FileKind::Symlink => "Symbolic link",
                    FileKind::File => "File",
                };
                let description = entry.size.map_or_else(
                    || kind.to_owned(),
                    |size| format!("{kind}. {size} bytes"),
                );
                let mut node = A11yNodeInfo::new(
                    self.accessibility_entry_id(root_id, entry),
                    A11yRole::ListItem,
                    item_bounds,
                )
                .with_name(&entry.name)
                .with_description(description);
                node.state.selected = filtered_index == self.selected;
                node.state.focused = focused && filtered_index == self.selected;
                frame.push_a11y(node);
            }
        });
    }

    /// Render the file picker into the given area.
    pub fn render(&mut self, area: Rect, frame: &mut Frame) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let width = area.width as usize;

        // First line: current path
        let path_display = truncate_str(&self.current_path, width);
        draw_line(frame, area.x, area.y, &path_display, self.style.path, width);

        // Remaining lines: entries
        let entry_area_height = (area.height as usize).saturating_sub(1);
        self.visible_height = entry_area_height;
        self.ensure_visible();

        let entries_area = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );
        self.push_accessibility_window(
            entries_area,
            frame,
            self.focused,
            self.scroll_offset,
        );

        for row in 0..entry_area_height {
            let idx = self.scroll_offset + row;
            let y = area.y.saturating_add(1).saturating_add(row as u16);

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

                draw_line(frame, area.x, y, &display, style, width);
            }
        }
    }
}

/// Calculate display width for a string using grapheme clusters.
fn text_width(text: &str) -> usize {
    display_width(text)
}

/// Truncate a string to fit within `max_width` display columns.
fn truncate_str(s: &str, max_width: usize) -> String {
    if text_width(s) <= max_width {
        return s.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let target = max_width - 3;
    let mut result = String::new();
    let mut current_width = 0;
    for grapheme in graphemes(s) {
        let w = grapheme_width(grapheme);
        if w == 0 {
            continue;
        }
        if current_width + w > target {
            break;
        }
        result.push_str(grapheme);
        current_width += w;
    }
    result.push_str("...");
    result
}

/// Draw a single line of text into the buffer, filling remaining width with spaces.
fn draw_line(frame: &mut Frame, x: u16, y: u16, text: &str, style: Style, width: usize) {
    let mut col = 0;
    for grapheme in graphemes(text) {
        if col >= width {
            break;
        }
        let w = grapheme_width(grapheme);
        if w == 0 {
            continue;
        }
        if col.saturating_add(w) > width {
            break;
        }

        let content = if w > 1 || grapheme.chars().count() > 1 {
            let id = frame.intern_with_width(grapheme, w.min(u8::MAX as usize) as u8);
            CellContent::from_grapheme(id)
        } else if let Some(c) = grapheme.chars().next() {
            CellContent::from_char(c)
        } else {
            continue;
        };

        let cell_x = x.saturating_add(col as u16);
        let mut cell = Cell::new(content);
        apply_style(&mut cell, style);
        frame.buffer.set_fast(cell_x, y, cell);
        col += w;
    }
    // Fill remaining with spaces
    while col < width {
        let cell_x = x.saturating_add(col as u16);
        let mut cell = Cell::from_char(' ');
        apply_style(&mut cell, style);
        frame.buffer.set_fast(cell_x, y, cell);
        col += 1;
    }
}

/// Apply a style to a cell using merge semantics.
///
/// - **fg:** replaced when set.
/// - **bg:** alpha-aware compositing (Porter-Duff SourceOver).
/// - **attrs:** OR-merged on top of existing flags (never cleared).
fn apply_style(cell: &mut Cell, style: Style) {
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg {
        match bg.a() {
            0 => {}                          // Fully transparent: no-op
            255 => cell.bg = bg,             // Fully opaque: replace
            _ => cell.bg = bg.over(cell.bg), // Composite src-over-dst
        }
    }
    if let Some(attrs) = style.attrs {
        let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
        cell.attrs = cell.attrs.merged_flags(cell_flags);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_a11y::tree::{
        A11yTree, A11yTreeBuilder, AnnouncementReason, ScreenReaderPolicy,
    };
    use ftui_render::grapheme_pool::GraphemePool;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("filepicker_fixture")
    }

    fn create_file_if_missing(path: &Path, contents: &str) {
        if path.exists() {
            return;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    fn ensure_fixture_dir() -> PathBuf {
        let root = fixture_root();
        fs::create_dir_all(&root).expect("create fixture root");

        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).expect("create src dir");

        create_file_if_missing(&root.join("README.md"), "# readme\n");
        create_file_if_missing(&root.join("notes.txt"), "notes\n");
        create_file_if_missing(&root.join(".hidden"), "hidden\n");
        create_file_if_missing(&src_dir.join("main.rs"), "fn main() {}\n");

        root
    }

    fn load_entries_sorted(dir: &Path) -> Vec<FileEntry> {
        let mut entries: Vec<FileEntry> = fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let file_type = entry.file_type().expect("file_type");
                let kind = if file_type.is_dir() {
                    FileKind::Directory
                } else if file_type.is_symlink() {
                    FileKind::Symlink
                } else {
                    FileKind::File
                };
                let size = if file_type.is_file() {
                    entry.metadata().ok().map(|m| m.len())
                } else {
                    None
                };
                FileEntry { name, kind, size }
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    fn render_picker_a11y(
        picker: &mut FilePicker,
        area: Rect,
    ) -> A11yTree {
        let mut builder = A11yTreeBuilder::new();
        {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(80, 24, &mut pool);
            frame.set_a11y(&mut builder);
            picker.render(area, &mut frame);
            frame.finish_a11y();
        }
        builder.build()
    }

    #[test]
    fn extras_file_picker_accessibility_tracks_focus_selection_and_kind() {
        let mut picker = FilePicker::new(sample_entries()).accessibility_id(7200);
        picker.set_path("/project");
        picker.set_focused(true);
        let tree = render_picker_a11y(&mut picker, Rect::new(0, 0, 40, 6));
        assert_eq!(tree.root_id(), Some(7200));
        let root = tree.root().unwrap();
        assert_eq!(root.role, A11yRole::List);
        assert!(root.name.as_deref().unwrap().contains("/project"));
        let focused = tree.focused().expect("selected file entry");
        assert_eq!(focused.role, A11yRole::ListItem);
        assert_eq!(focused.name.as_deref(), Some(".."));
        assert_eq!(focused.description.as_deref(), Some("Directory"));
        assert!(focused.state.selected);
    }

    #[test]
    fn extras_file_picker_cursor_feedback_depends_on_focus_ownership() {
        let mut picker = FilePicker::new(sample_entries()).accessibility_id(7201);
        picker.set_focused(true);
        let before = render_picker_a11y(&mut picker, Rect::new(0, 0, 40, 6));
        picker.move_down();
        let after = render_picker_a11y(&mut picker, Rect::new(0, 0, 40, 6));
        let batch =
            after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
        assert_eq!(batch.announcements.len(), 1);
        assert_eq!(batch.announcements[0].reason, AnnouncementReason::FocusChanged);
        assert!(batch.announcements[0].text.contains("src"));

        picker.set_focused(false);
        let unfocused = render_picker_a11y(&mut picker, Rect::new(0, 0, 40, 6));
        picker.move_down();
        let moved = render_picker_a11y(&mut picker, Rect::new(0, 0, 40, 6));
        assert!(moved.focused().is_none());
        assert!(
            moved
                .screen_reader_announcements_since(&unfocused, ScreenReaderPolicy::default())
                .announcements
                .is_empty()
        );
    }

    #[test]
    fn extras_file_picker_explicit_identity_survives_resize_and_manual_windowing() {
        let picker = FilePicker::new(sample_entries()).accessibility_id(7202);
        let mut builder = A11yTreeBuilder::new();
        {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(80, 24, &mut pool);
            frame.set_a11y(&mut builder);
            picker.push_accessibility_window(Rect::new(5, 4, 40, 2), &mut frame, true, 0);
            frame.finish_a11y();
        }
        let before = builder.build();
        let focused_id = before.focused_id();
        let mut builder = A11yTreeBuilder::new();
        {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(100, 30, &mut pool);
            frame.set_a11y(&mut builder);
            picker.push_accessibility_window(Rect::new(1, 1, 60, 3), &mut frame, true, 0);
            frame.finish_a11y();
        }
        let after = builder.build();
        assert_eq!(before.root_id(), after.root_id());
        assert_eq!(focused_id, after.focused_id());
    }

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
            .map(|i| {
                let idx = picker.filtered_indices[i];
                picker.entries[idx].name.as_str()
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
            .map(|i| {
                let idx = picker.filtered_indices[i];
                picker.entries[idx].name.as_str()
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
    fn filter_real_fs_extensions_and_hidden() {
        let root = ensure_fixture_dir();
        let entries = load_entries_sorted(&root);
        let mut picker = FilePicker::new(entries);
        picker.set_filter(FilePickerFilter {
            allowed_extensions: vec!["md".into()],
            show_hidden: false,
        });
        let names: Vec<String> = (0..picker.filtered_count())
            .map(|i| {
                let idx = picker.filtered_indices[i];
                picker.entries[idx].name.clone()
            })
            .collect();

        assert!(names.contains(&"README.md".to_string()));
        assert!(!names.contains(&"notes.txt".to_string()));
        assert!(!names.contains(&".hidden".to_string()));
        assert!(names.contains(&"src".to_string()));
    }

    #[test]
    fn filter_real_fs_show_hidden() {
        let root = ensure_fixture_dir();
        let entries = load_entries_sorted(&root);
        let mut picker = FilePicker::new(entries);
        picker.set_filter(FilePickerFilter {
            allowed_extensions: vec![],
            show_hidden: true,
        });
        let names: Vec<String> = (0..picker.filtered_count())
            .map(|i| {
                let idx = picker.filtered_indices[i];
                picker.entries[idx].name.clone()
            })
            .collect();
        assert!(names.contains(&".hidden".to_string()));
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
        assert!(text_width(&result) <= 8);
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

    // --- Additional edge case tests (bd-2gi6v) ---

    #[test]
    fn file_kind_debug_clone_copy_eq_hash() {
        use std::collections::HashSet;
        let kind = FileKind::Directory;
        let copied = kind;
        assert_eq!(kind, copied);
        assert_ne!(FileKind::File, FileKind::Directory);
        assert_ne!(FileKind::Symlink, FileKind::File);
        assert!(!format!("{:?}", kind).is_empty());

        // Hash: put in a HashSet
        let mut set = HashSet::new();
        set.insert(FileKind::File);
        set.insert(FileKind::Directory);
        set.insert(FileKind::Symlink);
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn file_entry_debug_clone_eq() {
        let entry = FileEntry::new("test.rs", FileKind::File).with_size(42);
        let cloned = entry.clone();
        assert_eq!(entry, cloned);
        assert!(!format!("{:?}", entry).is_empty());

        let different = FileEntry::new("other.rs", FileKind::File);
        assert_ne!(entry, different);
    }

    #[test]
    fn file_entry_is_dir() {
        assert!(FileEntry::new("d", FileKind::Directory).is_dir());
        assert!(!FileEntry::new("f", FileKind::File).is_dir());
        assert!(!FileEntry::new("l", FileKind::Symlink).is_dir());
    }

    #[test]
    fn file_entry_new_size_is_none() {
        let entry = FileEntry::new("x", FileKind::File);
        assert!(entry.size.is_none());
    }

    #[test]
    fn file_picker_style_debug_clone_default() {
        let style = FilePickerStyle::default();
        let cloned = style.clone();
        assert!(!format!("{:?}", cloned).is_empty());
    }

    #[test]
    fn file_picker_filter_debug_clone_default() {
        let filter = FilePickerFilter::default();
        let cloned = filter.clone();
        assert!(cloned.allowed_extensions.is_empty());
        assert!(!cloned.show_hidden);
        assert!(!format!("{:?}", filter).is_empty());
    }

    #[test]
    fn filter_directories_always_pass() {
        let filter = FilePickerFilter {
            allowed_extensions: vec!["rs".into()],
            show_hidden: false,
        };
        let dir = FileEntry::new(".hidden_dir", FileKind::Directory);
        assert!(filter.matches(&dir), "directories always pass filter");
    }

    #[test]
    fn filter_extension_case_insensitive() {
        let filter = FilePickerFilter {
            allowed_extensions: vec!["RS".into()],
            show_hidden: true,
        };
        let entry = FileEntry::new("main.rs", FileKind::File);
        assert!(
            filter.matches(&entry),
            "extension matching should be case-insensitive"
        );
    }

    #[test]
    fn file_picker_default_empty() {
        let picker = FilePicker::default();
        assert_eq!(picker.filtered_count(), 0);
        assert!(picker.selected_entry().is_none());
        assert_eq!(picker.path(), "/");
    }

    #[test]
    fn file_picker_debug_clone() {
        let picker = FilePicker::new(sample_entries());
        let cloned = picker.clone();
        assert_eq!(cloned.filtered_count(), picker.filtered_count());
        assert!(!format!("{:?}", picker).is_empty());
    }

    #[test]
    fn set_style_applies() {
        let mut picker = FilePicker::new(sample_entries());
        let style = FilePickerStyle {
            selected: Style::new().fg(ftui_render::cell::PackedRgba::rgb(255, 0, 0)),
            ..Default::default()
        };
        picker.set_style(style);
        // No panic, style is applied (verified implicitly via render)
    }

    #[test]
    fn page_up_from_top_stays_at_zero() {
        let mut picker = FilePicker::new(sample_entries());
        picker.visible_height = 3;
        picker.page_up();
        assert_eq!(picker.selected_index(), 0);
    }

    #[test]
    fn page_down_clamps_at_end() {
        let entries: Vec<FileEntry> = (0..10)
            .map(|i| FileEntry::new(format!("f{i}"), FileKind::File))
            .collect();
        let mut picker = FilePicker::new(entries);
        picker.visible_height = 5;
        picker.page_down();
        picker.page_down();
        picker.page_down(); // Should clamp to last
        assert_eq!(picker.selected_index(), picker.filtered_count() - 1);
    }

    #[test]
    fn move_to_last_empty_is_noop() {
        let mut picker = FilePicker::new(Vec::new());
        picker.move_to_last(); // Should not panic
        assert_eq!(picker.selected_index(), 0);
    }

    #[test]
    fn truncate_str_empty() {
        assert_eq!(truncate_str("", 10), "");
        assert_eq!(truncate_str("", 0), "");
    }

    #[test]
    fn truncate_str_zero_width() {
        assert_eq!(truncate_str("hello", 0), "");
    }

    #[test]
    fn text_width_simple() {
        assert_eq!(text_width("hello"), 5);
        assert_eq!(text_width(""), 0);
    }

    #[test]
    fn rebuild_filter_clamps_selected() {
        let mut picker = FilePicker::new(sample_entries());
        // Move to last
        picker.move_to_last();
        let old_sel = picker.selected_index();
        // Apply restrictive filter that removes most entries
        picker.set_filter(FilePickerFilter {
            allowed_extensions: vec!["toml".into()],
            show_hidden: false,
        });
        // Selected should be clamped to valid range
        assert!(picker.selected_index() < picker.filtered_count());
        assert!(picker.selected_index() <= old_sel);
    }

    #[test]
    fn render_updates_visible_height() {
        let mut picker = FilePicker::new(sample_entries());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 15, &mut pool);
        let area = Rect::from_size(40, 15);
        picker.render(area, &mut frame);
        // visible_height should be area.height - 1 (path line)
        assert_eq!(picker.visible_height, 14);
    }

    #[test]
    fn render_path_displayed() {
        let mut picker = FilePicker::new(sample_entries());
        picker.set_path("/usr/local");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        picker.render(Rect::from_size(40, 10), &mut frame);

        // Check first row contains path characters
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('/'));
    }
}

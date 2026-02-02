#![forbid(unsafe_code)]

//! File picker widget for browsing and selecting files.
//!
//! Provides a TUI file browser with keyboard navigation. The widget
//! renders a directory listing with cursor selection and supports
//! entering subdirectories and navigating back to parents.
//!
//! # Architecture
//!
//! - [`FilePicker`] — stateless configuration and rendering
//! - [`FilePickerState`] — mutable navigation state (cursor, directory, entries)
//! - [`DirEntry`] — a single file/directory entry
//!
//! The widget uses [`StatefulWidget`] so the application owns the state
//! and can read the selected path.

use crate::{StatefulWidget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use std::path::{Path, PathBuf};

/// A single entry in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Display name.
    pub name: String,
    /// Full path.
    pub path: PathBuf,
    /// Whether this is a directory.
    pub is_dir: bool,
}

impl DirEntry {
    /// Create a directory entry.
    pub fn dir(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            is_dir: true,
        }
    }

    /// Create a file entry.
    pub fn file(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            is_dir: false,
        }
    }
}

/// Mutable state for the file picker.
#[derive(Debug, Clone)]
pub struct FilePickerState {
    /// Current directory being displayed.
    pub current_dir: PathBuf,
    /// Directory entries (sorted: dirs first, then files).
    pub entries: Vec<DirEntry>,
    /// Currently highlighted index.
    pub cursor: usize,
    /// Scroll offset (first visible row).
    pub offset: usize,
    /// The selected/confirmed path (set when user presses enter on a file).
    pub selected: Option<PathBuf>,
    /// Navigation history for going back.
    history: Vec<(PathBuf, usize)>,
}

impl FilePickerState {
    /// Create a new state with the given directory and entries.
    pub fn new(current_dir: PathBuf, entries: Vec<DirEntry>) -> Self {
        Self {
            current_dir,
            entries,
            cursor: 0,
            offset: 0,
            selected: None,
            history: Vec::new(),
        }
    }

    /// Create state from a directory path by reading the filesystem.
    ///
    /// Sorts entries: directories first (alphabetical), then files (alphabetical).
    /// Returns an error if the directory cannot be read.
    pub fn from_path(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let entries = read_directory(&path)?;
        Ok(Self::new(path, entries))
    }

    /// Move cursor up.
    pub fn cursor_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// Move cursor down.
    pub fn cursor_down(&mut self) {
        if !self.entries.is_empty() && self.cursor < self.entries.len() - 1 {
            self.cursor += 1;
        }
    }

    /// Move cursor to the first entry.
    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to the last entry.
    pub fn cursor_end(&mut self) {
        if !self.entries.is_empty() {
            self.cursor = self.entries.len() - 1;
        }
    }

    /// Page up by `page_size` rows.
    pub fn page_up(&mut self, page_size: usize) {
        self.cursor = self.cursor.saturating_sub(page_size);
    }

    /// Page down by `page_size` rows.
    pub fn page_down(&mut self, page_size: usize) {
        if !self.entries.is_empty() {
            self.cursor = (self.cursor + page_size).min(self.entries.len() - 1);
        }
    }

    /// Enter the selected directory (if cursor is on a directory).
    ///
    /// Returns `Ok(true)` if navigation succeeded, `Ok(false)` if cursor is on a file,
    /// or an error if the directory cannot be read.
    pub fn enter(&mut self) -> std::io::Result<bool> {
        let Some(entry) = self.entries.get(self.cursor) else {
            return Ok(false);
        };

        if !entry.is_dir {
            // Select the file
            self.selected = Some(entry.path.clone());
            return Ok(false);
        }

        let new_dir = entry.path.clone();
        let new_entries = read_directory(&new_dir)?;

        self.history.push((self.current_dir.clone(), self.cursor));
        self.current_dir = new_dir;
        self.entries = new_entries;
        self.cursor = 0;
        self.offset = 0;
        Ok(true)
    }

    /// Go back to the parent directory.
    ///
    /// Returns `Ok(true)` if navigation succeeded.
    pub fn go_back(&mut self) -> std::io::Result<bool> {
        if let Some((prev_dir, prev_cursor)) = self.history.pop() {
            let entries = read_directory(&prev_dir)?;
            self.current_dir = prev_dir;
            self.entries = entries;
            self.cursor = prev_cursor.min(self.entries.len().saturating_sub(1));
            self.offset = 0;
            return Ok(true);
        }

        // No history — try parent directory
        if let Some(parent) = self.current_dir.parent().map(|p| p.to_path_buf()) {
            let entries = read_directory(&parent)?;
            self.history.push((self.current_dir.clone(), self.cursor));
            self.current_dir = parent;
            self.entries = entries;
            self.cursor = 0;
            self.offset = 0;
            return Ok(true);
        }

        Ok(false)
    }

    /// Ensure scroll offset keeps cursor visible for the given viewport height.
    fn adjust_scroll(&mut self, visible_rows: usize) {
        if visible_rows == 0 {
            return;
        }
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + visible_rows {
            self.offset = self.cursor + 1 - visible_rows;
        }
    }
}

/// Read a directory and return sorted entries (dirs first, then files).
fn read_directory(path: &Path) -> std::io::Result<Vec<DirEntry>> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry.file_type()?;
        let full_path = entry.path();

        if file_type.is_dir() {
            dirs.push(DirEntry::dir(name, full_path));
        } else {
            files.push(DirEntry::file(name, full_path));
        }
    }

    dirs.sort_by_key(|a| a.name.to_lowercase());
    files.sort_by_key(|a| a.name.to_lowercase());

    dirs.extend(files);
    Ok(dirs)
}

/// Configuration and rendering for the file picker widget.
///
/// # Example
///
/// ```ignore
/// let picker = FilePicker::new()
///     .dir_style(Style::new().fg(PackedRgba::rgb(100, 100, 255)))
///     .cursor_style(Style::new().bold());
///
/// let mut state = FilePickerState::from_path(".").unwrap();
/// picker.render(area, &mut frame, &mut state);
/// ```
#[derive(Debug, Clone)]
pub struct FilePicker {
    /// Style for directory entries.
    pub dir_style: Style,
    /// Style for file entries.
    pub file_style: Style,
    /// Style for the cursor row.
    pub cursor_style: Style,
    /// Style for the header (current directory).
    pub header_style: Style,
    /// Whether to show the current directory path as a header.
    pub show_header: bool,
    /// Prefix for directory entries.
    pub dir_prefix: &'static str,
    /// Prefix for file entries.
    pub file_prefix: &'static str,
}

impl Default for FilePicker {
    fn default() -> Self {
        Self {
            dir_style: Style::default(),
            file_style: Style::default(),
            cursor_style: Style::default(),
            header_style: Style::default(),
            show_header: true,
            dir_prefix: "📁 ",
            file_prefix: "  ",
        }
    }
}

impl FilePicker {
    /// Create a new file picker with default styles.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the directory entry style.
    pub fn dir_style(mut self, style: Style) -> Self {
        self.dir_style = style;
        self
    }

    /// Set the file entry style.
    pub fn file_style(mut self, style: Style) -> Self {
        self.file_style = style;
        self
    }

    /// Set the cursor (highlight) style.
    pub fn cursor_style(mut self, style: Style) -> Self {
        self.cursor_style = style;
        self
    }

    /// Set the header style.
    pub fn header_style(mut self, style: Style) -> Self {
        self.header_style = style;
        self
    }

    /// Toggle header display.
    pub fn show_header(mut self, show: bool) -> Self {
        self.show_header = show;
        self
    }
}

impl StatefulWidget for FilePicker {
    type State = FilePickerState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if area.is_empty() {
            return;
        }

        let mut y = area.y;
        let max_y = area.bottom();

        // Header: current directory path
        if self.show_header && y < max_y {
            let header = state.current_dir.to_string_lossy();
            draw_text_span(frame, area.x, y, &header, self.header_style, area.right());
            y += 1;
        }

        if y >= max_y {
            return;
        }

        let visible_rows = (max_y - y) as usize;
        state.adjust_scroll(visible_rows);

        if state.entries.is_empty() {
            draw_text_span(
                frame,
                area.x,
                y,
                "(empty directory)",
                self.file_style,
                area.right(),
            );
            return;
        }

        let end_idx = (state.offset + visible_rows).min(state.entries.len());
        for (i, entry) in state.entries[state.offset..end_idx].iter().enumerate() {
            if y >= max_y {
                break;
            }

            let actual_idx = state.offset + i;
            let is_cursor = actual_idx == state.cursor;

            let prefix = if entry.is_dir {
                self.dir_prefix
            } else {
                self.file_prefix
            };

            let base_style = if entry.is_dir {
                self.dir_style
            } else {
                self.file_style
            };

            let style = if is_cursor {
                self.cursor_style.merge(&base_style)
            } else {
                base_style
            };

            // Draw cursor indicator
            let mut x = area.x;
            if is_cursor {
                draw_text_span(frame, x, y, "> ", self.cursor_style, area.right());
                x += 2;
            } else {
                x += 2;
            }

            // Draw prefix + name
            x = draw_text_span(frame, x, y, prefix, style, area.right());
            draw_text_span(frame, x, y, &entry.name, style, area.right());

            y += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    fn buf_to_lines(buf: &ftui_render::buffer::Buffer) -> Vec<String> {
        let mut lines = Vec::new();
        for y in 0..buf.height() {
            let mut row = String::with_capacity(buf.width() as usize);
            for x in 0..buf.width() {
                let ch = buf
                    .get(x, y)
                    .and_then(|c| c.content.as_char())
                    .unwrap_or(' ');
                row.push(ch);
            }
            lines.push(row);
        }
        lines
    }

    fn make_entries() -> Vec<DirEntry> {
        vec![
            DirEntry::dir("docs", "/tmp/docs"),
            DirEntry::dir("src", "/tmp/src"),
            DirEntry::file("README.md", "/tmp/README.md"),
            DirEntry::file("main.rs", "/tmp/main.rs"),
        ]
    }

    fn make_state() -> FilePickerState {
        FilePickerState::new(PathBuf::from("/tmp"), make_entries())
    }

    #[test]
    fn dir_entry_constructors() {
        let d = DirEntry::dir("src", "/src");
        assert!(d.is_dir);
        assert_eq!(d.name, "src");

        let f = DirEntry::file("main.rs", "/main.rs");
        assert!(!f.is_dir);
        assert_eq!(f.name, "main.rs");
    }

    #[test]
    fn state_cursor_movement() {
        let mut state = make_state();
        assert_eq!(state.cursor, 0);

        state.cursor_down();
        assert_eq!(state.cursor, 1);

        state.cursor_down();
        state.cursor_down();
        assert_eq!(state.cursor, 3);

        // Can't go past end
        state.cursor_down();
        assert_eq!(state.cursor, 3);

        state.cursor_up();
        assert_eq!(state.cursor, 2);

        state.cursor_home();
        assert_eq!(state.cursor, 0);

        // Can't go before start
        state.cursor_up();
        assert_eq!(state.cursor, 0);

        state.cursor_end();
        assert_eq!(state.cursor, 3);
    }

    #[test]
    fn state_page_navigation() {
        let entries: Vec<DirEntry> = (0..20)
            .map(|i| DirEntry::file(format!("file{i}.txt"), format!("/tmp/file{i}.txt")))
            .collect();
        let mut state = FilePickerState::new(PathBuf::from("/tmp"), entries);

        state.page_down(5);
        assert_eq!(state.cursor, 5);

        state.page_down(5);
        assert_eq!(state.cursor, 10);

        state.page_up(3);
        assert_eq!(state.cursor, 7);

        state.page_up(100);
        assert_eq!(state.cursor, 0);

        state.page_down(100);
        assert_eq!(state.cursor, 19);
    }

    #[test]
    fn state_empty_entries() {
        let mut state = FilePickerState::new(PathBuf::from("/tmp"), vec![]);
        state.cursor_down(); // should not panic
        state.cursor_up();
        state.cursor_end();
        state.cursor_home();
        state.page_down(10);
        state.page_up(10);
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn adjust_scroll_keeps_cursor_visible() {
        let entries: Vec<DirEntry> = (0..20)
            .map(|i| DirEntry::file(format!("f{i}"), format!("/f{i}")))
            .collect();
        let mut state = FilePickerState::new(PathBuf::from("/"), entries);

        state.cursor = 15;
        state.adjust_scroll(5);
        // cursor=15 should be visible in a 5-row window
        assert!(state.offset <= 15);
        assert!(state.offset + 5 > 15);

        state.cursor = 0;
        state.adjust_scroll(5);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn render_basic() {
        let picker = FilePicker::new().show_header(false);
        let mut state = make_state();

        let area = Rect::new(0, 0, 30, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);

        picker.render(area, &mut frame, &mut state);
        let lines = buf_to_lines(&frame.buffer);

        // First entry should have cursor indicator "> "
        assert!(lines[0].starts_with("> "));
        // Should contain directory and file names
        let all_text = lines.join("\n");
        assert!(all_text.contains("docs"));
        assert!(all_text.contains("src"));
        assert!(all_text.contains("README.md"));
        assert!(all_text.contains("main.rs"));
    }

    #[test]
    fn render_with_header() {
        let picker = FilePicker::new().show_header(true);
        let mut state = make_state();

        let area = Rect::new(0, 0, 30, 6);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 6, &mut pool);

        picker.render(area, &mut frame, &mut state);
        let lines = buf_to_lines(&frame.buffer);

        // First line should be the directory path
        assert!(lines[0].starts_with("/tmp"));
    }

    #[test]
    fn render_empty_directory() {
        let picker = FilePicker::new().show_header(false);
        let mut state = FilePickerState::new(PathBuf::from("/empty"), vec![]);

        let area = Rect::new(0, 0, 30, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 3, &mut pool);

        picker.render(area, &mut frame, &mut state);
        let lines = buf_to_lines(&frame.buffer);

        assert!(lines[0].contains("empty directory"));
    }

    #[test]
    fn render_scrolling() {
        let entries: Vec<DirEntry> = (0..20)
            .map(|i| DirEntry::file(format!("file{i:02}.txt"), format!("/tmp/file{i:02}.txt")))
            .collect();
        let mut state = FilePickerState::new(PathBuf::from("/tmp"), entries);
        let picker = FilePicker::new().show_header(false);

        // Move cursor to item 15, viewport is 5 rows
        state.cursor = 15;
        let area = Rect::new(0, 0, 30, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);

        picker.render(area, &mut frame, &mut state);
        let lines = buf_to_lines(&frame.buffer);

        // file15 should be visible (with cursor)
        let all_text = lines.join("\n");
        assert!(all_text.contains("file15"));
    }

    #[test]
    fn cursor_style_applied_to_selected_row() {
        use ftui_render::cell::PackedRgba;

        let picker = FilePicker::new()
            .show_header(false)
            .cursor_style(Style::new().fg(PackedRgba::rgb(255, 0, 0)));
        let mut state = make_state();
        state.cursor = 1; // "src"

        let area = Rect::new(0, 0, 30, 4);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 4, &mut pool);

        picker.render(area, &mut frame, &mut state);

        // The cursor row (y=1) should have the cursor indicator
        let lines = buf_to_lines(&frame.buffer);
        assert!(lines[1].starts_with("> "));
        // Non-cursor rows should not
        assert!(!lines[0].starts_with("> "));
    }

    #[test]
    fn selected_set_on_file_entry() {
        let mut state = make_state();
        state.cursor = 2; // README.md (a file)

        // enter() on a file should set selected
        let result = state.enter();
        assert!(result.is_ok());
        assert_eq!(state.selected, Some(PathBuf::from("/tmp/README.md")));
    }
}

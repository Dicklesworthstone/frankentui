#![forbid(unsafe_code)]

//! File Browser screen — file system navigation and preview.
//!
//! Demonstrates:
//! - `FilePicker` with simulated directory entries
//! - `Tree` widget for directory structure
//! - `SyntaxHighlighter` for file preview
//! - `filesize::decimal()` for human-readable sizes

use std::cell::Cell;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_extras::filepicker::{FileEntry, FileKind, FilePicker, FilePickerFilter, FilePickerStyle};
use ftui_extras::filesize;
use ftui_extras::syntax::SyntaxHighlighter;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::{display_width, grapheme_width, graphemes};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::tree::{Tree, TreeGuides, TreeNode};

use super::{HelpEntry, Screen};
use crate::theme;

/// Which panel has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    FileTree,
    FilePicker,
    Preview,
}

impl Panel {
    fn next(self) -> Self {
        match self {
            Self::FileTree => Self::FilePicker,
            Self::FilePicker => Self::Preview,
            Self::Preview => Self::FileTree,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::FileTree => Self::Preview,
            Self::FilePicker => Self::FileTree,
            Self::Preview => Self::FilePicker,
        }
    }
}

/// Sample file content for preview.
const SAMPLE_RUST: &str = r#"use std::io;

fn main() -> io::Result<()> {
    let names = vec!["Alice", "Bob", "Charlie"];
    for name in &names {
        println!("Hello, {name}!");
    }
    Ok(())
}
"#;

const SAMPLE_PYTHON: &str = r#"#!/usr/bin/env python3
"""A simple Python script."""

def greet(name: str) -> str:
    return f"Hello, {name}!"

if __name__ == "__main__":
    for name in ["Alice", "Bob", "Charlie"]:
        print(greet(name))
"#;

const SAMPLE_TOML: &str = r#"[package]
name = "my-project"
version = "0.1.0"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
"#;

const SAMPLE_JSON: &str = r#"{
  "name": "ftui-demo",
  "version": "0.1.0",
  "description": "TUI demo showcase",
  "keywords": ["terminal", "ui", "tui"]
}
"#;

pub struct FileBrowser {
    focus: Panel,
    picker: FilePicker,
    highlighter: SyntaxHighlighter,
    preview_scroll: usize,
    show_hidden: bool,
    entries: Vec<FileEntry>,
    layout_tree: Cell<Rect>,
    layout_picker: Cell<Rect>,
    layout_preview: Cell<Rect>,
}

impl Default for FileBrowser {
    fn default() -> Self {
        Self::new()
    }
}

impl FileBrowser {
    pub fn new() -> Self {
        let entries = simulated_entries();
        let mut picker = FilePicker::new(entries.clone());
        picker.set_path("/home/user/projects/my-app");
        picker.set_style(Self::picker_style());
        picker.set_filter(FilePickerFilter {
            allowed_extensions: vec![],
            show_hidden: false,
        });

        let mut highlighter = SyntaxHighlighter::new();
        highlighter.set_theme(theme::syntax_theme());

        Self {
            focus: Panel::FilePicker,
            picker,
            highlighter,
            preview_scroll: 0,
            show_hidden: false,
            entries,
            layout_tree: Cell::new(Rect::default()),
            layout_picker: Cell::new(Rect::default()),
            layout_preview: Cell::new(Rect::default()),
        }
    }

    pub fn apply_theme(&mut self) {
        self.picker.set_style(Self::picker_style());
        self.highlighter.set_theme(theme::syntax_theme());
    }

    fn set_entries(&mut self, entries: Vec<FileEntry>) {
        self.entries = entries.clone();
        self.picker.set_entries(entries);
    }

    fn visible_entries(&self) -> Vec<&FileEntry> {
        let filter = FilePickerFilter {
            allowed_extensions: vec![],
            show_hidden: self.show_hidden,
        };
        self.entries.iter().filter(|e| filter.matches(e)).collect()
    }

    fn current_path(&self) -> &str {
        self.picker.path()
    }

    fn enter_selected_directory(&mut self) {
        let Some(entry) = self.picker.selected_entry() else {
            return;
        };
        if !entry.is_dir() {
            return;
        }
        let base = self.current_path();
        let new_path = join_path(base, &entry.name);
        self.picker.set_path(new_path.clone());
        self.set_entries(simulated_entries_for(&new_path));
        self.preview_scroll = 0;
    }

    fn go_up(&mut self) {
        let path = self.current_path().to_string();
        let parent = parent_path(&path).to_string();
        if parent == path {
            return;
        }
        let entries = simulated_entries_for(&parent);
        self.picker.set_path(parent);
        self.set_entries(entries);
        self.preview_scroll = 0;
    }

    fn picker_style() -> FilePickerStyle {
        FilePickerStyle {
            selected: Style::new()
                .fg(theme::fg::PRIMARY)
                .bg(theme::alpha::HIGHLIGHT),
            directory: Style::new().fg(theme::accent::INFO),
            file: Style::new().fg(theme::fg::PRIMARY),
            symlink: Style::new().fg(theme::accent::SECONDARY),
            path: Style::new().fg(theme::fg::SECONDARY),
        }
    }

    fn current_preview(&self) -> (String, &str) {
        let entry = self.picker.selected_entry();
        if let Some(entry) = entry {
            if entry.is_dir() {
                let mut listing = String::from("Directory contents:\n");
                for item in self.visible_entries().iter().take(8) {
                    let icon = icons::entry_icon(item);
                    listing.push_str(&format!("  {icon} {}\n", item.name));
                }
                return (listing, "plain");
            }

            let name = entry.name.as_str();
            let ext = name.rsplit('.').next().unwrap_or("");
            let meta = match ext {
                "png" | "jpg" | "jpeg" | "gif" => {
                    "Image preview (metadata only)\nResolution: 1920x1080\nColor: sRGB"
                }
                "mp3" | "wav" => "Audio preview\nDuration: 3:42\nCodec: AAC",
                "mp4" | "mov" => "Video preview\nDuration: 1:12\nCodec: H.264",
                _ => "",
            };
            if !meta.is_empty() {
                return (meta.to_string(), "plain");
            }
        }

        match entry.map(|e| e.name.as_str()) {
            Some("main.rs") | Some("lib.rs") | Some("mod.rs") => (SAMPLE_RUST.to_string(), "rust"),
            Some("app.py") | Some("test_app.py") | Some("scripts.py") => {
                (SAMPLE_PYTHON.to_string(), "python")
            }
            Some("Cargo.toml") | Some("config.toml") | Some("settings.toml") => {
                (SAMPLE_TOML.to_string(), "toml")
            }
            Some("package.json") | Some("data.json") => (SAMPLE_JSON.to_string(), "json"),
            _ => ("(no preview available)".to_string(), "plain"),
        }
    }

    fn render_tree_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == Panel::FileTree,
            theme::screen_accent::FILE_BROWSER,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Project Tree")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let tree = build_project_tree();
        let tree_widget = Tree::new(tree)
            .with_guides(TreeGuides::Unicode)
            .with_show_root(true)
            .with_label_style(Style::new().fg(theme::fg::PRIMARY))
            .with_root_style(Style::new().fg(theme::accent::INFO))
            .with_guide_style(Style::new().fg(theme::fg::MUTED));

        tree_widget.render(inner, frame);
    }

    fn render_picker_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == Panel::FilePicker,
            theme::screen_accent::FILE_BROWSER,
        );

        let hidden_label = if self.show_hidden { "+hidden" } else { "" };
        let title = format!(
            "Files ({} items{})",
            self.picker.filtered_count(),
            hidden_label,
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(&title)
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }
        let content_width = inner.width.max(1);

        // FilePicker::render needs &mut self, but view() has &self.
        // We work around by re-rendering manually with Paragraph lines.
        let selected_idx = self.picker.selected_index();
        let entries = self.visible_entries();
        let count = entries.len();

        // We'll render a simple list view since FilePicker::render needs &mut.
        let visible = inner.height as usize;
        let scroll = if selected_idx >= visible {
            selected_idx - visible + 1
        } else {
            0
        };

        // Render breadcrumbs
        let path_area = Rect::new(inner.x, inner.y, inner.width, 1);
        Paragraph::new(fit_to_width(
            &format_breadcrumbs(self.current_path()),
            content_width,
        ))
        .style(
            Style::new()
                .fg(theme::fg::SECONDARY)
                .bg(theme::alpha::SURFACE),
        )
        .render(path_area, frame);

        // Column header
        let header_area = Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 1);
        Paragraph::new(format_entry_header(content_width))
            .style(Style::new().fg(theme::fg::MUTED))
            .render(header_area, frame);

        // Render entries
        let list_area = Rect::new(
            inner.x,
            inner.y.saturating_add(2),
            inner.width,
            inner.height.saturating_sub(2),
        );

        let mut rendered_rows = 0u16;
        for (row, i) in (scroll..count.min(scroll + list_area.height as usize)).enumerate() {
            let Some(entry) = entries.get(i) else {
                break;
            };
            let y = list_area.y.saturating_add(row as u16);
            if y >= list_area.bottom() {
                break;
            }

            let line = format_entry_line(entry, content_width);

            let style = if i == selected_idx {
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::HIGHLIGHT)
            } else if entry.is_dir() {
                Style::new().fg(theme::accent::INFO)
            } else if entry.kind == FileKind::Symlink {
                Style::new().fg(theme::accent::SECONDARY)
            } else {
                Style::new().fg(theme::fg::PRIMARY)
            };

            let row_area = Rect::new(list_area.x, y, list_area.width, 1);
            Paragraph::new(&*line).style(style).render(row_area, frame);
            rendered_rows = rendered_rows.saturating_add(1);
        }

        if rendered_rows < list_area.height {
            let blank = " ".repeat(list_area.width as usize);
            for row in rendered_rows..list_area.height {
                let y = list_area.y.saturating_add(row);
                if y >= list_area.bottom() {
                    break;
                }
                let row_area = Rect::new(list_area.x, y, list_area.width, 1);
                Paragraph::new(&*blank).render(row_area, frame);
            }
        }
    }

    fn render_preview_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == Panel::Preview,
            theme::screen_accent::FILE_BROWSER,
        );

        let (entry_name, entry_icon) = self
            .picker
            .selected_entry()
            .map(|e| (e.name.as_str(), icons::entry_icon(e)))
            .unwrap_or(("(none)", ""));
        let title = if entry_icon.is_empty() {
            format!("Preview: {entry_name}")
        } else {
            format!("Preview: {entry_icon} {entry_name}")
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(&title)
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let header_body = Flex::vertical()
            .constraints([Constraint::Fixed(3), Constraint::Min(1)])
            .split(inner);

        let meta = preview_metadata(self.current_path(), self.picker.selected_entry());
        Paragraph::new(meta)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(header_body[0], frame);

        let (content, lang) = self.current_preview();
        let highlighted = self.highlighter.highlight(&content, lang);
        Paragraph::new(highlighted)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .scroll((self.preview_scroll as u16, 0))
            .render(header_body[1], frame);
    }
}

fn simulated_entries() -> Vec<FileEntry> {
    let mut entries = vec![
        FileEntry::new("src", FileKind::Directory),
        FileEntry::new("tests", FileKind::Directory),
        FileEntry::new("assets", FileKind::Directory),
        FileEntry::new("docs", FileKind::Directory),
        FileEntry::new(".git", FileKind::Directory),
        FileEntry::new("target", FileKind::Directory),
        FileEntry::new("main.rs", FileKind::File).with_size(2048),
        FileEntry::new("lib.rs", FileKind::File).with_size(4096),
        FileEntry::new("mod.rs", FileKind::File).with_size(512),
        FileEntry::new("app.py", FileKind::File).with_size(1536),
        FileEntry::new("test_app.py", FileKind::File).with_size(892),
        FileEntry::new("scripts.py", FileKind::File).with_size(1638),
        FileEntry::new("Cargo.toml", FileKind::File).with_size(512),
        FileEntry::new("config.toml", FileKind::File).with_size(256),
        FileEntry::new("settings.toml", FileKind::File).with_size(256),
        FileEntry::new("package.json", FileKind::File).with_size(384),
        FileEntry::new("data.json", FileKind::File).with_size(10240),
        FileEntry::new("README.md", FileKind::File).with_size(3072),
        FileEntry::new("logo.png", FileKind::File).with_size(424_128),
        FileEntry::new("cover.jpg", FileKind::File).with_size(512_000),
        FileEntry::new("demo.mp4", FileKind::File).with_size(12_582_912),
        FileEntry::new("song.mp3", FileKind::File).with_size(3_402_112),
        FileEntry::new(".gitignore", FileKind::File).with_size(128),
        FileEntry::new(".env", FileKind::File).with_size(64),
        FileEntry::new("build.sh", FileKind::Symlink).with_size(48),
    ];
    sort_entries(&mut entries);
    entries
}

fn simulated_entries_for(path: &str) -> Vec<FileEntry> {
    if path.ends_with("/src") {
        let mut entries = vec![
            FileEntry::new("main.rs", FileKind::File).with_size(4096),
            FileEntry::new("lib.rs", FileKind::File).with_size(5120),
            FileEntry::new("mod.rs", FileKind::File).with_size(1024),
            FileEntry::new("models", FileKind::Directory),
        ];
        sort_entries(&mut entries);
        return entries;
    }
    if path.ends_with("/assets") {
        let mut entries = vec![
            FileEntry::new("logo.png", FileKind::File).with_size(424_128),
            FileEntry::new("cover.jpg", FileKind::File).with_size(512_000),
            FileEntry::new("song.mp3", FileKind::File).with_size(3_402_112),
            FileEntry::new("demo.mp4", FileKind::File).with_size(12_582_912),
        ];
        sort_entries(&mut entries);
        return entries;
    }
    simulated_entries()
}

fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        let kind_rank = |entry: &FileEntry| match entry.kind {
            FileKind::Directory => 0,
            FileKind::Symlink => 1,
            FileKind::File => 2,
        };
        let rank_cmp = kind_rank(a).cmp(&kind_rank(b));
        if rank_cmp != std::cmp::Ordering::Equal {
            return rank_cmp;
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });
}

fn file_permissions(entry: &FileEntry) -> &'static str {
    match entry.kind {
        FileKind::Directory => "drwxr-xr-x",
        FileKind::Symlink => "lrwxr-xr-x",
        FileKind::File => {
            let name = entry.name.as_str();
            if name.ends_with(".sh") || name.ends_with(".py") {
                "-rwxr-xr-x"
            } else {
                "-rw-r--r--"
            }
        }
    }
}

const ICON_COL_WIDTH: usize = 2;
const PERMS_COL_WIDTH: usize = 10;
const SIZE_COL_WIDTH: usize = 10;
const GAP_ICON_NAME: usize = 1;
const GAP_NAME_PERMS: usize = 1;
const GAP_PERMS_SIZE: usize = 2;

fn format_entry_header(width: u16) -> String {
    let reserved = ICON_COL_WIDTH
        + PERMS_COL_WIDTH
        + SIZE_COL_WIDTH
        + GAP_ICON_NAME
        + GAP_NAME_PERMS
        + GAP_PERMS_SIZE;
    let name_width = width.saturating_sub(reserved as u16).saturating_sub(1) as usize;

    let h_icon = " ".repeat(ICON_COL_WIDTH);
    let h_name = pad_to_width("Name", name_width);
    let h_perms = pad_to_width("Perms", PERMS_COL_WIDTH);
    let h_size = format!("{:>width$}", "Size", width = SIZE_COL_WIDTH);

    let line = format!("{h_icon} {h_name} {h_perms}  {h_size}");
    fit_to_width(&line, width)
}

fn format_entry_line(entry: &FileEntry, width: u16) -> String {
    let icon = icons::entry_icon(entry);
    let perms = file_permissions(entry);
    let size_str = entry
        .size
        .map(filesize::decimal)
        .unwrap_or_else(|| "--".into());

    let icon_padded = pad_to_width(icon, ICON_COL_WIDTH);
    let perms_padded = pad_to_width(perms, PERMS_COL_WIDTH);
    let size_padded = format!("{:>width$}", size_str, width = SIZE_COL_WIDTH);

    let reserved = ICON_COL_WIDTH
        + PERMS_COL_WIDTH
        + SIZE_COL_WIDTH
        + GAP_ICON_NAME
        + GAP_NAME_PERMS
        + GAP_PERMS_SIZE;
    let name_width = width.saturating_sub(reserved as u16).saturating_sub(1) as usize;
    let name = pad_to_width(&entry.name, name_width);

    let line = format!("{icon_padded} {name} {perms_padded}  {size_padded}");
    fit_to_width(&line, width)
}

fn text_width(text: &str) -> usize {
    display_width(text)
}

fn pad_to_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in graphemes(text) {
        let g_width = grapheme_width(grapheme);
        if used + g_width > width {
            break;
        }
        out.push_str(grapheme);
        used += g_width;
    }
    if used < width {
        out.push_str(&" ".repeat(width - used));
    }
    out
}

fn fit_to_width(text: &str, width: u16) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for grapheme in graphemes(text) {
        let g_width = grapheme_width(grapheme);
        if used + g_width > width as usize {
            break;
        }
        out.push_str(grapheme);
        used += g_width;
    }
    if used < width as usize {
        out.push_str(&" ".repeat(width as usize - used));
    }
    out
}

fn format_breadcrumbs(path: &str) -> String {
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let mut out = String::new();
    if path.starts_with('/') {
        out.push_str("🏠 /");
    }
    if let Some(first) = parts.next() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(icons::directory_icon());
        out.push(' ');
        out.push_str(first);
    }
    for part in parts {
        out.push(' ');
        out.push_str(theme::icons::ARROW_RIGHT);
        out.push(' ');
        out.push_str(icons::directory_icon());
        out.push(' ');
        out.push_str(part);
    }
    out
}

fn join_path(base: &str, child: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{child}")
    } else {
        format!("{base}/{child}")
    }
}

fn parent_path(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if let Some(idx) = trimmed.rfind('/') {
        if idx == 0 { "/" } else { &trimmed[..idx] }
    } else {
        path
    }
}

fn preview_metadata(path: &str, entry: Option<&FileEntry>) -> String {
    match entry {
        Some(entry) => {
            let kind = match entry.kind {
                FileKind::Directory => "Directory",
                FileKind::Symlink => "Symlink",
                FileKind::File => "File",
            };
            let size = entry
                .size
                .map(filesize::decimal)
                .unwrap_or_else(|| "--".into());
            let perms = file_permissions(entry);
            format!("{kind}  •  Size: {size}  •  Perms: {perms}\nPath: {path}\nPreview:",)
        }
        None => format!("No selection\nPath: {path}\nPreview:"),
    }
}

fn tree_dir_label(name: &str) -> String {
    format!("{} {}", icons::directory_icon(), name)
}

fn tree_file_label(name: &str) -> String {
    format!("{} {}", icons::file_icon(name), name)
}

mod icons {
    use super::{FileEntry, FileKind};

    use crate::theme;

    #[inline]
    pub(crate) fn use_emoji() -> bool {
        // Demo requirement: always show emoji in the file browser.
        true
    }

    pub fn directory_icon() -> &'static str {
        if use_emoji() {
            theme::icons::FOLDER_CLOSED
        } else {
            "DR"
        }
    }

    pub fn symlink_icon() -> &'static str {
        if use_emoji() {
            theme::icons::ACTION_LINKED
        } else {
            "SY"
        }
    }

    pub fn file_icon(name: &str) -> &'static str {
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        if use_emoji() {
            match ext.as_str() {
                "rs" => "🦀",
                "py" => "🐍",
                "js" => "📜",
                "ts" => "📝",
                "md" => theme::icons::TYPE_DOCS,
                "json" | "toml" | "yaml" | "yml" | "env" | "gitignore" => "⚙️",
                "png" | "jpg" | "jpeg" | "gif" | "svg" => "🖼️",
                "mp3" | "wav" | "flac" => "🎵",
                "mp4" | "mov" | "mkv" => "🎬",
                "sh" | "bash" => "⚡️",
                _ => theme::icons::FILE,
            }
        } else {
            match ext.as_str() {
                "rs" => "RS",
                "py" => "PY",
                "js" => "JS",
                "ts" => "TS",
                "md" => "MD",
                "json" => "JS",
                "toml" => "TM",
                "yaml" | "yml" => "YM",
                "env" | "gitignore" => "CF",
                "png" | "jpg" | "jpeg" | "gif" | "svg" => "IM",
                "mp3" | "wav" | "flac" => "AU",
                "mp4" | "mov" | "mkv" => "VI",
                "sh" | "bash" => "SH",
                _ => "FI",
            }
        }
    }

    pub fn entry_icon(entry: &FileEntry) -> &'static str {
        match entry.kind {
            FileKind::Directory => directory_icon(),
            FileKind::Symlink => symlink_icon(),
            FileKind::File => file_icon(&entry.name),
        }
    }
}

fn build_project_tree() -> TreeNode {
    TreeNode::new(tree_dir_label("my-app")).with_children(vec![
        TreeNode::new(tree_dir_label("src")).with_children(vec![
            TreeNode::new(tree_file_label("main.rs")),
            TreeNode::new(tree_file_label("lib.rs")),
            TreeNode::new(tree_dir_label("models")).with_children(vec![
                TreeNode::new(tree_file_label("user.rs")),
                TreeNode::new(tree_file_label("post.rs")),
            ]),
        ]),
        TreeNode::new(tree_dir_label("tests")).with_children(vec![
            TreeNode::new(tree_file_label("test_app.py")),
            TreeNode::new(tree_file_label("integration.rs")),
        ]),
        TreeNode::new(tree_dir_label("docs")).with_children(vec![
            TreeNode::new(tree_file_label("README.md")),
            TreeNode::new(tree_file_label("API.md")),
        ]),
        TreeNode::new(tree_file_label("Cargo.toml")),
        TreeNode::new(tree_file_label("package.json")),
        TreeNode::new(tree_file_label(".gitignore")),
    ])
}

impl Screen for FileBrowser {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let tree = self.layout_tree.get();
                let picker = self.layout_picker.get();
                let preview = self.layout_preview.get();
                if tree.contains(mouse.x, mouse.y) {
                    self.focus = Panel::FileTree;
                } else if picker.contains(mouse.x, mouse.y) {
                    self.focus = Panel::FilePicker;
                } else if preview.contains(mouse.x, mouse.y) {
                    self.focus = Panel::Preview;
                }
            }
            return Cmd::None;
        }

        // Panel switching (Ctrl+Left/Right or h/l)
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            let next_panel = ((modifiers.contains(Modifiers::CTRL) || modifiers.is_empty())
                && *code == KeyCode::Right)
                || (*code == KeyCode::Char('l') && *modifiers == Modifiers::NONE);
            let prev_panel = ((modifiers.contains(Modifiers::CTRL) || modifiers.is_empty())
                && *code == KeyCode::Left)
                || (*code == KeyCode::Char('h') && *modifiers == Modifiers::NONE);

            if next_panel {
                self.focus = self.focus.next();
                return Cmd::None;
            }
            if prev_panel {
                self.focus = self.focus.prev();
                return Cmd::None;
            }
        }

        match self.focus {
            Panel::FilePicker => {
                if let Event::Key(KeyEvent {
                    code,
                    kind: KeyEventKind::Press,
                    modifiers,
                    ..
                }) = event
                {
                    match code {
                        // Vim: k or Up for move up
                        KeyCode::Up | KeyCode::Char('k') => self.picker.move_up(),
                        // Vim: j or Down for move down
                        KeyCode::Down | KeyCode::Char('j') => self.picker.move_down(),
                        // Vim: g or Home for first (gg would require state, so single g)
                        KeyCode::Home | KeyCode::Char('g')
                            if !modifiers.contains(Modifiers::SHIFT) =>
                        {
                            self.picker.move_to_first()
                        }
                        // Vim: G or End for last
                        KeyCode::End | KeyCode::Char('G') => self.picker.move_to_last(),
                        // Vim: Ctrl+U for half-page up, or PageUp
                        KeyCode::PageUp => self.picker.page_up(),
                        KeyCode::Char('u') if modifiers.contains(Modifiers::CTRL) => {
                            self.picker.page_up()
                        }
                        // Vim: Ctrl+D for half-page down, or PageDown
                        KeyCode::PageDown => self.picker.page_down(),
                        KeyCode::Char('d') if modifiers.contains(Modifiers::CTRL) => {
                            self.picker.page_down()
                        }
                        // Toggle hidden files: '.' (unix convention, frees 'h' for navigation)
                        KeyCode::Char('.') => {
                            self.show_hidden = !self.show_hidden;
                            self.picker.set_filter(FilePickerFilter {
                                allowed_extensions: vec![],
                                show_hidden: self.show_hidden,
                            });
                        }
                        KeyCode::Enter => self.enter_selected_directory(),
                        KeyCode::Backspace => self.go_up(),
                        _ => {}
                    }
                }
            }
            Panel::Preview => {
                if let Event::Key(KeyEvent {
                    code,
                    kind: KeyEventKind::Press,
                    ..
                }) = event
                {
                    match code {
                        // Vim: k or Up for scroll up
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.preview_scroll = self.preview_scroll.saturating_sub(1);
                        }
                        // Vim: j or Down for scroll down
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.preview_scroll = self.preview_scroll.saturating_add(1);
                        }
                        _ => {}
                    }
                }
            }
            Panel::FileTree => {}
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Clear the full area so panel gaps don't show stale borders.
        Paragraph::new("")
            .style(Style::new().bg(theme::alpha::SURFACE))
            .render(area, frame);

        let main = Flex::vertical()
            .constraints([Constraint::Min(1), Constraint::Fixed(1)])
            .split(area);

        let cols = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([
                Constraint::Fixed(25),
                Constraint::Percentage(40.0),
                Constraint::Min(20),
            ])
            .split(main[0]);

        self.layout_tree.set(cols[0]);
        self.layout_picker.set(cols[1]);
        self.layout_preview.set(cols[2]);

        self.render_tree_panel(frame, cols[0]);
        self.render_picker_panel(frame, cols[1]);
        self.render_preview_panel(frame, cols[2]);

        // Status bar
        let entry_info = self
            .picker
            .selected_entry()
            .map(|e| {
                let size = e.size.map(filesize::decimal).unwrap_or_default();
                format!("{} {} {}", icons::entry_icon(e), e.name, size)
            })
            .unwrap_or_else(|| "(no selection)".into());
        let status = format!(
            "{} | h/l: panels | j/k: navigate | g/G: first/last | Enter: open | Backspace: up | .: hidden",
            entry_info
        );
        let status_line = fit_to_width(&status, main[1].width);
        Paragraph::new(status_line)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(main[1], frame);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "←/→ or h/l",
                action: "Switch panel",
            },
            HelpEntry {
                key: "j/k or \u{2191}/\u{2193}",
                action: "Navigate",
            },
            HelpEntry {
                key: "g/G",
                action: "First/last",
            },
            HelpEntry {
                key: "Ctrl+D/U",
                action: "Page scroll",
            },
            HelpEntry {
                key: ".",
                action: "Toggle hidden",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "File Browser"
    }

    fn tab_label(&self) -> &'static str {
        "Files"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn initial_state() {
        let screen = FileBrowser::new();
        assert_eq!(screen.focus, Panel::FilePicker);
        assert_eq!(screen.title(), "File Browser");
        assert_eq!(screen.tab_label(), "Files");
    }

    #[test]
    fn file_icons_are_distinct() {
        assert_ne!(icons::file_icon("test.rs"), icons::file_icon("test.py"));
        assert_ne!(icons::file_icon("test.md"), icons::file_icon("test.json"));
    }

    #[test]
    fn directory_icon_different_from_file() {
        let dir_icon = icons::directory_icon();
        let file_icon = icons::file_icon("test.txt");
        assert_ne!(dir_icon, file_icon);
    }

    #[test]
    fn file_browser_navigates() {
        let mut screen = FileBrowser::new();
        let initial_path = screen.current_path().to_string();
        screen.enter_selected_directory();
        assert_ne!(screen.current_path(), initial_path);
    }

    #[test]
    fn panel_navigation() {
        let mut screen = FileBrowser::new();
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, Panel::Preview);
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, Panel::FileTree);
        screen.update(&ctrl_press(KeyCode::Left));
        assert_eq!(screen.focus, Panel::Preview);
    }

    #[test]
    fn file_selection() {
        let mut screen = FileBrowser::new();
        assert_eq!(screen.picker.selected_index(), 0);
        screen.update(&press(KeyCode::Down));
        assert_eq!(screen.picker.selected_index(), 1);
        screen.update(&press(KeyCode::Up));
        assert_eq!(screen.picker.selected_index(), 0);
    }

    #[test]
    fn toggle_hidden() {
        let mut screen = FileBrowser::new();
        assert!(!screen.show_hidden);
        screen.update(&press(KeyCode::Char('.')));
        assert!(screen.show_hidden);
    }

    #[test]
    fn vim_navigation_jk() {
        let mut screen = FileBrowser::new();
        assert_eq!(screen.picker.selected_index(), 0);
        screen.update(&press(KeyCode::Char('j')));
        assert_eq!(screen.picker.selected_index(), 1);
        screen.update(&press(KeyCode::Char('k')));
        assert_eq!(screen.picker.selected_index(), 0);
    }

    #[test]
    fn vim_navigation_hl_panels() {
        let mut screen = FileBrowser::new();
        assert_eq!(screen.focus, Panel::FilePicker);
        screen.update(&press(KeyCode::Char('l')));
        assert_eq!(screen.focus, Panel::Preview);
        screen.update(&press(KeyCode::Char('h')));
        assert_eq!(screen.focus, Panel::FilePicker);
    }

    #[test]
    fn simulated_entries_not_empty() {
        let entries = simulated_entries();
        assert!(entries.len() > 10);
        assert!(entries.iter().any(|e| e.is_dir()));
        assert!(entries.iter().any(|e| e.kind == FileKind::Symlink));
    }

    // --- Emoji regression tests (bd-zlcj8) ---

    /// Regression: use_emoji() must always return true for the demo showcase.
    #[test]
    fn use_emoji_always_true() {
        assert!(
            icons::use_emoji(),
            "Demo file browser must always show emoji"
        );
    }

    /// Regression: file icons must contain actual emoji characters, not ASCII fallbacks.
    #[test]
    fn file_icons_are_emoji() {
        let dir = icons::directory_icon();
        let file = icons::file_icon("test.rs");
        let sym = icons::symlink_icon();

        // Emoji icons should NOT be pure ASCII (2-letter codes like "DR", "RS", "SY").
        assert!(
            dir.len() > 2,
            "directory icon should be emoji, got: {dir:?}"
        );
        assert!(
            file.len() > 2,
            "file icon should be emoji, got: {file:?}"
        );
        assert!(
            sym.len() > 2,
            "symlink icon should be emoji, got: {sym:?}"
        );
    }

    /// Regression: breadcrumbs contain home emoji.
    #[test]
    fn breadcrumbs_contain_emoji() {
        let crumbs = format_breadcrumbs("/home/user/project");
        assert!(
            crumbs.contains('🏠'),
            "breadcrumbs should contain home emoji: {crumbs:?}"
        );
    }

    /// Regression: entry_icon returns emoji for all entry kinds.
    #[test]
    fn entry_icon_returns_emoji_for_all_kinds() {
        let dir_entry = FileEntry {
            name: "src".to_string(),
            kind: FileKind::Directory,
            size: 0,
            modified: "now".to_string(),
        };
        let file_entry = FileEntry {
            name: "main.rs".to_string(),
            kind: FileKind::File,
            size: 100,
            modified: "now".to_string(),
        };
        let sym_entry = FileEntry {
            name: "link".to_string(),
            kind: FileKind::Symlink,
            size: 0,
            modified: "now".to_string(),
        };

        for (entry, label) in [
            (&dir_entry, "directory"),
            (&file_entry, "file"),
            (&sym_entry, "symlink"),
        ] {
            let icon = icons::entry_icon(entry);
            assert!(
                icon.len() > 2,
                "{label} entry should have emoji icon, got: {icon:?}"
            );
        }
    }
}

#![forbid(unsafe_code)]

//! Live Markdown Editor screen — split editor + preview with search.
//!
//! Demonstrates:
//! - `TextArea` backed by ftui-text rope editor
//! - `MarkdownRenderer` for live preview
//! - Search with highlighted current match
//! - Diff mode to compare raw vs rendered line widths

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::CursorPosition;
use ftui_text::search::{SearchResult, search_ascii_case_insensitive};
use ftui_text::text::{Line, Span, Text};
use ftui_text::wrap::{WrapMode, display_width};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::textarea::TextArea;
use unicode_segmentation::UnicodeSegmentation;

use super::{HelpEntry, Screen};
use crate::theme;

const SAMPLE_MARKDOWN: &str = "\
# Live Markdown Editor

Write Markdown on the left, preview on the right.

## Goals

- Split view editor + preview
- Live updates without flicker
- Search with highlighted matches
- Diff mode: raw vs rendered width

## Notes

Inline math: $E = mc^2$

```rust
fn render(frame: &mut Frame) {
    // Draw widgets then diff
}
```

| Feature | Status |
| --- | --- |
| Live preview | on |
| Search | on |
| Diff mode | Ctrl+D |
";

const RULE_WIDTH: u16 = 36;

fn wrap_markdown_for_panel(text: &Text, width: u16) -> Text {
    let width = usize::from(width);
    if width == 0 {
        return text.clone();
    }

    let mut lines = Vec::new();
    for line in text.lines() {
        let plain = line.to_plain_text();
        let table_like = is_table_line(&plain) || is_table_like_line(&plain);
        if table_like || line.width() <= width {
            lines.push(line.clone());
            continue;
        }

        for wrapped in line.wrap(width, WrapMode::Word) {
            if wrapped.width() <= width {
                lines.push(wrapped);
            } else {
                let mut text = Text::from_lines([wrapped]);
                text.truncate(width, None);
                lines.extend(text.lines().iter().cloned());
            }
        }
    }

    Text::from_lines(lines)
}

fn is_table_line(plain: &str) -> bool {
    plain.chars().any(|c| {
        matches!(
            c,
            '┌' | '┬' | '┐' | '├' | '┼' | '┤' | '└' | '┴' | '┘' | '│' | '─'
        )
    })
}

fn is_table_like_line(plain: &str) -> bool {
    let trimmed = plain.trim_start();
    if !trimmed.starts_with('|') {
        return false;
    }
    trimmed.chars().filter(|&c| c == '|').count() >= 2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Editor,
    Search,
}

impl Focus {
    fn toggle(self) -> Self {
        match self {
            Self::Editor => Self::Search,
            Self::Search => Self::Editor,
        }
    }
}

pub struct MarkdownLiveEditor {
    editor: TextArea,
    search_input: TextInput,
    focus: Focus,
    search_results: Vec<SearchResult>,
    current_match: Option<usize>,
    md_theme: MarkdownTheme,
    diff_mode: bool,
    preview_scroll: u16,
}

impl Default for MarkdownLiveEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownLiveEditor {
    pub fn new() -> Self {
        let md_theme = Self::build_theme();

        let editor = TextArea::new()
            .with_text(SAMPLE_MARKDOWN)
            .with_line_numbers(true)
            .with_soft_wrap(true)
            .with_focus(true)
            .with_placeholder("Start writing Markdown...");

        let search_input = TextInput::new()
            .with_placeholder("Search in editor (Ctrl+F)")
            .with_focused(false);

        let mut screen = Self {
            editor,
            search_input,
            focus: Focus::Editor,
            search_results: Vec::new(),
            current_match: None,
            md_theme,
            diff_mode: false,
            preview_scroll: 0,
        };

        screen.apply_theme();
        screen.recompute_search();
        screen
    }

    pub fn apply_theme(&mut self) {
        let input_style = Style::new()
            .fg(theme::fg::PRIMARY)
            .bg(theme::alpha::SURFACE);
        let placeholder_style = Style::new().fg(theme::fg::MUTED);

        self.editor = self
            .editor
            .clone()
            .with_style(
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::SURFACE),
            )
            .with_cursor_line_style(Style::new().bg(theme::alpha::HIGHLIGHT))
            .with_selection_style(Style::new().bg(theme::accent::INFO).fg(theme::fg::PRIMARY));

        self.search_input = self
            .search_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style);

        self.md_theme = Self::build_theme();
    }

    fn build_theme() -> MarkdownTheme {
        MarkdownTheme {
            h1: Style::new().fg(theme::fg::PRIMARY).bold(),
            h2: Style::new().fg(theme::accent::PRIMARY).bold(),
            h3: Style::new().fg(theme::accent::SECONDARY).bold(),
            h4: Style::new().fg(theme::accent::INFO).bold(),
            h5: Style::new().fg(theme::accent::SUCCESS).bold(),
            h6: Style::new().fg(theme::fg::SECONDARY).bold(),
            code_inline: Style::new()
                .fg(theme::accent::WARNING)
                .bg(theme::alpha::SURFACE),
            code_block: Style::new()
                .fg(theme::fg::SECONDARY)
                .bg(theme::alpha::SURFACE),
            blockquote: Style::new().fg(theme::fg::MUTED).italic(),
            link: Style::new().fg(theme::accent::LINK).underline(),
            emphasis: Style::new().italic(),
            strong: Style::new().bold(),
            strikethrough: Style::new().strikethrough(),
            list_bullet: Style::new().fg(theme::accent::PRIMARY),
            horizontal_rule: Style::new().fg(theme::fg::MUTED).dim(),
            table_border: Style::new().fg(theme::fg::MUTED),
            table_header: Style::new()
                .fg(theme::accent::INFO)
                .bg(theme::alpha::SURFACE)
                .bold(),
            table_row: Style::new().fg(theme::fg::PRIMARY),
            table_row_alt: Style::new()
                .fg(theme::fg::PRIMARY)
                .bg(theme::alpha::SURFACE),
            task_done: Style::new().fg(theme::accent::SUCCESS),
            task_todo: Style::new().fg(theme::accent::INFO),
            math_inline: Style::new().fg(theme::accent::SECONDARY).italic(),
            math_block: Style::new().fg(theme::accent::SECONDARY).bold(),
            footnote_ref: Style::new().fg(theme::fg::MUTED).dim(),
            footnote_def: Style::new().fg(theme::fg::SECONDARY),
            admonition_note: Style::new().fg(theme::accent::INFO).bold(),
            admonition_tip: Style::new().fg(theme::accent::SUCCESS).bold(),
            admonition_important: Style::new().fg(theme::accent::SECONDARY).bold(),
            admonition_warning: Style::new().fg(theme::accent::WARNING).bold(),
            admonition_caution: Style::new().fg(theme::accent::ERROR).bold(),
        }
    }

    fn render_preview_text(&self, width: u16) -> Text {
        MarkdownRenderer::new(self.md_theme.clone())
            .rule_width(RULE_WIDTH.min(width))
            .table_max_width(width)
            .render(&self.editor.text())
    }

    fn sync_focus(&mut self) {
        let editor_focus = self.focus == Focus::Editor;
        self.editor.set_focused(editor_focus);
        self.search_input.set_focused(!editor_focus);
    }

    fn recompute_search(&mut self) {
        let query = self.search_input.value().trim();
        if query.is_empty() {
            self.search_results.clear();
            self.current_match = None;
            self.editor.clear_selection();
            return;
        }

        let text = self.editor.text();
        self.search_results = search_ascii_case_insensitive(&text, query);
        if self.search_results.is_empty() {
            self.current_match = None;
            self.editor.clear_selection();
        } else {
            let idx = self
                .current_match
                .unwrap_or(0)
                .min(self.search_results.len() - 1);
            self.current_match = Some(idx);
            self.select_match(idx);
        }
    }

    fn select_match(&mut self, idx: usize) {
        let Some(result) = self.search_results.get(idx) else {
            return;
        };
        let text = self.editor.text();
        let pos = Self::cursor_from_byte(&text, result.range.start);
        let match_text = result.text(&text);
        let match_len = match_text.graphemes(true).count();

        self.editor.clear_selection();
        self.editor.set_cursor_position(pos);
        for _ in 0..match_len {
            self.editor.select_right();
        }
    }

    fn next_match(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        let idx = self.current_match.unwrap_or(0);
        let next = (idx + 1) % self.search_results.len();
        self.current_match = Some(next);
        self.select_match(next);
    }

    fn prev_match(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        let idx = self.current_match.unwrap_or(0);
        let prev = idx.checked_sub(1).unwrap_or(self.search_results.len() - 1);
        self.current_match = Some(prev);
        self.select_match(prev);
    }

    fn cursor_from_byte(text: &str, target: usize) -> CursorPosition {
        let mut byte = 0usize;
        for (line_idx, line_text) in text.lines().enumerate() {
            let line_end = byte + line_text.len();
            if target <= line_end {
                let offset = target.saturating_sub(byte).min(line_text.len());
                let prefix = &line_text[..offset];
                let graphemes = prefix.graphemes(true).count();
                let visual_col = display_width(prefix);
                return CursorPosition::new(line_idx, graphemes, visual_col);
            }
            byte = line_end + 1;
        }
        CursorPosition::new(text.lines().count().saturating_sub(1), 0, 0)
    }

    fn render_search_bar(&self, frame: &mut Frame, area: Rect) {
        let border_style =
            theme::panel_border_style(self.focus == Focus::Search, theme::screen_accent::MARKDOWN);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Search")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let cols = Flex::horizontal()
            .constraints([
                Constraint::Fixed(10),
                Constraint::Min(1),
                Constraint::Fixed(16),
            ])
            .split(inner);

        Paragraph::new("Query:")
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(cols[0], frame);
        Widget::render(&self.search_input, cols[1], frame);

        let match_text = if self.search_results.is_empty() {
            "0 matches".to_string()
        } else {
            let current = self.current_match.map_or(0, |i| i + 1);
            format!("{current}/{} matches", self.search_results.len())
        };
        Paragraph::new(match_text)
            .style(theme::muted())
            .render(cols[2], frame);
    }

    fn render_editor_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style =
            theme::panel_border_style(self.focus == Focus::Editor, theme::screen_accent::MARKDOWN);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Editor")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        Widget::render(&self.editor, inner, frame);
    }

    fn render_preview_panel(&self, frame: &mut Frame, area: Rect) {
        let title = if self.diff_mode {
            "Preview (Diff Mode)"
        } else {
            "Preview"
        };

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title)
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::MARKDOWN));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rendered_md = self.render_preview_text(inner.width);
        let wrapped_md = wrap_markdown_for_panel(&rendered_md, inner.width);

        if self.diff_mode {
            let rows = Flex::vertical()
                .constraints([Constraint::Min(4), Constraint::Fixed(6)])
                .split(inner);
            Paragraph::new(wrapped_md.clone())
                .wrap(WrapMode::None)
                .scroll((self.preview_scroll, 0))
                .render(rows[0], frame);
            self.render_width_diff(frame, rows[1], &wrapped_md);
        } else {
            Paragraph::new(wrapped_md)
                .wrap(WrapMode::None)
                .scroll((self.preview_scroll, 0))
                .render(inner, frame);
        }
    }

    fn render_width_diff(&self, frame: &mut Frame, area: Rect, rendered_md: &Text) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Raw vs Rendered Width")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::MUTED));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let text = self.editor.text();
        let raw_lines: Vec<&str> = text.lines().collect();
        let rendered_lines = rendered_md.lines();
        let max_lines = inner.height as usize;
        let mut lines = Vec::new();

        for i in 0..max_lines {
            let raw = raw_lines.get(i).copied().unwrap_or("");
            let raw_w = display_width(raw);
            let rendered_w = rendered_lines.get(i).map(|line| line.width()).unwrap_or(0);
            let delta = rendered_w as i32 - raw_w as i32;
            let delta_style = if delta == 0 {
                theme::muted()
            } else if delta > 0 {
                Style::new().fg(theme::accent::SUCCESS)
            } else {
                Style::new().fg(theme::accent::WARNING)
            };

            lines.push(Line::from_spans([
                Span::styled(format!("{:02}", i + 1), theme::muted()),
                Span::raw(" raw "),
                Span::styled(format!("{raw_w:>3}"), Style::new().fg(theme::fg::PRIMARY)),
                Span::raw(" md "),
                Span::styled(
                    format!("{rendered_w:>3}"),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
                Span::raw(" Δ "),
                Span::styled(format!("{delta:+}"), delta_style),
            ]));
        }

        Paragraph::new(Text::from_lines(lines)).render(inner, frame);
    }
}

impl Screen for MarkdownLiveEditor {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                KeyCode::Tab => {
                    self.focus = self.focus.toggle();
                    self.sync_focus();
                    return Cmd::None;
                }
                KeyCode::Char('f') if modifiers.contains(Modifiers::CTRL) => {
                    self.focus = Focus::Search;
                    self.sync_focus();
                    return Cmd::None;
                }
                KeyCode::Escape if self.focus == Focus::Search => {
                    self.focus = Focus::Editor;
                    self.sync_focus();
                    return Cmd::None;
                }
                KeyCode::Char('d') if modifiers.contains(Modifiers::CTRL) => {
                    self.diff_mode = !self.diff_mode;
                    return Cmd::None;
                }
                KeyCode::Char('n') if modifiers.contains(Modifiers::CTRL) => {
                    self.next_match();
                    return Cmd::None;
                }
                KeyCode::Char('p') if modifiers.contains(Modifiers::CTRL) => {
                    self.prev_match();
                    return Cmd::None;
                }
                KeyCode::Up if modifiers.contains(Modifiers::CTRL) => {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1);
                    return Cmd::None;
                }
                KeyCode::Down if modifiers.contains(Modifiers::CTRL) => {
                    self.preview_scroll = self.preview_scroll.saturating_add(1);
                    return Cmd::None;
                }
                _ => {}
            }
        }

        match self.focus {
            Focus::Search => {
                if self.search_input.handle_event(event) {
                    self.recompute_search();
                }
            }
            Focus::Editor => {
                if self.editor.handle_event(event) {
                    self.recompute_search();
                }
            }
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let outer = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Live Markdown Editor")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::MARKDOWN));

        let inner = outer.inner(area);
        outer.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .gap(theme::spacing::XS)
            .constraints([Constraint::Fixed(3), Constraint::Min(1)])
            .split(inner);

        self.render_search_bar(frame, rows[0]);

        let cols = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(rows[1]);

        self.render_editor_panel(frame, cols[0]);
        self.render_preview_panel(frame, cols[1]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Tab",
                action: "Toggle focus",
            },
            HelpEntry {
                key: "Ctrl+F",
                action: "Focus search",
            },
            HelpEntry {
                key: "Ctrl+N/P",
                action: "Next/prev match",
            },
            HelpEntry {
                key: "Ctrl+D",
                action: "Toggle diff mode",
            },
            HelpEntry {
                key: "Ctrl+↑/↓",
                action: "Scroll preview",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Live Markdown"
    }

    fn tab_label(&self) -> &'static str {
        "MD Live"
    }
}

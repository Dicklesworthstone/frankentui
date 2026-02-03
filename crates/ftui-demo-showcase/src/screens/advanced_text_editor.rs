#![forbid(unsafe_code)]

//! Advanced Text Editor screen — multi-line editor with search/replace.
//!
//! Demonstrates:
//! - `TextArea` with line numbers, selection highlighting
//! - Search/replace functionality with match navigation
//! - Cursor position and selection length tracking
//! - Undo/redo integration

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::search::{SearchResult, search_ascii_case_insensitive};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::textarea::TextArea;

use super::{HelpEntry, Screen};
use crate::theme;

use std::collections::VecDeque;

const UNDO_HISTORY_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy)]
struct KeyChord {
    code: KeyCode,
    modifiers: Modifiers,
}

impl KeyChord {
    const fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self { code, modifiers }
    }

    fn matches(self, code: KeyCode, modifiers: Modifiers) -> bool {
        self.code == code && self.modifiers == modifiers
    }
}

#[derive(Debug, Clone, Copy)]
struct UndoKeybindings {
    undo: KeyChord,
    redo_primary: KeyChord,
    redo_secondary: KeyChord,
}

impl Default for UndoKeybindings {
    fn default() -> Self {
        Self {
            undo: KeyChord::new(KeyCode::Char('z'), Modifiers::CTRL),
            redo_primary: KeyChord::new(KeyCode::Char('y'), Modifiers::CTRL),
            redo_secondary: KeyChord::new(KeyCode::Char('Z'), Modifiers::CTRL | Modifiers::SHIFT),
        }
    }
}

/// Focus state for the editor screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// Main text editor.
    Editor,
    /// Search query input.
    Search,
    /// Replace text input.
    Replace,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Editor => Self::Search,
            Self::Search => Self::Replace,
            Self::Replace => Self::Editor,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Editor => Self::Replace,
            Self::Search => Self::Editor,
            Self::Replace => Self::Search,
        }
    }
}

/// Advanced Text Editor demo screen.
pub struct AdvancedTextEditor {
    /// Main text editor.
    editor: TextArea,
    /// Search query input.
    search_input: TextInput,
    /// Replace text input.
    replace_input: TextInput,
    /// Which panel has focus.
    focus: Focus,
    /// Whether the search/replace panel is visible.
    search_visible: bool,
    /// Cached search results.
    search_results: Vec<SearchResult>,
    /// Current match index (0-based, None if no matches).
    current_match: Option<usize>,
    /// Status message displayed at the bottom.
    status: String,
    /// Undo history (most recent at the back).
    undo_stack: VecDeque<String>,
    /// Redo history (most recent at the back).
    redo_stack: VecDeque<String>,
    /// Whether the undo history panel is visible.
    undo_panel_visible: bool,
    /// Undo/redo keybindings.
    undo_keys: UndoKeybindings,
}

impl Default for AdvancedTextEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl AdvancedTextEditor {
    /// Create a new Advanced Text Editor screen.
    pub fn new() -> Self {
        let sample_text = r#"Welcome to the Advanced Text Editor!

This is a demonstration of FrankenTUI's text editing capabilities.
You can edit text, select regions, search, and replace.

Features:
- Multi-line editing with line numbers
- Selection with Shift+Arrow keys
- Undo (Ctrl+Z) and Redo (Ctrl+Y)
- Search (Ctrl+F) with next/prev match
- Replace (Ctrl+H) single or all matches

Try editing this text or loading your own content.

Unicode support:
- Emoji: 🎉 🚀 ✨
- CJK: 你好世界
- Accented: café résumé naïve

The editor uses a rope data structure internally for efficient
operations on large buffers, with grapheme-aware cursor movement
and proper Unicode handling throughout.
"#;

        let editor = TextArea::new()
            .with_text(sample_text)
            .with_line_numbers(true)
            .with_focus(true)
            .with_placeholder("Start typing...");

        let search_input = TextInput::new()
            .with_placeholder("Search...")
            .with_focused(false);

        let replace_input = TextInput::new()
            .with_placeholder("Replace with...")
            .with_focused(false);

        Self {
            editor,
            search_input,
            replace_input,
            focus: Focus::Editor,
            search_visible: false,
            search_results: Vec::new(),
            current_match: None,
            status: "Ready | Ctrl+F: Search | Ctrl+H: Replace | ?: Help".into(),
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            undo_panel_visible: false,
            undo_keys: UndoKeybindings::default(),
        }
    }

    /// Configure undo/redo keybindings (customization support).
    #[allow(dead_code)]
    fn with_undo_keybindings(mut self, bindings: UndoKeybindings) -> Self {
        self.undo_keys = bindings;
        self
    }

    /// Apply the current theme to all widgets.
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
            .with_selection_style(
                Style::new()
                    .bg(theme::alpha::HIGHLIGHT)
                    .fg(theme::fg::PRIMARY),
            );

        self.search_input = self
            .search_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style);

        self.replace_input = self
            .replace_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style);
    }

    /// Update focus states for all widgets.
    fn update_focus_states(&mut self) {
        self.editor.set_focused(self.focus == Focus::Editor);
        self.search_input.set_focused(self.focus == Focus::Search);
        self.replace_input.set_focused(self.focus == Focus::Replace);
    }

    /// Perform a search with the current query.
    fn do_search(&mut self) {
        let query = self.search_input.value().to_string();
        if query.is_empty() {
            self.search_results.clear();
            self.current_match = None;
            self.update_status();
            return;
        }

        let text = self.editor.text();
        self.search_results = search_ascii_case_insensitive(&text, &query);

        if self.search_results.is_empty() {
            self.current_match = None;
        } else {
            // Find the match closest to the cursor
            let cursor_byte = self.cursor_to_byte_offset();
            let closest = self
                .search_results
                .iter()
                .enumerate()
                .min_by_key(|(_, r)| r.range.start.abs_diff(cursor_byte))
                .map(|(i, _)| i);
            self.current_match = closest;
            self.jump_to_current_match();
        }
        self.update_status();
    }

    /// Calculate the byte offset for the current cursor position.
    fn cursor_to_byte_offset(&self) -> usize {
        let cursor = self.editor.cursor();
        let text = self.editor.text();
        let mut byte_offset = 0;
        for (line_idx, line) in text.lines().enumerate() {
            if line_idx == cursor.line {
                // Count chars up to cursor.grapheme (approximation for ASCII search)
                for (c_idx, c) in line.chars().enumerate() {
                    if c_idx >= cursor.grapheme {
                        break;
                    }
                    byte_offset += c.len_utf8();
                }
                break;
            }
            byte_offset += line.len() + 1; // +1 for newline
        }
        byte_offset
    }

    /// Jump the cursor to the current match.
    fn jump_to_current_match(&mut self) {
        let Some(idx) = self.current_match else {
            return;
        };
        let Some(result) = self.search_results.get(idx) else {
            return;
        };

        // Convert byte offset to line/column position
        let text = self.editor.text();
        let target_byte = result.range.start;
        let mut line = 0;
        let mut column = 0;
        let mut byte = 0;

        for (line_idx, line_text) in text.lines().enumerate() {
            let line_end = byte + line_text.len();
            if target_byte <= line_end {
                line = line_idx;
                // Count chars within this line up to target byte
                let offset_in_line = target_byte.saturating_sub(byte);
                column = line_text[..offset_in_line.min(line_text.len())]
                    .chars()
                    .count();
                break;
            }
            byte = line_end + 1; // +1 for newline
        }

        // Set cursor position
        self.editor
            .editor_mut()
            .set_cursor(ftui_text::CursorPosition::new(line, column, column));
    }

    /// Move to the next search match.
    fn next_match(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        let idx = self.current_match.unwrap_or(0);
        self.current_match = Some((idx + 1) % self.search_results.len());
        self.jump_to_current_match();
        self.update_status();
    }

    /// Move to the previous search match.
    fn prev_match(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        let idx = self.current_match.unwrap_or(0);
        self.current_match = Some(idx.checked_sub(1).unwrap_or(self.search_results.len() - 1));
        self.jump_to_current_match();
        self.update_status();
    }

    /// Replace the current match with the replacement text.
    fn replace_current(&mut self) {
        let Some(idx) = self.current_match else {
            return;
        };
        let Some(result) = self.search_results.get(idx).cloned() else {
            return;
        };

        let replacement = self.replace_input.value().to_string();
        let text = self.editor.text();

        // Build new text with replacement
        let new_text = format!(
            "{}{}{}",
            &text[..result.range.start],
            replacement,
            &text[result.range.end..]
        );

        self.editor.set_text(&new_text);
        self.do_search(); // Re-run search
        self.update_status();
    }

    /// Replace all matches with the replacement text.
    fn replace_all(&mut self) {
        if self.search_results.is_empty() {
            return;
        }

        let query = self.search_input.value().to_string();
        if query.is_empty() {
            return;
        }

        let replacement = self.replace_input.value().to_string();
        let text = self.editor.text();

        // Replace from end to start to preserve byte offsets
        let mut new_text = text.clone();
        for result in self.search_results.iter().rev() {
            new_text = format!(
                "{}{}{}",
                &new_text[..result.range.start],
                replacement,
                &new_text[result.range.end..]
            );
        }

        let count = self.search_results.len();
        self.editor.set_text(&new_text);
        self.search_results.clear();
        self.current_match = None;
        self.status = format!("Replaced {count} occurrence(s)");
    }

    /// Update the status line.
    fn update_status(&mut self) {
        let cursor = self.editor.cursor();
        let selection_len = self
            .editor
            .selected_text()
            .map(|s| s.chars().count())
            .unwrap_or(0);

        let match_info = if !self.search_results.is_empty() {
            let current = self.current_match.map_or(0, |i| i + 1);
            let total = self.search_results.len();
            format!(" | Match {current}/{total}")
        } else if self.search_visible && !self.search_input.value().is_empty() {
            " | No matches".to_string()
        } else {
            String::new()
        };

        let sel_info = if selection_len > 0 {
            format!(" | Sel: {selection_len}")
        } else {
            String::new()
        };

        let undo_info = format!(
            "Undo:{} Redo:{}",
            self.undo_stack.len(),
            self.redo_stack.len()
        );
        let history_hint = if self.undo_panel_visible {
            "Ctrl+U: Hide history"
        } else {
            "Ctrl+U: Show history"
        };

        self.status = format!(
            "Ln {}, Col {}{}{} | {} | {}",
            cursor.line + 1,
            cursor.grapheme + 1,
            sel_info,
            match_info,
            undo_info,
            history_hint
        );
    }

    /// Render the main editor panel.
    fn render_editor_panel(&self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Editor;
        let border_style = theme::panel_border_style(focused, theme::screen_accent::FORMS_INPUT);

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

    fn render_undo_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Undo History ")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::accent::INFO));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("Undo ({})", self.undo_stack.len()));
        for entry in self.undo_stack.iter().rev().take(6) {
            lines.push(format!("  • {entry}"));
        }

        lines.push(String::new());
        lines.push(format!("Redo ({})", self.redo_stack.len()));
        for entry in self.redo_stack.iter().rev().take(6) {
            lines.push(format!("  • {entry}"));
        }

        Paragraph::new(lines.join("\n"))
            .style(theme::body())
            .render(inner, frame);
    }

    /// Render the search/replace panel.
    fn render_search_panel(&self, frame: &mut Frame, area: Rect) {
        if !self.search_visible || area.height < 4 {
            return;
        }

        let focused = self.focus == Focus::Search || self.focus == Focus::Replace;
        let border_style = theme::panel_border_style(focused, theme::screen_accent::FORMS_INPUT);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Search / Replace")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
            ])
            .split(inner);

        // Search row
        if !rows[0].is_empty() {
            let cols = Flex::horizontal()
                .constraints([Constraint::Fixed(10), Constraint::Min(1)])
                .split(rows[0]);
            Paragraph::new("Search:")
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(cols[0], frame);
            Widget::render(&self.search_input, cols[1], frame);
        }

        // Replace row
        if rows.len() > 1 && !rows[1].is_empty() {
            let cols = Flex::horizontal()
                .constraints([Constraint::Fixed(10), Constraint::Min(1)])
                .split(rows[1]);
            Paragraph::new("Replace:")
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(cols[0], frame);
            Widget::render(&self.replace_input, cols[1], frame);
        }

        // Buttons row
        if rows.len() > 2 && !rows[2].is_empty() {
            let match_info = if !self.search_results.is_empty() {
                let current = self.current_match.map_or(0, |i| i + 1);
                format!(
                    "{}/{} | Enter: Next | Shift+Enter: Prev | Ctrl+R: Replace | Ctrl+A: All",
                    current,
                    self.search_results.len()
                )
            } else {
                "Type to search | Enter: Next | Esc: Close".to_string()
            };
            Paragraph::new(match_info)
                .style(theme::muted())
                .render(rows[2], frame);
        }
    }

    fn record_undo(&mut self, description: &str) {
        self.undo_stack.push_back(description.to_string());
        self.redo_stack.clear();

        while self.undo_stack.len() > UNDO_HISTORY_LIMIT {
            self.undo_stack.pop_front();
        }
    }

    fn perform_undo(&mut self) {
        if self.undo_stack.pop_back().is_some() {
            self.editor.undo();
            self.redo_stack.push_back("Redo edit".to_string());
        }
        self.update_status();
    }

    fn perform_redo(&mut self) {
        if self.redo_stack.pop_back().is_some() {
            self.editor.redo();
            self.undo_stack.push_back("Edit text".to_string());
        }
        self.update_status();
    }
}

impl Screen for AdvancedTextEditor {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        // Handle focus switching with Ctrl+Arrow
        if let Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
            && self.search_visible
        {
            self.focus = self.focus.next();
            self.update_focus_states();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Left,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
            && self.search_visible
        {
            self.focus = self.focus.prev();
            self.update_focus_states();
            return Cmd::None;
        }

        // Global shortcuts
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            if self.undo_keys.undo.matches(*code, *modifiers) {
                self.perform_undo();
                return Cmd::None;
            }
            if self.undo_keys.redo_primary.matches(*code, *modifiers)
                || self.undo_keys.redo_secondary.matches(*code, *modifiers)
            {
                self.perform_redo();
                return Cmd::None;
            }

            let ctrl = modifiers.contains(Modifiers::CTRL);
            let shift = modifiers.contains(Modifiers::SHIFT);

            match (*code, ctrl, shift) {
                // Ctrl+U: Toggle undo history panel
                (KeyCode::Char('u'), true, false) => {
                    self.undo_panel_visible = !self.undo_panel_visible;
                    self.update_status();
                    return Cmd::None;
                }
                // Ctrl+F: Toggle search panel and focus search input
                (KeyCode::Char('f'), true, false) => {
                    self.search_visible = true;
                    self.focus = Focus::Search;
                    self.update_focus_states();
                    return Cmd::None;
                }
                // Ctrl+H: Toggle replace panel
                (KeyCode::Char('h'), true, false) => {
                    self.search_visible = true;
                    self.focus = Focus::Replace;
                    self.update_focus_states();
                    return Cmd::None;
                }
                // Escape: Close search panel if open, or clear selection
                (KeyCode::Escape, false, false) => {
                    if self.search_visible {
                        self.search_visible = false;
                        self.focus = Focus::Editor;
                        self.update_focus_states();
                    } else {
                        self.editor.clear_selection();
                    }
                    self.update_status();
                    return Cmd::None;
                }
                // F3 or Ctrl+G: Next match
                (KeyCode::F(3), false, false) | (KeyCode::Char('g'), true, false) => {
                    self.next_match();
                    return Cmd::None;
                }
                // Shift+F3 or Ctrl+Shift+G: Previous match
                (KeyCode::F(3), false, true) | (KeyCode::Char('G'), true, true) => {
                    self.prev_match();
                    return Cmd::None;
                }
                _ => {}
            }
        }

        // Route events to focused widget
        match self.focus {
            Focus::Editor => {
                let before = self.editor.text();
                self.editor.handle_event(event);
                let after = self.editor.text();
                if before != after {
                    self.record_undo("Edit text");
                }
                self.update_status();
            }
            Focus::Search => {
                // Handle Enter for next/prev match
                if let Event::Key(KeyEvent {
                    code: KeyCode::Enter,
                    modifiers,
                    kind: KeyEventKind::Press,
                    ..
                }) = event
                {
                    if modifiers.contains(Modifiers::SHIFT) {
                        self.prev_match();
                    } else {
                        self.do_search();
                        self.next_match();
                    }
                    return Cmd::None;
                }
                self.search_input.handle_event(event);
                self.do_search();
            }
            Focus::Replace => {
                // Handle Ctrl+R for replace current, Ctrl+A for replace all
                if let Event::Key(KeyEvent {
                    code,
                    modifiers,
                    kind: KeyEventKind::Press,
                    ..
                }) = event
                {
                    let ctrl = modifiers.contains(Modifiers::CTRL);
                    match (*code, ctrl) {
                        (KeyCode::Char('r'), true) => {
                            self.replace_current();
                            return Cmd::None;
                        }
                        (KeyCode::Char('a'), true) => {
                            self.replace_all();
                            return Cmd::None;
                        }
                        (KeyCode::Enter, false) => {
                            self.replace_current();
                            return Cmd::None;
                        }
                        _ => {}
                    }
                }
                self.replace_input.handle_event(event);
            }
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Layout: editor + optional search panel + status bar
        let main_height = if self.search_visible {
            area.height.saturating_sub(6) // 5 for search panel + 1 for status
        } else {
            area.height.saturating_sub(1) // 1 for status
        };

        let chunks = if self.search_visible {
            Flex::vertical()
                .constraints([
                    Constraint::Fixed(main_height),
                    Constraint::Fixed(5),
                    Constraint::Fixed(1),
                ])
                .split(area)
        } else {
            Flex::vertical()
                .constraints([Constraint::Fixed(main_height), Constraint::Fixed(1)])
                .split(area)
        };

        // Editor panel (optionally split with undo history)
        if self.undo_panel_visible {
            let cols = Flex::horizontal()
                .constraints([Constraint::Min(50), Constraint::Fixed(28)])
                .split(chunks[0]);
            self.render_editor_panel(frame, cols[0]);
            self.render_undo_panel(frame, cols[1]);
        } else {
            self.render_editor_panel(frame, chunks[0]);
        }

        // Search panel (if visible)
        if self.search_visible && chunks.len() > 2 {
            self.render_search_panel(frame, chunks[1]);
        }

        // Status bar
        let status_idx = chunks.len() - 1;
        Paragraph::new(&*self.status)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(chunks[status_idx], frame);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Ctrl+F",
                action: "Search",
            },
            HelpEntry {
                key: "Ctrl+H",
                action: "Replace",
            },
            HelpEntry {
                key: "Ctrl+G / F3",
                action: "Next match",
            },
            HelpEntry {
                key: "Shift+F3",
                action: "Previous match",
            },
            HelpEntry {
                key: "Ctrl+Z",
                action: "Undo",
            },
            HelpEntry {
                key: "Ctrl+Y",
                action: "Redo",
            },
            HelpEntry {
                key: "Ctrl+Shift+Z",
                action: "Redo (alt)",
            },
            HelpEntry {
                key: "Ctrl+U",
                action: "Toggle history panel",
            },
            HelpEntry {
                key: "Shift+Arrow",
                action: "Select text",
            },
            HelpEntry {
                key: "Ctrl+A",
                action: "Select all / Replace all",
            },
            HelpEntry {
                key: "Ctrl+R",
                action: "Replace current",
            },
            HelpEntry {
                key: "Esc",
                action: "Close search / Clear selection",
            },
        ]
    }

    fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn next_undo_description(&self) -> Option<&str> {
        self.undo_stack.back().map(String::as_str)
    }

    fn undo(&mut self) -> bool {
        self.perform_undo();
        true
    }

    fn redo(&mut self) -> bool {
        self.perform_redo();
        true
    }

    fn title(&self) -> &'static str {
        "Advanced Text Editor"
    }

    fn tab_label(&self) -> &'static str {
        "Editor"
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
        let screen = AdvancedTextEditor::new();
        assert_eq!(screen.focus, Focus::Editor);
        assert!(!screen.search_visible);
        assert_eq!(screen.title(), "Advanced Text Editor");
        assert_eq!(screen.tab_label(), "Editor");
    }

    #[test]
    fn ctrl_f_opens_search() {
        let mut screen = AdvancedTextEditor::new();
        assert!(!screen.search_visible);

        screen.update(&ctrl_press(KeyCode::Char('f')));
        assert!(screen.search_visible);
        assert_eq!(screen.focus, Focus::Search);
    }

    #[test]
    fn ctrl_h_opens_replace() {
        let mut screen = AdvancedTextEditor::new();
        screen.update(&ctrl_press(KeyCode::Char('h')));
        assert!(screen.search_visible);
        assert_eq!(screen.focus, Focus::Replace);
    }

    #[test]
    fn escape_closes_search() {
        let mut screen = AdvancedTextEditor::new();
        screen.update(&ctrl_press(KeyCode::Char('f')));
        assert!(screen.search_visible);

        screen.update(&press(KeyCode::Escape));
        assert!(!screen.search_visible);
        assert_eq!(screen.focus, Focus::Editor);
    }

    #[test]
    fn ctrl_u_toggles_history_panel() {
        let mut screen = AdvancedTextEditor::new();
        assert!(!screen.undo_panel_visible);

        screen.update(&ctrl_press(KeyCode::Char('u')));
        assert!(screen.undo_panel_visible);

        screen.update(&ctrl_press(KeyCode::Char('u')));
        assert!(!screen.undo_panel_visible);
    }

    #[test]
    fn focus_cycles_with_ctrl_arrows() {
        let mut screen = AdvancedTextEditor::new();
        screen.search_visible = true;
        screen.focus = Focus::Editor;
        screen.update_focus_states();

        // Ctrl+Right cycles forward
        screen.update(&Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        }));
        assert_eq!(screen.focus, Focus::Search);

        screen.update(&Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        }));
        assert_eq!(screen.focus, Focus::Replace);

        screen.update(&Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        }));
        assert_eq!(screen.focus, Focus::Editor);
    }

    #[test]
    fn undo_redo_updates_history() {
        let mut screen = AdvancedTextEditor::new();
        assert_eq!(screen.undo_stack.len(), 0);
        assert_eq!(screen.redo_stack.len(), 0);

        screen.update(&press(KeyCode::Char('a')));
        assert_eq!(screen.undo_stack.len(), 1);
        assert_eq!(screen.redo_stack.len(), 0);

        screen.update(&ctrl_press(KeyCode::Char('z')));
        assert_eq!(screen.undo_stack.len(), 0);
        assert_eq!(screen.redo_stack.len(), 1);

        screen.update(&ctrl_press(KeyCode::Char('y')));
        assert_eq!(screen.undo_stack.len(), 1);
        assert_eq!(screen.redo_stack.len(), 0);
    }

    #[test]
    fn editor_receives_text() {
        let mut screen = AdvancedTextEditor::new();
        let initial_len = screen.editor.text().len();

        screen.update(&press(KeyCode::Char('X')));
        // Text should change (character inserted)
        let new_len = screen.editor.text().len();
        assert!(new_len != initial_len || screen.editor.text().contains('X'));
    }

    #[test]
    fn search_finds_matches() {
        let mut screen = AdvancedTextEditor::new();
        screen.editor.set_text("hello world hello");
        screen.search_visible = true;
        screen.focus = Focus::Search;
        screen.update_focus_states();

        // Type search query
        for ch in "hello".chars() {
            screen.update(&press(KeyCode::Char(ch)));
        }

        assert_eq!(screen.search_results.len(), 2);
        assert!(screen.current_match.is_some());
    }

    #[test]
    fn keybindings_non_empty() {
        let screen = AdvancedTextEditor::new();
        assert!(!screen.keybindings().is_empty());
    }

    #[test]
    fn default_impl() {
        let screen = AdvancedTextEditor::default();
        assert!(!screen.editor.is_empty());
    }

    #[test]
    fn render_without_panic() {
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = AdvancedTextEditor::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        screen.view(&mut frame, area);
    }

    #[test]
    fn render_with_search_visible() {
        use ftui_render::grapheme_pool::GraphemePool;
        let mut screen = AdvancedTextEditor::new();
        screen.search_visible = true;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        screen.view(&mut frame, area);
    }

    #[test]
    fn replace_all_works() {
        let mut screen = AdvancedTextEditor::new();
        screen.editor.set_text("foo bar foo baz foo");

        // Set up search
        screen.search_visible = true;
        screen.focus = Focus::Search;
        for ch in "foo".chars() {
            screen.search_input.handle_event(&press(KeyCode::Char(ch)));
        }
        screen.do_search();
        assert_eq!(screen.search_results.len(), 3);

        // Set up replacement
        screen.focus = Focus::Replace;
        for ch in "XXX".chars() {
            screen.replace_input.handle_event(&press(KeyCode::Char(ch)));
        }

        // Replace all
        screen.replace_all();
        assert_eq!(screen.editor.text(), "XXX bar XXX baz XXX");
    }
}

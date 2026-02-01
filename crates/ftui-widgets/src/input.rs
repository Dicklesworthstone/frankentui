#![forbid(unsafe_code)]

//! Text input widget.
//!
//! A single-line text input field with cursor management, scrolling, selection,
//! word-level operations, and styling. Grapheme-cluster aware for correct Unicode handling.

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, CellContent};
use ftui_render::frame::Frame;
use ftui_style::Style;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::Widget;

/// A single-line text input widget.
#[derive(Debug, Clone, Default)]
pub struct TextInput {
    /// Text value.
    value: String,
    /// Cursor position (grapheme index).
    cursor: usize,
    /// Scroll offset (visual cells) for horizontal scrolling.
    scroll_cells: usize,
    /// Selection anchor (grapheme index). When set, selection spans from anchor to cursor.
    selection_anchor: Option<usize>,
    /// Placeholder text.
    placeholder: String,
    /// Mask character for password mode.
    mask_char: Option<char>,
    /// Maximum length in graphemes (None = unlimited).
    max_length: Option<usize>,
    /// Base style.
    style: Style,
    /// Cursor style.
    cursor_style: Style,
    /// Placeholder style.
    placeholder_style: Style,
    /// Selection highlight style.
    selection_style: Style,
    /// Whether the input is focused (controls cursor output).
    focused: bool,
}

impl TextInput {
    /// Create a new empty text input.
    pub fn new() -> Self {
        Self::default()
    }

    // --- Builder methods ---

    /// Set the text value (builder).
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self.cursor = self.value.graphemes(true).count();
        self.selection_anchor = None;
        self
    }

    /// Set the placeholder text (builder).
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Set password mode with mask character (builder).
    pub fn with_mask(mut self, mask: char) -> Self {
        self.mask_char = Some(mask);
        self
    }

    /// Set maximum length in graphemes (builder).
    pub fn with_max_length(mut self, max: usize) -> Self {
        self.max_length = Some(max);
        self
    }

    /// Set base style (builder).
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set cursor style (builder).
    pub fn with_cursor_style(mut self, style: Style) -> Self {
        self.cursor_style = style;
        self
    }

    /// Set placeholder style (builder).
    pub fn with_placeholder_style(mut self, style: Style) -> Self {
        self.placeholder_style = style;
        self
    }

    /// Set selection style (builder).
    pub fn with_selection_style(mut self, style: Style) -> Self {
        self.selection_style = style;
        self
    }

    /// Set whether the input is focused (builder).
    pub fn with_focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    // --- Value access ---

    /// Get the current value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Set the value, clamping cursor to valid range.
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        let max = self.grapheme_count();
        self.cursor = self.cursor.min(max);
        self.selection_anchor = None;
    }

    /// Clear all text.
    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.scroll_cells = 0;
        self.selection_anchor = None;
    }

    /// Get the cursor position (grapheme index).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Get the cursor screen position relative to a render area.
    ///
    /// Returns `(x, y)` where x is the column and y is the row.
    /// Useful for `Frame::set_cursor()`.
    pub fn cursor_position(&self, area: Rect) -> (u16, u16) {
        let cursor_visual = self.cursor_visual_pos();
        let effective_scroll = self.effective_scroll(area.width as usize);
        let rel_x = cursor_visual.saturating_sub(effective_scroll);
        let x = area
            .x
            .saturating_add(rel_x as u16)
            .min(area.right().saturating_sub(1));
        (x, area.y)
    }

    /// Get selected text, if any.
    pub fn selected_text(&self) -> Option<&str> {
        let anchor = self.selection_anchor?;
        let (start, end) = self.selection_range(anchor);
        let byte_start = self.grapheme_byte_offset(start);
        let byte_end = self.grapheme_byte_offset(end);
        Some(&self.value[byte_start..byte_end])
    }

    // --- Event handling ---

    /// Handle a terminal event.
    ///
    /// Returns `true` if the state changed.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        if let Event::Key(key) = event
            && (key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat)
        {
            return self.handle_key(key);
        }
        false
    }

    fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(Modifiers::CTRL);
        let shift = key.modifiers.contains(Modifiers::SHIFT);

        match key.code {
            KeyCode::Char(c) if !ctrl => {
                self.delete_selection();
                self.insert_char(c);
                true
            }
            // Ctrl+A: select all
            KeyCode::Char('a') if ctrl => {
                self.select_all();
                true
            }
            KeyCode::Backspace => {
                if self.selection_anchor.is_some() {
                    self.delete_selection();
                } else if ctrl {
                    self.delete_word_back();
                } else {
                    self.delete_char_back();
                }
                true
            }
            KeyCode::Delete => {
                if self.selection_anchor.is_some() {
                    self.delete_selection();
                } else if ctrl {
                    self.delete_word_forward();
                } else {
                    self.delete_char_forward();
                }
                true
            }
            KeyCode::Left => {
                if ctrl {
                    self.move_cursor_word_left(shift);
                } else if shift {
                    self.move_cursor_left_select();
                } else {
                    self.move_cursor_left();
                }
                true
            }
            KeyCode::Right => {
                if ctrl {
                    self.move_cursor_word_right(shift);
                } else if shift {
                    self.move_cursor_right_select();
                } else {
                    self.move_cursor_right();
                }
                true
            }
            KeyCode::Home => {
                if shift {
                    self.ensure_selection_anchor();
                } else {
                    self.selection_anchor = None;
                }
                self.cursor = 0;
                self.scroll_cells = 0;
                true
            }
            KeyCode::End => {
                if shift {
                    self.ensure_selection_anchor();
                } else {
                    self.selection_anchor = None;
                }
                self.cursor = self.grapheme_count();
                true
            }
            _ => false,
        }
    }

    // --- Editing operations ---

    fn insert_char(&mut self, c: char) {
        if let Some(max) = self.max_length
            && self.grapheme_count() >= max
        {
            return;
        }

        let byte_offset = self.grapheme_byte_offset(self.cursor);
        self.value.insert(byte_offset, c);
        self.cursor += 1;
    }

    fn delete_char_back(&mut self) {
        if self.cursor > 0 {
            let byte_start = self.grapheme_byte_offset(self.cursor - 1);
            let byte_end = self.grapheme_byte_offset(self.cursor);
            self.value.drain(byte_start..byte_end);
            self.cursor -= 1;
        }
    }

    fn delete_char_forward(&mut self) {
        let count = self.grapheme_count();
        if self.cursor < count {
            let byte_start = self.grapheme_byte_offset(self.cursor);
            let byte_end = self.grapheme_byte_offset(self.cursor + 1);
            self.value.drain(byte_start..byte_end);
        }
    }

    fn delete_word_back(&mut self) {
        let old_cursor = self.cursor;
        self.move_cursor_word_left(false);
        let new_cursor = self.cursor;
        if new_cursor < old_cursor {
            let byte_start = self.grapheme_byte_offset(new_cursor);
            let byte_end = self.grapheme_byte_offset(old_cursor);
            self.value.drain(byte_start..byte_end);
        }
    }

    fn delete_word_forward(&mut self) {
        let old_cursor = self.cursor;
        // Use standard movement logic to find end of deletion
        self.move_cursor_word_right(false);
        let new_cursor = self.cursor;
        // Reset cursor to start (deletion happens forward from here)
        self.cursor = old_cursor;

        if new_cursor > old_cursor {
            let byte_start = self.grapheme_byte_offset(old_cursor);
            let byte_end = self.grapheme_byte_offset(new_cursor);
            self.value.drain(byte_start..byte_end);
        }
    }

    // --- Selection ---

    /// Select all text.
    pub fn select_all(&mut self) {
        self.selection_anchor = Some(0);
        self.cursor = self.grapheme_count();
    }

    /// Delete selected text. No-op if no selection.
    fn delete_selection(&mut self) {
        if let Some(anchor) = self.selection_anchor.take() {
            let (start, end) = self.selection_range(anchor);
            let byte_start = self.grapheme_byte_offset(start);
            let byte_end = self.grapheme_byte_offset(end);
            self.value.drain(byte_start..byte_end);
            self.cursor = start;
        }
    }

    fn ensure_selection_anchor(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
    }

    fn selection_range(&self, anchor: usize) -> (usize, usize) {
        if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        }
    }

    fn is_in_selection(&self, grapheme_idx: usize) -> bool {
        if let Some(anchor) = self.selection_anchor {
            let (start, end) = self.selection_range(anchor);
            grapheme_idx >= start && grapheme_idx < end
        } else {
            false
        }
    }

    // --- Cursor movement ---

    fn move_cursor_left(&mut self) {
        if let Some(anchor) = self.selection_anchor.take() {
            self.cursor = self.cursor.min(anchor);
        } else if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn move_cursor_right(&mut self) {
        if let Some(anchor) = self.selection_anchor.take() {
            self.cursor = self.cursor.max(anchor);
        } else if self.cursor < self.grapheme_count() {
            self.cursor += 1;
        }
    }

    fn move_cursor_left_select(&mut self) {
        self.ensure_selection_anchor();
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn move_cursor_right_select(&mut self) {
        self.ensure_selection_anchor();
        if self.cursor < self.grapheme_count() {
            self.cursor += 1;
        }
    }

    fn move_cursor_word_left(&mut self, select: bool) {
        if select {
            self.ensure_selection_anchor();
        } else {
            self.selection_anchor = None;
        }

        let graphemes: Vec<&str> = self.value.graphemes(true).collect();
        let mut pos = self.cursor;

        if pos == 0 {
            return;
        }

        // Helper to determine class: 0=Space, 1=AlphaNum, 2=Punct
        let class = |g: &str| {
            if g.chars().all(char::is_whitespace) {
                0
            } else if g.chars().any(char::is_alphanumeric) {
                1
            } else {
                2
            }
        };

        // Determine class of character *before* cursor
        let target_class = class(graphemes[pos - 1]);

        // Skip all preceding characters of the same class
        while pos > 0 && class(graphemes[pos - 1]) == target_class {
            pos -= 1;
        }

        self.cursor = pos;
    }

    fn move_cursor_word_right(&mut self, select: bool) {
        if select {
            self.ensure_selection_anchor();
        } else {
            self.selection_anchor = None;
        }

        let graphemes: Vec<&str> = self.value.graphemes(true).collect();
        let max = graphemes.len();
        let mut pos = self.cursor;

        if pos >= max {
            return;
        }

        // Helper to determine class: 0=Space, 1=AlphaNum, 2=Punct
        let class = |g: &str| {
            if g.chars().all(char::is_whitespace) {
                0
            } else if g.chars().any(char::is_alphanumeric) {
                1
            } else {
                2
            }
        };

        // Determine class of character *at* cursor
        let target_class = class(graphemes[pos]);

        // Skip all following characters of the same class
        while pos < max && class(graphemes[pos]) == target_class {
            pos += 1;
        }

        self.cursor = pos;
    }

    // --- Internal helpers ---

    fn grapheme_count(&self) -> usize {
        self.value.graphemes(true).count()
    }

    fn grapheme_byte_offset(&self, grapheme_idx: usize) -> usize {
        self.value
            .grapheme_indices(true)
            .nth(grapheme_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    fn grapheme_width(&self, g: &str) -> usize {
        if self.mask_char.is_some() {
            1
        } else {
            UnicodeWidthStr::width(g)
        }
    }

    fn cursor_visual_pos(&self) -> usize {
        if self.value.is_empty() {
            return 0;
        }
        self.value
            .graphemes(true)
            .take(self.cursor)
            .map(|g| self.grapheme_width(g))
            .sum()
    }

    fn effective_scroll(&self, viewport_width: usize) -> usize {
        let cursor_visual = self.cursor_visual_pos();
        let mut scroll = self.scroll_cells;
        if cursor_visual < scroll {
            scroll = cursor_visual;
        }
        if cursor_visual >= scroll + viewport_width {
            scroll = cursor_visual - viewport_width + 1;
        }
        scroll
    }
}

impl Widget for TextInput {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "TextInput",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.width < 1 || area.height < 1 {
            return;
        }

        let deg = frame.buffer.degradation;

        // TextInput is essential — always render content, but skip styling
        // at NoStyling+. At Skeleton, still render the raw text value.
        // We explicitly DO NOT check deg.render_content() here because this widget is essential.
        if deg.apply_styling() {
            crate::set_style_area(&mut frame.buffer, area, self.style);
        }

        let graphemes: Vec<&str> = self.value.graphemes(true).collect();
        let show_placeholder = self.value.is_empty() && !self.placeholder.is_empty();

        let viewport_width = area.width as usize;
        let cursor_visual_pos = self.cursor_visual_pos();
        let effective_scroll = self.effective_scroll(viewport_width);

        // Render content
        let mut visual_x: usize = 0;
        let y = area.y;

        if show_placeholder {
            let placeholder_style = if deg.apply_styling() {
                self.placeholder_style
            } else {
                Style::default()
            };
            for g in self.placeholder.graphemes(true) {
                let w = UnicodeWidthStr::width(g);

                if visual_x + w <= effective_scroll {
                    visual_x += w;
                    continue;
                }
                if visual_x.saturating_sub(effective_scroll) >= viewport_width {
                    break;
                }

                if let Some(c) = g.chars().next() {
                    let mut cell = Cell::from_char(c);
                    crate::apply_style(&mut cell, placeholder_style);
                    let rel_x = visual_x.saturating_sub(effective_scroll);
                    if rel_x < viewport_width {
                        frame.buffer.set(area.x + rel_x as u16, y, cell);
                    }
                }
                visual_x += w;
            }
        } else {
            for (gi, g) in graphemes.iter().enumerate() {
                let w = self.grapheme_width(g);

                if visual_x + w <= effective_scroll {
                    visual_x += w;
                    continue;
                }
                if visual_x.saturating_sub(effective_scroll) >= viewport_width {
                    break;
                }

                let text_char = if let Some(mask) = self.mask_char {
                    mask
                } else {
                    g.chars().next().unwrap_or(' ')
                };

                let cell_style = if !deg.apply_styling() {
                    Style::default()
                } else if self.is_in_selection(gi) {
                    self.selection_style
                } else {
                    self.style
                };

                let mut cell = Cell::from_char(text_char);
                crate::apply_style(&mut cell, cell_style);

                let rel_x = visual_x.saturating_sub(effective_scroll);
                if rel_x < viewport_width {
                    frame.buffer.set(area.x + rel_x as u16, y, cell);
                }
                visual_x += w;
            }
        }

        // Set cursor style at cursor position
        let cursor_rel_x = cursor_visual_pos.saturating_sub(effective_scroll);
        if cursor_rel_x < viewport_width {
            let cursor_screen_x = area.x + cursor_rel_x as u16;
            if let Some(cell) = frame.buffer.get_mut(cursor_screen_x, y) {
                if !deg.apply_styling() {
                    // At NoStyling, just use reverse video for cursor
                    use ftui_render::cell::StyleFlags;
                    let current_flags = cell.attrs.flags();
                    let new_flags = current_flags ^ StyleFlags::REVERSE;
                    cell.attrs = cell.attrs.with_flags(new_flags);
                } else if self.cursor_style.is_empty() {
                    // Default: toggle reverse video for cursor visibility
                    use ftui_render::cell::StyleFlags;
                    let current_flags = cell.attrs.flags();
                    let new_flags = current_flags ^ StyleFlags::REVERSE;
                    cell.attrs = cell.attrs.with_flags(new_flags);
                } else {
                    crate::apply_style(cell, self.cursor_style);
                }
            }

            // Set frame cursor position for hardware cursor
            // Note: This positions the terminal's cursor at the text input position,
            // which is important for accessibility and IME input.
            frame.set_cursor(Some((cursor_screen_x, y)));
        }
    }

    fn is_essential(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_input() {
        let input = TextInput::new();
        assert!(input.value().is_empty());
        assert_eq!(input.cursor(), 0);
        assert!(input.selected_text().is_none());
    }

    #[test]
    fn test_with_value() {
        let input = TextInput::new().with_value("hello");
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn test_set_value() {
        let mut input = TextInput::new().with_value("hello world");
        input.cursor = 11;
        input.set_value("hi");
        assert_eq!(input.value(), "hi");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn test_clear() {
        let mut input = TextInput::new().with_value("hello");
        input.clear();
        assert!(input.value().is_empty());
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_insert_char() {
        let mut input = TextInput::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('c');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn test_insert_char_mid() {
        let mut input = TextInput::new().with_value("ac");
        input.cursor = 1;
        input.insert_char('b');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn test_max_length() {
        let mut input = TextInput::new().with_max_length(3);
        for c in "abcdef".chars() {
            input.insert_char(c);
        }
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn test_delete_char_back() {
        let mut input = TextInput::new().with_value("hello");
        input.delete_char_back();
        assert_eq!(input.value(), "hell");
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn test_delete_char_back_at_start() {
        let mut input = TextInput::new().with_value("hello");
        input.cursor = 0;
        input.delete_char_back();
        assert_eq!(input.value(), "hello");
    }

    #[test]
    fn test_delete_char_forward() {
        let mut input = TextInput::new().with_value("hello");
        input.cursor = 0;
        input.delete_char_forward();
        assert_eq!(input.value(), "ello");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_delete_char_forward_at_end() {
        let mut input = TextInput::new().with_value("hello");
        input.delete_char_forward();
        assert_eq!(input.value(), "hello");
    }

    #[test]
    fn test_cursor_left_right() {
        let mut input = TextInput::new().with_value("hello");
        assert_eq!(input.cursor(), 5);
        input.move_cursor_left();
        assert_eq!(input.cursor(), 4);
        input.move_cursor_left();
        assert_eq!(input.cursor(), 3);
        input.move_cursor_right();
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn test_cursor_bounds() {
        let mut input = TextInput::new().with_value("hi");
        input.cursor = 0;
        input.move_cursor_left();
        assert_eq!(input.cursor(), 0);
        input.cursor = 2;
        input.move_cursor_right();
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn test_word_movement_left() {
        let mut input = TextInput::new().with_value("hello world test");
        // "hello world test"
        //                 ^ (16)
        input.move_cursor_word_left(false);
        assert_eq!(input.cursor(), 12); // "hello world |test"

        input.move_cursor_word_left(false);
        assert_eq!(input.cursor(), 11); // "hello world| test" (stopped after space)

        input.move_cursor_word_left(false);
        assert_eq!(input.cursor(), 6); // "hello |world test"

        input.move_cursor_word_left(false);
        assert_eq!(input.cursor(), 5); // "hello| world test"

        input.move_cursor_word_left(false);
        assert_eq!(input.cursor(), 0); // "|hello world test"
    }

    #[test]
    fn test_word_movement_right() {
        let mut input = TextInput::new().with_value("hello world test");
        input.cursor = 0;
        // "|hello world test"

        input.move_cursor_word_right(false);
        assert_eq!(input.cursor(), 5); // "hello| world test"

        input.move_cursor_word_right(false);
        assert_eq!(input.cursor(), 6); // "hello |world test"

        input.move_cursor_word_right(false);
        assert_eq!(input.cursor(), 11); // "hello world| test"

        input.move_cursor_word_right(false);
        assert_eq!(input.cursor(), 12); // "hello world |test"

        input.move_cursor_word_right(false);
        assert_eq!(input.cursor(), 16); // "hello world test|"
    }

    #[test]
    fn test_delete_word_back() {
        let mut input = TextInput::new().with_value("hello world");
        // "hello world|"
        input.delete_word_back();
        assert_eq!(input.value(), "hello "); // Deleted "world"

        input.delete_word_back();
        assert_eq!(input.value(), "hello"); // Deleted " "

        input.delete_word_back();
        assert_eq!(input.value(), ""); // Deleted "hello"
    }

    #[test]
    fn test_delete_word_forward() {
        let mut input = TextInput::new().with_value("hello world");
        input.cursor = 0;
        // "|hello world"
        input.delete_word_forward();
        assert_eq!(input.value(), " world"); // Deleted "hello"

        input.delete_word_forward();
        assert_eq!(input.value(), "world"); // Deleted " "

        input.delete_word_forward();
        assert_eq!(input.value(), ""); // Deleted "world"
    }

    #[test]
    fn test_select_all() {
        let mut input = TextInput::new().with_value("hello");
        input.select_all();
        assert_eq!(input.selected_text(), Some("hello"));
    }

    #[test]
    fn test_delete_selection() {
        let mut input = TextInput::new().with_value("hello world");
        input.selection_anchor = Some(0);
        input.cursor = 5;
        input.delete_selection();
        assert_eq!(input.value(), " world");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_insert_replaces_selection() {
        let mut input = TextInput::new().with_value("hello");
        input.select_all();
        input.delete_selection();
        input.insert_char('x');
        assert_eq!(input.value(), "x");
    }

    #[test]
    fn test_unicode_grapheme_handling() {
        let mut input = TextInput::new();
        input.set_value("café");
        assert_eq!(input.grapheme_count(), 4);
        input.cursor = 4;
        input.delete_char_back();
        assert_eq!(input.value(), "caf");
    }

    #[test]
    fn test_handle_event_char() {
        let mut input = TextInput::new();
        let event = Event::Key(KeyEvent::new(KeyCode::Char('a')));
        assert!(input.handle_event(&event));
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn test_handle_event_backspace() {
        let mut input = TextInput::new().with_value("ab");
        let event = Event::Key(KeyEvent::new(KeyCode::Backspace));
        assert!(input.handle_event(&event));
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn test_handle_event_ctrl_a() {
        let mut input = TextInput::new().with_value("hello");
        let event = Event::Key(KeyEvent::new(KeyCode::Char('a')).with_modifiers(Modifiers::CTRL));
        assert!(input.handle_event(&event));
        assert_eq!(input.selected_text(), Some("hello"));
    }

    #[test]
    fn test_handle_event_ctrl_backspace() {
        let mut input = TextInput::new().with_value("hello world");
        let event = Event::Key(KeyEvent::new(KeyCode::Backspace).with_modifiers(Modifiers::CTRL));
        assert!(input.handle_event(&event));
        assert_eq!(input.value(), "hello ");
    }

    #[test]
    fn test_handle_event_home_end() {
        let mut input = TextInput::new().with_value("hello");
        input.cursor = 3;
        let home = Event::Key(KeyEvent::new(KeyCode::Home));
        assert!(input.handle_event(&home));
        assert_eq!(input.cursor(), 0);
        let end = Event::Key(KeyEvent::new(KeyCode::End));
        assert!(input.handle_event(&end));
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn test_shift_left_creates_selection() {
        let mut input = TextInput::new().with_value("hello");
        let event = Event::Key(KeyEvent::new(KeyCode::Left).with_modifiers(Modifiers::SHIFT));
        assert!(input.handle_event(&event));
        assert_eq!(input.cursor(), 4);
        assert_eq!(input.selection_anchor, Some(5));
        assert_eq!(input.selected_text(), Some("o"));
    }

    #[test]
    fn test_cursor_position() {
        let input = TextInput::new().with_value("hello");
        let area = Rect::new(10, 5, 20, 1);
        let (x, y) = input.cursor_position(area);
        assert_eq!(x, 15);
        assert_eq!(y, 5);
    }

    #[test]
    fn test_cursor_position_empty() {
        let input = TextInput::new();
        let area = Rect::new(0, 0, 80, 1);
        let (x, y) = input.cursor_position(area);
        assert_eq!(x, 0);
        assert_eq!(y, 0);
    }

    #[test]
    fn test_password_mask() {
        let input = TextInput::new().with_mask('*').with_value("secret");
        assert_eq!(input.value(), "secret");
        assert_eq!(input.cursor_visual_pos(), 6);
    }

    #[test]
    fn test_render_basic() {
        use ftui_render::frame::Frame;
        use ftui_render::grapheme_pool::GraphemePool;

        let input = TextInput::new().with_value("hi");
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        input.render(area, &mut frame);
        let cell_h = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell_h.content.as_char(), Some('h'));
        let cell_i = frame.buffer.get(1, 0).unwrap();
        assert_eq!(cell_i.content.as_char(), Some('i'));
    }

    #[test]
    fn test_left_collapses_selection() {
        let mut input = TextInput::new().with_value("hello");
        input.selection_anchor = Some(1);
        input.cursor = 4;
        input.move_cursor_left();
        assert_eq!(input.cursor(), 1);
        assert!(input.selection_anchor.is_none());
    }

    #[test]
    fn test_right_collapses_selection() {
        let mut input = TextInput::new().with_value("hello");
        input.selection_anchor = Some(1);
        input.cursor = 4;
        input.move_cursor_right();
        assert_eq!(input.cursor(), 4);
        assert!(input.selection_anchor.is_none());
    }

    #[test]
    fn test_render_sets_frame_cursor() {
        use ftui_render::frame::Frame;
        use ftui_render::grapheme_pool::GraphemePool;

        let input = TextInput::new().with_value("hello");
        let area = Rect::new(5, 3, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 10, &mut pool);
        input.render(area, &mut frame);

        // Cursor should be positioned at the end of "hello" (5 chars)
        // area.x = 5, cursor_visual_pos = 5, effective_scroll = 0
        // So cursor_screen_x = 5 + 5 = 10
        assert_eq!(frame.cursor_position, Some((10, 3)));
    }

    #[test]
    fn test_render_cursor_mid_text() {
        use ftui_render::frame::Frame;
        use ftui_render::grapheme_pool::GraphemePool;

        let mut input = TextInput::new().with_value("hello");
        input.cursor = 2; // After "he"
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        input.render(area, &mut frame);

        // Cursor after "he" = visual position 2
        assert_eq!(frame.cursor_position, Some((2, 0)));
    }
}

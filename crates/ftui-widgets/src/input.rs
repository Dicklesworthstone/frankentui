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
use ftui_text::grapheme_width;
use unicode_segmentation::UnicodeSegmentation;

use crate::Widget;
use crate::undo_support::{TextEditOperation, TextInputUndoExt, UndoSupport, UndoWidgetId};

/// A single-line text input widget.
#[derive(Debug, Clone, Default)]
pub struct TextInput {
    /// Unique ID for undo tracking.
    undo_id: UndoWidgetId,
    /// Text value.
    value: String,
    /// Cursor position (grapheme index).
    cursor: usize,
    /// Scroll offset (visual cells) for horizontal scrolling.
    scroll_cells: std::cell::Cell<usize>,
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
    #[must_use]
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self.cursor = self.value.graphemes(true).count();
        self.selection_anchor = None;
        self
    }

    /// Set the placeholder text (builder).
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Set password mode with mask character (builder).
    #[must_use]
    pub fn with_mask(mut self, mask: char) -> Self {
        self.mask_char = Some(mask);
        self
    }

    /// Set maximum length in graphemes (builder).
    #[must_use]
    pub fn with_max_length(mut self, max: usize) -> Self {
        self.max_length = Some(max);
        self
    }

    /// Set base style (builder).
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set cursor style (builder).
    #[must_use]
    pub fn with_cursor_style(mut self, style: Style) -> Self {
        self.cursor_style = style;
        self
    }

    /// Set placeholder style (builder).
    #[must_use]
    pub fn with_placeholder_style(mut self, style: Style) -> Self {
        self.placeholder_style = style;
        self
    }

    /// Set selection style (builder).
    #[must_use]
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
        self.scroll_cells.set(0);
        self.selection_anchor = None;
    }

    /// Clear all text.
    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.scroll_cells.set(0);
        self.selection_anchor = None;
    }

    /// Get the cursor position (grapheme index).
    #[inline]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Check if the input is focused.
    #[inline]
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Set focus state.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
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
    #[must_use]
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
        match event {
            Event::Key(key)
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat =>
            {
                self.handle_key(key)
            }
            Event::Paste(paste) => {
                self.delete_selection();
                self.insert_text(&paste.text);
                true
            }
            _ => false,
        }
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
            // Ctrl+W: delete word back
            KeyCode::Char('w') if ctrl => {
                self.delete_word_back();
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
                self.scroll_cells.set(0);
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

    /// Insert text at the current cursor position.
    ///
    /// This method:
    /// - Replaces newlines and tabs with spaces.
    /// - Filters out other control characters.
    /// - Respects `max_length` (truncating if necessary).
    /// - Efficiently inserts the result in one operation.
    pub fn insert_text(&mut self, text: &str) {
        // Map line breaks/tabs to spaces, filter other control chars
        let clean_text: String = text
            .chars()
            .map(|c| {
                if c == '\n' || c == '\r' || c == '\t' {
                    ' '
                } else {
                    c
                }
            })
            .filter(|c| !c.is_control())
            .collect();

        if clean_text.is_empty() {
            return;
        }

        let current_count = self.grapheme_count();
        let old_cursor = self.cursor;
        let avail = if let Some(max) = self.max_length {
            if current_count >= max {
                // Allow trying to insert 1 grapheme to see if it merges (combining char)
                1
            } else {
                max - current_count
            }
        } else {
            usize::MAX
        };

        // Calculate grapheme count of new text to see if we need to truncate
        let new_graphemes = clean_text.graphemes(true).count();
        let to_insert = if new_graphemes > avail {
            // Find byte index to truncate at
            let end_byte = clean_text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .nth(avail)
                .unwrap_or(clean_text.len());
            &clean_text[..end_byte]
        } else {
            clean_text.as_str()
        };

        if to_insert.is_empty() {
            return;
        }

        let byte_offset = self.grapheme_byte_offset(self.cursor);
        self.value.insert_str(byte_offset, to_insert);

        // Check if we exceeded max_length
        let new_total = self.grapheme_count();
        if let Some(max) = self.max_length
            && new_total > max
        {
            // Revert change
            self.value.drain(byte_offset..byte_offset + to_insert.len());
            return;
        }

        let gc = self.grapheme_count();
        let delta = gc.saturating_sub(current_count);
        self.cursor = (old_cursor + delta).min(gc);
    }

    fn insert_char(&mut self, c: char) {
        // Strict control character filtering to prevent terminal corruption
        if c.is_control() {
            return;
        }

        let old_count = self.grapheme_count();
        let byte_offset = self.grapheme_byte_offset(self.cursor);
        self.value.insert(byte_offset, c);

        let new_count = self.grapheme_count();

        // Check constraints
        if let Some(max) = self.max_length
            && new_count > max
        {
            // Revert change
            let char_len = c.len_utf8();
            self.value.drain(byte_offset..byte_offset + char_len);
            return;
        }

        // Only advance cursor if we added a new grapheme.
        // If we inserted a combining char that merged with the previous one,
        // the count stays the same, and the cursor should stay after that merged grapheme (same index).
        if new_count > old_count {
            self.cursor += 1;
        }
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

    fn get_grapheme_class(g: &str) -> u8 {
        if g.chars().all(char::is_whitespace) {
            0
        } else if g.chars().any(char::is_alphanumeric) {
            1
        } else {
            2
        }
    }

    fn move_cursor_word_left(&mut self, select: bool) {
        if select {
            self.ensure_selection_anchor();
        } else {
            self.selection_anchor = None;
        }

        if self.cursor == 0 {
            return;
        }

        let graphemes: Vec<&str> = self.value.graphemes(true).collect();
        let mut pos = self.cursor;

        // 1. Skip trailing whitespace
        while pos > 0 && Self::get_grapheme_class(graphemes[pos - 1]) == 0 {
            pos -= 1;
        }

        // 2. Skip trailing punctuation
        while pos > 0 && Self::get_grapheme_class(graphemes[pos - 1]) == 2 {
            pos -= 1;
        }

        // 3. Skip the previous word
        while pos > 0 && Self::get_grapheme_class(graphemes[pos - 1]) == 1 {
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

        if self.cursor >= max {
            return;
        }

        let mut pos = self.cursor;

        // 1. Skip current word
        while pos < max && Self::get_grapheme_class(graphemes[pos]) == 1 {
            pos += 1;
        }

        // 2. Skip whitespace and punctuation
        while pos < max && Self::get_grapheme_class(graphemes[pos]) != 1 {
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
        if let Some(mask) = self.mask_char {
            let mut buf = [0u8; 4];
            let mask_str = mask.encode_utf8(&mut buf);
            grapheme_width(mask_str)
        } else {
            grapheme_width(g)
        }
    }

    fn prev_grapheme_width(&self) -> usize {
        if self.cursor == 0 {
            return 0;
        }
        self.value
            .graphemes(true)
            .nth(self.cursor - 1)
            .map(|g| self.grapheme_width(g))
            .unwrap_or(0)
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
        let mut scroll = self.scroll_cells.get();
        if cursor_visual < scroll {
            scroll = cursor_visual;
        }
        if cursor_visual >= scroll + viewport_width {
            let candidate_scroll = cursor_visual - viewport_width + 1;
            // Ensure the character BEFORE the cursor is also fully visible
            // (prevent "hole" artifact for wide characters where start < scroll)
            let prev_width = self.prev_grapheme_width();
            let max_scroll_for_prev = cursor_visual.saturating_sub(prev_width);

            scroll = candidate_scroll.min(max_scroll_for_prev);
        }
        self.scroll_cells.set(scroll);
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
                let w = self.grapheme_width(g);
                if w == 0 {
                    continue;
                }

                // Fully scrolled out (left)
                if visual_x + w <= effective_scroll {
                    visual_x += w;
                    continue;
                }

                // Partially scrolled out (left) - skip drawing
                if visual_x < effective_scroll {
                    visual_x += w;
                    continue;
                }

                let rel_x = visual_x - effective_scroll;

                // Fully clipped (right)
                if rel_x >= viewport_width {
                    break;
                }

                // Partially clipped (right) - skip drawing
                if rel_x + w > viewport_width {
                    break;
                }

                let mut cell = if g.chars().count() > 1 || w > 1 {
                    let id = frame.intern_with_width(g, w as u8);
                    Cell::new(CellContent::from_grapheme(id))
                } else if let Some(c) = g.chars().next() {
                    Cell::from_char(c)
                } else {
                    visual_x += w;
                    continue;
                };
                crate::apply_style(&mut cell, placeholder_style);

                frame
                    .buffer
                    .set(area.x.saturating_add(rel_x as u16), y, cell);
                visual_x += w;
            }
        } else {
            for (gi, g) in graphemes.iter().enumerate() {
                let w = self.grapheme_width(g);
                if w == 0 {
                    continue;
                }

                // Fully scrolled out (left)
                if visual_x + w <= effective_scroll {
                    visual_x += w;
                    continue;
                }

                // Partially scrolled out (left) - skip drawing
                if visual_x < effective_scroll {
                    visual_x += w;
                    continue;
                }

                let rel_x = visual_x - effective_scroll;

                // Fully clipped (right)
                if rel_x >= viewport_width {
                    break;
                }

                // Partially clipped (right) - skip drawing
                if rel_x + w > viewport_width {
                    break;
                }

                let cell_style = if !deg.apply_styling() {
                    Style::default()
                } else if self.is_in_selection(gi) {
                    self.selection_style
                } else {
                    self.style
                };

                let mut cell = if let Some(mask) = self.mask_char {
                    Cell::from_char(mask)
                } else if g.chars().count() > 1 || w > 1 {
                    let id = frame.intern_with_width(g, w as u8);
                    Cell::new(CellContent::from_grapheme(id))
                } else {
                    Cell::from_char(g.chars().next().unwrap_or(' '))
                };
                crate::apply_style(&mut cell, cell_style);

                frame
                    .buffer
                    .set(area.x.saturating_add(rel_x as u16), y, cell);
                visual_x += w;
            }
        }

        if self.focused {
            // Set cursor style at cursor position
            let cursor_rel_x = cursor_visual_pos.saturating_sub(effective_scroll);
            if cursor_rel_x < viewport_width {
                let cursor_screen_x = area.x.saturating_add(cursor_rel_x as u16);
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
            }

            frame.set_cursor(Some(self.cursor_position(area)));
            frame.set_cursor_visible(true);
        }
    }

    fn is_essential(&self) -> bool {
        true
    }
}

// ============================================================================
// Undo Support Implementation
// ============================================================================

/// Snapshot of TextInput state for undo.
#[derive(Debug, Clone)]
pub struct TextInputSnapshot {
    value: String,
    cursor: usize,
    selection_anchor: Option<usize>,
}

impl UndoSupport for TextInput {
    fn undo_widget_id(&self) -> UndoWidgetId {
        self.undo_id
    }

    fn create_snapshot(&self) -> Box<dyn std::any::Any + Send> {
        Box::new(TextInputSnapshot {
            value: self.value.clone(),
            cursor: self.cursor,
            selection_anchor: self.selection_anchor,
        })
    }

    fn restore_snapshot(&mut self, snapshot: &dyn std::any::Any) -> bool {
        if let Some(snap) = snapshot.downcast_ref::<TextInputSnapshot>() {
            self.value = snap.value.clone();
            self.cursor = snap.cursor;
            self.selection_anchor = snap.selection_anchor;
            self.scroll_cells.set(0); // Reset scroll on restore
            true
        } else {
            false
        }
    }
}

impl TextInputUndoExt for TextInput {
    fn text_value(&self) -> &str {
        &self.value
    }

    fn set_text_value(&mut self, value: &str) {
        self.value = value.to_string();
        let max = self.grapheme_count();
        self.cursor = self.cursor.min(max);
        self.selection_anchor = None;
    }

    fn cursor_position(&self) -> usize {
        self.cursor
    }

    fn set_cursor_position(&mut self, pos: usize) {
        let max = self.grapheme_count();
        self.cursor = pos.min(max);
    }

    fn insert_text_at(&mut self, position: usize, text: &str) {
        let byte_offset = self.grapheme_byte_offset(position);
        self.value.insert_str(byte_offset, text);
        let inserted_graphemes = text.graphemes(true).count();
        if self.cursor >= position {
            self.cursor += inserted_graphemes;
        }
    }

    fn delete_text_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let byte_start = self.grapheme_byte_offset(start);
        let byte_end = self.grapheme_byte_offset(end);
        self.value.drain(byte_start..byte_end);
        let deleted_count = end - start;
        if self.cursor > end {
            self.cursor -= deleted_count;
        } else if self.cursor > start {
            self.cursor = start;
        }
    }
}

impl TextInput {
    /// Create an undo command for the given text edit operation.
    ///
    /// This creates a command that can be added to a [`HistoryManager`] for undo/redo support.
    /// The command includes callbacks that will be called when the operation is undone or redone.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut input = TextInput::new();
    /// let old_value = input.value().to_string();
    ///
    /// // Perform the edit
    /// input.set_value("new text");
    ///
    /// // Create undo command
    /// if let Some(cmd) = input.create_text_edit_command(TextEditOperation::SetValue {
    ///     old_value,
    ///     new_value: "new text".to_string(),
    /// }) {
    ///     history.push(cmd);
    /// }
    /// ```
    ///
    /// [`HistoryManager`]: ftui_runtime::undo::HistoryManager
    #[must_use]
    pub fn create_text_edit_command(
        &self,
        operation: TextEditOperation,
    ) -> Option<crate::undo_support::WidgetTextEditCmd> {
        Some(crate::undo_support::WidgetTextEditCmd::new(
            self.undo_id,
            operation,
        ))
    }

    /// Get the undo widget ID.
    ///
    /// This can be used to associate undo commands with this widget instance.
    #[must_use]
    pub fn undo_id(&self) -> UndoWidgetId {
        self.undo_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn cell_at(frame: &Frame, x: u16, y: u16) -> Cell {
        frame
            .buffer
            .get(x, y)
            .copied()
            .unwrap_or_else(|| panic!("test cell should exist at ({x},{y})"))
    }

    #[allow(dead_code)]
    #[test]
    fn test_empty_input() {
        let input = TextInput::new();
        assert!(input.value().is_empty());
        assert_eq!(input.cursor(), 0);
        assert!(input.selected_text().is_none());
    }

    #[test]
    fn test_with_value() {
        let mut input = TextInput::new().with_value("hello");
        input.set_focused(true);
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
        input.set_focused(true);
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
        assert_eq!(input.cursor(), 6); // "hello |world test"

        input.move_cursor_word_left(false);
        assert_eq!(input.cursor(), 0); // "|hello world test"
    }

    #[test]
    fn test_word_movement_right() {
        let mut input = TextInput::new().with_value("hello world test");
        input.cursor = 0;
        // "|hello world test"

        input.move_cursor_word_right(false);
        assert_eq!(input.cursor(), 6); // "hello |world test"

        input.move_cursor_word_right(false);
        assert_eq!(input.cursor(), 12); // "hello world |test"

        input.move_cursor_word_right(false);
        assert_eq!(input.cursor(), 16); // "hello world test|"
    }

    #[test]
    fn test_word_movement_skips_punctuation() {
        let mut input = TextInput::new().with_value("hello, world");
        input.cursor = 0;
        // "|hello, world"

        input.move_cursor_word_right(false);
        assert_eq!(input.cursor(), 7); // "hello, |world"

        input.move_cursor_word_left(false);
        assert_eq!(input.cursor(), 0); // "|hello, world"
    }

    #[test]
    fn test_delete_word_back() {
        let mut input = TextInput::new().with_value("hello world");
        // "hello world|"
        input.delete_word_back();
        assert_eq!(input.value(), "hello "); // Deleted "world"

        // "hello |" — word-left skips space then "hello"
        input.delete_word_back();
        assert_eq!(input.value(), ""); // Deleted "hello "
    }

    #[test]
    fn test_delete_word_forward() {
        let mut input = TextInput::new().with_value("hello world");
        input.cursor = 0;
        // "|hello world" — word-right skips "hello" then space
        input.delete_word_forward();
        assert_eq!(input.value(), "world"); // Deleted "hello "

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
    fn test_multi_codepoint_grapheme_cursor_movement() {
        let mut input = TextInput::new().with_value("a👩‍💻b");
        assert_eq!(input.grapheme_count(), 3);
        assert_eq!(input.cursor(), 3);

        input.move_cursor_left();
        assert_eq!(input.cursor(), 2);
        input.move_cursor_left();
        assert_eq!(input.cursor(), 1);
        input.move_cursor_left();
        assert_eq!(input.cursor(), 0);

        input.move_cursor_right();
        assert_eq!(input.cursor(), 1);
        input.move_cursor_right();
        assert_eq!(input.cursor(), 2);
        input.move_cursor_right();
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn test_delete_back_multi_codepoint_grapheme() {
        let mut input = TextInput::new().with_value("a👩‍💻b");
        input.cursor = 2; // after the emoji grapheme
        input.delete_char_back();
        assert_eq!(input.value(), "ab");
        assert_eq!(input.cursor(), 1);
        assert_eq!(input.grapheme_count(), 2);
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
        let cell_h = cell_at(&frame, 0, 0);
        assert_eq!(cell_h.content.as_char(), Some('h'));
        let cell_i = cell_at(&frame, 1, 0);
        assert_eq!(cell_i.content.as_char(), Some('i'));
    }

    #[test]
    fn test_render_sets_cursor_when_focused() {
        use ftui_render::frame::Frame;
        use ftui_render::grapheme_pool::GraphemePool;

        let input = TextInput::new().with_value("hi").with_focused(true);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        input.render(area, &mut frame);

        assert_eq!(frame.cursor_position, Some((2, 0)));
        assert!(frame.cursor_visible);
    }

    #[test]
    fn test_render_does_not_set_cursor_when_unfocused() {
        use ftui_render::frame::Frame;
        use ftui_render::grapheme_pool::GraphemePool;

        let input = TextInput::new().with_value("hi");
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        input.render(area, &mut frame);

        assert!(frame.cursor_position.is_none());
    }

    #[test]
    fn test_render_grapheme_uses_pool() {
        use ftui_render::frame::Frame;
        use ftui_render::grapheme_pool::GraphemePool;

        let grapheme = "👩‍💻";
        let input = TextInput::new().with_value(grapheme);
        let area = Rect::new(0, 0, 6, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 1, &mut pool);
        input.render(area, &mut frame);

        let cell = cell_at(&frame, 0, 0);
        assert!(cell.content.is_grapheme());
        let width = grapheme_width(grapheme);
        if width > 1 {
            assert!(cell_at(&frame, 1, 0).is_continuation());
        }
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

        let input = TextInput::new().with_value("hello").with_focused(true);
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

        let mut input = TextInput::new().with_value("hello").with_focused(true);
        input.cursor = 2; // After "he"
        let area = Rect::new(0, 0, 20, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        input.render(area, &mut frame);

        // Cursor after "he" = visual position 2
        assert_eq!(frame.cursor_position, Some((2, 0)));
    }

    // ========================================================================
    // Undo Support Tests
    // ========================================================================

    #[test]
    fn test_undo_widget_id_is_stable() {
        let input = TextInput::new();
        let id1 = input.undo_id();
        let id2 = input.undo_id();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_undo_widget_id_unique_per_instance() {
        let input1 = TextInput::new();
        let input2 = TextInput::new();
        assert_ne!(input1.undo_id(), input2.undo_id());
    }

    #[test]
    fn test_snapshot_and_restore() {
        let mut input = TextInput::new().with_value("hello");
        input.cursor = 3;
        input.selection_anchor = Some(1);

        let snapshot = input.create_snapshot();

        // Modify the input
        input.set_value("world");
        input.cursor = 5;
        input.selection_anchor = None;

        assert_eq!(input.value(), "world");
        assert_eq!(input.cursor(), 5);

        // Restore from snapshot
        assert!(input.restore_snapshot(snapshot.as_ref()));
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor(), 3);
        assert_eq!(input.selection_anchor, Some(1));
    }

    #[test]
    fn test_text_input_undo_ext_insert() {
        let mut input = TextInput::new().with_value("hello");
        input.cursor = 2;

        input.insert_text_at(2, " world");
        // "hello" with " world" inserted at position 2 = "he" + " world" + "llo"
        assert_eq!(input.value(), "he worldllo");
        assert_eq!(input.cursor(), 8); // cursor moved by inserted text length (6 graphemes)
    }

    #[test]
    fn test_text_input_undo_ext_delete() {
        let mut input = TextInput::new().with_value("hello world");
        input.cursor = 8;

        input.delete_text_range(5, 11); // Delete " world"
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor(), 5); // cursor clamped to end of remaining text
    }

    #[test]
    fn test_create_text_edit_command() {
        let input = TextInput::new().with_value("hello");
        let cmd = input.create_text_edit_command(TextEditOperation::Insert {
            position: 0,
            text: "hi".to_string(),
        });
        assert!(cmd.is_some());
        let cmd = cmd.expect("test command should exist");
        assert_eq!(cmd.widget_id(), input.undo_id());
        assert_eq!(cmd.description(), "Insert text");
    }

    #[test]
    fn test_paste_bulk_insert() {
        let mut input = TextInput::new().with_value("hello");
        input.cursor = 5;
        let event = Event::Paste(ftui_core::event::PasteEvent::bracketed(" world"));
        assert!(input.handle_event(&event));
        assert_eq!(input.value(), "hello world");
        assert_eq!(input.cursor(), 11);
    }

    #[test]
    fn test_paste_multi_grapheme_sequence() {
        let mut input = TextInput::new().with_value("hi");
        input.cursor = 2;
        let event = Event::Paste(ftui_core::event::PasteEvent::new("👩‍💻🔥", false));
        assert!(input.handle_event(&event));
        assert_eq!(input.value(), "hi👩‍💻🔥");
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn test_paste_max_length() {
        let mut input = TextInput::new().with_value("abc").with_max_length(5);
        input.cursor = 3;
        // Paste "def" (3 chars). Should be truncated to "de" (2 chars) to fit max 5.
        let event = Event::Paste(ftui_core::event::PasteEvent::bracketed("def"));
        assert!(input.handle_event(&event));
        assert_eq!(input.value(), "abcde");
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn test_paste_combining_merge() {
        let mut input = TextInput::new().with_value("e");
        input.cursor = 1;
        // Paste combining acute accent (U+0301). Should merge with 'e' -> 'é'.
        // Grapheme count stays 1. Cursor stays 1 (after the merged grapheme).
        let event = Event::Paste(ftui_core::event::PasteEvent::bracketed("\u{0301}"));
        assert!(input.handle_event(&event));
        assert_eq!(input.value(), "e\u{0301}");
        assert_eq!(input.grapheme_count(), 1);
        assert_eq!(input.cursor(), 1);
    }

    #[test]
    fn test_paste_combining_merge_mid_string() {
        let mut input = TextInput::new().with_value("ab");
        input.cursor = 1; // between a and b
        let event = Event::Paste(ftui_core::event::PasteEvent::bracketed("\u{0301}"));
        assert!(input.handle_event(&event));
        assert_eq!(input.value(), "a\u{0301}b");
        assert_eq!(input.grapheme_count(), 2);
        assert_eq!(input.cursor(), 1);
    }

    #[test]
    fn test_wide_char_scroll_visibility() {
        use ftui_render::frame::Frame;
        use ftui_render::grapheme_pool::GraphemePool;

        let wide_char = "\u{3000}"; // Ideographic space, Width 2
        let mut input = TextInput::new().with_value(wide_char).with_focused(true);
        input.cursor = 1; // After the char

        // Viewport width 2.
        // cursor_visual_pos = 2.
        // effective_scroll: 2 >= 0 + 2 -> scroll = 1.
        // Render: char at 0..2. 0 < 1 -> Skipped!
        // Expectation: We should see it.
        let area = Rect::new(0, 0, 2, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);
        input.render(area, &mut frame);

        let cell = cell_at(&frame, 0, 0);
        // If bug exists, this assertion will fail because cell is empty/default
        assert!(!cell.is_empty(), "Wide char should be visible");
    }
}

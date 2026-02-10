//! Multi-line text editing widget.
//!
//! [`TextArea`] wraps [`Editor`] for text manipulation and
//! provides Frame-based rendering with viewport scrolling and cursor display.
//!
//! # Example
//! ```
//! use ftui_widgets::textarea::{TextArea, TextAreaState};
//!
//! let mut ta = TextArea::new();
//! ta.insert_text("Hello\nWorld");
//! assert_eq!(ta.line_count(), 2);
//! ```

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::editor::{Editor, Selection};
use ftui_text::wrap::display_width;
use ftui_text::{CursorNavigator, CursorPosition};
use unicode_segmentation::UnicodeSegmentation;

use crate::{StatefulWidget, Widget, apply_style, draw_text_span};

/// Multi-line text editor widget.
#[derive(Debug, Clone)]
pub struct TextArea {
    editor: Editor,
    /// Placeholder text shown when empty.
    placeholder: String,
    /// Whether the widget has input focus.
    focused: bool,
    /// Show line numbers in gutter.
    show_line_numbers: bool,
    /// Base style.
    style: Style,
    /// Cursor line highlight style.
    cursor_line_style: Option<Style>,
    /// Selection highlight style.
    selection_style: Style,
    /// Placeholder style.
    placeholder_style: Style,
    /// Line number style.
    line_number_style: Style,
    /// Soft-wrap long lines.
    ///
    /// Visual wrapping preserves whitespace and breaks on word boundaries with
    /// a grapheme fallback for long segments.
    soft_wrap: bool,
    /// Maximum height in lines (0 = unlimited / fill area).
    max_height: usize,
    /// Viewport scroll offset (first visible line).
    scroll_top: std::cell::Cell<usize>,
    /// Horizontal scroll offset (visual columns).
    scroll_left: std::cell::Cell<usize>,
    /// Last viewport height for page movement and visibility checks.
    #[allow(dead_code)]
    last_viewport_height: std::cell::Cell<usize>,
    /// Last viewport width for visibility checks.
    last_viewport_width: std::cell::Cell<usize>,
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new()
    }
}

/// Render state tracked across frames.
#[derive(Debug, Clone, Default)]
pub struct TextAreaState {
    /// Viewport height from last render.
    pub last_viewport_height: u16,
    /// Viewport width from last render.
    pub last_viewport_width: u16,
}

#[derive(Debug, Clone)]
struct WrappedSlice {
    text: String,
    start_byte: usize,
    start_col: usize,
    width: usize,
}

impl TextArea {
    /// Create a new empty text area.
    #[must_use]
    pub fn new() -> Self {
        Self {
            editor: Editor::new(),
            placeholder: String::new(),
            focused: false,
            show_line_numbers: false,
            style: Style::default(),
            cursor_line_style: None,
            selection_style: Style::new().reverse(),
            placeholder_style: Style::new().dim(),
            line_number_style: Style::new().dim(),
            soft_wrap: false,
            max_height: 0,
            scroll_top: std::cell::Cell::new(usize::MAX), // sentinel: will be set on first render
            scroll_left: std::cell::Cell::new(0),
            last_viewport_height: std::cell::Cell::new(0),
            last_viewport_width: std::cell::Cell::new(0),
        }
    }

    // ── Event Handling ─────────────────────────────────────────────

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
                self.insert_text(&paste.text);
                true
            }
            _ => false,
        }
    }

    fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(Modifiers::CTRL);
        let shift = key.modifiers.contains(Modifiers::SHIFT);
        let _alt = key.modifiers.contains(Modifiers::ALT);

        match key.code {
            KeyCode::Char(c) if !ctrl => {
                self.insert_char(c);
                true
            }
            KeyCode::Enter => {
                self.insert_newline();
                true
            }
            KeyCode::Backspace => {
                if ctrl {
                    self.delete_word_backward();
                } else {
                    self.delete_backward();
                }
                true
            }
            KeyCode::Delete => {
                self.delete_forward();
                true
            }
            KeyCode::Left => {
                if ctrl {
                    self.move_word_left();
                } else if shift {
                    self.select_left();
                } else {
                    self.move_left();
                }
                true
            }
            KeyCode::Right => {
                if ctrl {
                    self.move_word_right();
                } else if shift {
                    self.select_right();
                } else {
                    self.move_right();
                }
                true
            }
            KeyCode::Up => {
                if shift {
                    self.select_up();
                } else {
                    self.move_up();
                }
                true
            }
            KeyCode::Down => {
                if shift {
                    self.select_down();
                } else {
                    self.move_down();
                }
                true
            }
            KeyCode::Home => {
                self.move_to_line_start();
                true
            }
            KeyCode::End => {
                self.move_to_line_end();
                true
            }
            KeyCode::PageUp => {
                let page = self.last_viewport_height.get().max(1);
                for _ in 0..page {
                    self.editor.move_up();
                }
                self.ensure_cursor_visible();
                true
            }
            KeyCode::PageDown => {
                let page = self.last_viewport_height.get().max(1);
                for _ in 0..page {
                    self.editor.move_down();
                }
                self.ensure_cursor_visible();
                true
            }
            KeyCode::Char('a') if ctrl => {
                self.select_all();
                true
            }
            // Ctrl+K: Delete to end of line (common emacs/shell binding)
            KeyCode::Char('k') if ctrl => {
                self.delete_to_end_of_line();
                true
            }
            // Ctrl+Z: Undo
            KeyCode::Char('z') if ctrl => {
                self.undo();
                true
            }
            // Ctrl+Y: Redo
            KeyCode::Char('y') if ctrl => {
                self.redo();
                true
            }
            _ => false,
        }
    }

    // ── Builder methods ────────────────────────────────────────────

    /// Set initial text content (builder).
    #[must_use]
    pub fn with_text(mut self, text: &str) -> Self {
        self.editor = Editor::with_text(text);
        self.editor.move_to_document_start();
        self
    }

    /// Set placeholder text (builder).
    #[must_use]
    pub fn with_placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Set focused state (builder).
    #[must_use]
    pub fn with_focus(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Enable line numbers (builder).
    #[must_use]
    pub fn with_line_numbers(mut self, show: bool) -> Self {
        self.show_line_numbers = show;
        self
    }

    /// Set base style (builder).
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set cursor line highlight style (builder).
    #[must_use]
    pub fn with_cursor_line_style(mut self, style: Style) -> Self {
        self.cursor_line_style = Some(style);
        self
    }

    /// Set selection style (builder).
    #[must_use]
    pub fn with_selection_style(mut self, style: Style) -> Self {
        self.selection_style = style;
        self
    }

    /// Enable soft wrapping (builder).
    #[must_use]
    pub fn with_soft_wrap(mut self, wrap: bool) -> Self {
        self.soft_wrap = wrap;
        self
    }

    /// Set maximum height in lines (builder). 0 = fill available area.
    #[must_use]
    pub fn with_max_height(mut self, max: usize) -> Self {
        self.max_height = max;
        self
    }

    // ── State access ───────────────────────────────────────────────

    /// Get the full text content.
    #[must_use]
    pub fn text(&self) -> String {
        self.editor.text()
    }

    /// Set the full text content (resets cursor and undo history).
    pub fn set_text(&mut self, text: &str) {
        self.editor.set_text(text);
        self.scroll_top.set(0);
        self.scroll_left.set(0);
    }

    /// Number of lines.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.editor.line_count()
    }

    /// Current cursor position.
    #[inline]
    #[must_use]
    pub fn cursor(&self) -> CursorPosition {
        self.editor.cursor()
    }

    /// Set cursor position (clamped to bounds). Clears selection.
    pub fn set_cursor_position(&mut self, pos: CursorPosition) {
        self.editor.set_cursor(pos);
        self.ensure_cursor_visible();
    }

    /// Whether the textarea is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }

    /// Current selection, if any.
    #[must_use = "use the returned selection (if any)"]
    pub fn selection(&self) -> Option<Selection> {
        self.editor.selection()
    }

    /// Get selected text.
    #[must_use = "use the returned selected text (if any)"]
    pub fn selected_text(&self) -> Option<String> {
        self.editor.selected_text()
    }

    /// Whether the widget has focus.
    #[must_use]
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Set focus state.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Access the underlying editor.
    #[must_use]
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// Mutable access to the underlying editor.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    // ── Editing operations (delegated to Editor) ───────────────────

    /// Insert text at cursor.
    pub fn insert_text(&mut self, text: &str) {
        self.editor.insert_text(text);
        self.ensure_cursor_visible();
    }

    /// Insert a single character.
    pub fn insert_char(&mut self, ch: char) {
        self.editor.insert_char(ch);
        self.ensure_cursor_visible();
    }

    /// Insert a newline.
    pub fn insert_newline(&mut self) {
        self.editor.insert_newline();
        self.ensure_cursor_visible();
    }

    /// Delete backward (backspace).
    pub fn delete_backward(&mut self) {
        self.editor.delete_backward();
        self.ensure_cursor_visible();
    }

    /// Delete forward (delete key).
    pub fn delete_forward(&mut self) {
        self.editor.delete_forward();
        self.ensure_cursor_visible();
    }

    /// Delete word backward (Ctrl+Backspace).
    pub fn delete_word_backward(&mut self) {
        self.editor.delete_word_backward();
        self.ensure_cursor_visible();
    }

    /// Delete to end of line (Ctrl+K).
    pub fn delete_to_end_of_line(&mut self) {
        self.editor.delete_to_end_of_line();
        self.ensure_cursor_visible();
    }

    /// Undo last edit.
    pub fn undo(&mut self) {
        self.editor.undo();
        self.ensure_cursor_visible();
    }

    /// Redo last undo.
    pub fn redo(&mut self) {
        self.editor.redo();
        self.ensure_cursor_visible();
    }

    // ── Navigation ─────────────────────────────────────────────────

    /// Move cursor left.
    pub fn move_left(&mut self) {
        self.editor.move_left();
        self.ensure_cursor_visible();
    }

    /// Move cursor right.
    pub fn move_right(&mut self) {
        self.editor.move_right();
        self.ensure_cursor_visible();
    }

    /// Move cursor up.
    pub fn move_up(&mut self) {
        self.editor.move_up();
        self.ensure_cursor_visible();
    }

    /// Move cursor down.
    pub fn move_down(&mut self) {
        self.editor.move_down();
        self.ensure_cursor_visible();
    }

    /// Move cursor left by word.
    pub fn move_word_left(&mut self) {
        self.editor.move_word_left();
        self.ensure_cursor_visible();
    }

    /// Move cursor right by word.
    pub fn move_word_right(&mut self) {
        self.editor.move_word_right();
        self.ensure_cursor_visible();
    }

    /// Move to start of line.
    pub fn move_to_line_start(&mut self) {
        self.editor.move_to_line_start();
        self.ensure_cursor_visible();
    }

    /// Move to end of line.
    pub fn move_to_line_end(&mut self) {
        self.editor.move_to_line_end();
        self.ensure_cursor_visible();
    }

    /// Move to start of document.
    pub fn move_to_document_start(&mut self) {
        self.editor.move_to_document_start();
        self.ensure_cursor_visible();
    }

    /// Move to end of document.
    pub fn move_to_document_end(&mut self) {
        self.editor.move_to_document_end();
        self.ensure_cursor_visible();
    }

    // ── Selection ──────────────────────────────────────────────────

    /// Extend selection left.
    pub fn select_left(&mut self) {
        self.editor.select_left();
        self.ensure_cursor_visible();
    }

    /// Extend selection right.
    pub fn select_right(&mut self) {
        self.editor.select_right();
        self.ensure_cursor_visible();
    }

    /// Extend selection up.
    pub fn select_up(&mut self) {
        self.editor.select_up();
        self.ensure_cursor_visible();
    }

    /// Extend selection down.
    pub fn select_down(&mut self) {
        self.editor.select_down();
        self.ensure_cursor_visible();
    }

    /// Select all.
    pub fn select_all(&mut self) {
        self.editor.select_all();
    }

    /// Clear selection.
    pub fn clear_selection(&mut self) {
        self.editor.clear_selection();
    }

    // ── Viewport management ────────────────────────────────────────

    /// Page up (move viewport and cursor up by viewport height).
    pub fn page_up(&mut self, state: &TextAreaState) {
        let page = state.last_viewport_height.max(1) as usize;
        for _ in 0..page {
            self.editor.move_up();
        }
        self.ensure_cursor_visible();
    }

    /// Page down (move viewport and cursor down by viewport height).
    pub fn page_down(&mut self, state: &TextAreaState) {
        let page = state.last_viewport_height.max(1) as usize;
        for _ in 0..page {
            self.editor.move_down();
        }
        self.ensure_cursor_visible();
    }

    /// Width of the line number gutter.
    fn gutter_width(&self) -> u16 {
        if !self.show_line_numbers {
            return 0;
        }
        let digits = {
            let mut count = self.line_count().max(1);
            let mut d: u16 = 0;
            while count > 0 {
                d += 1;
                count /= 10;
            }
            d
        };
        digits + 2 // digit width + space + separator
    }

    /// Count how many wrapped lines this text will occupy.
    ///
    /// This is a zero-allocation version of `wrap_line_slices` for layout calculations.
    fn measure_wrap_count(line_text: &str, max_width: usize) -> usize {
        if line_text.is_empty() {
            return 1;
        }

        let mut count = 0;
        let mut current_width = 0;
        let mut has_content = false;

        Self::run_wrapping_logic(line_text, max_width, |_, width, flush| {
            if flush {
                count += 1;
                current_width = 0;
                has_content = false;
            } else {
                current_width = width;
                has_content = true;
            }
        });

        // If there's pending content or if we flushed but started a new empty line (which shouldn't happen with this logic usually,
        // but let's be safe), count the last line.
        // Actually run_wrapping_logic only flushes when a line is full.
        // We need to count the current line if it has content or if it's the only line.
        if has_content || count == 0 {
            count += 1;
        }

        count
    }

    /// Core wrapping logic that emits events for layout or slicing.
    ///
    /// The callback receives `(start_index, width, flush)`.
    /// - `flush == true`: The current line is full/done. `width` is the width of the flushed line.
    /// - `flush == false`: Update current line width.
    fn run_wrapping_logic<F>(line_text: &str, max_width: usize, mut callback: F)
    where
        F: FnMut(usize, usize, bool),
    {
        let mut current_width = 0;
        let mut byte_cursor = 0;

        for segment in line_text.split_word_bounds() {
            let seg_len = segment.len();
            let seg_width: usize = segment.graphemes(true).map(display_width).sum();

            if max_width > 0 && current_width + seg_width > max_width {
                // Flush current
                callback(byte_cursor, current_width, true);
                current_width = 0;
            }

            if max_width > 0 && seg_width > max_width {
                for grapheme in segment.graphemes(true) {
                    let g_width = display_width(grapheme);
                    let g_len = grapheme.len();

                    if max_width > 0 && current_width + g_width > max_width && current_width > 0 {
                        callback(byte_cursor, current_width, true);
                        current_width = 0;
                    }

                    current_width += g_width;
                    byte_cursor += g_len;
                    callback(byte_cursor, current_width, false);
                }
                continue;
            }

            current_width += seg_width;
            byte_cursor += seg_len;
            callback(byte_cursor, current_width, false);
        }
    }

    fn wrap_line_slices(line_text: &str, max_width: usize) -> Vec<WrappedSlice> {
        if line_text.is_empty() {
            return vec![WrappedSlice {
                text: String::new(),
                start_byte: 0,
                start_col: 0,
                width: 0,
            }];
        }

        let mut slices = Vec::new();
        let mut current_text = String::new();
        let mut current_width = 0;
        let mut slice_start_byte = 0;
        let mut slice_start_col = 0;
        let mut byte_cursor = 0;
        let mut col_cursor = 0;

        let push_current = |slices: &mut Vec<WrappedSlice>,
                            text: &mut String,
                            width: &mut usize,
                            start_byte: &mut usize,
                            start_col: &mut usize,
                            byte_cursor: usize,
                            col_cursor: usize| {
            // Push even if empty if it's forced by logic (though usually check width > 0)
            // But we match original logic:
            if text.is_empty() && *width == 0 {
                return;
            }
            slices.push(WrappedSlice {
                text: std::mem::take(text),
                start_byte: *start_byte,
                start_col: *start_col,
                width: *width,
            });
            *start_byte = byte_cursor;
            *start_col = col_cursor;
            *width = 0;
        };

        for segment in line_text.split_word_bounds() {
            let seg_len = segment.len();
            let seg_width: usize = segment.graphemes(true).map(display_width).sum();

            if max_width > 0 && current_width + seg_width > max_width {
                push_current(
                    &mut slices,
                    &mut current_text,
                    &mut current_width,
                    &mut slice_start_byte,
                    &mut slice_start_col,
                    byte_cursor,
                    col_cursor,
                );
            }

            if max_width > 0 && seg_width > max_width {
                for grapheme in segment.graphemes(true) {
                    let g_width = display_width(grapheme);
                    let g_len = grapheme.len();

                    if max_width > 0 && current_width + g_width > max_width && current_width > 0 {
                        push_current(
                            &mut slices,
                            &mut current_text,
                            &mut current_width,
                            &mut slice_start_byte,
                            &mut slice_start_col,
                            byte_cursor,
                            col_cursor,
                        );
                    }

                    current_text.push_str(grapheme);
                    current_width += g_width;
                    byte_cursor += g_len;
                    col_cursor += g_width;
                }
                continue;
            }

            current_text.push_str(segment);
            current_width += seg_width;
            byte_cursor += seg_len;
            col_cursor += seg_width;
        }

        if !current_text.is_empty() || current_width > 0 || slices.is_empty() {
            slices.push(WrappedSlice {
                text: current_text,
                start_byte: slice_start_byte,
                start_col: slice_start_col,
                width: current_width,
            });
        }

        slices
    }

    fn cursor_wrap_position(
        line_text: &str,
        max_width: usize,
        cursor_col: usize,
    ) -> (usize, usize) {
        let slices = Self::wrap_line_slices(line_text, max_width);
        if slices.is_empty() {
            return (0, 0);
        }

        for (idx, slice) in slices.iter().enumerate() {
            let end_col = slice.start_col.saturating_add(slice.width);
            if cursor_col <= end_col || idx == slices.len().saturating_sub(1) {
                let col_in_slice = cursor_col.saturating_sub(slice.start_col);
                return (idx, col_in_slice.min(slice.width));
            }
        }

        (0, 0)
    }

    /// Get the visual width of the character immediately before the cursor.
    fn get_prev_char_width(&self) -> usize {
        let cursor = self.editor.cursor();
        if cursor.grapheme == 0 {
            return 0;
        }
        let rope = self.editor.rope();
        let line = rope
            .line(cursor.line)
            .unwrap_or(std::borrow::Cow::Borrowed(""));

        line.graphemes(true)
            .nth(cursor.grapheme - 1)
            .map(display_width)
            .unwrap_or(0)
    }

    /// Ensure the cursor line and column are visible in the viewport.
    fn ensure_cursor_visible(&mut self) {
        let cursor = self.editor.cursor();

        let last_height = self.last_viewport_height.get();

        // Use a default viewport of 20 lines if we haven't rendered yet (height is 0)

        let vp_height = if last_height == 0 { 20 } else { last_height };

        let last_width = self.last_viewport_width.get();

        let vp_width = if last_width == 0 { 80 } else { last_width };

        if self.scroll_top.get() == usize::MAX {
            self.scroll_top.set(0);
        }

        self.ensure_cursor_visible_internal(vp_height, vp_width, cursor);
    }

    fn ensure_cursor_visible_internal(
        &mut self,

        vp_height: usize,

        vp_width: usize,

        cursor: CursorPosition,
    ) {
        let current_top = self.scroll_top.get();

        // Vertical scroll

        if cursor.line < current_top {
            self.scroll_top.set(cursor.line);
        } else if vp_height > 0 && cursor.line >= current_top + vp_height {
            self.scroll_top
                .set(cursor.line.saturating_sub(vp_height - 1));
        }

        // Horizontal scroll

        if !self.soft_wrap {
            let current_left = self.scroll_left.get();

            let visual_col = cursor.visual_col;

            // Scroll left if cursor is before viewport

            if visual_col < current_left {
                self.scroll_left.set(visual_col);
            }
            // Scroll right if cursor is past viewport
            // Need space for gutter if we had access to it, but this is a best effort
            // approximation using the last known width.
            // Effective text width approx vp_width - gutter (assume ~4 for gutter if unknown)
            // But we use raw vp_width here.
            else if vp_width > 0 && visual_col >= current_left + vp_width {
                let candidate_scroll = visual_col.saturating_sub(vp_width - 1);
                let prev_width = self.get_prev_char_width();
                let max_scroll_for_prev = visual_col.saturating_sub(prev_width);

                self.scroll_left
                    .set(candidate_scroll.min(max_scroll_for_prev));
            }
        }
    }
}

impl Widget for TextArea {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width < 1 || area.height < 1 {
            return;
        }

        self.last_viewport_height.set(area.height as usize);

        let deg = frame.buffer.degradation;
        if deg.apply_styling() {
            crate::set_style_area(&mut frame.buffer, area, self.style);
        }

        let gutter_w = self.gutter_width();
        let text_area_x = area.x.saturating_add(gutter_w);
        let text_area_w = area.width.saturating_sub(gutter_w) as usize;
        let vp_height = area.height as usize;

        self.last_viewport_width.set(text_area_w);

        let cursor = self.editor.cursor();
        // Use a mutable copy for scroll adjustment
        let mut scroll_top = if self.scroll_top.get() == usize::MAX {
            0
        } else {
            self.scroll_top.get()
        };
        if vp_height > 0 {
            if cursor.line < scroll_top {
                scroll_top = cursor.line;
            } else if cursor.line >= scroll_top + vp_height {
                scroll_top = cursor.line.saturating_sub(vp_height - 1);
            }
        }
        self.scroll_top.set(scroll_top);

        let mut scroll_left = self.scroll_left.get();
        if !self.soft_wrap && text_area_w > 0 {
            let visual_col = cursor.visual_col;
            if visual_col < scroll_left {
                scroll_left = visual_col;
            } else if visual_col >= scroll_left + text_area_w {
                let candidate_scroll = visual_col.saturating_sub(text_area_w - 1);
                let prev_width = self.get_prev_char_width();
                let max_scroll_for_prev = visual_col.saturating_sub(prev_width);

                scroll_left = candidate_scroll.min(max_scroll_for_prev);
            }
        }
        self.scroll_left.set(scroll_left);

        let rope = self.editor.rope();
        let nav = CursorNavigator::new(rope);

        // Selection byte range for highlighting
        let sel_range = self.editor.selection().and_then(|sel| {
            if sel.is_empty() {
                None
            } else {
                let (a, b) = sel.byte_range(&nav);
                Some((a, b))
            }
        });

        // Show placeholder if empty
        if self.editor.is_empty() && !self.placeholder.is_empty() {
            let style = if deg.apply_styling() {
                self.placeholder_style
            } else {
                Style::default()
            };
            draw_text_span(
                frame,
                text_area_x,
                area.y,
                &self.placeholder,
                style,
                area.right(),
            );
            if self.focused {
                frame.set_cursor(Some((text_area_x, area.y)));
            }
            return;
        }

        if self.soft_wrap {
            self.scroll_left.set(0);

            // Compute cursor virtual position (wrapped lines)
            let mut cursor_virtual = 0;
            for line_idx in 0..cursor.line {
                let line_text = rope
                    .line(line_idx)
                    .unwrap_or(std::borrow::Cow::Borrowed(""));
                let line_text = line_text.strip_suffix('\n').unwrap_or(&line_text);
                cursor_virtual += Self::measure_wrap_count(line_text, text_area_w);
            }

            let cursor_line_text = rope
                .line(cursor.line)
                .unwrap_or(std::borrow::Cow::Borrowed(""));
            let cursor_line_text = cursor_line_text
                .strip_suffix('\n')
                .unwrap_or(&cursor_line_text);
            let (cursor_wrap_idx, cursor_col_in_wrap) =
                Self::cursor_wrap_position(cursor_line_text, text_area_w, cursor.visual_col);
            cursor_virtual = cursor_virtual.saturating_add(cursor_wrap_idx);

            // Adjust scroll to keep cursor visible
            let mut scroll_virtual = self.scroll_top.get();
            if cursor_virtual < scroll_virtual {
                scroll_virtual = cursor_virtual;
            } else if cursor_virtual >= scroll_virtual + vp_height {
                scroll_virtual = cursor_virtual.saturating_sub(vp_height - 1);
            }
            self.scroll_top.set(scroll_virtual);

            // Render wrapped lines in virtual space
            let mut virtual_index = 0usize;
            for line_idx in 0..self.editor.line_count() {
                if virtual_index >= scroll_virtual + vp_height {
                    break;
                }

                let line_text = rope
                    .line(line_idx)
                    .unwrap_or(std::borrow::Cow::Borrowed(""));
                let line_text = line_text.strip_suffix('\n').unwrap_or(&line_text);

                // Fast path: check if this whole physical line is skipped
                let wrap_count = Self::measure_wrap_count(line_text, text_area_w);
                if virtual_index + wrap_count <= scroll_virtual {
                    virtual_index += wrap_count;
                    continue;
                }

                let line_start_byte = nav.to_byte_index(nav.from_line_grapheme(line_idx, 0));
                let slices = Self::wrap_line_slices(line_text, text_area_w);

                for (slice_idx, slice) in slices.iter().enumerate() {
                    if virtual_index < scroll_virtual {
                        virtual_index += 1;
                        continue;
                    }

                    let row = virtual_index.saturating_sub(scroll_virtual);
                    if row >= vp_height {
                        break;
                    }

                    let y = area.y.saturating_add(row as u16);

                    // Line number gutter (only for first wrapped slice)
                    if self.show_line_numbers && slice_idx == 0 {
                        let style = if deg.apply_styling() {
                            self.line_number_style
                        } else {
                            Style::default()
                        };
                        let num_str =
                            format!("{:>width$} ", line_idx + 1, width = (gutter_w - 2) as usize);
                        draw_text_span(frame, area.x, y, &num_str, style, text_area_x);
                    }

                    // Cursor line highlight (only for the active wrapped slice)
                    if line_idx == cursor.line
                        && slice_idx == cursor_wrap_idx
                        && let Some(cl_style) = self.cursor_line_style
                        && deg.apply_styling()
                    {
                        for cx in text_area_x..area.right() {
                            if let Some(cell) = frame.buffer.get_mut(cx, y) {
                                apply_style(cell, cl_style);
                            }
                        }
                    }

                    // Render graphemes inside the wrapped slice
                    let mut visual_x: usize = 0;
                    let mut grapheme_byte_offset = line_start_byte + slice.start_byte;

                    for g in slice.text.graphemes(true) {
                        let g_width = display_width(g);
                        let g_byte_len = g.len();

                        if visual_x >= text_area_w {
                            break;
                        }

                        let px = text_area_x + visual_x as u16;

                        // Determine style (selection highlight)
                        let mut g_style = self.style;
                        if let Some((sel_start, sel_end)) = sel_range
                            && grapheme_byte_offset >= sel_start
                            && grapheme_byte_offset < sel_end
                            && deg.apply_styling()
                        {
                            g_style = g_style.merge(&self.selection_style);
                        }

                        if g_width > 0 {
                            draw_text_span(frame, px, y, g, g_style, area.right());
                        }

                        visual_x += g_width;
                        grapheme_byte_offset += g_byte_len;
                    }

                    virtual_index += 1;
                }
            }

            // Set cursor position if focused
            if self.focused && cursor_virtual >= scroll_virtual {
                let row = cursor_virtual.saturating_sub(scroll_virtual);
                if row < vp_height {
                    let cursor_screen_x = text_area_x.saturating_add(cursor_col_in_wrap as u16);
                    let cursor_screen_y = area.y.saturating_add(row as u16);
                    if cursor_screen_x < area.right() && cursor_screen_y < area.bottom() {
                        frame.set_cursor(Some((cursor_screen_x, cursor_screen_y)));
                    }
                }
            }

            return;
        }

        // Render visible lines (no soft wrap)
        for row in 0..vp_height {
            let line_idx = scroll_top + row;
            let y = area.y.saturating_add(row as u16);

            if line_idx >= self.editor.line_count() {
                break;
            }

            // Line number gutter
            if self.show_line_numbers {
                let style = if deg.apply_styling() {
                    self.line_number_style
                } else {
                    Style::default()
                };
                let num_str = format!("{:>width$} ", line_idx + 1, width = (gutter_w - 2) as usize);
                draw_text_span(frame, area.x, y, &num_str, style, text_area_x);
            }

            // Cursor line highlight
            if line_idx == cursor.line
                && let Some(cl_style) = self.cursor_line_style
                && deg.apply_styling()
            {
                for cx in text_area_x..area.right() {
                    if let Some(cell) = frame.buffer.get_mut(cx, y) {
                        apply_style(cell, cl_style);
                    }
                }
            }

            // Get line text
            let line_text = rope
                .line(line_idx)
                .unwrap_or(std::borrow::Cow::Borrowed(""));
            let line_text = line_text.strip_suffix('\n').unwrap_or(&line_text);

            // Calculate line byte offset for selection mapping
            let line_start_byte = nav.to_byte_index(nav.from_line_grapheme(line_idx, 0));

            // Render each grapheme
            let mut visual_x: usize = 0;
            let graphemes: Vec<&str> = line_text.graphemes(true).collect();
            let mut grapheme_byte_offset = line_start_byte;

            for g in &graphemes {
                let g_width = display_width(g);
                let g_byte_len = g.len();

                // Skip graphemes before horizontal scroll
                if visual_x + g_width <= scroll_left {
                    visual_x += g_width;
                    grapheme_byte_offset += g_byte_len;
                    continue;
                }

                // Handle partial overlap at left edge
                if visual_x < scroll_left {
                    visual_x += g_width;
                    grapheme_byte_offset += g_byte_len;
                    continue;
                }

                // Stop if past viewport
                let screen_x = visual_x.saturating_sub(scroll_left);
                if screen_x >= text_area_w {
                    break;
                }

                let px = text_area_x + screen_x as u16;

                // Determine style (selection highlight)
                let mut g_style = self.style;
                if let Some((sel_start, sel_end)) = sel_range
                    && grapheme_byte_offset >= sel_start
                    && grapheme_byte_offset < sel_end
                    && deg.apply_styling()
                {
                    g_style = g_style.merge(&self.selection_style);
                }

                // Write grapheme to buffer
                if g_width > 0 {
                    draw_text_span(frame, px, y, g, g_style, area.right());
                }

                visual_x += g_width;
                grapheme_byte_offset += g_byte_len;
            }
        }

        // Set cursor position if focused
        if self.focused {
            let cursor_row = cursor.line.saturating_sub(scroll_top);
            if cursor_row < vp_height {
                let cursor_screen_x = (cursor.visual_col.saturating_sub(scroll_left) as u16)
                    .saturating_add(text_area_x);
                let cursor_screen_y = area.y.saturating_add(cursor_row as u16);
                if cursor_screen_x < area.right() && cursor_screen_y < area.bottom() {
                    frame.set_cursor(Some((cursor_screen_x, cursor_screen_y)));
                }
            }
        }
    }

    fn is_essential(&self) -> bool {
        true
    }
}

impl StatefulWidget for TextArea {
    type State = TextAreaState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        state.last_viewport_height = area.height;
        state.last_viewport_width = area.width;
        Widget::render(self, area, frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_textarea_is_empty() {
        let ta = TextArea::new();
        assert!(ta.is_empty());
        assert_eq!(ta.text(), "");
        assert_eq!(ta.line_count(), 1); // empty rope has 1 line
    }

    #[test]
    fn with_text_builder() {
        let ta = TextArea::new().with_text("hello\nworld");
        assert_eq!(ta.text(), "hello\nworld");
        assert_eq!(ta.line_count(), 2);
    }

    #[test]
    fn insert_text_and_newline() {
        let mut ta = TextArea::new();
        ta.insert_text("hello");
        ta.insert_newline();
        ta.insert_text("world");
        assert_eq!(ta.text(), "hello\nworld");
        assert_eq!(ta.line_count(), 2);
    }

    #[test]
    fn delete_backward_works() {
        let mut ta = TextArea::new().with_text("hello");
        ta.move_to_document_end();
        ta.delete_backward();
        assert_eq!(ta.text(), "hell");
    }

    #[test]
    fn cursor_movement() {
        let mut ta = TextArea::new().with_text("abc\ndef\nghi");
        ta.move_to_document_start();
        assert_eq!(ta.cursor().line, 0);
        assert_eq!(ta.cursor().grapheme, 0);

        ta.move_down();
        assert_eq!(ta.cursor().line, 1);

        ta.move_to_line_end();
        assert_eq!(ta.cursor().grapheme, 3);

        ta.move_to_document_end();
        assert_eq!(ta.cursor().line, 2);
    }

    #[test]
    fn undo_redo() {
        let mut ta = TextArea::new();
        ta.insert_text("abc");
        assert_eq!(ta.text(), "abc");
        ta.undo();
        assert_eq!(ta.text(), "");
        ta.redo();
        assert_eq!(ta.text(), "abc");
    }

    #[test]
    fn selection_and_delete() {
        let mut ta = TextArea::new().with_text("hello world");
        ta.move_to_document_start();
        for _ in 0..5 {
            ta.select_right();
        }
        assert_eq!(ta.selected_text(), Some("hello".to_string()));
        ta.delete_backward();
        assert_eq!(ta.text(), " world");
    }

    #[test]
    fn select_all() {
        let mut ta = TextArea::new().with_text("abc\ndef");
        ta.select_all();
        assert_eq!(ta.selected_text(), Some("abc\ndef".to_string()));
    }

    #[test]
    fn set_text_resets() {
        let mut ta = TextArea::new().with_text("old");
        ta.insert_text(" stuff");
        ta.set_text("new");
        assert_eq!(ta.text(), "new");
    }

    #[test]
    fn scroll_follows_cursor() {
        let mut ta = TextArea::new();
        // Insert many lines
        for i in 0..50 {
            ta.insert_text(&format!("line {}\n", i));
        }
        // Cursor should be at the bottom, scroll_top adjusted
        assert!(ta.scroll_top.get() > 0);
        assert!(ta.cursor().line >= 49);

        // Move to top
        ta.move_to_document_start();
        assert_eq!(ta.scroll_top.get(), 0);
    }

    #[test]
    fn gutter_width_without_line_numbers() {
        let ta = TextArea::new();
        assert_eq!(ta.gutter_width(), 0);
    }

    #[test]
    fn gutter_width_with_line_numbers() {
        let mut ta = TextArea::new().with_line_numbers(true);
        ta.insert_text("a\nb\nc");
        assert_eq!(ta.gutter_width(), 3); // 1 digit + space + separator
    }

    #[test]
    fn gutter_width_many_lines() {
        let mut ta = TextArea::new().with_line_numbers(true);
        for i in 0..100 {
            ta.insert_text(&format!("line {}\n", i));
        }
        assert_eq!(ta.gutter_width(), 5); // 3 digits + space + separator
    }

    #[test]
    fn focus_state() {
        let mut ta = TextArea::new();
        assert!(!ta.is_focused());
        ta.set_focused(true);
        assert!(ta.is_focused());
    }

    #[test]
    fn word_movement() {
        let mut ta = TextArea::new().with_text("hello world foo");
        ta.move_to_document_start();
        ta.move_word_right();
        assert_eq!(ta.cursor().grapheme, 6);
        ta.move_word_left();
        assert_eq!(ta.cursor().grapheme, 0);
    }

    #[test]
    fn page_up_down() {
        let mut ta = TextArea::new();
        for i in 0..50 {
            ta.insert_text(&format!("line {}\n", i));
        }
        ta.move_to_document_start();
        let state = TextAreaState {
            last_viewport_height: 10,
            last_viewport_width: 80,
        };
        ta.page_down(&state);
        assert!(ta.cursor().line >= 10);
        ta.page_up(&state);
        assert_eq!(ta.cursor().line, 0);
    }

    #[test]
    fn insert_replaces_selection() {
        let mut ta = TextArea::new().with_text("hello world");
        ta.move_to_document_start();
        for _ in 0..5 {
            ta.select_right();
        }
        ta.insert_text("goodbye");
        assert_eq!(ta.text(), "goodbye world");
    }

    #[test]
    fn insert_single_char() {
        let mut ta = TextArea::new();
        ta.insert_char('X');
        assert_eq!(ta.text(), "X");
        assert_eq!(ta.cursor().grapheme, 1);
    }

    #[test]
    fn insert_multiline_text() {
        let mut ta = TextArea::new();
        ta.insert_text("line1\nline2\nline3");
        assert_eq!(ta.line_count(), 3);
        assert_eq!(ta.cursor().line, 2);
    }

    #[test]
    fn delete_forward_works() {
        let mut ta = TextArea::new().with_text("hello");
        ta.move_to_document_start();
        ta.delete_forward();
        assert_eq!(ta.text(), "ello");
    }

    #[test]
    fn delete_backward_at_line_start_joins_lines() {
        let mut ta = TextArea::new().with_text("abc\ndef");
        // Move to start of line 2
        ta.move_to_document_start();
        ta.move_down();
        ta.move_to_line_start();
        ta.delete_backward();
        assert_eq!(ta.text(), "abcdef");
        assert_eq!(ta.line_count(), 1);
    }

    #[test]
    fn cursor_horizontal_movement() {
        let mut ta = TextArea::new().with_text("abc");
        ta.move_to_document_start();
        ta.move_right();
        assert_eq!(ta.cursor().grapheme, 1);
        ta.move_right();
        assert_eq!(ta.cursor().grapheme, 2);
        ta.move_left();
        assert_eq!(ta.cursor().grapheme, 1);
    }

    #[test]
    fn cursor_vertical_maintains_column() {
        let mut ta = TextArea::new().with_text("abcde\nfg\nhijkl");
        ta.move_to_document_start();
        ta.move_to_line_end(); // col 5
        ta.move_down(); // line 1 only has 2 chars, should clamp
        assert_eq!(ta.cursor().line, 1);
        ta.move_down(); // line 2 has 5 chars, should restore col
        assert_eq!(ta.cursor().line, 2);
    }

    #[test]
    fn selection_shift_arrow() {
        let mut ta = TextArea::new().with_text("abcdef");
        ta.move_to_document_start();
        ta.select_right();
        ta.select_right();
        ta.select_right();
        assert_eq!(ta.selected_text(), Some("abc".to_string()));
    }

    #[test]
    fn selection_extends_up_down() {
        let mut ta = TextArea::new().with_text("line1\nline2\nline3");
        ta.move_to_document_start();
        ta.select_down();
        let sel = ta.selected_text().unwrap();
        assert!(sel.contains('\n'));
    }

    #[test]
    fn undo_chain() {
        let mut ta = TextArea::new();
        ta.insert_text("a");
        ta.insert_text("b");
        ta.insert_text("c");
        assert_eq!(ta.text(), "abc");
        ta.undo();
        ta.undo();
        ta.undo();
        assert_eq!(ta.text(), "");
    }

    #[test]
    fn redo_discarded_on_new_edit() {
        let mut ta = TextArea::new();
        ta.insert_text("abc");
        ta.undo();
        ta.insert_text("xyz");
        ta.redo(); // should be no-op
        assert_eq!(ta.text(), "xyz");
    }

    #[test]
    fn clear_selection() {
        let mut ta = TextArea::new().with_text("hello");
        ta.select_all();
        assert!(ta.selection().is_some());
        ta.clear_selection();
        assert!(ta.selection().is_none());
    }

    #[test]
    fn delete_word_backward() {
        let mut ta = TextArea::new().with_text("hello world");
        ta.move_to_document_end();
        ta.delete_word_backward();
        assert_eq!(ta.text(), "hello ");
    }

    #[test]
    fn delete_to_end_of_line() {
        let mut ta = TextArea::new().with_text("hello world");
        ta.move_to_document_start();
        ta.move_right(); // after 'h'
        ta.delete_to_end_of_line();
        assert_eq!(ta.text(), "h");
    }

    #[test]
    fn placeholder_builder() {
        let ta = TextArea::new().with_placeholder("Enter text...");
        assert!(ta.is_empty());
        assert_eq!(ta.placeholder, "Enter text...");
    }

    #[test]
    fn soft_wrap_builder() {
        let ta = TextArea::new().with_soft_wrap(true);
        assert!(ta.soft_wrap);
    }

    #[test]
    fn soft_wrap_renders_wrapped_lines() {
        use crate::Widget;
        use ftui_render::grapheme_pool::GraphemePool;

        let ta = TextArea::new().with_soft_wrap(true).with_text("abcdef");
        let area = Rect::new(0, 0, 3, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 2, &mut pool);
        Widget::render(&ta, area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('a'));
        assert_eq!(frame.buffer.get(2, 0).unwrap().content.as_char(), Some('c'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('d'));
        assert_eq!(frame.buffer.get(2, 1).unwrap().content.as_char(), Some('f'));
    }

    #[test]
    fn max_height_builder() {
        let ta = TextArea::new().with_max_height(10);
        assert_eq!(ta.max_height, 10);
    }

    #[test]
    fn editor_access() {
        let mut ta = TextArea::new().with_text("test");
        assert_eq!(ta.editor().text(), "test");
        ta.editor_mut().insert_char('!');
        assert!(ta.text().contains('!'));
    }

    #[test]
    fn move_to_line_start_and_end() {
        let mut ta = TextArea::new().with_text("hello world");
        ta.move_to_document_start();
        ta.move_to_line_end();
        assert_eq!(ta.cursor().grapheme, 11);
        ta.move_to_line_start();
        assert_eq!(ta.cursor().grapheme, 0);
    }

    #[test]
    fn render_empty_with_placeholder() {
        use ftui_render::grapheme_pool::GraphemePool;
        let ta = TextArea::new()
            .with_placeholder("Type here")
            .with_focus(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);
        let area = Rect::new(0, 0, 20, 5);
        Widget::render(&ta, area, &mut frame);
        // Placeholder should be rendered
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('T'));
        // Cursor should be set
        assert!(frame.cursor_position.is_some());
    }

    #[test]
    fn render_with_content() {
        use ftui_render::grapheme_pool::GraphemePool;
        let ta = TextArea::new().with_text("abc\ndef").with_focus(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);
        let area = Rect::new(0, 0, 20, 5);
        Widget::render(&ta, area, &mut frame);
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('a'));
    }

    #[test]
    fn render_line_numbers_without_styling() {
        use ftui_render::budget::DegradationLevel;
        use ftui_render::grapheme_pool::GraphemePool;

        let ta = TextArea::new().with_text("a\nb").with_line_numbers(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(8, 2, &mut pool);
        frame.set_degradation(DegradationLevel::NoStyling);

        Widget::render(&ta, Rect::new(0, 0, 8, 2), &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('1'));
    }

    #[test]
    fn stateful_render_updates_viewport_state() {
        use ftui_render::grapheme_pool::GraphemePool;

        let ta = TextArea::new();
        let mut state = TextAreaState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let area = Rect::new(0, 0, 10, 3);

        StatefulWidget::render(&ta, area, &mut frame, &mut state);

        assert_eq!(state.last_viewport_height, 3);
        assert_eq!(state.last_viewport_width, 10);
    }

    #[test]
    fn render_zero_area_no_panic() {
        let ta = TextArea::new().with_text("test");
        use ftui_render::grapheme_pool::GraphemePool;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        Widget::render(&ta, Rect::new(0, 0, 0, 0), &mut frame);
    }

    #[test]
    fn is_essential() {
        let ta = TextArea::new();
        assert!(Widget::is_essential(&ta));
    }

    #[test]
    fn default_impl() {
        let ta = TextArea::default();
        assert!(ta.is_empty());
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut ta = TextArea::new().with_text("abcdef");
        ta.move_to_document_start();
        ta.move_right();
        ta.move_right();
        ta.move_right();
        ta.insert_newline();
        assert_eq!(ta.line_count(), 2);
        assert_eq!(ta.cursor().line, 1);
    }

    #[test]
    fn unicode_grapheme_cluster() {
        let mut ta = TextArea::new();
        ta.insert_text("café");
        // 'é' is a single grapheme even if composed
        assert_eq!(ta.text(), "café");
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn insert_delete_inverse(text in "[a-zA-Z0-9 ]{1,50}") {
                let mut ta = TextArea::new();
                ta.insert_text(&text);
                // Delete all characters backwards
                for _ in 0..text.len() {
                    ta.delete_backward();
                }
                prop_assert!(ta.is_empty() || ta.text().is_empty());
            }

            #[test]
            fn undo_redo_inverse(text in "[a-zA-Z0-9]{1,30}") {
                let mut ta = TextArea::new();
                ta.insert_text(&text);
                let after_insert = ta.text();
                ta.undo();
                ta.redo();
                prop_assert_eq!(ta.text(), after_insert);
            }

            #[test]
            fn cursor_always_valid(ops in proptest::collection::vec(0u8..10, 1..20)) {
                let mut ta = TextArea::new().with_text("abc\ndef\nghi\njkl");
                for op in ops {
                    match op {
                        0 => ta.move_left(),
                        1 => ta.move_right(),
                        2 => ta.move_up(),
                        3 => ta.move_down(),
                        4 => ta.move_to_line_start(),
                        5 => ta.move_to_line_end(),
                        6 => ta.move_to_document_start(),
                        7 => ta.move_to_document_end(),
                        8 => ta.move_word_left(),
                        _ => ta.move_word_right(),
                    }
                    let cursor = ta.cursor();
                    prop_assert!(cursor.line < ta.line_count(),
                        "cursor line {} >= line_count {}", cursor.line, ta.line_count());
                }
            }

            #[test]
            fn selection_ordered(n in 1usize..20) {
                let mut ta = TextArea::new().with_text("hello world foo bar");
                ta.move_to_document_start();
                for _ in 0..n {
                    ta.select_right();
                }
                if let Some(sel) = ta.selection() {
                    // When selecting right from start, anchor should be at/before head
                    prop_assert!(sel.anchor.line <= sel.head.line
                        || (sel.anchor.line == sel.head.line
                            && sel.anchor.grapheme <= sel.head.grapheme));
                }
            }
        }
    }
}

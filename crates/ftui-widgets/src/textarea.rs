//! Multi-line text editing widget.
//!
//! [`TextArea`] wraps [`Editor`](ftui_text::Editor) for text manipulation and
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

use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::editor::{Editor, Selection};
use ftui_text::wrap::display_width;
use ftui_text::{CursorNavigator, CursorPosition};
use unicode_segmentation::UnicodeSegmentation;

use crate::{Widget, apply_style, draw_text_span};

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
    soft_wrap: bool,
    /// Maximum height in lines (0 = unlimited / fill area).
    max_height: usize,
    /// Viewport scroll offset (first visible line).
    scroll_top: usize,
    /// Horizontal scroll offset (visual columns).
    scroll_left: usize,
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
            scroll_top: usize::MAX, // sentinel: will be set on first render
            scroll_left: 0,
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
        self.scroll_top = 0;
        self.scroll_left = 0;
    }

    /// Number of lines.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.editor.line_count()
    }

    /// Current cursor position.
    #[must_use]
    pub fn cursor(&self) -> CursorPosition {
        self.editor.cursor()
    }

    /// Whether the textarea is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }

    /// Current selection, if any.
    #[must_use]
    pub fn selection(&self) -> Option<Selection> {
        self.editor.selection()
    }

    /// Get selected text.
    #[must_use]
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
        let digits = if self.line_count() == 0 {
            1
        } else {
            (self.line_count() as f64).log10().floor() as u16 + 1
        };
        digits + 2 // digit width + space + separator
    }

    /// Ensure the cursor line and column are visible in the viewport.
    fn ensure_cursor_visible(&mut self) {
        let cursor = self.editor.cursor();
        // Use a default viewport of 20 lines if we haven't rendered yet
        let vp_height = if self.scroll_top == usize::MAX {
            self.scroll_top = 0;
            20usize
        } else {
            20usize // Will be overridden in render, but safe default
        };
        self.ensure_cursor_visible_with_height(vp_height, cursor);
    }

    fn ensure_cursor_visible_with_height(&mut self, vp_height: usize, cursor: CursorPosition) {
        if vp_height == 0 {
            return;
        }
        // Vertical scroll
        if cursor.line < self.scroll_top {
            self.scroll_top = cursor.line;
        } else if cursor.line >= self.scroll_top + vp_height {
            self.scroll_top = cursor.line.saturating_sub(vp_height - 1);
        }
        // Horizontal scroll (only in no-wrap mode)
        if !self.soft_wrap {
            let visual_col = cursor.visual_col;
            if visual_col < self.scroll_left {
                self.scroll_left = visual_col;
            } else if visual_col >= self.scroll_left + 40 {
                // Rough heuristic; actual width comes from render
                self.scroll_left = visual_col.saturating_sub(39);
            }
        }
    }
}

impl Widget for TextArea {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width < 1 || area.height < 1 {
            return;
        }

        let deg = frame.buffer.degradation;
        if deg.apply_styling() {
            crate::set_style_area(&mut frame.buffer, area, self.style);
        }

        let gutter_w = self.gutter_width();
        let text_area_x = area.x + gutter_w;
        let text_area_w = area.width.saturating_sub(gutter_w) as usize;
        let vp_height = area.height as usize;

        // Use a mutable copy for scroll adjustment
        let scroll_top = if self.scroll_top == usize::MAX {
            0
        } else {
            self.scroll_top
        };

        let cursor = self.editor.cursor();
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

        // Render visible lines
        for row in 0..vp_height {
            let line_idx = scroll_top + row;
            let y = area.y + row as u16;

            if line_idx >= self.editor.line_count() {
                break;
            }

            // Line number gutter
            if self.show_line_numbers && deg.apply_styling() {
                let num_str = format!("{:>width$} ", line_idx + 1, width = (gutter_w - 2) as usize);
                draw_text_span(
                    frame,
                    area.x,
                    y,
                    &num_str,
                    self.line_number_style,
                    text_area_x,
                );
            }

            // Cursor line highlight
            if line_idx == cursor.line
                && let Some(cl_style) = self.cursor_line_style
                    && deg.apply_styling() {
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
            let line_start_byte =
                nav.to_byte_index(CursorNavigator::new(rope).from_line_grapheme(line_idx, 0));

            // Render each grapheme
            let mut visual_x: usize = 0;
            let graphemes: Vec<&str> = line_text.graphemes(true).collect();
            let mut grapheme_byte_offset = line_start_byte;

            for g in &graphemes {
                let g_width = display_width(g);
                let g_byte_len = g.len();

                // Skip graphemes before horizontal scroll
                if visual_x + g_width <= self.scroll_left {
                    visual_x += g_width;
                    grapheme_byte_offset += g_byte_len;
                    continue;
                }

                // Stop if past viewport
                let screen_x = visual_x.saturating_sub(self.scroll_left);
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
                let cursor_screen_x =
                    cursor.visual_col.saturating_sub(self.scroll_left) as u16 + text_area_x;
                let cursor_screen_y = area.y + cursor_row as u16;
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
        assert!(ta.scroll_top > 0);
        assert!(ta.cursor().line >= 49);

        // Move to top
        ta.move_to_document_start();
        assert_eq!(ta.scroll_top, 0);
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
        assert_eq!(ta.cursor().grapheme, 5);
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
}

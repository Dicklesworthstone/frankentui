#![forbid(unsafe_code)]

//! Core text editing operations on top of Rope + CursorNavigator.
//!
//! [`Editor`] combines a [`Rope`] with a [`CursorPosition`] and provides
//! the standard editing operations (insert, delete, cursor movement) that
//! power TextArea and other editing widgets.
//!
//! # Example
//! ```
//! use ftui_text::editor::Editor;
//!
//! let mut ed = Editor::new();
//! ed.insert_text("hello");
//! ed.insert_char(' ');
//! ed.insert_text("world");
//! assert_eq!(ed.text(), "hello world");
//!
//! // Move cursor and delete
//! ed.move_left();
//! ed.move_left();
//! ed.move_left();
//! ed.move_left();
//! ed.move_left();
//! ed.delete_backward(); // deletes the space
//! assert_eq!(ed.text(), "helloworld");
//! ```

use crate::cursor::{CursorNavigator, CursorPosition};
use crate::rope::Rope;

/// A single edit operation for undo/redo.
#[derive(Debug, Clone)]
enum EditOp {
    Insert { byte_offset: usize, text: String },
    Delete { byte_offset: usize, text: String },
}

impl EditOp {
    fn inverse(&self) -> Self {
        match self {
            Self::Insert { byte_offset, text } => Self::Delete {
                byte_offset: *byte_offset,
                text: text.clone(),
            },
            Self::Delete { byte_offset, text } => Self::Insert {
                byte_offset: *byte_offset,
                text: text.clone(),
            },
        }
    }
}

/// Selection defined by anchor (fixed) and head (moving with cursor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The fixed end of the selection.
    pub anchor: CursorPosition,
    /// The moving end (same as cursor).
    pub head: CursorPosition,
}

impl Selection {
    /// Byte range of the selection (start, end) where start <= end.
    #[must_use]
    pub fn byte_range(&self, nav: &CursorNavigator<'_>) -> (usize, usize) {
        let a = nav.to_byte_index(self.anchor);
        let b = nav.to_byte_index(self.head);
        if a <= b { (a, b) } else { (b, a) }
    }

    /// Whether the selection is empty (anchor == head).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Core text editor combining Rope storage with cursor management.
///
/// Provides insert/delete/move operations with grapheme-aware cursor
/// handling, undo/redo, and selection support.
/// Cursor is always kept in valid bounds.
#[derive(Debug, Clone)]
pub struct Editor {
    /// The text buffer.
    rope: Rope,
    /// Current cursor position.
    cursor: CursorPosition,
    /// Active selection (None when no selection).
    selection: Option<Selection>,
    /// Undo stack: (operation, cursor-before).
    undo_stack: Vec<(EditOp, CursorPosition)>,
    /// Redo stack: (operation, cursor-before).
    redo_stack: Vec<(EditOp, CursorPosition)>,
    /// Maximum undo history depth.
    max_history: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// Create an empty editor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            cursor: CursorPosition::default(),
            selection: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_history: 1000,
        }
    }

    /// Create an editor with initial text. Cursor starts at the end.
    #[must_use]
    pub fn with_text(text: &str) -> Self {
        let rope = Rope::from_text(text);
        let nav = CursorNavigator::new(&rope);
        let cursor = nav.document_end();
        Self {
            rope,
            cursor,
            selection: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_history: 1000,
        }
    }

    /// Set the maximum undo history depth.
    pub fn set_max_history(&mut self, max: usize) {
        self.max_history = max;
    }

    /// Get the full text content as a string.
    #[must_use]
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Get a reference to the underlying rope.
    #[must_use]
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Get the current cursor position.
    #[must_use]
    pub fn cursor(&self) -> CursorPosition {
        self.cursor
    }

    /// Set cursor position (will be clamped to valid bounds). Clears selection.
    pub fn set_cursor(&mut self, pos: CursorPosition) {
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.clamp(pos);
        self.selection = None;
    }

    /// Current selection, if any.
    #[must_use]
    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// Whether undo is available.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether redo is available.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Check if the editor is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rope.is_empty()
    }

    /// Number of lines in the buffer.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Get the text of a specific line (without trailing newline).
    #[must_use]
    pub fn line_text(&self, line: usize) -> Option<String> {
        self.rope.line(line).map(|cow| {
            let s = cow.as_ref();
            s.trim_end_matches('\n').trim_end_matches('\r').to_string()
        })
    }

    // ====================================================================
    // Insert operations
    // ====================================================================

    /// Insert a single character at the cursor position.
    pub fn insert_char(&mut self, ch: char) {
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        self.insert_text(s);
    }

    /// Insert text at the cursor position. Deletes selection first if active.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.delete_selection_inner();
        let nav = CursorNavigator::new(&self.rope);
        let byte_idx = nav.to_byte_index(self.cursor);
        let char_idx = self.rope.byte_to_char(byte_idx);

        self.push_undo(EditOp::Insert {
            byte_offset: byte_idx,
            text: text.to_string(),
        });

        self.rope.insert(char_idx, text);

        // Move cursor to end of inserted text
        let new_byte_idx = byte_idx + text.len();
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.from_byte_index(new_byte_idx);
    }

    /// Insert a newline at the cursor position.
    pub fn insert_newline(&mut self) {
        self.insert_text("\n");
    }

    // ====================================================================
    // Delete operations
    // ====================================================================

    /// Delete the character before the cursor (backspace). Deletes selection if active.
    ///
    /// Returns `true` if a character was deleted.
    pub fn delete_backward(&mut self) -> bool {
        if self.delete_selection_inner() {
            return true;
        }
        let nav = CursorNavigator::new(&self.rope);
        let old_pos = self.cursor;
        let new_pos = nav.move_left(old_pos);

        if new_pos == old_pos {
            return false; // At beginning, nothing to delete
        }

        let start_byte = nav.to_byte_index(new_pos);
        let end_byte = nav.to_byte_index(old_pos);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.push_undo(EditOp::Delete {
            byte_offset: start_byte,
            text: deleted,
        });

        self.rope.remove(start_char..end_char);

        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.from_byte_index(start_byte);
        true
    }

    /// Delete the character after the cursor (delete key). Deletes selection if active.
    ///
    /// Returns `true` if a character was deleted.
    pub fn delete_forward(&mut self) -> bool {
        if self.delete_selection_inner() {
            return true;
        }
        let nav = CursorNavigator::new(&self.rope);
        let old_pos = self.cursor;
        let next_pos = nav.move_right(old_pos);

        if next_pos == old_pos {
            return false; // At end, nothing to delete
        }

        let start_byte = nav.to_byte_index(old_pos);
        let end_byte = nav.to_byte_index(next_pos);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.push_undo(EditOp::Delete {
            byte_offset: start_byte,
            text: deleted,
        });

        self.rope.remove(start_char..end_char);

        // Cursor stays at same position, just re-clamp
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.clamp(self.cursor);
        true
    }

    /// Delete the word before the cursor (Ctrl+Backspace).
    ///
    /// Returns `true` if any text was deleted.
    pub fn delete_word_backward(&mut self) -> bool {
        if self.delete_selection_inner() {
            return true;
        }
        let nav = CursorNavigator::new(&self.rope);
        let old_pos = self.cursor;
        let word_start = nav.move_word_left(old_pos);

        if word_start == old_pos {
            return false;
        }

        let start_byte = nav.to_byte_index(word_start);
        let end_byte = nav.to_byte_index(old_pos);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.push_undo(EditOp::Delete {
            byte_offset: start_byte,
            text: deleted,
        });

        self.rope.remove(start_char..end_char);

        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.from_byte_index(start_byte);
        true
    }

    /// Delete from cursor to end of line (Ctrl+K).
    ///
    /// Returns `true` if any text was deleted.
    pub fn delete_to_end_of_line(&mut self) -> bool {
        if self.delete_selection_inner() {
            return true;
        }
        let nav = CursorNavigator::new(&self.rope);
        let old_pos = self.cursor;
        let line_end = nav.line_end(old_pos);

        if line_end == old_pos {
            // At end of line: delete the newline to join lines
            return self.delete_forward();
        }

        let start_byte = nav.to_byte_index(old_pos);
        let end_byte = nav.to_byte_index(line_end);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.push_undo(EditOp::Delete {
            byte_offset: start_byte,
            text: deleted,
        });

        self.rope.remove(start_char..end_char);

        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.clamp(self.cursor);
        true
    }

    // ====================================================================
    // Undo / redo
    // ====================================================================

    /// Push an edit operation onto the undo stack.
    fn push_undo(&mut self, op: EditOp) {
        self.undo_stack.push((op, self.cursor));
        if self.undo_stack.len() > self.max_history {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// Undo the last edit operation.
    pub fn undo(&mut self) -> bool {
        let Some((op, cursor_before)) = self.undo_stack.pop() else {
            return false;
        };
        let inverse = op.inverse();
        self.apply_op(&inverse);
        self.redo_stack.push((inverse, self.cursor));
        self.cursor = cursor_before;
        true
    }

    /// Redo the last undone operation.
    pub fn redo(&mut self) -> bool {
        let Some((op, cursor_before)) = self.redo_stack.pop() else {
            return false;
        };
        let inverse = op.inverse();
        self.apply_op(&inverse);
        self.undo_stack.push((inverse, self.cursor));
        self.cursor = cursor_before;
        // Move cursor to the correct position after redo
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.clamp(self.cursor);
        true
    }

    /// Apply an edit operation directly to the rope.
    fn apply_op(&mut self, op: &EditOp) {
        match op {
            EditOp::Insert { byte_offset, text } => {
                let char_idx = self.rope.byte_to_char(*byte_offset);
                self.rope.insert(char_idx, text);
            }
            EditOp::Delete { byte_offset, text } => {
                let start_char = self.rope.byte_to_char(*byte_offset);
                let end_char = self.rope.byte_to_char(*byte_offset + text.len());
                self.rope.remove(start_char..end_char);
            }
        }
    }

    // ====================================================================
    // Selection helpers
    // ====================================================================

    /// Delete the current selection if active. Returns true if something was deleted.
    fn delete_selection_inner(&mut self) -> bool {
        let Some(sel) = self.selection.take() else {
            return false;
        };
        if sel.is_empty() {
            return false;
        }
        let nav = CursorNavigator::new(&self.rope);
        let (start_byte, end_byte) = sel.byte_range(&nav);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.push_undo(EditOp::Delete {
            byte_offset: start_byte,
            text: deleted,
        });

        self.rope.remove(start_char..end_char);
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.from_byte_index(start_byte);
        true
    }

    // ====================================================================
    // Cursor movement (clears selection)
    // ====================================================================

    /// Move cursor left by one grapheme.
    pub fn move_left(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_left(self.cursor);
    }

    /// Move cursor right by one grapheme.
    pub fn move_right(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_right(self.cursor);
    }

    /// Move cursor up one line.
    pub fn move_up(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_up(self.cursor);
    }

    /// Move cursor down one line.
    pub fn move_down(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_down(self.cursor);
    }

    /// Move cursor left by one word.
    pub fn move_word_left(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_word_left(self.cursor);
    }

    /// Move cursor right by one word.
    pub fn move_word_right(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_word_right(self.cursor);
    }

    /// Move cursor to start of line.
    pub fn move_to_line_start(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.line_start(self.cursor);
    }

    /// Move cursor to end of line.
    pub fn move_to_line_end(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.line_end(self.cursor);
    }

    /// Move cursor to start of document.
    pub fn move_to_document_start(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.document_start();
    }

    /// Move cursor to end of document.
    pub fn move_to_document_end(&mut self) {
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.document_end();
    }

    // ====================================================================
    // Selection extension
    // ====================================================================

    /// Extend selection left by one grapheme.
    pub fn select_left(&mut self) {
        self.extend_selection(|nav, pos| nav.move_left(pos));
    }

    /// Extend selection right by one grapheme.
    pub fn select_right(&mut self) {
        self.extend_selection(|nav, pos| nav.move_right(pos));
    }

    /// Extend selection up one line.
    pub fn select_up(&mut self) {
        self.extend_selection(|nav, pos| nav.move_up(pos));
    }

    /// Extend selection down one line.
    pub fn select_down(&mut self) {
        self.extend_selection(|nav, pos| nav.move_down(pos));
    }

    /// Extend selection left by one word.
    pub fn select_word_left(&mut self) {
        self.extend_selection(|nav, pos| nav.move_word_left(pos));
    }

    /// Extend selection right by one word.
    pub fn select_word_right(&mut self) {
        self.extend_selection(|nav, pos| nav.move_word_right(pos));
    }

    /// Select all text.
    pub fn select_all(&mut self) {
        let nav = CursorNavigator::new(&self.rope);
        let start = nav.document_start();
        let end = nav.document_end();
        self.selection = Some(Selection {
            anchor: start,
            head: end,
        });
        self.cursor = end;
    }

    /// Clear current selection without moving cursor.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Get selected text, if any non-empty selection exists.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        if sel.is_empty() {
            return None;
        }
        let nav = CursorNavigator::new(&self.rope);
        let (start, end) = sel.byte_range(&nav);
        let start_char = self.rope.byte_to_char(start);
        let end_char = self.rope.byte_to_char(end);
        Some(self.rope.slice(start_char..end_char).into_owned())
    }

    fn extend_selection(
        &mut self,
        f: impl FnOnce(&CursorNavigator<'_>, CursorPosition) -> CursorPosition,
    ) {
        let anchor = match self.selection {
            Some(sel) => sel.anchor,
            None => self.cursor,
        };
        let nav = CursorNavigator::new(&self.rope);
        let new_head = f(&nav, self.cursor);
        self.cursor = new_head;
        self.selection = Some(Selection {
            anchor,
            head: new_head,
        });
    }

    // ====================================================================
    // Content replacement
    // ====================================================================

    /// Replace all content and reset cursor to end. Clears undo history.
    pub fn set_text(&mut self, text: &str) {
        self.rope.replace(text);
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.document_end();
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// Clear all content and reset cursor. Clears undo history.
    pub fn clear(&mut self) {
        self.rope.clear();
        self.cursor = CursorPosition::default();
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_editor_is_empty() {
        let ed = Editor::new();
        assert!(ed.is_empty());
        assert_eq!(ed.text(), "");
        assert_eq!(ed.cursor(), CursorPosition::default());
    }

    #[test]
    fn with_text_cursor_at_end() {
        let ed = Editor::with_text("hello");
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.cursor().line, 0);
        assert_eq!(ed.cursor().grapheme, 5);
    }

    #[test]
    fn insert_char_at_end() {
        let mut ed = Editor::new();
        ed.insert_char('a');
        ed.insert_char('b');
        ed.insert_char('c');
        assert_eq!(ed.text(), "abc");
        assert_eq!(ed.cursor().grapheme, 3);
    }

    #[test]
    fn insert_text() {
        let mut ed = Editor::new();
        ed.insert_text("hello world");
        assert_eq!(ed.text(), "hello world");
    }

    #[test]
    fn insert_in_middle() {
        let mut ed = Editor::with_text("helo");
        // Move cursor to position 3 (after "hel")
        ed.set_cursor(CursorPosition::new(0, 3, 3));
        ed.insert_char('l');
        assert_eq!(ed.text(), "hello");
    }

    #[test]
    fn insert_newline() {
        let mut ed = Editor::with_text("hello world");
        // Move cursor after "hello"
        ed.set_cursor(CursorPosition::new(0, 5, 5));
        ed.insert_newline();
        assert_eq!(ed.text(), "hello\n world");
        assert_eq!(ed.cursor().line, 1);
        assert_eq!(ed.line_count(), 2);
    }

    #[test]
    fn delete_backward() {
        let mut ed = Editor::with_text("hello");
        assert!(ed.delete_backward());
        assert_eq!(ed.text(), "hell");
    }

    #[test]
    fn delete_backward_at_beginning() {
        let mut ed = Editor::with_text("hello");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        assert!(!ed.delete_backward());
        assert_eq!(ed.text(), "hello");
    }

    #[test]
    fn delete_backward_joins_lines() {
        let mut ed = Editor::with_text("hello\nworld");
        // Cursor at start of "world"
        ed.set_cursor(CursorPosition::new(1, 0, 0));
        assert!(ed.delete_backward());
        assert_eq!(ed.text(), "helloworld");
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn delete_forward() {
        let mut ed = Editor::with_text("hello");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        assert!(ed.delete_forward());
        assert_eq!(ed.text(), "ello");
    }

    #[test]
    fn delete_forward_at_end() {
        let mut ed = Editor::with_text("hello");
        assert!(!ed.delete_forward());
        assert_eq!(ed.text(), "hello");
    }

    #[test]
    fn delete_forward_joins_lines() {
        let mut ed = Editor::with_text("hello\nworld");
        // Cursor at end of "hello"
        ed.set_cursor(CursorPosition::new(0, 5, 5));
        assert!(ed.delete_forward());
        assert_eq!(ed.text(), "helloworld");
    }

    #[test]
    fn move_left_right() {
        let mut ed = Editor::with_text("abc");
        assert_eq!(ed.cursor().grapheme, 3);

        ed.move_left();
        assert_eq!(ed.cursor().grapheme, 2);

        ed.move_left();
        assert_eq!(ed.cursor().grapheme, 1);

        ed.move_right();
        assert_eq!(ed.cursor().grapheme, 2);
    }

    #[test]
    fn move_left_at_start_is_noop() {
        let mut ed = Editor::with_text("abc");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        ed.move_left();
        assert_eq!(ed.cursor().grapheme, 0);
        assert_eq!(ed.cursor().line, 0);
    }

    #[test]
    fn move_right_at_end_is_noop() {
        let mut ed = Editor::with_text("abc");
        ed.move_right();
        assert_eq!(ed.cursor().grapheme, 3);
    }

    #[test]
    fn move_up_down() {
        let mut ed = Editor::with_text("line 1\nline 2\nline 3");
        // Cursor at end of "line 3"
        assert_eq!(ed.cursor().line, 2);

        ed.move_up();
        assert_eq!(ed.cursor().line, 1);

        ed.move_up();
        assert_eq!(ed.cursor().line, 0);

        // At top, stays
        ed.move_up();
        assert_eq!(ed.cursor().line, 0);

        ed.move_down();
        assert_eq!(ed.cursor().line, 1);
    }

    #[test]
    fn move_to_line_start_end() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 5, 5));

        ed.move_to_line_start();
        assert_eq!(ed.cursor().grapheme, 0);

        ed.move_to_line_end();
        assert_eq!(ed.cursor().grapheme, 11);
    }

    #[test]
    fn move_to_document_start_end() {
        let mut ed = Editor::with_text("line 1\nline 2\nline 3");

        ed.move_to_document_start();
        assert_eq!(ed.cursor().line, 0);
        assert_eq!(ed.cursor().grapheme, 0);

        ed.move_to_document_end();
        assert_eq!(ed.cursor().line, 2);
    }

    #[test]
    fn move_word_left_right() {
        let mut ed = Editor::with_text("hello world foo");
        // Cursor at end (grapheme 15)
        let start = ed.cursor().grapheme;

        ed.move_word_left();
        let after_first = ed.cursor().grapheme;
        assert!(after_first < start, "word_left should move cursor left");

        ed.move_word_left();
        let after_second = ed.cursor().grapheme;
        assert!(
            after_second < after_first,
            "second word_left should move further left"
        );

        ed.move_word_right();
        let after_right = ed.cursor().grapheme;
        assert!(
            after_right > after_second,
            "word_right should move cursor right"
        );
    }

    #[test]
    fn delete_word_backward() {
        let mut ed = Editor::with_text("hello world");
        assert!(ed.delete_word_backward());
        assert_eq!(ed.text(), "hello ");
    }

    #[test]
    fn delete_to_end_of_line() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 5, 5));
        assert!(ed.delete_to_end_of_line());
        assert_eq!(ed.text(), "hello");
    }

    #[test]
    fn delete_to_end_joins_when_at_line_end() {
        let mut ed = Editor::with_text("hello\nworld");
        ed.set_cursor(CursorPosition::new(0, 5, 5));
        assert!(ed.delete_to_end_of_line());
        assert_eq!(ed.text(), "helloworld");
    }

    #[test]
    fn set_text_replaces_content() {
        let mut ed = Editor::with_text("old");
        ed.set_text("new content");
        assert_eq!(ed.text(), "new content");
    }

    #[test]
    fn clear_resets() {
        let mut ed = Editor::with_text("hello");
        ed.clear();
        assert!(ed.is_empty());
        assert_eq!(ed.cursor(), CursorPosition::default());
    }

    #[test]
    fn line_text_works() {
        let ed = Editor::with_text("line 0\nline 1\nline 2");
        assert_eq!(ed.line_text(0), Some("line 0".to_string()));
        assert_eq!(ed.line_text(1), Some("line 1".to_string()));
        assert_eq!(ed.line_text(2), Some("line 2".to_string()));
        assert_eq!(ed.line_text(3), None);
    }

    #[test]
    fn cursor_stays_in_bounds_after_delete() {
        let mut ed = Editor::with_text("a");
        assert!(ed.delete_backward());
        assert_eq!(ed.text(), "");
        assert_eq!(ed.cursor(), CursorPosition::default());

        // Further deletes are no-ops
        assert!(!ed.delete_backward());
        assert!(!ed.delete_forward());
    }

    #[test]
    fn multiline_editing() {
        let mut ed = Editor::new();
        ed.insert_text("first");
        ed.insert_newline();
        ed.insert_text("second");
        ed.insert_newline();
        ed.insert_text("third");

        assert_eq!(ed.text(), "first\nsecond\nthird");
        assert_eq!(ed.line_count(), 3);
        assert_eq!(ed.cursor().line, 2);

        // Move up and insert at start of middle line
        ed.move_up();
        ed.move_to_line_start();
        ed.insert_text(">> ");
        assert_eq!(ed.line_text(1), Some(">> second".to_string()));
    }

    // ================================================================
    // Undo / Redo tests
    // ================================================================

    #[test]
    fn undo_insert() {
        let mut ed = Editor::new();
        ed.insert_text("hello");
        assert!(ed.can_undo());
        assert!(ed.undo());
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn undo_delete() {
        let mut ed = Editor::with_text("hello");
        ed.delete_backward();
        assert_eq!(ed.text(), "hell");
        assert!(ed.undo());
        assert_eq!(ed.text(), "hello");
    }

    #[test]
    fn redo_after_undo() {
        let mut ed = Editor::new();
        ed.insert_text("abc");
        ed.undo();
        assert_eq!(ed.text(), "");
        assert!(ed.can_redo());
        assert!(ed.redo());
        assert_eq!(ed.text(), "abc");
    }

    #[test]
    fn redo_cleared_on_new_edit() {
        let mut ed = Editor::new();
        ed.insert_text("abc");
        ed.undo();
        ed.insert_text("xyz");
        assert!(!ed.can_redo());
    }

    #[test]
    fn multiple_undo_redo() {
        let mut ed = Editor::new();
        ed.insert_text("a");
        ed.insert_text("b");
        ed.insert_text("c");
        assert_eq!(ed.text(), "abc");

        ed.undo();
        assert_eq!(ed.text(), "ab");
        ed.undo();
        assert_eq!(ed.text(), "a");
        ed.undo();
        assert_eq!(ed.text(), "");

        ed.redo();
        assert_eq!(ed.text(), "a");
        ed.redo();
        assert_eq!(ed.text(), "ab");
    }

    #[test]
    fn undo_restores_cursor() {
        let mut ed = Editor::new();
        let before = ed.cursor();
        ed.insert_text("x");
        ed.undo();
        assert_eq!(ed.cursor(), before);
    }

    #[test]
    fn max_history_respected() {
        let mut ed = Editor::new();
        ed.set_max_history(3);
        for c in ['a', 'b', 'c', 'd', 'e'] {
            ed.insert_text(&c.to_string());
        }
        assert!(ed.undo());
        assert!(ed.undo());
        assert!(ed.undo());
        assert!(!ed.undo());
        assert_eq!(ed.text(), "ab");
    }

    #[test]
    fn set_text_clears_undo() {
        let mut ed = Editor::new();
        ed.insert_text("abc");
        ed.set_text("new");
        assert!(!ed.can_undo());
        assert!(!ed.can_redo());
    }

    #[test]
    fn clear_clears_undo() {
        let mut ed = Editor::new();
        ed.insert_text("abc");
        ed.clear();
        assert!(!ed.can_undo());
    }

    // ================================================================
    // Selection tests
    // ================================================================

    #[test]
    fn select_right_creates_selection() {
        let mut ed = Editor::with_text("hello");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        ed.select_right();
        ed.select_right();
        ed.select_right();
        let sel = ed.selection().unwrap();
        assert_eq!(sel.anchor, CursorPosition::new(0, 0, 0));
        assert_eq!(sel.head.grapheme, 3);
        assert_eq!(ed.selected_text(), Some("hel".to_string()));
    }

    #[test]
    fn select_all_selects_everything() {
        let mut ed = Editor::with_text("abc\ndef");
        ed.select_all();
        assert_eq!(ed.selected_text(), Some("abc\ndef".to_string()));
    }

    #[test]
    fn insert_replaces_selection() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        for _ in 0..5 {
            ed.select_right();
        }
        ed.insert_text("goodbye");
        assert_eq!(ed.text(), "goodbye world");
        assert!(ed.selection().is_none());
    }

    #[test]
    fn delete_backward_removes_selection() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        for _ in 0..5 {
            ed.select_right();
        }
        ed.delete_backward();
        assert_eq!(ed.text(), " world");
    }

    #[test]
    fn movement_clears_selection() {
        let mut ed = Editor::with_text("hello");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        ed.select_right();
        ed.select_right();
        assert!(ed.selection().is_some());
        ed.move_right();
        assert!(ed.selection().is_none());
    }

    #[test]
    fn undo_selection_delete() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        for _ in 0..5 {
            ed.select_right();
        }
        ed.delete_backward();
        assert_eq!(ed.text(), " world");
        ed.undo();
        assert_eq!(ed.text(), "hello world");
    }
}

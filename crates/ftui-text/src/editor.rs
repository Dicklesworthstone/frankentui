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
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupKind {
    Insert,
    DeleteBackward,
    DeleteForward,
    Other,
}

#[derive(Debug, Clone)]
struct UndoGroup {
    ops: Vec<EditOp>,
    before: CursorPosition,
    after: CursorPosition,
    kind: GroupKind,
    last_tick: u64,
    last_was_space: bool,
    closed: bool,
    bytes: usize,
}

/// A single edit operation for undo/redo.
#[derive(Debug, Clone)]
enum EditOp {
    Insert {
        byte_offset: usize,
        text: String,
    },
    Delete {
        byte_offset: usize,
        text: String,
    },
    Replace {
        byte_offset: usize,
        deleted: String,
        inserted: String,
    },
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
            Self::Replace {
                byte_offset,
                deleted,
                inserted,
            } => Self::Replace {
                byte_offset: *byte_offset,
                deleted: inserted.clone(),
                inserted: deleted.clone(),
            },
        }
    }

    fn byte_len(&self) -> usize {
        match self {
            Self::Insert { text, .. } => text.len(),
            Self::Delete { text, .. } => text.len(),
            Self::Replace {
                deleted, inserted, ..
            } => deleted.len() + inserted.len(),
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
    /// Completed and open edit groups.
    undo_stack: Vec<UndoGroup>,
    /// Undone groups, retaining their original cursor endpoints.
    redo_stack: Vec<UndoGroup>,
    /// Maximum undo history depth.
    max_history: usize,
    /// Current size of undo history in bytes.
    current_undo_size: usize,
    /// Maximum size of undo history in bytes (default 10MB).
    max_undo_size: usize,
    now_tick: u64,
    coalesce_idle: Duration,
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
            current_undo_size: 0,
            max_undo_size: 10 * 1024 * 1024, // 10MB default
            now_tick: 0,
            coalesce_idle: Duration::from_millis(500),
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
            current_undo_size: 0,
            max_undo_size: 10 * 1024 * 1024,
            now_tick: 0,
            coalesce_idle: Duration::from_millis(500),
        }
    }

    /// Set the maximum undo history depth.
    pub fn set_max_history(&mut self, max: usize) {
        self.max_history = max;
        self.prune_undo();
    }

    /// Set the maximum undo history size in bytes.
    pub fn set_max_undo_size(&mut self, bytes: usize) {
        self.max_undo_size = bytes;
        self.prune_undo();
    }

    /// Set the maximum idle interval between edits in a group (default 500 ms).
    /// Zero disables coalescing. Changing this setting closes the current group.
    pub fn set_coalesce_idle(&mut self, idle: Duration) {
        self.break_undo_group();
        self.coalesce_idle = idle;
    }

    /// Supply monotonic milliseconds for subsequent edits; no clock is read here.
    /// A backwards timestamp closes the group, as when a replay clock restarts.
    pub fn tick(&mut self, now_ms: u64) {
        if now_ms < self.now_tick {
            self.break_undo_group();
        }
        self.now_tick = now_ms;
    }

    /// Make the next edit start a new undo group.
    pub fn break_undo_group(&mut self) {
        if let Some(group) = self.undo_stack.last_mut() {
            group.closed = true;
        }
    }

    /// Number of retained undo groups (one group is one undo step).
    #[must_use]
    pub fn undo_group_count(&self) -> usize {
        self.undo_stack.len()
    }

    /// Number of groups available to redo.
    #[must_use]
    pub fn redo_group_count(&self) -> usize {
        self.redo_stack.len()
    }

    /// Number of primitive edits retained across all undo groups.
    #[must_use]
    pub fn undo_op_count(&self) -> usize {
        self.undo_stack.iter().map(|group| group.ops.len()).sum()
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
        self.break_undo_group();
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

    /// Insert a typed character. Adjacent typing coalesces until a new word,
    /// cursor/selection change, direction change, or idle timeout.
    pub fn insert_char(&mut self, ch: char) {
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        self.insert(s, GroupKind::Insert);
    }

    /// Insert text at the cursor position. Deletes selection first if active.
    ///
    /// Control characters (except newline and tab) are stripped to prevent
    /// terminal corruption. Each call is a separate paste undo group, even if
    /// it contains only one character; use [`Self::insert_char`] for typing.
    pub fn insert_text(&mut self, text: &str) {
        // Explicit text insertion is a paste, even for a one-character payload.
        self.insert(text, GroupKind::Other);
    }

    fn insert(&mut self, text: &str, kind: GroupKind) {
        if text.is_empty() {
            return;
        }

        // Sanitize input: allow \n and \t, strip other control chars
        let sanitized: String = text
            .chars()
            .filter(|&c| !c.is_control() || c == '\n' || c == '\t')
            .collect();

        if sanitized.is_empty() {
            return;
        }

        if let Some((start_byte, deleted)) = self.extract_selection() {
            let char_idx = self.rope.byte_to_char(start_byte);

            self.push_undo(
                EditOp::Replace {
                    byte_offset: start_byte,
                    deleted,
                    inserted: sanitized.clone(),
                },
                GroupKind::Other,
            );

            self.rope.insert(char_idx, &sanitized);

            let new_byte_idx = start_byte + sanitized.len();
            let nav = CursorNavigator::new(&self.rope);
            self.cursor = nav.from_byte_index(new_byte_idx);
        } else {
            let nav = CursorNavigator::new(&self.rope);
            let byte_idx = nav.to_byte_index(self.cursor);
            let char_idx = self.rope.byte_to_char(byte_idx);

            self.push_undo(
                EditOp::Insert {
                    byte_offset: byte_idx,
                    text: sanitized.clone(),
                },
                if sanitized.contains('\n') {
                    GroupKind::Other
                } else {
                    kind
                },
            );

            self.rope.insert(char_idx, &sanitized);

            // Move cursor to end of inserted text
            let new_byte_idx = byte_idx + sanitized.len();
            let nav = CursorNavigator::new(&self.rope);
            self.cursor = nav.from_byte_index(new_byte_idx);
        }
        self.finish_edit();
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
        let new_pos = nav.move_grapheme_backward(old_pos);

        if new_pos == old_pos {
            return false; // At beginning, nothing to delete
        }

        let start_byte = nav.to_byte_index(new_pos);
        let end_byte = nav.to_byte_index(old_pos);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.push_undo(
            EditOp::Delete {
                byte_offset: start_byte,
                text: deleted,
            },
            GroupKind::DeleteBackward,
        );

        self.rope.remove(start_char..end_char);

        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.from_byte_index(start_byte);
        self.finish_edit();
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
        let next_pos = nav.move_grapheme_forward(old_pos);

        if next_pos == old_pos {
            return false; // At end, nothing to delete
        }

        let start_byte = nav.to_byte_index(old_pos);
        let end_byte = nav.to_byte_index(next_pos);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.push_undo(
            EditOp::Delete {
                byte_offset: start_byte,
                text: deleted,
            },
            GroupKind::DeleteForward,
        );

        self.rope.remove(start_char..end_char);

        // Cursor stays at same position, just re-clamp
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.clamp(self.cursor);
        self.finish_edit();
        true
    }

    /// Delete the word before the cursor (Ctrl+Backspace).
    ///
    /// Returns `true` if any text was deleted.
    pub fn delete_word_backward(&mut self) -> bool {
        self.break_undo_group();
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

        self.push_undo(
            EditOp::Delete {
                byte_offset: start_byte,
                text: deleted,
            },
            GroupKind::Other,
        );

        self.rope.remove(start_char..end_char);

        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.from_byte_index(start_byte);
        self.finish_edit();
        true
    }

    /// Delete the word after the cursor (Ctrl+Delete).
    ///
    /// Returns `true` if any text was deleted.
    pub fn delete_word_forward(&mut self) -> bool {
        self.break_undo_group();
        if self.delete_selection_inner() {
            return true;
        }
        let nav = CursorNavigator::new(&self.rope);
        let old_pos = self.cursor;
        let word_end = nav.move_word_right(old_pos);

        if word_end == old_pos {
            return false;
        }

        let start_byte = nav.to_byte_index(old_pos);
        let end_byte = nav.to_byte_index(word_end);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.push_undo(
            EditOp::Delete {
                byte_offset: start_byte,
                text: deleted,
            },
            GroupKind::Other,
        );

        self.rope.remove(start_char..end_char);

        // Re-clamp cursor just in case, matching other forward deletions
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.clamp(self.cursor);
        self.finish_edit();
        true
    }

    /// Delete from cursor to end of line (Ctrl+K).
    ///
    /// Returns `true` if any text was deleted.
    pub fn delete_to_end_of_line(&mut self) -> bool {
        self.break_undo_group();
        if self.delete_selection_inner() {
            return true;
        }
        let nav = CursorNavigator::new(&self.rope);
        let old_pos = self.cursor;
        let line_end = nav.logical_line_end(old_pos);

        if line_end == old_pos {
            // At end of line: delete the newline to join lines
            let deleted = self.delete_forward();
            self.break_undo_group();
            return deleted;
        }

        let start_byte = nav.to_byte_index(old_pos);
        let end_byte = nav.to_byte_index(line_end);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.push_undo(
            EditOp::Delete {
                byte_offset: start_byte,
                text: deleted,
            },
            GroupKind::Other,
        );

        self.rope.remove(start_char..end_char);

        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.clamp(self.cursor);
        self.finish_edit();
        true
    }

    // ====================================================================
    // Undo / redo
    // ====================================================================

    /// Push an edit operation onto the undo stack.
    fn push_undo(&mut self, op: EditOp, kind: GroupKind) {
        let bytes = op.byte_len();
        let space =
            matches!(&op, EditOp::Insert { text, .. } if text.chars().all(char::is_whitespace));
        let join = self.undo_stack.last().is_some_and(|group| {
            !group.closed
                && kind != GroupKind::Other
                && kind == group.kind
                && self.cursor == group.after
                && !self.coalesce_idle.is_zero()
                && Duration::from_millis(self.now_tick.saturating_sub(group.last_tick))
                    <= self.coalesce_idle
                // Keep the separator with the word before it: undoing "world"
                // in "hello world" leaves "hello ".
                && !(kind == GroupKind::Insert && group.last_was_space && !space)
        });
        if join {
            let group = self.undo_stack.last_mut().expect("join requires a group");
            group.ops.push(op);
            group.bytes += bytes;
            group.last_tick = self.now_tick;
            group.last_was_space = space;
        } else {
            self.break_undo_group();
            self.undo_stack.push(UndoGroup {
                ops: vec![op],
                before: self.cursor,
                after: self.cursor,
                kind,
                last_tick: self.now_tick,
                last_was_space: space,
                closed: kind == GroupKind::Other,
                bytes,
            });
        }
        self.current_undo_size += bytes;
        self.prune_undo();
        self.redo_stack.clear();
    }

    fn finish_edit(&mut self) {
        if let Some(group) = self.undo_stack.last_mut() {
            group.after = self.cursor;
        }
    }

    fn prune_undo(&mut self) {
        while !self.undo_stack.is_empty()
            && (self.undo_stack.len() > self.max_history
                || self.current_undo_size > self.max_undo_size)
        {
            let group = self.undo_stack.remove(0);
            self.current_undo_size -= group.bytes;
        }
    }

    /// Undo the last group and restore its original cursor position.
    pub fn undo(&mut self) -> bool {
        self.break_undo_group();
        let Some(group) = self.undo_stack.pop() else {
            return false;
        };
        self.current_undo_size -= group.bytes;
        for op in group.ops.iter().rev() {
            self.apply_op(&op.inverse());
        }
        self.cursor = group.before;
        self.redo_stack.push(group);
        self.selection = None;
        true
    }

    /// Redo the last undone group and restore its post-edit cursor position.
    pub fn redo(&mut self) -> bool {
        self.break_undo_group();
        let Some(group) = self.redo_stack.pop() else {
            return false;
        };
        for op in &group.ops {
            self.apply_op(op);
        }
        self.cursor = group.after;
        self.current_undo_size += group.bytes;
        self.undo_stack.push(group);
        self.prune_undo();
        self.selection = None;
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
            EditOp::Replace {
                byte_offset,
                deleted,
                inserted,
            } => {
                let start_char = self.rope.byte_to_char(*byte_offset);
                let end_char = self.rope.byte_to_char(*byte_offset + deleted.len());
                self.rope.remove(start_char..end_char);
                self.rope.insert(start_char, inserted);
            }
        }
    }

    // ====================================================================
    // Selection helpers
    // ====================================================================

    /// Delete the current selection if active, returning the deleted text and byte offset.
    /// If nothing was deleted, returns `None`. Does NOT push to the undo stack.
    /// Leaves the cursor unchanged so the caller can record its pre-edit position
    /// before moving it to a valid position in the edited text.
    fn extract_selection(&mut self) -> Option<(usize, String)> {
        let sel = self.selection.take()?;
        if sel.is_empty() {
            return None;
        }
        let nav = CursorNavigator::new(&self.rope);
        let (start_byte, end_byte) = sel.byte_range(&nav);
        let start_char = self.rope.byte_to_char(start_byte);
        let end_char = self.rope.byte_to_char(end_byte);
        let deleted = self.rope.slice(start_char..end_char).into_owned();

        self.rope.remove(start_char..end_char);
        Some((start_byte, deleted))
    }

    /// Delete the current selection if active. Returns true if something was deleted.
    fn delete_selection_inner(&mut self) -> bool {
        if let Some((start_byte, deleted)) = self.extract_selection() {
            self.push_undo(
                EditOp::Delete {
                    byte_offset: start_byte,
                    text: deleted,
                },
                GroupKind::Other,
            );
            let nav = CursorNavigator::new(&self.rope);
            self.cursor = nav.from_byte_index(start_byte);
            self.finish_edit();
            true
        } else {
            false
        }
    }

    // ====================================================================
    // Cursor movement (clears selection)
    // ====================================================================

    /// Move cursor backward by one grapheme in logical document order.
    pub fn move_backward(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_grapheme_backward(self.cursor);
    }

    /// Move cursor forward by one grapheme in logical document order.
    pub fn move_forward(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_grapheme_forward(self.cursor);
    }

    /// Move cursor left by one grapheme.
    pub fn move_left(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_left(self.cursor);
    }

    /// Move cursor right by one grapheme.
    pub fn move_right(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_right(self.cursor);
    }

    /// Move cursor up one line.
    pub fn move_up(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_up(self.cursor);
    }

    /// Move cursor down one line.
    pub fn move_down(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_down(self.cursor);
    }

    /// Move cursor left by one word.
    pub fn move_word_left(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_word_left(self.cursor);
    }

    /// Move cursor right by one word.
    pub fn move_word_right(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_word_right(self.cursor);
    }

    /// Clear selection and move to the preceding blank-line paragraph boundary.
    /// See [`CursorNavigator::move_paragraph_up`] for whitespace rules and examples.
    pub fn move_paragraph_up(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_paragraph_up(self.cursor);
    }

    /// Clear selection and move to the following blank-line paragraph boundary.
    /// See [`CursorNavigator::move_paragraph_down`] for whitespace rules and examples.
    pub fn move_paragraph_down(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.move_paragraph_down(self.cursor);
    }

    /// Move cursor to start of line.
    pub fn move_to_line_start(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.line_start(self.cursor);
    }

    /// Move cursor to end of line.
    pub fn move_to_line_end(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.line_end(self.cursor);
    }

    /// Move cursor to start of document.
    pub fn move_to_document_start(&mut self) {
        self.break_undo_group();
        self.selection = None;
        let nav = CursorNavigator::new(&self.rope);
        self.cursor = nav.document_start();
    }

    /// Move cursor to end of document.
    pub fn move_to_document_end(&mut self) {
        self.break_undo_group();
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

    /// Extend selection to the preceding blank-line paragraph boundary.
    /// See [`CursorNavigator::move_paragraph_up`] for whitespace rules and examples.
    pub fn select_paragraph_up(&mut self) {
        self.extend_selection(|nav, pos| nav.move_paragraph_up(pos));
    }

    /// Extend selection to the following blank-line paragraph boundary.
    /// See [`CursorNavigator::move_paragraph_down`] for whitespace rules and examples.
    pub fn select_paragraph_down(&mut self) {
        self.extend_selection(|nav, pos| nav.move_paragraph_down(pos));
    }

    /// Select all text.
    pub fn select_all(&mut self) {
        self.break_undo_group();
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
        self.break_undo_group();
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
        let nav = CursorNavigator::new(&self.rope);
        let new_head = f(&nav, self.cursor);
        self.extend_selection_to(new_head);
    }

    /// Extend selection to a specific cursor position.
    pub fn extend_selection_to(&mut self, new_head: CursorPosition) {
        self.break_undo_group();
        let anchor = match self.selection {
            Some(sel) => sel.anchor,
            None => self.cursor,
        };
        let nav = CursorNavigator::new(&self.rope);
        let new_head = nav.clamp(new_head);
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
        self.current_undo_size = 0;
    }

    /// Clear all content and reset cursor. Clears undo history.
    pub fn clear(&mut self) {
        self.rope.clear();
        self.cursor = CursorPosition::default();
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.current_undo_size = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_groups_typing_hello_world() {
        let mut ed = Editor::new();
        for c in "hello world".chars() {
            ed.insert_char(c);
        }
        assert_eq!(ed.undo_group_count(), 2);
        assert_eq!(ed.undo_op_count(), 11);
        ed.undo();
        assert_eq!(ed.text(), "hello ");
        ed.undo();
        assert_eq!(ed.text(), "");
        assert_eq!(ed.cursor(), CursorPosition::default());
        ed.redo();
        assert_eq!(ed.text(), "hello ");
        ed.redo();
        assert_eq!(ed.text(), "hello world");
        assert_eq!(ed.cursor().grapheme, 11);
    }

    #[test]
    fn undo_groups_deletions_and_direction_changes() {
        for backwards in [false, true] {
            let mut ed = Editor::with_text("a界e\u{301}bc");
            if !backwards {
                ed.move_to_document_start();
            }
            let before = ed.cursor();
            for _ in 0..5 {
                assert!(if backwards {
                    ed.delete_backward()
                } else {
                    ed.delete_forward()
                });
            }
            assert_eq!(ed.undo_group_count(), 1);
            assert_eq!(ed.undo_op_count(), 5);
            assert_eq!(ed.text(), "");
            assert!(ed.undo());
            assert_eq!(ed.text(), "a界e\u{301}bc");
            assert_eq!(ed.cursor(), before);
            assert!(ed.redo());
            assert_eq!(ed.text(), "");
        }
        let mut ed = Editor::with_text("abcd");
        ed.set_cursor(CursorPosition::new(0, 2, 2));
        ed.delete_backward();
        ed.delete_forward();
        ed.insert_char('x');
        assert_eq!(ed.undo_group_count(), 3);
        while ed.undo() {}
        assert_eq!(ed.text(), "abcd");
    }

    #[test]
    fn undo_groups_virtual_idle_boundary_and_clock_reset() {
        for (elapsed, expected) in [(499, 1), (500, 1), (501, 2)] {
            let mut ed = Editor::new();
            ed.tick(0);
            ed.insert_char('a');
            ed.tick(elapsed);
            ed.insert_char('b');
            assert_eq!(ed.undo_group_count(), expected);
        }
        let mut ed = Editor::new();
        ed.tick(100);
        ed.insert_char('a');
        ed.tick(0);
        ed.insert_char('b');
        assert_eq!(ed.undo_group_count(), 2);
        ed.set_coalesce_idle(Duration::ZERO);
        ed.insert_char('c');
        ed.insert_char('d');
        assert_eq!(ed.undo_group_count(), 4);
    }

    #[test]
    fn undo_groups_paste_newline_and_cursor_endpoints() {
        let mut ed = Editor::new();
        ed.insert_char('a');
        ed.insert_text("bc");
        ed.insert_char('d');
        ed.insert_newline();
        ed.insert_char('e');
        assert_eq!(ed.undo_group_count(), 5);
        let after = ed.cursor();
        ed.move_to_document_start();
        ed.undo();
        assert_eq!(ed.text(), "abcd\n");
        ed.move_to_document_start();
        ed.redo();
        assert_eq!(ed.text(), "abcd\ne");
        assert_eq!(ed.cursor(), after);
        // Undo/new edit must not reopen a previously closed group.
        ed.undo();
        ed.insert_char('f');
        assert_eq!(ed.undo_group_count(), 5);
        assert!(!ed.can_redo());
    }

    #[test]
    fn undo_groups_navigation_and_selection_close_typing() {
        let boundaries: &[fn(&mut Editor)] = &[
            Editor::move_left,
            Editor::move_right,
            Editor::move_up,
            Editor::move_down,
            Editor::move_word_left,
            Editor::move_word_right,
            Editor::move_paragraph_up,
            Editor::move_paragraph_down,
            Editor::move_to_line_start,
            Editor::move_to_line_end,
            Editor::move_to_document_start,
            Editor::move_to_document_end,
            Editor::select_left,
            Editor::select_right,
            Editor::select_up,
            Editor::select_down,
            Editor::select_word_left,
            Editor::select_word_right,
            Editor::select_paragraph_up,
            Editor::select_paragraph_down,
            Editor::select_all,
            Editor::clear_selection,
            Editor::break_undo_group,
        ];
        for boundary in boundaries {
            let mut ed = Editor::new();
            ed.insert_char('a');
            boundary(&mut ed);
            ed.insert_char('b');
            assert_eq!(ed.undo_group_count(), 2);
            while ed.undo() {}
            assert_eq!(ed.text(), "");
        }
    }

    #[test]
    fn undo_groups_limits_prune_whole_groups_and_redo() {
        let mut ed = Editor::new();
        ed.set_max_history(2);
        for text in ["one", "two", "three"] {
            for c in text.chars() {
                ed.insert_char(c);
            }
            ed.break_undo_group();
        }
        assert_eq!(ed.undo_group_count(), 2);
        assert_eq!(ed.current_undo_size, 8);
        ed.set_max_undo_size(5);
        assert_eq!(ed.undo_group_count(), 1);
        assert_eq!(ed.current_undo_size, 5);
        ed.undo();
        assert_eq!(ed.text(), "onetwo");
        assert_eq!(ed.current_undo_size, 0);
        ed.set_max_history(0);
        ed.redo();
        assert_eq!(ed.text(), "onetwothree");
        assert_eq!(ed.current_undo_size, 0);
        assert!(!ed.can_undo());
    }

    proptest::proptest! {
        #[test]
        fn undo_groups_roundtrip_and_accounting(
            ops in proptest::collection::vec((0u8..8, 0u64..800), 0..100)
        ) {
            let mut ed = Editor::with_text("界e\u{301}\nseed");
            let mut first_edit_cursor = None;
            let mut last_edit_cursor = None;
            let mut tick = 0;
            for (op, elapsed) in ops {
                tick += elapsed;
                ed.tick(tick);
                let before_cursor = ed.cursor();
                let before_text = ed.text();
                match op {
                    0 => ed.insert_char('x'),
                    1 => ed.insert_char(' '),
                    2 => ed.insert_char('\u{301}'),
                    3 => { ed.delete_backward(); }
                    4 => { ed.delete_forward(); }
                    5 => ed.move_left(),
                    6 => ed.insert_text("paste"),
                    _ => ed.insert_newline(),
                }
                if ed.text() != before_text {
                    first_edit_cursor.get_or_insert(before_cursor);
                    last_edit_cursor = Some(ed.cursor());
                }
                let bytes: usize = ed.undo_stack.iter()
                    .flat_map(|g| &g.ops).map(EditOp::byte_len).sum();
                proptest::prop_assert_eq!(ed.current_undo_size, bytes);
                proptest::prop_assert_eq!(ed.cursor(), CursorNavigator::new(ed.rope()).clamp(ed.cursor()));
            }
            let final_text = ed.text();
            let count = ed.undo_group_count();
            for _ in 0..count {
                proptest::prop_assert!(ed.undo());
                let bytes: usize = ed.undo_stack.iter()
                    .flat_map(|g| &g.ops).map(EditOp::byte_len).sum();
                proptest::prop_assert_eq!(ed.current_undo_size, bytes);
            }
            proptest::prop_assert_eq!(ed.text(), "界e\u{301}\nseed");
            if let Some(cursor) = first_edit_cursor {
                proptest::prop_assert_eq!(ed.cursor(), cursor);
            }
            proptest::prop_assert!(!ed.can_undo());
            for _ in 0..count {
                proptest::prop_assert!(ed.redo());
                let bytes: usize = ed.undo_stack.iter()
                    .flat_map(|g| &g.ops).map(EditOp::byte_len).sum();
                proptest::prop_assert_eq!(ed.current_undo_size, bytes);
            }
            proptest::prop_assert_eq!(ed.text(), final_text);
            if let Some(cursor) = last_edit_cursor {
                proptest::prop_assert_eq!(ed.cursor(), cursor);
            }
        }
    }

    #[test]
    fn selection_edit_undo_restores_original_cursor_head() {
        let original = "界e\u{301}\nbeta\ntail";
        for backwards in [false, true] {
            for edit in 0..8 {
                let mut ed = Editor::with_text(original);
                let start = CursorPosition::new(0, 1, 2);
                let end = CursorPosition::new(1, 2, 2);
                let (anchor, head) = if backwards {
                    (end, start)
                } else {
                    (start, end)
                };
                ed.set_cursor(anchor);
                ed.extend_selection_to(head);
                assert_eq!(ed.selected_text().as_deref(), Some("e\u{301}\nbe"));
                let expected = match edit {
                    0 => {
                        ed.insert_text("XY");
                        "界XYta\ntail"
                    }
                    1 => {
                        ed.insert_char('é');
                        "界éta\ntail"
                    }
                    2 => {
                        ed.insert_newline();
                        "界\nta\ntail"
                    }
                    3 => {
                        assert!(ed.delete_backward());
                        "界ta\ntail"
                    }
                    4 => {
                        assert!(ed.delete_forward());
                        "界ta\ntail"
                    }
                    5 => {
                        assert!(ed.delete_word_backward());
                        "界ta\ntail"
                    }
                    6 => {
                        assert!(ed.delete_word_forward());
                        "界ta\ntail"
                    }
                    _ => {
                        assert!(ed.delete_to_end_of_line());
                        "界ta\ntail"
                    }
                };
                assert_eq!(ed.text(), expected);
                let after = ed.cursor();
                for _ in 0..2 {
                    assert!(ed.undo());
                    assert_eq!(ed.text(), original);
                    assert_eq!(ed.cursor(), head, "backwards={backwards}, edit={edit}");
                    assert!(ed.selection().is_none());
                    assert!(!ed.can_undo());
                    assert!(ed.redo());
                    assert_eq!(ed.text(), expected);
                    assert_eq!(ed.cursor(), after);
                    assert!(ed.selection().is_none());
                    assert!(!ed.can_redo());
                }
            }
        }
    }

    #[test]
    fn selection_edit_ignored_input_preserves_selection_and_history() {
        let mut ed = Editor::with_text("original");
        ed.select_all();
        let selection = ed.selection();
        let cursor = ed.cursor();
        ed.insert_text("");
        ed.insert_text("\u{1b}\u{7}");
        assert_eq!(ed.text(), "original");
        assert_eq!(ed.selection(), selection);
        assert_eq!(ed.cursor(), cursor);
        assert!(!ed.can_undo());
    }

    #[test]
    fn paragraph_selection_preserves_anchor_and_exact_text() {
        for newline in ["\n", "\r\n"] {
            let text = ["界e\u{301}", "", "second", "", "last"].join(newline);
            let mut ed = Editor::with_text(&text);
            ed.move_to_document_start();
            let anchor = ed.cursor();
            ed.select_paragraph_down();
            assert_eq!(ed.cursor(), CursorPosition::new(1, 0, 0));
            assert_eq!(ed.selected_text(), Some(format!("界e\u{301}{newline}")));
            ed.select_paragraph_down();
            let sel = ed.selection().unwrap();
            assert_eq!(sel.anchor, anchor);
            assert_eq!(sel.head, CursorPosition::new(3, 0, 0));
            assert_eq!(
                ed.selected_text(),
                Some(format!("界e\u{301}{newline}{newline}second{newline}"))
            );
            ed.select_paragraph_up();
            assert_eq!(ed.selection().unwrap().anchor, anchor);
            assert_eq!(ed.cursor(), CursorPosition::new(1, 0, 0));
            ed.move_paragraph_down();
            assert!(ed.selection().is_none());
            assert_eq!(ed.cursor(), CursorPosition::new(3, 0, 0));
            ed.move_to_document_end();
            let end = ed.cursor();
            ed.select_paragraph_up();
            assert_eq!(ed.selection().unwrap().anchor, end);
            assert_eq!(ed.selected_text(), Some(format!("{newline}last")));
            ed.move_paragraph_up();
            assert!(ed.selection().is_none());
            assert_eq!(ed.cursor(), CursorPosition::new(1, 0, 0));
            assert_eq!(ed.text(), text);
        }
    }

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
    fn delete_removes_unicode_line_separators() {
        // ropey counts U+2028 and U+2029 as line breaks; the cursor navigator
        // did not, so it saw the separator as content on the previous line and
        // produced an empty delete range. Every delete path shares that
        // navigation, so all of them are worth pinning (bd-jlecn).
        for sep in ['\u{2028}', '\u{2029}'] {
            let mut ed = Editor::new();
            ed.insert_char(sep);
            assert!(ed.delete_backward(), "backspace U+{:04X}", sep as u32);
            assert!(ed.is_empty(), "backspace left U+{:04X} behind", sep as u32);

            let mut ed = Editor::new();
            ed.insert_char(sep);
            ed.move_backward();
            assert!(ed.delete_forward(), "delete U+{:04X}", sep as u32);
            assert!(ed.is_empty(), "delete left U+{:04X} behind", sep as u32);

            let mut ed = Editor::new();
            ed.insert_char(sep);
            ed.move_backward();
            assert!(
                ed.delete_to_end_of_line(),
                "delete_to_end_of_line U+{:04X}",
                sep as u32
            );
            assert!(
                ed.is_empty(),
                "delete_to_end_of_line left U+{:04X} behind",
                sep as u32
            );
        }
    }

    #[test]
    fn delete_backward_rtl_single_char() {
        let mut ed = Editor::new();
        // U+1E800 Mende Kikakui Syllable M001 Ki (RTL, outside BMP)
        ed.insert_char('𞠀');
        assert_eq!(ed.text(), "𞠀");
        assert!(!ed.is_empty());
        assert!(ed.delete_backward());
        assert!(ed.is_empty());
    }

    #[test]
    fn delete_backward_rtl_multiple_chars() {
        let mut ed = Editor::new();
        // Arabic text: "مرحبا"
        ed.insert_text("مرحبا");
        assert_eq!(ed.text(), "مرحبا");
        assert!(ed.delete_backward());
        assert_eq!(ed.text(), "مرحب");
        assert!(ed.delete_backward());
        assert_eq!(ed.text(), "مرح");
    }

    #[test]
    fn delete_forward_rtl() {
        let mut ed = Editor::new();
        ed.insert_text("مرحبا");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        assert!(ed.delete_forward());
        assert_eq!(ed.text(), "رحبا");
    }

    #[test]
    fn delete_to_end_of_line_rtl() {
        let mut ed = Editor::new();
        ed.insert_text("مرحبا\nworld");
        ed.set_cursor(CursorPosition::new(0, 2, 0));
        assert!(ed.delete_to_end_of_line());
        assert_eq!(ed.line_text(0), Some("مر".to_string()));
        assert_eq!(ed.line_text(1), Some("world".to_string()));
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
    fn delete_word_forward() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        assert!(ed.delete_word_forward());
        assert_eq!(ed.text(), "world");
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

    // ================================================================
    // Edge case tests
    // ================================================================

    #[test]
    fn insert_empty_text_is_noop() {
        let mut ed = Editor::with_text("hello");
        let before = ed.text();
        ed.insert_text("");
        assert_eq!(ed.text(), before);
        // No undo entry for empty insert
        assert!(!ed.can_undo());
    }

    #[test]
    fn unicode_emoji_handling() {
        let mut ed = Editor::new();
        ed.insert_text("hello 🎉 world");
        assert_eq!(ed.text(), "hello 🎉 world");
        // Emoji counts as one grapheme
        ed.move_left(); // d
        ed.move_left(); // l
        ed.move_left(); // r
        ed.move_left(); // o
        ed.move_left(); // w
        ed.move_left(); // space
        ed.move_left(); // emoji (single grapheme move)
        ed.delete_backward(); // deletes space before emoji
        assert_eq!(ed.text(), "hello🎉 world");
    }

    #[test]
    fn unicode_combining_character() {
        let mut ed = Editor::new();
        // é as e + combining acute accent (decomposed form)
        ed.insert_text("caf\u{0065}\u{0301}");
        // Text stays in decomposed form (e + combining accent)
        assert_eq!(ed.text(), "caf\u{0065}\u{0301}");
        // The combining sequence is one grapheme, so delete_backward removes both
        ed.delete_backward();
        assert_eq!(ed.text(), "caf");
    }

    #[test]
    fn unicode_zwj_sequence() {
        let mut ed = Editor::new();
        // Woman astronaut: woman + ZWJ + rocket
        ed.insert_text("👩\u{200D}🚀");
        let text = ed.text();
        assert!(text.contains("👩"));
        // Move left should treat ZWJ sequence as one grapheme
        ed.move_left();
        // We're now before the ZWJ sequence
        ed.insert_char('x');
        assert!(ed.text().starts_with('x'));
    }

    #[test]
    fn unicode_cjk_wide_chars() {
        let mut ed = Editor::new();
        ed.insert_text("世界");
        assert_eq!(ed.text(), "世界");
        ed.move_left();
        assert_eq!(ed.cursor().grapheme, 1);
        ed.move_left();
        assert_eq!(ed.cursor().grapheme, 0);
    }

    #[test]
    fn crlf_handling() {
        let ed = Editor::with_text("hello\r\nworld");
        assert_eq!(ed.line_count(), 2);
        assert_eq!(ed.line_text(0), Some("hello".to_string()));
        assert_eq!(ed.line_text(1), Some("world".to_string()));
    }

    #[test]
    fn mixed_newlines() {
        let ed = Editor::with_text("line1\nline2\r\nline3");
        assert_eq!(ed.line_count(), 3);
        assert_eq!(ed.line_text(0), Some("line1".to_string()));
        assert_eq!(ed.line_text(1), Some("line2".to_string()));
        assert_eq!(ed.line_text(2), Some("line3".to_string()));
    }

    #[test]
    fn trailing_newline() {
        let ed = Editor::with_text("hello\n");
        assert_eq!(ed.line_count(), 2);
        assert_eq!(ed.line_text(0), Some("hello".to_string()));
        assert_eq!(ed.line_text(1), Some(String::new()));
    }

    #[test]
    fn only_newlines() {
        let ed = Editor::with_text("\n\n\n");
        assert_eq!(ed.line_count(), 4);
        for i in 0..4 {
            assert_eq!(ed.line_text(i), Some(String::new()));
        }
    }

    #[test]
    fn delete_word_backward_at_start_is_noop() {
        let mut ed = Editor::with_text("hello");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        assert!(!ed.delete_word_backward());
        assert_eq!(ed.text(), "hello");
    }

    #[test]
    fn delete_word_backward_multiple_spaces() {
        let mut ed = Editor::with_text("hello    world");
        // Cursor at end
        assert!(ed.delete_word_backward());
        // Should delete "world"
        let remaining = ed.text();
        assert!(remaining.starts_with("hello"));
    }

    #[test]
    fn delete_to_end_at_document_end() {
        let mut ed = Editor::with_text("hello");
        // Cursor already at end from with_text
        assert!(!ed.delete_to_end_of_line());
        assert_eq!(ed.text(), "hello");
    }

    #[test]
    fn select_word_operations() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        // move_word_right now skips the word and trailing whitespace
        ed.select_word_right();
        assert_eq!(ed.selected_text(), Some("hello ".to_string()));
        ed.clear_selection();
        ed.move_to_line_end();
        ed.select_word_left();
        assert_eq!(ed.selected_text(), Some("world".to_string()));
    }

    #[test]
    fn select_up_down() {
        let mut ed = Editor::with_text("line1\nline2\nline3");
        ed.set_cursor(CursorPosition::new(1, 3, 3));
        ed.select_up();
        let sel = ed.selection().expect("should have selection");
        assert_eq!(sel.anchor.line, 1);
        assert_eq!(sel.head.line, 0);
        ed.select_down();
        ed.select_down();
        let sel = ed.selection().expect("should have selection");
        assert_eq!(sel.head.line, 2);
    }

    #[test]
    fn selection_extending_preserves_anchor() {
        let mut ed = Editor::with_text("abcdef");
        ed.set_cursor(CursorPosition::new(0, 2, 2));
        ed.select_right();
        ed.select_right();
        ed.select_right();
        let sel = ed.selection().unwrap();
        assert_eq!(sel.anchor.grapheme, 2);
        assert_eq!(sel.head.grapheme, 5);
        // Now extend left
        ed.select_left();
        let sel = ed.selection().unwrap();
        assert_eq!(sel.anchor.grapheme, 2);
        assert_eq!(sel.head.grapheme, 4);
    }

    #[test]
    fn empty_selection_returns_none() {
        let mut ed = Editor::with_text("hello");
        ed.set_cursor(CursorPosition::new(0, 2, 2));
        // Create selection with same anchor and head
        ed.select_right();
        ed.select_left();
        // Now anchor == head
        let sel = ed.selection().unwrap();
        assert!(sel.is_empty());
        assert_eq!(ed.selected_text(), None);
    }

    #[test]
    fn cursor_clamp_after_set_text() {
        let mut ed = Editor::with_text("very long text here");
        ed.set_text("hi");
        // Cursor should be at end of "hi"
        assert_eq!(ed.cursor().line, 0);
        assert_eq!(ed.cursor().grapheme, 2);
    }

    #[test]
    fn undo_redo_with_selection() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 6, 6));
        // Select "world"
        for _ in 0..5 {
            ed.select_right();
        }
        ed.insert_text("universe");
        assert_eq!(ed.text(), "hello universe");

        // Atomic Replace: 1 undo restores the deleted text and removes inserted
        ed.undo();
        assert_eq!(ed.text(), "hello world");

        // 1 redo reapplies the edit
        ed.redo();
        assert_eq!(ed.text(), "hello universe");
    }

    #[test]
    fn rapid_insert_delete_cycle() {
        let mut ed = Editor::new();
        for i in 0..100 {
            ed.insert_char(char::from_u32('a' as u32 + (i % 26)).unwrap());
            if i % 3 == 0 {
                ed.delete_backward();
            }
        }
        // Should not panic, cursor should be valid
        let cursor = ed.cursor();
        assert!(cursor.line == 0);
        assert!(cursor.grapheme <= ed.text().chars().count());
    }

    #[test]
    fn multiline_select_all_and_replace() {
        let mut ed = Editor::with_text("line1\nline2\nline3");
        ed.select_all();
        ed.insert_text("replaced");
        assert_eq!(ed.text(), "replaced");
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn delete_forward_with_selection() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 0, 0));
        for _ in 0..5 {
            ed.select_right();
        }
        // delete_forward with selection should delete selection, not char after
        ed.delete_forward();
        assert_eq!(ed.text(), " world");
    }

    #[test]
    fn delete_word_backward_with_selection() {
        let mut ed = Editor::with_text("hello world");
        ed.set_cursor(CursorPosition::new(0, 6, 6));
        for _ in 0..5 {
            ed.select_right();
        }
        // Should delete selection, not word
        ed.delete_word_backward();
        assert_eq!(ed.text(), "hello ");
    }

    #[test]
    fn default_impl() {
        let ed = Editor::default();
        assert!(ed.is_empty());
        assert_eq!(ed.cursor(), CursorPosition::default());
    }

    #[test]
    fn line_text_out_of_bounds() {
        let ed = Editor::with_text("hello");
        assert_eq!(ed.line_text(0), Some("hello".to_string()));
        assert_eq!(ed.line_text(1), None);
        assert_eq!(ed.line_text(100), None);
    }

    #[test]
    fn rope_accessor() {
        let ed = Editor::with_text("test");
        let rope = ed.rope();
        assert_eq!(rope.len_bytes(), 4);
    }

    #[test]
    fn insert_text_sanitizes_controls() {
        let mut ed = Editor::new();
        // Insert text with ESC (\x1b) and BEL (\x07) mixed with safe chars
        ed.insert_text("hello\x1bworld\x07\n\t!");
        // Should contain "hello", "world", "\n", "\t", "!" but NO control chars
        assert_eq!(ed.text(), "helloworld\n\t!");
    }

    #[test]
    fn cursor_position_after_multiline_insert() {
        let mut ed = Editor::new();
        ed.insert_text("hello\nworld\nfoo");
        assert_eq!(ed.cursor().line, 2);
        assert_eq!(ed.line_count(), 3);
    }

    #[test]
    fn delete_backward_across_lines() {
        let mut ed = Editor::with_text("abc\ndef");
        ed.set_cursor(CursorPosition::new(1, 0, 0));
        ed.delete_backward();
        assert_eq!(ed.text(), "abcdef");
        assert_eq!(ed.cursor().line, 0);
        assert_eq!(ed.cursor().grapheme, 3);
    }

    #[test]
    fn very_long_line() {
        let long_text: String = "a".repeat(10000);
        let mut ed = Editor::with_text(&long_text);
        assert_eq!(ed.text().len(), 10000);
        ed.move_to_line_start();
        assert_eq!(ed.cursor().grapheme, 0);
        ed.move_to_line_end();
        assert_eq!(ed.cursor().grapheme, 10000);
    }

    #[test]
    fn many_lines() {
        let text: String = (0..1000)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let ed = Editor::with_text(&text);
        assert_eq!(ed.line_count(), 1000);
        assert_eq!(ed.line_text(999), Some("line999".to_string()));
    }

    #[test]
    fn selection_byte_range_order() {
        use crate::cursor::CursorNavigator;

        let mut ed = Editor::with_text("hello world");
        // Select backwards (anchor after head)
        ed.set_cursor(CursorPosition::new(0, 8, 8));
        ed.select_left();
        ed.select_left();
        ed.select_left();

        let sel = ed.selection().unwrap();
        let nav = CursorNavigator::new(ed.rope());
        let (start, end) = sel.byte_range(&nav);
        // byte_range should always have start <= end
        assert!(start <= end);
        assert_eq!(end - start, 3);
    }
}

// ================================================================
// Property-based tests
// ================================================================

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // Strategy for generating valid text content (ASCII + some unicode)
    fn text_strategy() -> impl Strategy<Value = String> {
        prop::string::string_regex("[a-zA-Z0-9 \n]{0,100}")
            .unwrap()
            .prop_filter("non-empty or empty", |_| true)
    }

    // Strategy for text with unicode
    fn unicode_text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just("a".to_string()),
                Just(" ".to_string()),
                Just("\n".to_string()),
                Just("é".to_string()),
                Just("世".to_string()),
                Just("🎉".to_string()),
            ],
            0..50,
        )
        .prop_map(|v| v.join(""))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Property: Cursor is always within valid bounds after any operation
        #[test]
        fn cursor_always_in_bounds(text in text_strategy()) {
            let mut ed = Editor::with_text(&text);

            // After creation
            let c = ed.cursor();
            prop_assert!(c.line < ed.line_count() || (c.line == 0 && ed.line_count() == 1));

            // After various movements
            ed.move_left();
            let c = ed.cursor();
            prop_assert!(c.line < ed.line_count() || (c.line == 0 && ed.line_count() == 1));

            ed.move_right();
            let c = ed.cursor();
            prop_assert!(c.line < ed.line_count() || (c.line == 0 && ed.line_count() == 1));

            ed.move_up();
            let c = ed.cursor();
            prop_assert!(c.line < ed.line_count() || (c.line == 0 && ed.line_count() == 1));

            ed.move_down();
            let c = ed.cursor();
            prop_assert!(c.line < ed.line_count() || (c.line == 0 && ed.line_count() == 1));

            ed.move_to_line_start();
            let c = ed.cursor();
            prop_assert_eq!(c.grapheme, 0);

            ed.move_to_document_start();
            let c = ed.cursor();
            prop_assert_eq!(c.line, 0);
            prop_assert_eq!(c.grapheme, 0);
        }

        // Property: Undo after insert restores original text
        #[test]
        fn undo_insert_restores_text(base in text_strategy(), insert in "[a-z]{1,20}") {
            let mut ed = Editor::with_text(&base);
            let original = ed.text();
            ed.insert_text(&insert);
            prop_assert!(ed.can_undo());
            ed.undo();
            prop_assert_eq!(ed.text(), original);
        }

        // Property: Undo after delete restores original text
        #[test]
        fn undo_delete_restores_text(text in "[a-zA-Z]{5,50}") {
            let mut ed = Editor::with_text(&text);
            let original = ed.text();
            if ed.delete_backward() {
                prop_assert!(ed.can_undo());
                ed.undo();
                prop_assert_eq!(ed.text(), original);
            }
        }

        // Property: Redo after undo restores the edit
        #[test]
        fn redo_after_undo_restores(text in text_strategy(), insert in "[a-z]{1,10}") {
            let mut ed = Editor::with_text(&text);
            ed.insert_text(&insert);
            let after_insert = ed.text();
            ed.undo();
            prop_assert!(ed.can_redo());
            ed.redo();
            prop_assert_eq!(ed.text(), after_insert);
        }

        // Property: select_all + delete = empty
        #[test]
        fn select_all_delete_empties(text in text_strategy()) {
            let mut ed = Editor::with_text(&text);
            ed.select_all();
            ed.delete_backward();
            prop_assert!(ed.is_empty());
        }

        // Property: Line count equals newline count + 1
        #[test]
        fn line_count_matches_newlines(text in text_strategy()) {
            let ed = Editor::with_text(&text);
            let newline_count = text.matches('\n').count();
            // Line count is at least 1, and each \n adds a line
            prop_assert_eq!(ed.line_count(), newline_count + 1);
        }

        // Property: text() roundtrip through set_text
        #[test]
        fn set_text_roundtrip(text in text_strategy()) {
            let mut ed = Editor::new();
            ed.set_text(&text);
            prop_assert_eq!(ed.text(), text);
        }

        // Property: Cursor stays in bounds after unicode operations
        #[test]
        fn unicode_cursor_bounds(text in unicode_text_strategy()) {
            let mut ed = Editor::with_text(&text);

            // Move around
            for _ in 0..10 {
                ed.move_left();
            }
            let c = ed.cursor();
            prop_assert!(c.line < ed.line_count() || ed.line_count() == 1);

            for _ in 0..10 {
                ed.move_right();
            }
            let c = ed.cursor();
            prop_assert!(c.line < ed.line_count() || ed.line_count() == 1);
        }

        // Property: insert_char then delete_backward = original (when no prior content at cursor)
        #[test]
        fn insert_delete_roundtrip(ch in prop::char::any().prop_filter("printable", |c| !c.is_control())) {
            let mut ed = Editor::new();
            ed.insert_char(ch);
            ed.delete_backward();
            prop_assert!(ed.is_empty());
        }

        // Property: Multiple undos don't panic and eventually can't undo
        #[test]
        fn multiple_undos_safe(ops in prop::collection::vec(0..3u8, 0..20)) {
            let mut ed = Editor::new();
            for op in ops {
                match op {
                    0 => { ed.insert_char('x'); }
                    1 => { ed.delete_backward(); }
                    _ => { ed.undo(); }
                }
            }
            // Should be able to undo until stack is empty
            while ed.can_undo() {
                prop_assert!(ed.undo());
            }
            prop_assert!(!ed.can_undo());
        }

        // Property: Selection byte_range always has start <= end
        #[test]
        fn selection_range_ordered(text in "[a-zA-Z]{10,50}") {
            use crate::cursor::CursorNavigator;

            let mut ed = Editor::with_text(&text);
            ed.set_cursor(CursorPosition::new(0, 5, 5));

            // Select in various directions
            ed.select_left();
            ed.select_left();

            if let Some(sel) = ed.selection() {
                let nav = CursorNavigator::new(ed.rope());
                let (start, end) = sel.byte_range(&nav);
                prop_assert!(start <= end);
            }

            ed.select_right();
            ed.select_right();
            ed.select_right();
            ed.select_right();

            if let Some(sel) = ed.selection() {
                let nav = CursorNavigator::new(ed.rope());
                let (start, end) = sel.byte_range(&nav);
                prop_assert!(start <= end);
            }
        }

        // Property: Word movement always makes progress or stays at boundary
        #[test]
        fn word_movement_progress(text in "[a-zA-Z ]{5,50}") {
            let mut ed = Editor::with_text(&text);
            ed.set_cursor(CursorPosition::new(0, 0, 0));

            let start = ed.cursor();
            ed.move_word_right();
            let after = ed.cursor();
            // Either made progress or was already at end
            prop_assert!(after.grapheme >= start.grapheme);

            ed.move_to_line_end();
            let end_pos = ed.cursor();
            ed.move_word_left();
            let after_left = ed.cursor();
            // Either made progress or was already at start
            prop_assert!(after_left.grapheme <= end_pos.grapheme);
        }

        // Property: Document start/end are at expected positions
        #[test]
        fn document_bounds(text in text_strategy()) {
            let mut ed = Editor::with_text(&text);

            ed.move_to_document_start();
            prop_assert_eq!(ed.cursor().line, 0);
            prop_assert_eq!(ed.cursor().grapheme, 0);

            ed.move_to_document_end();
            let c = ed.cursor();
            let last_line = ed.line_count().saturating_sub(1);
            prop_assert_eq!(c.line, last_line);
        }
    }
}

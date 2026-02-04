#![forbid(unsafe_code)]

//! Cursor utilities for text editing widgets.
//!
//! Provides grapheme-aware cursor movement and mapping between logical
//! positions (line + grapheme) and visual columns (cell width).

use crate::rope::Rope;
use crate::wrap::{display_width, graphemes};
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

/// Logical + visual cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorPosition {
    /// Line index (0-based).
    pub line: usize,
    /// Grapheme index within the line (0-based).
    pub grapheme: usize,
    /// Visual column in cells (0-based).
    pub visual_col: usize,
}

impl CursorPosition {
    /// Create a cursor position with explicit fields.
    #[must_use]
    pub const fn new(line: usize, grapheme: usize, visual_col: usize) -> Self {
        Self {
            line,
            grapheme,
            visual_col,
        }
    }
}

/// Cursor navigation helper for rope-backed text.
#[derive(Debug, Clone, Copy)]
pub struct CursorNavigator<'a> {
    rope: &'a Rope,
}

impl<'a> CursorNavigator<'a> {
    /// Create a new navigator for the given rope.
    #[must_use]
    pub const fn new(rope: &'a Rope) -> Self {
        Self { rope }
    }

    /// Clamp an arbitrary position to valid ranges.
    #[must_use]
    pub fn clamp(&self, pos: CursorPosition) -> CursorPosition {
        let line = clamp_line_index(self.rope, pos.line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        let grapheme = pos.grapheme.min(grapheme_count(line_text));
        let visual_col = visual_col_for_grapheme(line_text, grapheme);
        CursorPosition::new(line, grapheme, visual_col)
    }

    /// Build a position from line + grapheme index.
    #[must_use]
    pub fn from_line_grapheme(&self, line: usize, grapheme: usize) -> CursorPosition {
        let line = clamp_line_index(self.rope, line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        let grapheme = grapheme.min(grapheme_count(line_text));
        let visual_col = visual_col_for_grapheme(line_text, grapheme);
        CursorPosition::new(line, grapheme, visual_col)
    }

    /// Build a position from line + visual column.
    #[must_use]
    pub fn from_visual_col(&self, line: usize, visual_col: usize) -> CursorPosition {
        let line = clamp_line_index(self.rope, line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        let grapheme = grapheme_index_at_visual_col(line_text, visual_col);
        let visual_col = visual_col_for_grapheme(line_text, grapheme);
        CursorPosition::new(line, grapheme, visual_col)
    }

    /// Convert a cursor position to a byte index into the rope.
    #[must_use]
    pub fn to_byte_index(&self, pos: CursorPosition) -> usize {
        let pos = self.clamp(pos);
        let line_start_char = self.rope.line_to_char(pos.line);
        let line_start_byte = self.rope.char_to_byte(line_start_char);
        let line_text = line_text(self.rope, pos.line);
        let line_text = strip_trailing_newline(&line_text);
        let byte_offset = grapheme_byte_offset(line_text, pos.grapheme);
        line_start_byte.saturating_add(byte_offset)
    }

    /// Convert a byte index into a cursor position.
    #[must_use]
    pub fn from_byte_index(&self, byte_idx: usize) -> CursorPosition {
        let (line, col_chars) = self.rope.byte_to_line_col(byte_idx);
        let line = clamp_line_index(self.rope, line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        let grapheme = grapheme_index_from_char_offset(line_text, col_chars);
        self.from_line_grapheme(line, grapheme)
    }

    /// Move cursor left by one grapheme (across line boundaries).
    #[must_use]
    pub fn move_left(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        if pos.grapheme > 0 {
            return self.from_line_grapheme(pos.line, pos.grapheme - 1);
        }
        if pos.line == 0 {
            return pos;
        }
        let prev_line = pos.line - 1;
        let prev_text = line_text(self.rope, prev_line);
        let prev_text = strip_trailing_newline(&prev_text);
        let prev_end = grapheme_count(prev_text);
        self.from_line_grapheme(prev_line, prev_end)
    }

    /// Move cursor right by one grapheme (across line boundaries).
    #[must_use]
    pub fn move_right(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        let line_text = line_text(self.rope, pos.line);
        let line_text = strip_trailing_newline(&line_text);
        let line_end = grapheme_count(line_text);
        if pos.grapheme < line_end {
            return self.from_line_grapheme(pos.line, pos.grapheme + 1);
        }
        let last_line = last_line_index(self.rope);
        if pos.line >= last_line {
            return pos;
        }
        self.from_line_grapheme(pos.line + 1, 0)
    }

    /// Move cursor up one line, preserving visual column.
    #[must_use]
    pub fn move_up(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        if pos.line == 0 {
            return pos;
        }
        self.from_visual_col(pos.line - 1, pos.visual_col)
    }

    /// Move cursor down one line, preserving visual column.
    #[must_use]
    pub fn move_down(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        let last_line = last_line_index(self.rope);
        if pos.line >= last_line {
            return pos;
        }
        self.from_visual_col(pos.line + 1, pos.visual_col)
    }

    /// Move cursor to start of line.
    #[must_use]
    pub fn line_start(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        self.from_line_grapheme(pos.line, 0)
    }

    /// Move cursor to end of line.
    #[must_use]
    pub fn line_end(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        let line_text = line_text(self.rope, pos.line);
        let line_text = strip_trailing_newline(&line_text);
        let end = grapheme_count(line_text);
        self.from_line_grapheme(pos.line, end)
    }

    /// Move cursor to start of document.
    #[must_use]
    pub fn document_start(&self) -> CursorPosition {
        self.from_line_grapheme(0, 0)
    }

    /// Move cursor to end of document.
    #[must_use]
    pub fn document_end(&self) -> CursorPosition {
        let last_line = last_line_index(self.rope);
        let line_text = line_text(self.rope, last_line);
        let line_text = strip_trailing_newline(&line_text);
        let end = grapheme_count(line_text);
        self.from_line_grapheme(last_line, end)
    }

    /// Move cursor left by one word boundary.
    #[must_use]
    pub fn move_word_left(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        if pos.line == 0 && pos.grapheme == 0 {
            return pos;
        }
        if pos.grapheme == 0 {
            let prev_line = pos.line - 1;
            let prev_text = line_text(self.rope, prev_line);
            let prev_text = strip_trailing_newline(&prev_text);
            let end = grapheme_count(prev_text);
            let next = move_word_left_in_line(prev_text, end);
            return self.from_line_grapheme(prev_line, next);
        }
        let line_text = line_text(self.rope, pos.line);
        let line_text = strip_trailing_newline(&line_text);
        let next = move_word_left_in_line(line_text, pos.grapheme);
        self.from_line_grapheme(pos.line, next)
    }

    /// Move cursor right by one word boundary.
    #[must_use]
    pub fn move_word_right(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        let line_text = line_text(self.rope, pos.line);
        let line_text = strip_trailing_newline(&line_text);
        let end = grapheme_count(line_text);
        if pos.grapheme >= end {
            let last_line = last_line_index(self.rope);
            if pos.line >= last_line {
                return pos;
            }
            return self.from_line_grapheme(pos.line + 1, 0);
        }
        let next = move_word_right_in_line(line_text, pos.grapheme);
        self.from_line_grapheme(pos.line, next)
    }
}

fn clamp_line_index(rope: &Rope, line: usize) -> usize {
    let last = last_line_index(rope);
    if line > last { last } else { line }
}

fn last_line_index(rope: &Rope) -> usize {
    let lines = rope.len_lines();
    if lines == 0 { 0 } else { lines - 1 }
}

fn line_text<'a>(rope: &'a Rope, line: usize) -> Cow<'a, str> {
    rope.line(line).unwrap_or(Cow::Borrowed(""))
}

fn strip_trailing_newline(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

fn grapheme_count(text: &str) -> usize {
    graphemes(text).count()
}

fn visual_col_for_grapheme(text: &str, grapheme_idx: usize) -> usize {
    graphemes(text).take(grapheme_idx).map(display_width).sum()
}

fn grapheme_index_at_visual_col(text: &str, visual_col: usize) -> usize {
    let mut col = 0usize;
    let mut idx = 0usize;
    for g in graphemes(text) {
        let w = display_width(g);
        if col.saturating_add(w) > visual_col {
            break;
        }
        col = col.saturating_add(w);
        idx = idx.saturating_add(1);
    }
    idx
}

fn grapheme_byte_offset(text: &str, grapheme_idx: usize) -> usize {
    text.grapheme_indices(true)
        .nth(grapheme_idx)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

fn grapheme_index_from_char_offset(text: &str, char_offset: usize) -> usize {
    let mut char_count = 0usize;
    let mut g_idx = 0usize;
    for g in graphemes(text) {
        let g_chars = g.chars().count();
        if char_count.saturating_add(g_chars) > char_offset {
            return g_idx;
        }
        char_count = char_count.saturating_add(g_chars);
        g_idx = g_idx.saturating_add(1);
    }
    g_idx
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphemeClass {
    Space,
    Word,
    Punct,
}

fn grapheme_class(g: &str) -> GraphemeClass {
    if g.chars().all(char::is_whitespace) {
        GraphemeClass::Space
    } else if g.chars().any(char::is_alphanumeric) {
        GraphemeClass::Word
    } else {
        GraphemeClass::Punct
    }
}

fn move_word_left_in_line(text: &str, grapheme_idx: usize) -> usize {
    let graphemes: Vec<&str> = graphemes(text).collect();
    let mut pos = grapheme_idx.min(graphemes.len());
    if pos == 0 {
        return 0;
    }
    while pos > 0 && grapheme_class(graphemes[pos - 1]) == GraphemeClass::Space {
        pos = pos.saturating_sub(1);
    }
    if pos == 0 {
        return 0;
    }
    let target = grapheme_class(graphemes[pos - 1]);
    while pos > 0 && grapheme_class(graphemes[pos - 1]) == target {
        pos = pos.saturating_sub(1);
    }
    pos
}

fn move_word_right_in_line(text: &str, grapheme_idx: usize) -> usize {
    let mut iter = graphemes(text).peekable();
    let mut pos = 0usize;

    while pos < grapheme_idx {
        if iter.next().is_none() {
            return pos;
        }
        pos = pos.saturating_add(1);
    }

    let Some(current) = iter.peek() else {
        return pos;
    };

    if grapheme_class(current) == GraphemeClass::Space {
        while let Some(g) = iter.peek() {
            if grapheme_class(g) != GraphemeClass::Space {
                break;
            }
            iter.next();
            pos = pos.saturating_add(1);
        }
        return pos;
    }

    let target = grapheme_class(current);
    while let Some(g) = iter.peek() {
        if grapheme_class(g) != target {
            break;
        }
        iter.next();
        pos = pos.saturating_add(1);
    }

    while let Some(g) = iter.peek() {
        if grapheme_class(g) != GraphemeClass::Space {
            break;
        }
        iter.next();
        pos = pos.saturating_add(1);
    }

    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(text: &str) -> Rope {
        Rope::from_text(text)
    }

    #[test]
    fn left_right_grapheme_moves() {
        let r = rope("ab");
        let nav = CursorNavigator::new(&r);
        let mut pos = nav.from_line_grapheme(0, 0);
        pos = nav.move_right(pos);
        assert_eq!(pos.grapheme, 1);
        pos = nav.move_right(pos);
        assert_eq!(pos.grapheme, 2);
        pos = nav.move_left(pos);
        assert_eq!(pos.grapheme, 1);
    }

    #[test]
    fn combining_mark_is_single_grapheme() {
        let r = rope("e\u{0301}x");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 1);
        assert_eq!(pos.visual_col, 1);
        let next = nav.move_right(pos);
        assert_eq!(next.grapheme, 2);
    }

    #[test]
    fn emoji_zwj_grapheme_width() {
        let r = rope("\u{1F469}\u{200D}\u{1F680}x");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 1);
        assert_eq!(pos.visual_col, 2);
        let next = nav.move_right(pos);
        assert_eq!(next.grapheme, 2);
    }

    #[test]
    fn tab_counts_as_one_cell() {
        let r = rope("a\tb");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 2);
        assert_eq!(pos.visual_col, 2);
        let mid = nav.from_visual_col(0, 1);
        assert_eq!(mid.grapheme, 1);
        assert_eq!(mid.visual_col, 1);
    }

    #[test]
    fn visual_col_to_grapheme_clamps_inside_wide() {
        let r = rope("ab\u{754C}");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_visual_col(0, 3);
        assert_eq!(pos.grapheme, 2);
        assert_eq!(pos.visual_col, 2);
    }

    #[test]
    fn move_up_down_preserves_visual_col() {
        let r = rope("abcd\nx\u{754C}");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 3); // visual_col = 3
        let down = nav.move_down(pos);
        assert_eq!(down.line, 1);
        assert_eq!(down.grapheme, 2);
        assert_eq!(down.visual_col, 3);
        let up = nav.move_up(down);
        assert_eq!(up.line, 0);
    }

    #[test]
    fn word_movement_respects_classes() {
        let r = rope("hello  world!!!");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 0);
        // move_word_right skips the word class then any trailing whitespace
        let right = nav.move_word_right(pos);
        assert_eq!(right.grapheme, 7); // past "hello" + spaces
        let right = nav.move_word_right(right);
        assert_eq!(right.grapheme, 12); // past "world" (no trailing space before punct)
        let right = nav.move_word_right(right);
        assert_eq!(right.grapheme, 15); // past "!!!"
        let left = nav.move_word_left(right);
        assert_eq!(left.grapheme, 12); // back to start of "!!!"
    }

    #[test]
    fn byte_index_roundtrip() {
        let r = rope("a\nbc");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(1, 1);
        let byte = nav.to_byte_index(pos);
        let back = nav.from_byte_index(byte);
        assert_eq!(back.line, 1);
        assert_eq!(back.grapheme, 1);
    }

    // ====== Empty text ======

    #[test]
    fn empty_text_navigation() {
        let r = rope("");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 0);
        assert_eq!(pos.line, 0);
        assert_eq!(pos.grapheme, 0);
        assert_eq!(pos.visual_col, 0);
    }

    #[test]
    fn empty_text_move_left_is_noop() {
        let r = rope("");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 0);
        let moved = nav.move_left(pos);
        assert_eq!(moved, pos);
    }

    #[test]
    fn empty_text_move_right_is_noop() {
        let r = rope("");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 0);
        let moved = nav.move_right(pos);
        assert_eq!(moved, pos);
    }

    #[test]
    fn empty_text_document_start_end() {
        let r = rope("");
        let nav = CursorNavigator::new(&r);
        let start = nav.document_start();
        let end = nav.document_end();
        assert_eq!(start, end);
        assert_eq!(start.line, 0);
        assert_eq!(start.grapheme, 0);
    }

    // ====== Clamping ======

    #[test]
    fn clamp_out_of_bounds_line() {
        let r = rope("abc");
        let nav = CursorNavigator::new(&r);
        let pos = CursorPosition::new(100, 0, 0);
        let clamped = nav.clamp(pos);
        assert_eq!(clamped.line, 0);
    }

    #[test]
    fn clamp_out_of_bounds_grapheme() {
        let r = rope("abc");
        let nav = CursorNavigator::new(&r);
        let pos = CursorPosition::new(0, 100, 0);
        let clamped = nav.clamp(pos);
        assert_eq!(clamped.grapheme, 3);
        assert_eq!(clamped.visual_col, 3);
    }

    #[test]
    fn clamp_multiline_out_of_bounds() {
        let r = rope("abc\ndef");
        let nav = CursorNavigator::new(&r);
        let pos = CursorPosition::new(5, 50, 0);
        let clamped = nav.clamp(pos);
        assert_eq!(clamped.line, 1);
        assert_eq!(clamped.grapheme, 3);
    }

    // ====== Line start/end ======

    #[test]
    fn line_start_moves_to_column_zero() {
        let r = rope("hello world");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 5);
        let start = nav.line_start(pos);
        assert_eq!(start.grapheme, 0);
        assert_eq!(start.visual_col, 0);
    }

    #[test]
    fn line_end_moves_to_last_grapheme() {
        let r = rope("hello");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 0);
        let end = nav.line_end(pos);
        assert_eq!(end.grapheme, 5);
        assert_eq!(end.visual_col, 5);
    }

    #[test]
    fn line_start_end_multiline() {
        let r = rope("abc\nde");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(1, 1);
        let start = nav.line_start(pos);
        assert_eq!(start.line, 1);
        assert_eq!(start.grapheme, 0);
        let end = nav.line_end(pos);
        assert_eq!(end.line, 1);
        assert_eq!(end.grapheme, 2);
    }

    // ====== Document start/end ======

    #[test]
    fn document_start_is_0_0() {
        let r = rope("abc\ndef\nghi");
        let nav = CursorNavigator::new(&r);
        let start = nav.document_start();
        assert_eq!(start.line, 0);
        assert_eq!(start.grapheme, 0);
        assert_eq!(start.visual_col, 0);
    }

    #[test]
    fn document_end_is_last_line_last_grapheme() {
        let r = rope("abc\ndef\nghi");
        let nav = CursorNavigator::new(&r);
        let end = nav.document_end();
        assert_eq!(end.line, 2);
        assert_eq!(end.grapheme, 3);
        assert_eq!(end.visual_col, 3);
    }

    // ====== Cross-line movement ======

    #[test]
    fn move_left_wraps_to_previous_line() {
        let r = rope("abc\ndef");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(1, 0);
        let moved = nav.move_left(pos);
        assert_eq!(moved.line, 0);
        assert_eq!(moved.grapheme, 3);
    }

    #[test]
    fn move_right_wraps_to_next_line() {
        let r = rope("abc\ndef");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 3);
        let moved = nav.move_right(pos);
        assert_eq!(moved.line, 1);
        assert_eq!(moved.grapheme, 0);
    }

    #[test]
    fn move_left_at_document_start_is_noop() {
        let r = rope("abc");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 0);
        let moved = nav.move_left(pos);
        assert_eq!(moved, pos);
    }

    #[test]
    fn move_right_at_document_end_is_noop() {
        let r = rope("abc");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 3);
        let moved = nav.move_right(pos);
        assert_eq!(moved, pos);
    }

    // ====== Up/down movement ======

    #[test]
    fn move_up_at_first_line_is_noop() {
        let r = rope("abc\ndef");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 1);
        let moved = nav.move_up(pos);
        assert_eq!(moved, pos);
    }

    #[test]
    fn move_down_at_last_line_is_noop() {
        let r = rope("abc\ndef");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(1, 1);
        let moved = nav.move_down(pos);
        assert_eq!(moved, pos);
    }

    #[test]
    fn move_down_shorter_line_clamps_grapheme() {
        let r = rope("abcdef\nxy");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 5); // visual_col=5
        let down = nav.move_down(pos);
        assert_eq!(down.line, 1);
        assert_eq!(down.grapheme, 2); // "xy" only has 2 graphemes
        assert_eq!(down.visual_col, 2);
    }

    #[test]
    fn move_up_shorter_line_clamps_grapheme() {
        let r = rope("xy\nabcdef");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(1, 5); // visual_col=5
        let up = nav.move_up(pos);
        assert_eq!(up.line, 0);
        assert_eq!(up.grapheme, 2);
        assert_eq!(up.visual_col, 2);
    }

    // ====== Wide character visual column handling ======

    #[test]
    fn wide_char_visual_col() {
        // CJK characters are 2 cells wide
        let r = rope("\u{4E16}\u{754C}"); // "世界"
        let nav = CursorNavigator::new(&r);
        let pos0 = nav.from_line_grapheme(0, 0);
        assert_eq!(pos0.visual_col, 0);
        let pos1 = nav.from_line_grapheme(0, 1);
        assert_eq!(pos1.visual_col, 2);
        let pos2 = nav.from_line_grapheme(0, 2);
        assert_eq!(pos2.visual_col, 4);
    }

    #[test]
    fn from_visual_col_with_wide_chars() {
        let r = rope("\u{4E16}\u{754C}x"); // "世界x"
        let nav = CursorNavigator::new(&r);
        // visual_col=1 falls inside first wide char -> snap to grapheme 0
        let pos = nav.from_visual_col(0, 1);
        assert_eq!(pos.grapheme, 0);
        assert_eq!(pos.visual_col, 0);
        // visual_col=2 starts at second char
        let pos = nav.from_visual_col(0, 2);
        assert_eq!(pos.grapheme, 1);
        assert_eq!(pos.visual_col, 2);
        // visual_col=4 is 'x'
        let pos = nav.from_visual_col(0, 4);
        assert_eq!(pos.grapheme, 2);
        assert_eq!(pos.visual_col, 4);
    }

    // ====== Word movement ======

    #[test]
    fn word_right_from_start() {
        let r = rope("hello world");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 0);
        let moved = nav.move_word_right(pos);
        assert_eq!(moved.grapheme, 6); // start of "world"
    }

    #[test]
    fn word_left_from_end() {
        let r = rope("hello world");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 11);
        let moved = nav.move_word_left(pos);
        assert_eq!(moved.grapheme, 6); // start of "world"
    }

    #[test]
    fn word_right_at_line_end_wraps() {
        let r = rope("hello\nworld");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 5);
        let moved = nav.move_word_right(pos);
        assert_eq!(moved.line, 1);
        assert_eq!(moved.grapheme, 0);
    }

    #[test]
    fn word_left_at_line_start_wraps() {
        let r = rope("hello\nworld");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(1, 0);
        let moved = nav.move_word_left(pos);
        assert_eq!(moved.line, 0);
        // Should go to previous line end, finding word boundary
        assert!(moved.grapheme <= 5);
    }

    #[test]
    fn word_right_skips_punctuation() {
        let r = rope("a!!b");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(0, 1);
        let moved = nav.move_word_right(pos);
        assert_eq!(moved.grapheme, 3); // skips "!!" (punctuation class)
    }

    #[test]
    fn word_movement_at_document_boundaries() {
        let r = rope("abc");
        let nav = CursorNavigator::new(&r);
        // word left at start is noop
        let start = nav.from_line_grapheme(0, 0);
        let left = nav.move_word_left(start);
        assert_eq!(left, start);
        // word right at end is noop
        let end = nav.from_line_grapheme(0, 3);
        let right = nav.move_word_right(end);
        assert_eq!(right, end);
    }

    // ====== Byte index roundtrips ======

    #[test]
    fn byte_index_roundtrip_multibyte() {
        let r = rope("a\u{1F600}b"); // a 😀 b
        let nav = CursorNavigator::new(&r);
        for g in 0..=3 {
            let pos = nav.from_line_grapheme(0, g);
            let byte = nav.to_byte_index(pos);
            let back = nav.from_byte_index(byte);
            assert_eq!(
                back.grapheme, pos.grapheme,
                "roundtrip failed for grapheme {g}"
            );
        }
    }

    #[test]
    fn byte_index_roundtrip_multiline_unicode() {
        let r = rope("ab\n\u{4E16}\u{754C}");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_line_grapheme(1, 1); // 界
        let byte = nav.to_byte_index(pos);
        let back = nav.from_byte_index(byte);
        assert_eq!(back.line, 1);
        assert_eq!(back.grapheme, 1);
    }

    // ====== from_visual_col edge cases ======

    #[test]
    fn from_visual_col_beyond_line_clamps() {
        let r = rope("abc");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_visual_col(0, 100);
        assert_eq!(pos.grapheme, 3);
        assert_eq!(pos.visual_col, 3);
    }

    #[test]
    fn from_visual_col_zero_on_empty_line() {
        let r = rope("abc\n\ndef");
        let nav = CursorNavigator::new(&r);
        let pos = nav.from_visual_col(1, 5);
        assert_eq!(pos.grapheme, 0);
        assert_eq!(pos.visual_col, 0);
    }

    // ====== Internal helper tests ======

    #[test]
    fn grapheme_class_classification() {
        use super::GraphemeClass;
        use super::grapheme_class;
        assert_eq!(grapheme_class(" "), GraphemeClass::Space);
        assert_eq!(grapheme_class("\t"), GraphemeClass::Space);
        assert_eq!(grapheme_class("a"), GraphemeClass::Word);
        assert_eq!(grapheme_class("5"), GraphemeClass::Word);
        assert_eq!(grapheme_class("!"), GraphemeClass::Punct);
        assert_eq!(grapheme_class("."), GraphemeClass::Punct);
    }

    #[test]
    fn move_word_left_in_line_edge_cases() {
        use super::move_word_left_in_line;
        // Already at start
        assert_eq!(move_word_left_in_line("hello", 0), 0);
        // Single word
        assert_eq!(move_word_left_in_line("hello", 5), 0);
        // Empty string
        assert_eq!(move_word_left_in_line("", 0), 0);
    }

    #[test]
    fn move_word_right_in_line_edge_cases() {
        use super::move_word_right_in_line;
        // Already at end
        assert_eq!(move_word_right_in_line("hello", 5), 5);
        // Single word from start
        assert_eq!(move_word_right_in_line("hello", 0), 5);
        // Empty string
        assert_eq!(move_word_right_in_line("", 0), 0);
    }
}

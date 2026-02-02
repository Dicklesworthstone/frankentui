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
    let target = grapheme_class(graphemes[pos - 1]);
    while pos > 0 && grapheme_class(graphemes[pos - 1]) == target {
        pos = pos.saturating_sub(1);
    }
    pos
}

fn move_word_right_in_line(text: &str, grapheme_idx: usize) -> usize {
    let graphemes: Vec<&str> = graphemes(text).collect();
    let max = graphemes.len();
    let mut pos = grapheme_idx.min(max);
    if pos >= max {
        return max;
    }
    let target = grapheme_class(graphemes[pos]);
    while pos < max && grapheme_class(graphemes[pos]) == target {
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
        let right = nav.move_word_right(pos);
        assert_eq!(right.grapheme, 5);
        let right = nav.move_word_right(right);
        assert_eq!(right.grapheme, 7); // skips spaces
        let right = nav.move_word_right(right);
        assert_eq!(right.grapheme, 12); // end of "world"
        let left = nav.move_word_left(right);
        assert_eq!(left.grapheme, 7);
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
}

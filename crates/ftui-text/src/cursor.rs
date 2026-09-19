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

    /// Move cursor backward by one grapheme in logical document order (across line boundaries).
    #[must_use]
    pub fn move_grapheme_backward(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        if pos.grapheme > 0 {
            return self.from_line_grapheme(pos.line, pos.grapheme - 1);
        }
        if pos.line == 0 {
            return pos;
        }
        let prev_line = pos.line - 1;
        let prev_raw = line_text(self.rope, prev_line);
        let prev_text = strip_trailing_newline(&prev_raw);
        let prev_end = grapheme_count(prev_text);
        self.from_line_grapheme(prev_line, prev_end)
    }

    /// Move cursor forward by one grapheme in logical document order (across line boundaries).
    #[must_use]
    pub fn move_grapheme_forward(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        let raw = line_text(self.rope, pos.line);
        let current_text = strip_trailing_newline(&raw);
        let line_end = grapheme_count(current_text);
        if pos.grapheme < line_end {
            return self.from_line_grapheme(pos.line, pos.grapheme + 1);
        }
        let last_line = last_line_index(self.rope);
        if pos.line >= last_line {
            return pos;
        }
        self.from_line_grapheme(pos.line + 1, 0)
    }

    /// Move cursor left by one grapheme (across line boundaries).
    ///
    /// When the `bidi` feature is enabled and the line contains RTL characters,
    /// this moves in visual left order on screen. Otherwise, moves backward in
    /// logical document order.
    #[must_use]
    pub fn move_left(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        #[cfg(feature = "bidi")]
        {
            let raw = line_text(self.rope, pos.line);
            let current_text = strip_trailing_newline(&raw);
            if crate::bidi::has_rtl(current_text) {
                let seg = crate::bidi::BidiSegment::new(current_text, None);
                let next_grapheme = seg.move_left(pos.grapheme);
                if next_grapheme != pos.grapheme {
                    return self.from_line_grapheme(pos.line, next_grapheme);
                }
                if pos.line == 0 {
                    return pos;
                }
                let prev_line = pos.line - 1;
                let prev_raw = line_text(self.rope, prev_line);
                let prev_text = strip_trailing_newline(&prev_raw);
                let prev_seg = crate::bidi::BidiSegment::new(prev_text, None);
                let prev_end = prev_seg.logical_cursor_pos(prev_seg.len());
                return self.from_line_grapheme(prev_line, prev_end);
            }
        }
        self.move_grapheme_backward(pos)
    }

    /// Move cursor right by one grapheme (across line boundaries).
    ///
    /// When the `bidi` feature is enabled and the line contains RTL characters,
    /// this moves in visual right order on screen. Otherwise, moves forward in
    /// logical document order.
    #[must_use]
    pub fn move_right(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        #[cfg(feature = "bidi")]
        {
            let raw = line_text(self.rope, pos.line);
            let current_text = strip_trailing_newline(&raw);
            if crate::bidi::has_rtl(current_text) {
                let seg = crate::bidi::BidiSegment::new(current_text, None);
                let next_grapheme = seg.move_right(pos.grapheme);
                if next_grapheme != pos.grapheme {
                    return self.from_line_grapheme(pos.line, next_grapheme);
                }
                let last_line = last_line_index(self.rope);
                if pos.line >= last_line {
                    return pos;
                }
                let next_line = pos.line + 1;
                let next_raw = line_text(self.rope, next_line);
                let next_text = strip_trailing_newline(&next_raw);
                let next_seg = crate::bidi::BidiSegment::new(next_text, None);
                let next_start = next_seg.logical_cursor_pos(0);
                return self.from_line_grapheme(next_line, next_start);
            }
        }
        self.move_grapheme_forward(pos)
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

    /// Move to column zero of the blank line after the current paragraph.
    ///
    /// Empty and Unicode-whitespace-only lines are blank. Starting on a blank
    /// line skips that blank run and the next paragraph. If no boundary remains,
    /// move to the document end. For `one\n\ntwo\n\nthree`, repeated moves from
    /// the start visit lines 1, 3, then the end of line 4.
    #[must_use]
    pub fn move_paragraph_down(&self, pos: CursorPosition) -> CursorPosition {
        let mut line = self.clamp(pos).line;
        let last = last_line_index(self.rope);
        while line < last && is_blank_line(self.rope, line) {
            line += 1;
        }
        if line == last {
            return self.document_end();
        }
        while line < last && !is_blank_line(self.rope, line) {
            line += 1;
        }
        if !is_blank_line(self.rope, line) {
            self.document_end()
        } else {
            self.from_line_grapheme(line, 0)
        }
    }

    /// Move to column zero of the blank line before the current paragraph.
    ///
    /// Empty and Unicode-whitespace-only lines are blank. Starting on a blank
    /// line skips that blank run and the preceding paragraph. If no boundary
    /// remains, move to the document start. For `one\n\ntwo\n\nthree`, repeated
    /// moves from the end visit lines 3, 1, then the start of line 0.
    #[must_use]
    pub fn move_paragraph_up(&self, pos: CursorPosition) -> CursorPosition {
        let mut line = self.clamp(pos).line;
        while line > 0 && is_blank_line(self.rope, line) {
            line -= 1;
        }
        while line > 0 && !is_blank_line(self.rope, line) {
            line -= 1;
        }
        self.from_line_grapheme(line, 0)
    }

    /// Move cursor to start of line in logical document order (grapheme 0).
    #[must_use]
    pub fn logical_line_start(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        self.from_line_grapheme(pos.line, 0)
    }

    /// Move cursor to end of line in logical document order (after last grapheme).
    #[must_use]
    pub fn logical_line_end(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        let line_text = line_text(self.rope, pos.line);
        let line_text = strip_trailing_newline(&line_text);
        let end = grapheme_count(line_text);
        self.from_line_grapheme(pos.line, end)
    }

    /// Move cursor to start of line.
    ///
    /// When the `bidi` feature is enabled and the line contains RTL characters,
    /// this moves to the visual left start of the line. Otherwise, moves to the
    /// logical start of the line.
    #[must_use]
    pub fn line_start(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        #[cfg(feature = "bidi")]
        {
            let line_text = line_text(self.rope, pos.line);
            let line_text = strip_trailing_newline(&line_text);
            if crate::bidi::has_rtl(line_text) {
                let seg = crate::bidi::BidiSegment::new(line_text, None);
                return self.from_line_grapheme(pos.line, seg.logical_cursor_pos(0));
            }
        }
        self.logical_line_start(pos)
    }

    /// Move cursor to end of line.
    ///
    /// When the `bidi` feature is enabled and the line contains RTL characters,
    /// this moves to the visual right end of the line. Otherwise, moves to the
    /// logical end of the line.
    #[must_use]
    pub fn line_end(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        #[cfg(feature = "bidi")]
        {
            let line_text = line_text(self.rope, pos.line);
            let line_text = strip_trailing_newline(&line_text);
            if crate::bidi::has_rtl(line_text) {
                let seg = crate::bidi::BidiSegment::new(line_text, None);
                return self.from_line_grapheme(pos.line, seg.logical_cursor_pos(seg.len()));
            }
        }
        self.logical_line_end(pos)
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

/// Strip the line terminator from a line produced by [`Rope::line`].
///
/// ropey splits lines on the full Unicode set — LF, CR, CRLF, VT, FF, NEL,
/// LINE SEPARATOR and PARAGRAPH SEPARATOR — so a line handed back by the rope
/// can end with any of them. Stripping only LF and CR left the other five
/// sitting in the line body, and every caller here is asking "what is this
/// line's content", so they all saw a terminator as a grapheme.
///
/// That mismatch is what made backspace unable to remove U+2028 or U+2029
/// (bd-jlecn): with the terminator counted as content, `move_grapheme_backward`
/// returned a position on the previous line that resolved to the *same* byte
/// offset as the cursor, so the delete range was empty.
fn strip_trailing_newline(text: &str) -> &str {
    // CRLF first: the CR belongs to the same terminator as the LF.
    if let Some(stripped) = text.strip_suffix('\n') {
        return stripped.strip_suffix('\r').unwrap_or(stripped);
    }
    for terminator in [
        '\r', '\u{000B}', '\u{000C}', '\u{0085}', '\u{2028}', '\u{2029}',
    ] {
        if let Some(stripped) = text.strip_suffix(terminator) {
            return stripped;
        }
    }
    text
}

fn is_blank_line(rope: &Rope, line: usize) -> bool {
    strip_trailing_newline(&line_text(rope, line))
        .chars()
        .all(char::is_whitespace)
}

fn grapheme_count(text: &str) -> usize {
    graphemes(text).count()
}

fn visual_col_for_grapheme(text: &str, grapheme_idx: usize) -> usize {
    #[cfg(feature = "bidi")]
    if crate::bidi::has_rtl(text) {
        let seg = crate::bidi::BidiSegment::new(text, None);
        let visual_grapheme = seg.visual_cursor_pos(grapheme_idx);
        let mut col = 0usize;
        for v in 0..visual_grapheme {
            if let Some(ch) = seg.char_at_visual(v) {
                let mut buf = [0u8; 4];
                col = col.saturating_add(display_width(ch.encode_utf8(&mut buf)));
            }
        }
        return col;
    }
    graphemes(text).take(grapheme_idx).map(display_width).sum()
}

fn grapheme_index_at_visual_col(text: &str, visual_col: usize) -> usize {
    #[cfg(feature = "bidi")]
    if crate::bidi::has_rtl(text) {
        let seg = crate::bidi::BidiSegment::new(text, None);
        let mut col = 0usize;
        let mut visual_idx = 0usize;
        for v in 0..seg.len() {
            let w = if let Some(ch) = seg.char_at_visual(v) {
                let mut buf = [0u8; 4];
                display_width(ch.encode_utf8(&mut buf))
            } else {
                1
            };
            if col.saturating_add(w) > visual_col {
                break;
            }
            col = col.saturating_add(w);
            visual_idx = visual_idx.saturating_add(1);
        }
        return seg.logical_cursor_pos(visual_idx);
    }
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
    if grapheme_idx == 0 {
        return 0;
    }

    let byte_offset = grapheme_byte_offset(text, grapheme_idx);
    let before_cursor = &text[..byte_offset];
    let mut pos = grapheme_idx;

    let mut iter = before_cursor.graphemes(true).rev();

    while let Some(g) = iter.next() {
        if grapheme_class(g) == GraphemeClass::Space {
            pos = pos.saturating_sub(1);
        } else {
            pos = pos.saturating_sub(1);
            let target = grapheme_class(g);
            for g_next in iter {
                if grapheme_class(g_next) == target {
                    pos = pos.saturating_sub(1);
                } else {
                    break;
                }
            }
            break;
        }
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
    use proptest::prelude::*;

    fn rope(text: &str) -> Rope {
        Rope::from_text(text)
    }

    #[test]
    fn paragraph_moves_over_mixed_blank_lines() {
        for newline in ["\n", "\r\n"] {
            let text = [
                "one",
                "continued",
                "",
                "two",
                "",
                "\t",
                "three",
                "\u{2003}",
                "last",
            ]
            .join(newline);
            let r = rope(&text);
            let nav = CursorNavigator::new(&r);
            let mut pos = nav.document_start();
            for line in [2, 4, 7, 8, 8] {
                pos = nav.move_paragraph_down(pos);
                assert_eq!(pos.line, line, "fixture={text:?}");
                assert_eq!(pos.grapheme, if line == 8 { 4 } else { 0 });
            }
            // Up lands on the last blank before a paragraph; down lands on
            // the first blank after it. A multi-line separator is asymmetric.
            for line in [7, 5, 2, 0, 0] {
                pos = nav.move_paragraph_up(pos);
                assert_eq!(pos, CursorPosition::new(line, 0, 0), "fixture={text:?}");
            }
            assert_eq!(
                nav.move_paragraph_up(nav.from_line_grapheme(1, 3)),
                nav.document_start()
            );
        }
    }

    #[test]
    fn paragraph_moves_at_empty_single_and_trailing_boundaries() {
        for text in [
            "", "one", "one\ntwo", "\n", " \n\t", "one\n", "one\r\n", "one\n  ",
        ] {
            let r = rope(text);
            let nav = CursorNavigator::new(&r);
            let start = nav.document_start();
            let end = nav.document_end();
            assert_eq!(nav.move_paragraph_up(start), start, "fixture={text:?}");
            assert_eq!(nav.move_paragraph_down(end), end, "fixture={text:?}");
            assert_eq!(nav.move_paragraph_up(end), start, "fixture={text:?}");
            let down = nav.move_paragraph_down(start);
            assert_eq!(nav.move_paragraph_down(down), end, "fixture={text:?}");
            if text == "one\n  " {
                assert_eq!(down, CursorPosition::new(1, 0, 0));
            } else {
                assert_eq!(down, end, "fixture={text:?}");
            }
            let invalid = CursorPosition::new(usize::MAX, usize::MAX, usize::MAX);
            assert_eq!(nav.move_paragraph_down(invalid), end);
            assert_eq!(nav.move_paragraph_up(invalid), start);
        }
    }

    proptest! {
        #[test]
        fn paragraph_moves_are_monotone_and_idempotent_at_ends(
            lines in proptest::collection::vec(
                prop::sample::select(vec!["", "  ", "\t", "\u{2003}", "word", "two words", "界e\u{301}"]),
                0..=40,
            ),
            crlf in any::<bool>(),
            line in 0usize..50,
            column in 0usize..20,
        ) {
            let text = lines.join(if crlf { "\r\n" } else { "\n" });
            let r = rope(&text);
            let nav = CursorNavigator::new(&r);
            let pos = nav.from_line_grapheme(line, column);
            let up = nav.move_paragraph_up(pos);
            let down = nav.move_paragraph_down(pos);
            prop_assert!(nav.to_byte_index(up) <= nav.to_byte_index(pos), "fixture={:?}", text);
            prop_assert!(nav.to_byte_index(down) >= nav.to_byte_index(pos), "fixture={:?}", text);
            prop_assert_eq!(up, nav.clamp(up));
            prop_assert_eq!(down, nav.clamp(down));
            prop_assert_eq!(nav.move_paragraph_up(nav.document_start()), nav.document_start());
            prop_assert_eq!(nav.move_paragraph_down(nav.document_end()), nav.document_end());
        }
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

    #[cfg(feature = "bidi")]
    #[test]
    fn rtl_cursor_navigation() {
        // Arabic text: "مرحبا" (5 characters, pure RTL)
        let r = rope("\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}");
        let nav = CursorNavigator::new(&r);

        // At logical 0 (visual right end):
        let pos0 = nav.from_line_grapheme(0, 0);
        // Right at visual right edge should be a no-op
        let right = nav.move_right(pos0);
        assert_eq!(right.grapheme, 0);

        // Left moves visually left (logical +1)
        let left1 = nav.move_left(pos0);
        assert_eq!(left1.grapheme, 1);

        // Home goes to visual left (logical 5)
        let home = nav.line_start(pos0);
        assert_eq!(home.grapheme, 5);

        // End goes to visual right (logical 0)
        let end = nav.line_end(home);
        assert_eq!(end.grapheme, 0);

        // Logical movements are independent of bidi visual reordering:
        assert_eq!(nav.move_grapheme_forward(pos0).grapheme, 1);
        assert_eq!(nav.move_grapheme_backward(pos0).grapheme, 0);
        let pos5 = nav.from_line_grapheme(0, 5);
        assert_eq!(nav.move_grapheme_backward(pos5).grapheme, 4);
        assert_eq!(nav.move_grapheme_forward(pos5).grapheme, 5);

        assert_eq!(nav.logical_line_start(pos5).grapheme, 0);
        assert_eq!(nav.logical_line_end(pos0).grapheme, 5);
    }

    #[test]
    fn logical_cursor_navigation_multiline() {
        let r = rope("abc\ndef");
        let nav = CursorNavigator::new(&r);

        let pos_start = nav.from_line_grapheme(0, 0);
        assert_eq!(nav.logical_line_start(pos_start).grapheme, 0);
        assert_eq!(nav.logical_line_end(pos_start).grapheme, 3);

        // Moving forward past end of line wraps to next line
        let pos_eol = nav.from_line_grapheme(0, 3);
        let next_line = nav.move_grapheme_forward(pos_eol);
        assert_eq!(next_line.line, 1);
        assert_eq!(next_line.grapheme, 0);

        // Moving backward from start of line 1 wraps to end of line 0
        let prev_line = nav.move_grapheme_backward(next_line);
        assert_eq!(prev_line.line, 0);
        assert_eq!(prev_line.grapheme, 3);
    }

    #[test]
    fn strip_trailing_newline_covers_every_terminator_ropey_splits_on() {
        // ropey splits lines on all of these, so `Rope::line` can hand back a
        // line ending with any one of them. Leaving them in the body made them
        // count as graphemes, which is what broke backspace on U+2028/U+2029.
        assert_eq!(strip_trailing_newline("ab\n"), "ab");
        assert_eq!(strip_trailing_newline("ab\r\n"), "ab");
        assert_eq!(strip_trailing_newline("ab\r"), "ab");
        assert_eq!(strip_trailing_newline("ab\u{000B}"), "ab");
        assert_eq!(strip_trailing_newline("ab\u{000C}"), "ab");
        assert_eq!(strip_trailing_newline("ab\u{0085}"), "ab");
        assert_eq!(strip_trailing_newline("ab\u{2028}"), "ab");
        assert_eq!(strip_trailing_newline("ab\u{2029}"), "ab");
        // Only a trailing terminator goes; interior ones and plain text stay.
        assert_eq!(strip_trailing_newline("ab"), "ab");
        assert_eq!(strip_trailing_newline("a\u{2028}b"), "a\u{2028}b");
        assert_eq!(strip_trailing_newline(""), "");
    }

    #[test]
    fn a_separator_only_line_has_no_content_to_navigate() {
        // The rope reports two lines for a lone LINE SEPARATOR. Line 0 is the
        // separator itself, and its *content* is empty - so stepping back from
        // the start of line 1 must land at the start of line 0, not after it.
        for sep in ['\u{2028}', '\u{2029}'] {
            let r = rope(&sep.to_string());
            let nav = CursorNavigator::new(&r);
            let start_of_second = nav.from_line_grapheme(1, 0);
            let back = nav.move_grapheme_backward(start_of_second);
            assert_eq!(back.line, 0, "separator U+{:04X}", sep as u32);
            assert_eq!(back.grapheme, 0, "separator U+{:04X}", sep as u32);
            // Distinct byte offsets, so a delete between them is non-empty.
            assert_ne!(
                nav.to_byte_index(back),
                nav.to_byte_index(start_of_second),
                "separator U+{:04X} collapsed to an empty range",
                sep as u32
            );
        }
    }
}

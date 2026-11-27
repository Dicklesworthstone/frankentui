#![forbid(unsafe_code)]

//! Cursor utilities for text editing widgets.
//!
//! Provides grapheme-aware cursor movement and mapping between logical
//! positions (line + grapheme) and visual columns (cell width).

use crate::rope::Rope;
use crate::wrap::{display_width, graphemes};
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

/// Which side of a logical grapheme boundary the caret is visually attached to.
///
/// At a bidirectional run boundary, one logical grapheme boundary corresponds
/// to two distinct display positions:
/// - [`CaretAffinity::Upstream`] attaches to the trailing edge of the
///   logically preceding grapheme (or the paragraph start at boundary `0`).
/// - [`CaretAffinity::Downstream`] attaches to the leading edge of the
///   logically following grapheme (or the paragraph end at boundary `len`).
///
/// On single-direction lines both affinities resolve to the same display column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaretAffinity {
    /// Attached to the logically following grapheme (leading edge), or the
    /// paragraph end at the end of a line.
    #[default]
    Downstream,
    /// Attached to the logically preceding grapheme (trailing edge), or the
    /// paragraph start at boundary `0`.
    Upstream,
}

/// Logical + visual cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorPosition {
    /// Line index (0-based).
    pub line: usize,
    /// Grapheme index within the line (0-based).
    pub grapheme: usize,
    /// Visual column in cells (0-based).
    pub visual_col: usize,
    /// Caret affinity at bidirectional direction changes.
    pub affinity: CaretAffinity,
}

impl CursorPosition {
    /// Create a cursor position with explicit fields and default
    /// ([`CaretAffinity::Downstream`]) affinity.
    #[must_use]
    pub const fn new(line: usize, grapheme: usize, visual_col: usize) -> Self {
        Self {
            line,
            grapheme,
            visual_col,
            affinity: CaretAffinity::Downstream,
        }
    }

    /// Return this cursor position with an explicit [`CaretAffinity`].
    #[must_use]
    pub const fn with_affinity(mut self, affinity: CaretAffinity) -> Self {
        self.affinity = affinity;
        self
    }
}

/// Cursor navigation helper for rope-backed text.
///
/// By default every operation works in *logical* order: left/right step one
/// grapheme backward/forward, Home/End go to grapheme 0 / the line end, and
/// visual columns are the summed cell widths of the logical graphemes. That is
/// what the built-in editors (`Editor`, `TextArea`) draw, so cursor, selection
/// and mouse hit testing stay consistent with the rendered text.
///
/// With the `bidi` feature, [`with_visual_bidi`](Self::with_visual_bidi) opts
/// in to *visual* bidi cursor behavior on lines containing RTL text. Only use
/// it when the line is also *rendered* in UAX#9 visual order (for example via
/// [`crate::bidi::reorder`]); see that method for the exact semantics.
#[derive(Debug, Clone, Copy)]
pub struct CursorNavigator<'a> {
    rope: &'a Rope,
    #[cfg(feature = "bidi")]
    visual_bidi: bool,
}

impl<'a> CursorNavigator<'a> {
    /// Create a new navigator for the given rope (logical cursor movement).
    #[must_use]
    pub const fn new(rope: &'a Rope) -> Self {
        Self {
            rope,
            #[cfg(feature = "bidi")]
            visual_bidi: false,
        }
    }

    /// Opt in (or out) of visual bidi cursor behavior. Off by default.
    ///
    /// When enabled, on lines that contain RTL text:
    /// - `visual_col` is the cell column of the caret in UAX#9 visual order
    ///   (via [`VisualLineLayout`], with every extended grapheme cluster kept
    ///   whole and measured with its full display width);
    /// - [`CaretAffinity`] tracks which side of a bidirectional run boundary
    ///   the caret is visually attached to (`Upstream` = trailing edge of the
    ///   preceding grapheme, `Downstream` = leading edge of the following
    ///   grapheme);
    /// - [`move_left`](Self::move_left) / [`move_right`](Self::move_right)
    ///   step one visual boundary slot strictly left / right on screen,
    ///   keeping distinct stops around zero-width marks (such as RLM/LRM),
    ///   reaching the logical end of trailing RTL runs, and round-tripping
    ///   monotonically without loops;
    /// - [`line_start`](Self::line_start) / [`line_end`](Self::line_end) go to
    ///   the leftmost / rightmost display edge;
    /// - [`from_visual_col`](Self::from_visual_col) hit-tests against the
    ///   visual layout, snapping clicks inside wide clusters to their left
    ///   edge.
    ///
    /// Pair this with visual-order rendering (such as `TextArea::with_visual_bidi(true)`
    /// or `TextInput::with_visual_bidi(true)` in `ftui-widgets`).
    #[cfg(feature = "bidi")]
    #[must_use]
    pub const fn with_visual_bidi(mut self, enabled: bool) -> Self {
        self.visual_bidi = enabled;
        self
    }

    /// Whether visual bidi cursor behavior is enabled.
    #[cfg(feature = "bidi")]
    #[must_use]
    pub const fn visual_bidi(&self) -> bool {
        self.visual_bidi
    }

    /// Visual layout for `text` when visual bidi behavior applies to it.
    #[cfg(feature = "bidi")]
    fn visual_layout<'t>(&self, text: &'t str) -> Option<VisualLineLayout<'t>> {
        if self.visual_bidi {
            VisualLineLayout::new(text)
        } else {
            None
        }
    }

    /// Clamp an arbitrary position to valid ranges, preserving [`CaretAffinity`]
    /// on visual-bidi RTL lines.
    #[must_use]
    pub fn clamp(&self, pos: CursorPosition) -> CursorPosition {
        let line = clamp_line_index(self.rope, pos.line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        let grapheme = pos.grapheme.min(grapheme_count(line_text));
        #[cfg(feature = "bidi")]
        if let Some(layout) = self.visual_layout(line_text) {
            let visual_col = layout.visual_col(grapheme, pos.affinity);
            return CursorPosition::new(line, grapheme, visual_col).with_affinity(pos.affinity);
        }
        let visual_col = visual_col_for_grapheme(line_text, grapheme);
        CursorPosition::new(line, grapheme, visual_col)
    }

    /// Build a position from line + grapheme index using canonical affinity.
    #[must_use]
    pub fn from_line_grapheme(&self, line: usize, grapheme: usize) -> CursorPosition {
        let line = clamp_line_index(self.rope, line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        let grapheme = grapheme.min(grapheme_count(line_text));
        #[cfg(feature = "bidi")]
        if let Some(layout) = self.visual_layout(line_text) {
            let affinity = layout.canonical_affinity(grapheme);
            let visual_col = layout.visual_col(grapheme, affinity);
            return CursorPosition::new(line, grapheme, visual_col).with_affinity(affinity);
        }
        let visual_col = visual_col_for_grapheme(line_text, grapheme);
        CursorPosition::new(line, grapheme, visual_col)
    }

    /// Build a position from line + grapheme index with an explicit [`CaretAffinity`].
    ///
    /// On visual-bidi lines containing RTL text, `affinity` selects which side
    /// of a direction change the caret attaches to. In logical mode (or on
    /// pure-LTR lines), both affinities map to the same display column and the
    /// returned position uses [`CaretAffinity::Downstream`].
    #[must_use]
    pub fn from_line_grapheme_with_affinity(
        &self,
        line: usize,
        grapheme: usize,
        affinity: CaretAffinity,
    ) -> CursorPosition {
        let line = clamp_line_index(self.rope, line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        let grapheme = grapheme.min(grapheme_count(line_text));
        #[cfg(feature = "bidi")]
        if let Some(layout) = self.visual_layout(line_text) {
            let visual_col = layout.visual_col(grapheme, affinity);
            return CursorPosition::new(line, grapheme, visual_col).with_affinity(affinity);
        }
        #[cfg(not(feature = "bidi"))]
        let _ = affinity;
        let visual_col = visual_col_for_grapheme(line_text, grapheme);
        CursorPosition::new(line, grapheme, visual_col)
    }

    /// Build a position from line + visual column.
    #[must_use]
    pub fn from_visual_col(&self, line: usize, visual_col: usize) -> CursorPosition {
        let line = clamp_line_index(self.rope, line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        #[cfg(feature = "bidi")]
        if let Some(layout) = self.visual_layout(line_text) {
            let (grapheme, col, affinity) = layout.hit_test(visual_col);
            return CursorPosition::new(line, grapheme, col).with_affinity(affinity);
        }
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

    /// Convert a byte index into a cursor position using canonical affinity.
    #[must_use]
    pub fn from_byte_index(&self, byte_idx: usize) -> CursorPosition {
        let (line, col_chars) = self.rope.byte_to_line_col(byte_idx);
        let line = clamp_line_index(self.rope, line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        let grapheme = grapheme_index_from_char_offset(line_text, col_chars);
        self.from_line_grapheme(line, grapheme)
    }

    /// Convert a byte index into a cursor position with an explicit [`CaretAffinity`].
    #[must_use]
    pub fn from_byte_index_with_affinity(
        &self,
        byte_idx: usize,
        affinity: CaretAffinity,
    ) -> CursorPosition {
        let (line, col_chars) = self.rope.byte_to_line_col(byte_idx);
        let line = clamp_line_index(self.rope, line);
        let line_text = line_text(self.rope, line);
        let line_text = strip_trailing_newline(&line_text);
        let grapheme = grapheme_index_from_char_offset(line_text, col_chars);
        self.from_line_grapheme_with_affinity(line, grapheme, affinity)
    }

    /// Move cursor backward by one grapheme in logical document order (across line boundaries).
    ///
    /// On visual-bidi lines, stepping logically backward sets
    /// [`CaretAffinity::Downstream`] so the caret remembers that it arrived at
    /// the leading edge of the grapheme it stepped over.
    #[must_use]
    pub fn move_grapheme_backward(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        if pos.grapheme > 0 {
            return self.from_line_grapheme_with_affinity(
                pos.line,
                pos.grapheme - 1,
                CaretAffinity::Downstream,
            );
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
    ///
    /// On visual-bidi lines, stepping logically forward sets
    /// [`CaretAffinity::Upstream`] so the caret remembers that it arrived at
    /// the trailing edge of the grapheme it stepped over.
    #[must_use]
    pub fn move_grapheme_forward(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        let raw = line_text(self.rope, pos.line);
        let current_text = strip_trailing_newline(&raw);
        let line_end = grapheme_count(current_text);
        if pos.grapheme < line_end {
            return self.from_line_grapheme_with_affinity(
                pos.line,
                pos.grapheme + 1,
                CaretAffinity::Upstream,
            );
        }
        let last_line = last_line_index(self.rope);
        if pos.line >= last_line {
            return pos;
        }
        self.from_line_grapheme(pos.line + 1, 0)
    }

    /// Move cursor left by one grapheme (across line boundaries).
    ///
    /// Moves backward in logical document order, unless visual bidi behavior
    /// was enabled with [`with_visual_bidi`](Self::with_visual_bidi) and the
    /// line contains RTL text: then it moves one visual boundary slot left on
    /// screen from `(pos.grapheme, pos.affinity)`, wrapping to the visual end
    /// of the previous line.
    #[must_use]
    pub fn move_left(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        #[cfg(feature = "bidi")]
        if self.visual_bidi {
            let raw = line_text(self.rope, pos.line);
            let current_text = strip_trailing_newline(&raw);
            if let Some(layout) = VisualLineLayout::new(current_text) {
                if let Some((g, col, affinity)) = layout.step_left(pos.grapheme, pos.affinity) {
                    return CursorPosition::new(pos.line, g, col).with_affinity(affinity);
                }
            } else if let Some(prev) = pos.grapheme.checked_sub(1) {
                return self.from_line_grapheme(pos.line, prev);
            }
            if pos.line == 0 {
                return pos;
            }
            // Wrap to the visual end of the previous line.
            return self.line_end(self.from_line_grapheme(pos.line - 1, 0));
        }
        self.move_grapheme_backward(pos)
    }

    /// Move cursor right by one grapheme (across line boundaries).
    ///
    /// Moves forward in logical document order, unless visual bidi behavior
    /// was enabled with [`with_visual_bidi`](Self::with_visual_bidi) and the
    /// line contains RTL text: then it moves one visual boundary slot right on
    /// screen from `(pos.grapheme, pos.affinity)`, wrapping to the visual start
    /// of the next line.
    #[must_use]
    pub fn move_right(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        #[cfg(feature = "bidi")]
        if self.visual_bidi {
            let raw = line_text(self.rope, pos.line);
            let current_text = strip_trailing_newline(&raw);
            if let Some(layout) = VisualLineLayout::new(current_text) {
                if let Some((g, col, affinity)) = layout.step_right(pos.grapheme, pos.affinity) {
                    return CursorPosition::new(pos.line, g, col).with_affinity(affinity);
                }
            } else if pos.grapheme < grapheme_count(current_text) {
                return self.from_line_grapheme(pos.line, pos.grapheme + 1);
            }
            if pos.line >= last_line_index(self.rope) {
                return pos;
            }
            // Wrap to the visual start of the next line.
            return self.line_start(self.from_line_grapheme(pos.line + 1, 0));
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
    /// Goes to the logical start of the line (grapheme 0), unless visual bidi
    /// behavior was enabled with [`with_visual_bidi`](Self::with_visual_bidi)
    /// and the line contains RTL text: then it goes to the leftmost display stop.
    #[must_use]
    pub fn line_start(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        #[cfg(feature = "bidi")]
        {
            let line_text = line_text(self.rope, pos.line);
            let line_text = strip_trailing_newline(&line_text);
            if let Some(layout) = self.visual_layout(line_text) {
                let (grapheme, col, affinity) = layout.leftmost();
                return CursorPosition::new(pos.line, grapheme, col).with_affinity(affinity);
            }
        }
        self.logical_line_start(pos)
    }

    /// Move cursor to end of line.
    ///
    /// Goes to the logical end of the line (after the last grapheme), unless
    /// visual bidi behavior was enabled with
    /// [`with_visual_bidi`](Self::with_visual_bidi) and the line contains RTL
    /// text: then it goes to the rightmost display stop.
    #[must_use]
    pub fn line_end(&self, pos: CursorPosition) -> CursorPosition {
        let pos = self.clamp(pos);
        #[cfg(feature = "bidi")]
        {
            let line_text = line_text(self.rope, pos.line);
            let line_text = strip_trailing_newline(&line_text);
            if let Some(layout) = self.visual_layout(line_text) {
                let (grapheme, col, affinity) = layout.rightmost();
                return CursorPosition::new(pos.line, grapheme, col).with_affinity(affinity);
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

/// A single extended grapheme cluster laid out in UAX#9 visual order.
#[cfg(feature = "bidi")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualGrapheme<'a> {
    /// Full extended grapheme cluster text from the laid-out line or slice.
    pub text: &'a str,
    /// 0-based logical grapheme index within the laid-out line or slice.
    pub logical_index: usize,
    /// Start byte offset within the laid-out slice (inclusive).
    pub byte_start: usize,
    /// End byte offset within the laid-out slice (exclusive).
    pub byte_end: usize,
    /// Leftmost display column (0-based cells).
    pub visual_col: usize,
    /// Display width in terminal cells.
    pub width: usize,
    /// Resolved UAX#9 embedding level (odd = RTL, even = LTR).
    pub level: u8,
}

#[cfg(feature = "bidi")]
impl VisualGrapheme<'_> {
    /// Whether this grapheme cluster belongs to an RTL run.
    #[inline]
    #[must_use]
    pub const fn is_rtl(&self) -> bool {
        self.level % 2 == 1
    }
}

/// Visual (UAX#9) grapheme and caret layout of one line or wrapped slice.
///
/// Resolves paragraph direction and per-cluster embedding levels using
/// [`unicode_bidi::BidiInfo`] (including rules L1/L2) while keeping every
/// extended grapheme cluster intact and measuring its whole display width
/// (combining marks, pointed Hebrew/Arabic, ZWJ families, flags, keycaps).
///
/// Exposes both the visual grapheme stream (with original byte offsets for
/// rendering, syntax highlighting, and selection mapping) and a bijective
/// visual-slot caret map with [`CaretAffinity`] support:
/// - Distinct logical boundaries around zero-width marks (e.g. RLM/LRM) retain
///   distinct visual slots and never collapse or loop.
/// - Direction-change boundaries expose both their `Upstream` (trailing edge
///   of `g - 1`) and `Downstream` (leading edge of `g`) visual slots.
/// - The logical end of a line ending in an RTL run inside an LTR paragraph is
///   cleanly reachable both at its `Downstream` right display edge and at its
///   `Upstream` trailing-RTL edge.
#[cfg(feature = "bidi")]
#[derive(Debug, Clone)]
pub struct VisualLineLayout<'a> {
    visual_graphemes: Vec<VisualGrapheme<'a>>,
    /// Display column of each visual boundary slot `0..=grapheme_count`.
    slot_cols: Vec<usize>,
    /// Visual slot for `(logical_grapheme, CaretAffinity::Upstream)`.
    upstream_slots: Vec<usize>,
    /// Visual slot for `(logical_grapheme, CaretAffinity::Downstream)`.
    downstream_slots: Vec<usize>,
    /// Canonical (lower-embedding-level) affinity for each logical boundary.
    canonical_affinities: Vec<CaretAffinity>,
    /// Canonical visual slot for each logical boundary (bijection `0..=N -> 0..=N`).
    canonical_slots: Vec<usize>,
    /// Inverse of `canonical_slots`: canonical logical boundary at each visual slot.
    slot_to_canonical_grapheme: Vec<usize>,
    /// Whether the enclosing paragraph base direction is RTL.
    base_is_rtl: bool,
}

#[cfg(feature = "bidi")]
impl<'a> VisualLineLayout<'a> {
    /// Build the layout for `text`, or `None` when `text` contains no RTL
    /// characters (pure-LTR lines can use the fast logical path directly).
    #[must_use]
    pub fn new(text: &'a str) -> Option<Self> {
        if !crate::bidi::has_rtl(text) {
            return None;
        }
        Some(Self::for_line(text))
    }

    /// Build the visual layout for a single logical line (including pure-LTR
    /// or empty lines).
    #[must_use]
    pub fn for_line(text: &'a str) -> Self {
        Self::for_wrapped_slice(text, 0..text.len())
    }

    /// Build the visual layout for a wrapped slice `slice_byte_range` of
    /// `full_line`, preserving the enclosing logical line's UAX#9 paragraph
    /// direction and resolved levels while applying rules L1/L2 to the slice.
    #[must_use]
    pub fn for_wrapped_slice(full_line: &'a str, slice_byte_range: std::ops::Range<usize>) -> Self {
        let start = slice_byte_range.start.min(full_line.len());
        let end = slice_byte_range.end.clamp(start, full_line.len());
        let slice = &full_line[start..end];

        if slice.is_empty() {
            return Self {
                visual_graphemes: Vec::new(),
                slot_cols: vec![0],
                upstream_slots: vec![0],
                downstream_slots: vec![0],
                canonical_affinities: vec![CaretAffinity::Downstream],
                canonical_slots: vec![0],
                slot_to_canonical_grapheme: vec![0],
                base_is_rtl: false,
            };
        }

        if !crate::bidi::has_rtl(full_line) {
            let mut visual_graphemes = Vec::new();
            let mut slot_cols = vec![0usize];
            let mut col = 0usize;
            for (logical_index, (byte_start, g)) in slice.grapheme_indices(true).enumerate() {
                let width = display_width(g);
                visual_graphemes.push(VisualGrapheme {
                    text: g,
                    logical_index,
                    byte_start,
                    byte_end: byte_start + g.len(),
                    visual_col: col,
                    width,
                    level: 0,
                });
                col += width;
                slot_cols.push(col);
            }
            let n = visual_graphemes.len();
            return Self {
                visual_graphemes,
                slot_cols,
                upstream_slots: (0..=n).collect(),
                downstream_slots: (0..=n).collect(),
                canonical_affinities: vec![CaretAffinity::Downstream; n + 1],
                canonical_slots: (0..=n).collect(),
                slot_to_canonical_grapheme: (0..=n).collect(),
                base_is_rtl: false,
            };
        }

        let bidi_info = unicode_bidi::BidiInfo::new(full_line, None);
        let para = bidi_info
            .paragraphs
            .iter()
            .find(|p| start >= p.range.start && start < p.range.end)
            .or_else(|| bidi_info.paragraphs.first());

        let (para_level, resolved_levels) = if let Some(para) = para {
            let clamped_start = start.clamp(para.range.start, para.range.end);
            let clamped_end = end.clamp(clamped_start, para.range.end);
            let (levels, _runs) = bidi_info.visual_runs(para, clamped_start..clamped_end);
            (para.level.number(), levels)
        } else {
            (0u8, Vec::new())
        };
        let base_is_rtl = para_level % 2 == 1;

        let mut logical: Vec<(&'a str, usize, usize, usize, u8)> = Vec::new();
        for (rel_start, g) in slice.grapheme_indices(true) {
            let abs_start = start + rel_start;
            let lvl = resolved_levels
                .get(abs_start)
                .map_or(para_level, unicode_bidi::Level::number);
            logical.push((g, rel_start, rel_start + g.len(), display_width(g), lvl));
        }
        let n = logical.len();

        // UAX#9 Rule L2 applied directly at extended-grapheme granularity so
        // multi-scalar clusters (combining marks, pointed Hebrew/Arabic, flags,
        // keycaps, ZWJ sequences) are never split or reversed internally.
        let mut visual_to_logical: Vec<usize> = (0..n).collect();
        let max_level = logical.iter().map(|entry| entry.4).max().unwrap_or(0);
        let min_odd = logical
            .iter()
            .map(|entry| entry.4)
            .filter(|&l| l % 2 == 1)
            .min()
            .unwrap_or(max_level.saturating_add(1));

        for level in (min_odd..=max_level).rev() {
            let mut i = 0;
            while i < n {
                if logical[visual_to_logical[i]].4 >= level {
                    let run_start = i;
                    while i < n && logical[visual_to_logical[i]].4 >= level {
                        i += 1;
                    }
                    visual_to_logical[run_start..i].reverse();
                } else {
                    i += 1;
                }
            }
        }

        let mut logical_to_visual = vec![0usize; n];
        for (v, &g) in visual_to_logical.iter().enumerate() {
            logical_to_visual[g] = v;
        }

        let mut visual_graphemes = Vec::with_capacity(n);
        let mut slot_cols = Vec::with_capacity(n + 1);
        let mut col = 0usize;
        slot_cols.push(0);
        for &g_idx in &visual_to_logical {
            let (g_text, byte_start, byte_end, width, level) = logical[g_idx];
            visual_graphemes.push(VisualGrapheme {
                text: g_text,
                logical_index: g_idx,
                byte_start,
                byte_end,
                visual_col: col,
                width,
                level,
            });
            col += width;
            slot_cols.push(col);
        }

        let mut upstream_slots = Vec::with_capacity(n + 1);
        let mut downstream_slots = Vec::with_capacity(n + 1);
        let mut canonical_affinities = Vec::with_capacity(n + 1);
        let mut canonical_slots = Vec::with_capacity(n + 1);
        let mut slot_to_canonical_grapheme = vec![usize::MAX; n + 1];

        for g in 0..=n {
            let up_slot = if g == 0 {
                if base_is_rtl { n } else { 0 }
            } else {
                let prev = g - 1;
                let v = logical_to_visual[prev];
                if logical[prev].4 % 2 == 1 { v } else { v + 1 }
            };
            let down_slot = if g == n {
                if base_is_rtl { 0 } else { n }
            } else {
                let v = logical_to_visual[g];
                if logical[g].4 % 2 == 1 { v + 1 } else { v }
            };

            let level_left = if g > 0 { logical[g - 1].4 } else { para_level };
            let level_right = if g < n { logical[g].4 } else { para_level };

            let (canon_aff, canon_slot) = if level_left <= level_right {
                (CaretAffinity::Upstream, up_slot)
            } else {
                (CaretAffinity::Downstream, down_slot)
            };

            upstream_slots.push(up_slot);
            downstream_slots.push(down_slot);
            canonical_affinities.push(canon_aff);
            canonical_slots.push(canon_slot);
            if slot_to_canonical_grapheme[canon_slot] == usize::MAX {
                slot_to_canonical_grapheme[canon_slot] = g;
            }
        }

        // Belt-and-suspenders guard: for any valid L2 level ordering every slot
        // 0..=n is filled bijectively above; fill any hypothetical gap from the
        // nearest adjacent visual grapheme edge so indexing can never panic.
        for s in 0..=n {
            if slot_to_canonical_grapheme[s] == usize::MAX {
                let fallback = if s < n {
                    let vg = visual_graphemes[s];
                    if vg.is_rtl() {
                        vg.logical_index + 1
                    } else {
                        vg.logical_index
                    }
                } else {
                    let vg = visual_graphemes[n - 1];
                    if vg.is_rtl() {
                        vg.logical_index
                    } else {
                        vg.logical_index + 1
                    }
                };
                slot_to_canonical_grapheme[s] = fallback;
            }
        }

        Self {
            visual_graphemes,
            slot_cols,
            upstream_slots,
            downstream_slots,
            canonical_affinities,
            canonical_slots,
            slot_to_canonical_grapheme,
            base_is_rtl,
        }
    }

    /// Grapheme clusters in visual left-to-right display order.
    #[inline]
    #[must_use]
    pub fn visual_graphemes(&self) -> &[VisualGrapheme<'a>] {
        &self.visual_graphemes
    }

    /// Whether the enclosing paragraph's resolved base direction is RTL.
    #[inline]
    #[must_use]
    pub const fn base_is_rtl(&self) -> bool {
        self.base_is_rtl
    }

    /// Number of grapheme clusters in this layout.
    #[inline]
    #[must_use]
    pub fn grapheme_count(&self) -> usize {
        self.visual_graphemes.len()
    }

    /// Total display width in cells.
    #[inline]
    #[must_use]
    pub fn total_width(&self) -> usize {
        self.slot_cols.last().copied().unwrap_or(0)
    }

    /// Canonical (lower-embedding-level) affinity for logical boundary `grapheme`.
    #[inline]
    #[must_use]
    pub fn canonical_affinity(&self, grapheme: usize) -> CaretAffinity {
        let last = self.canonical_affinities.len() - 1;
        self.canonical_affinities[grapheme.min(last)]
    }

    /// Visual boundary slot (`0..=grapheme_count`) for `(grapheme, affinity)`.
    #[inline]
    #[must_use]
    pub fn slot_for(&self, grapheme: usize, affinity: CaretAffinity) -> usize {
        let last = self.upstream_slots.len() - 1;
        let idx = grapheme.min(last);
        match affinity {
            CaretAffinity::Upstream => self.upstream_slots[idx],
            CaretAffinity::Downstream => self.downstream_slots[idx],
        }
    }

    /// Display column (cells) for `(grapheme, affinity)`.
    #[inline]
    #[must_use]
    pub fn visual_col(&self, grapheme: usize, affinity: CaretAffinity) -> usize {
        let slot = self.slot_for(grapheme, affinity);
        self.slot_cols[slot]
    }

    /// Canonical `(logical_grapheme, visual_col, affinity)` stop at visual `slot`.
    #[inline]
    #[must_use]
    pub fn stop_at_slot(&self, slot: usize) -> (usize, usize, CaretAffinity) {
        let last = self.slot_cols.len() - 1;
        let slot = slot.min(last);
        let grapheme = self.slot_to_canonical_grapheme[slot];
        let affinity = if self.canonical_slots[grapheme] == slot {
            self.canonical_affinities[grapheme]
        } else if self.upstream_slots[grapheme] == slot {
            CaretAffinity::Upstream
        } else {
            CaretAffinity::Downstream
        };
        (grapheme, self.slot_cols[slot], affinity)
    }

    /// Hit-test a display column against this visual layout, returning
    /// `(logical_grapheme, snapped_visual_col, affinity)`.
    ///
    /// Clicks inside a multi-cell wide cluster snap cleanly to its left edge.
    #[must_use]
    pub fn hit_test(&self, visual_col: usize) -> (usize, usize, CaretAffinity) {
        let slot = self
            .slot_cols
            .partition_point(|&c| c <= visual_col)
            .saturating_sub(1);
        self.stop_at_slot(slot)
    }

    /// Step one visual boundary slot right from `(grapheme, affinity)`.
    #[must_use]
    pub fn step_right(
        &self,
        grapheme: usize,
        affinity: CaretAffinity,
    ) -> Option<(usize, usize, CaretAffinity)> {
        let cur = self.slot_for(grapheme, affinity);
        if cur + 1 < self.slot_cols.len() {
            Some(self.stop_at_slot(cur + 1))
        } else {
            None
        }
    }

    /// Step one visual boundary slot left from `(grapheme, affinity)`.
    #[must_use]
    pub fn step_left(
        &self,
        grapheme: usize,
        affinity: CaretAffinity,
    ) -> Option<(usize, usize, CaretAffinity)> {
        let cur = self.slot_for(grapheme, affinity);
        cur.checked_sub(1).map(|prev| self.stop_at_slot(prev))
    }

    /// Leftmost display stop `(logical_grapheme, visual_col, affinity)` (col 0).
    #[inline]
    #[must_use]
    pub fn leftmost(&self) -> (usize, usize, CaretAffinity) {
        self.stop_at_slot(0)
    }

    /// Rightmost display stop `(logical_grapheme, visual_col, affinity)` (full width).
    #[inline]
    #[must_use]
    pub fn rightmost(&self) -> (usize, usize, CaretAffinity) {
        self.stop_at_slot(self.slot_cols.len() - 1)
    }
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
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);

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

    // ---- #102 regression tests -------------------------------------------

    /// Grapheme boundaries of `text` paired with the logical (summed
    /// whole-cluster width) column of each boundary.
    fn logical_cols(text: &str) -> Vec<usize> {
        let mut cols = vec![0];
        let mut col = 0;
        for g in graphemes(text) {
            col += display_width(g);
            cols.push(col);
        }
        cols
    }

    const MIXED_FIXTURES: &[&str] = &[
        "a\u{0301} \u{05D0}\u{05D1}",
        "ab \u{05D0}\u{05D1}",
        "x\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{05D0}\u{05D1}",
        "\u{1F1FA}\u{1F1F8} 1\u{FE0F}\u{20E3} \u{05D0}\u{05D1} end",
        "\u{05E9}\u{05B8}\u{05C1}\u{05DC}\u{05D5}\u{05B9}\u{05DD}",
        "\u{0628}\u{0650}\u{0633}\u{0652}\u{0645}\u{0650} abc, (\u{05D0}!) e\u{0301}",
        "\u{200F}a\u{200E}\u{05D0}",
        "\u{4E16}\u{754C} \u{05D0}\u{05D1} \u{1F600}",
    ];

    /// The default navigator is logical (v0.8.0 behavior) even when the
    /// `bidi` feature is compiled in: it must agree with editors that draw
    /// text in logical order.
    #[test]
    fn default_navigator_is_logical_for_rtl_and_clusters() {
        for text in MIXED_FIXTURES {
            let r = rope(text);
            let nav = CursorNavigator::new(&r);
            let cols = logical_cols(text);
            let n = cols.len() - 1;
            for (g, &col) in cols.iter().enumerate() {
                let pos = nav.from_line_grapheme(0, g);
                assert_eq!(pos.visual_col, col, "fixture={text:?} g={g}");
                // Zero-width clusters share a column; hit testing lands on
                // the last boundary at that column.
                let last_at_col = cols.iter().rposition(|&c| c == col).unwrap();
                assert_eq!(
                    nav.from_visual_col(0, col).grapheme,
                    last_at_col,
                    "fixture={text:?} g={g}"
                );
            }
            let mut pos = nav.document_start();
            for g in 1..=n {
                pos = nav.move_right(pos);
                assert_eq!(pos.grapheme, g, "fixture={text:?}");
            }
            assert_eq!(nav.move_right(pos), pos);
            assert_eq!(nav.line_start(pos).grapheme, 0);
            assert_eq!(nav.line_end(nav.document_start()).grapheme, n);
            assert_eq!(nav.move_left(nav.document_start()), nav.document_start());
        }
    }

    /// Issue #102 (1): a grapheme index was passed to the scalar-indexed
    /// `BidiSegment` maps, so the caret after "a\u{301} " landed at column 1.
    #[cfg(feature = "bidi")]
    #[test]
    fn issue_102_combining_cluster_before_rtl_visual_col() {
        let text = "a\u{0301} \u{05D0}\u{05D1}";
        let r = Rope::from_text(text);
        for nav in [
            CursorNavigator::new(&r),
            CursorNavigator::new(&r).with_visual_bidi(true),
        ] {
            let pos = nav.from_line_grapheme(0, 2);
            assert_eq!(nav.to_byte_index(pos), "a\u{0301} ".len());
            assert_eq!(pos.visual_col, 2);
            assert_eq!(nav.from_visual_col(0, 2), pos);
        }
        // ASCII control.
        let r = Rope::from_text("a \u{05D0}\u{05D1}");
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);
        assert_eq!(nav.from_line_grapheme(0, 2).visual_col, 2);
    }

    /// Multi-scalar clusters keep their whole display width in the visual
    /// layout, and LTR prefixes before an RTL run lay out exactly like the
    /// logical sum.
    #[cfg(feature = "bidi")]
    #[test]
    fn visual_bidi_measures_whole_clusters() {
        // Emoji ZWJ family (width 2) before Hebrew.
        let text = "x\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{05D0}\u{05D1}";
        let r = rope(text);
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);
        let cols = logical_cols(text);
        for (g, &col) in cols.iter().enumerate().take(3 + 1) {
            assert_eq!(nav.from_line_grapheme(0, g).visual_col, col, "g={g}");
        }
        let total = *cols.last().unwrap();
        assert_eq!(nav.line_end(nav.document_start()).visual_col, total);

        // Flag + keycap prefix.
        let text = "\u{1F1FA}\u{1F1F8} 1\u{FE0F}\u{20E3} \u{05D0}\u{05D1} end";
        let r = rope(text);
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);
        let cols = logical_cols(text);
        for (g, &col) in cols.iter().enumerate().take(4 + 1) {
            assert_eq!(nav.from_line_grapheme(0, g).visual_col, col, "g={g}");
        }

        // Pointed Hebrew (pure RTL, 4 clusters over 7 scalars): logical
        // boundary g sits 4 - g cells from the left edge.
        let text = "\u{05E9}\u{05B8}\u{05C1}\u{05DC}\u{05D5}\u{05B9}\u{05DD}";
        let r = rope(text);
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);
        for g in 0..=4 {
            assert_eq!(nav.from_line_grapheme(0, g).visual_col, 4 - g, "g={g}");
            assert_eq!(nav.from_visual_col(0, 4 - g).grapheme, g, "g={g}");
        }
        assert_eq!(nav.line_start(nav.document_start()).grapheme, 4);
        assert_eq!(nav.line_end(nav.document_end()).grapheme, 0);
    }

    /// Issue #102 (2) & bd-b8isx: visual movement must be strictly monotonic
    /// in visual slot order, non-decreasing in display column, visit every
    /// logical grapheme boundary (including distinct stops around zero-width
    /// controls like RLM/LRM without collapsing or looping), reach the logical
    /// end of trailing RTL runs, and exact-round-trip in reverse at every
    /// junction.
    #[cfg(feature = "bidi")]
    #[test]
    fn visual_bidi_moves_are_monotonic_and_round_trip() {
        let r = rope("ab \u{05D0}\u{05D1}");
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);
        let at4 = nav.from_line_grapheme(0, 4);
        let right = nav.move_right(at4);
        assert!(
            right == at4 || right.visual_col > at4.visual_col,
            "{at4:?} -> {right:?}"
        );
        // Logical end of a line ending in an RTL run in an LTR paragraph is
        // cleanly reachable both at the visual right edge (Downstream) and
        // immediately left of the trailing RTL glyph (Upstream).
        assert_eq!(right.grapheme, 5);
        assert_eq!(right.visual_col, 5);
        assert_eq!(right.affinity, CaretAffinity::Downstream);

        for text in MIXED_FIXTURES {
            let r = rope(text);
            let nav = CursorNavigator::new(&r).with_visual_bidi(true);
            let n = grapheme_count(text);
            let mut pos = nav.line_start(nav.document_start());
            assert_eq!(pos.visual_col, 0, "fixture={text:?}");
            let mut stops = vec![pos];
            for _ in 0..=n + 1 {
                let next = nav.move_right(pos);
                if next == pos {
                    break;
                }
                assert!(
                    next.visual_col >= pos.visual_col && next.grapheme != pos.grapheme,
                    "fixture={text:?} {pos:?} -> {next:?}"
                );
                pos = next;
                stops.push(pos);
            }
            assert_eq!(
                stops.len(),
                n + 1,
                "fixture={text:?}: every logical boundary (including around zero-width marks) must be a distinct stop"
            );
            // Every logical boundary 0..=n must be visited exactly once.
            let mut visited: Vec<usize> = stops.iter().map(|s| s.grapheme).collect();
            visited.sort_unstable();
            assert_eq!(visited, (0..=n).collect::<Vec<_>>(), "fixture={text:?}");

            assert_eq!(pos, nav.line_end(pos), "fixture={text:?}");
            assert_eq!(
                pos.visual_col,
                logical_cols(text).last().copied().unwrap(),
                "fixture={text:?}: rightmost stop is the full line width"
            );
            for w in stops.windows(2).rev() {
                assert_eq!(nav.move_left(w[1]), w[0], "fixture={text:?}");
            }
            for stop in &stops {
                let hit = nav.from_visual_col(0, stop.visual_col);
                assert_eq!(hit.visual_col, stop.visual_col, "fixture={text:?}");
                let same_col_count = stops
                    .iter()
                    .filter(|s| s.visual_col == stop.visual_col)
                    .count();
                if same_col_count == 1 {
                    assert_eq!(hit, *stop, "fixture={text:?}");
                }
                assert_eq!(nav.from_byte_index(nav.to_byte_index(*stop)), *stop);
            }
        }
    }

    #[cfg(feature = "bidi")]
    #[test]
    fn visual_bidi_caret_affinity_remembers_visual_edge_at_junctions() {
        // LTR paragraph with embedded RTL run: "ab אב cd"
        // Visual cells: 0:a 1:b 2:' ' 3:ב 4:א 5:' ' 6:c 7:d
        let r = rope("ab \u{05D0}\u{05D1} cd");
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);

        // Logical boundary g=3 (between ' ' and 'א') sits at col 3 Upstream
        // and col 5 Downstream.
        let g3_up = nav.from_line_grapheme_with_affinity(0, 3, CaretAffinity::Upstream);
        let g3_down = nav.from_line_grapheme_with_affinity(0, 3, CaretAffinity::Downstream);
        assert_eq!(g3_up.visual_col, 3);
        assert_eq!(g3_down.visual_col, 5);
        // Stepping from g3_up moves relative to col 3.
        assert_eq!(nav.move_left(g3_up).visual_col, 2);
        assert_eq!(nav.move_right(g3_up).visual_col, 4);
        // Stepping from g3_down moves relative to col 5.
        assert_eq!(nav.move_left(g3_down).visual_col, 4);
        assert_eq!(nav.move_right(g3_down).visual_col, 6);

        // Logical boundary g=5 (between 'ב' and ' ') sits at col 3 Upstream
        // and col 5 Downstream.
        let g5_up = nav.from_line_grapheme_with_affinity(0, 5, CaretAffinity::Upstream);
        let g5_down = nav.from_line_grapheme_with_affinity(0, 5, CaretAffinity::Downstream);
        assert_eq!(g5_up.visual_col, 3);
        assert_eq!(g5_down.visual_col, 5);
        assert_eq!(nav.move_left(g5_up).visual_col, 2);
        assert_eq!(nav.move_right(g5_up).visual_col, 4);
        assert_eq!(nav.move_left(g5_down).visual_col, 4);
        assert_eq!(nav.move_right(g5_down).visual_col, 6);

        // Stepping logically forward into g=3 sets Upstream (col 3), while
        // stepping logically backward from g=4 into g=3 sets Downstream (col 5).
        let from_ltr = nav.move_grapheme_forward(nav.from_line_grapheme(0, 2));
        assert_eq!(from_ltr.grapheme, 3);
        assert_eq!(from_ltr.affinity, CaretAffinity::Upstream);
        assert_eq!(from_ltr.visual_col, 3);

        let from_rtl = nav.move_grapheme_backward(nav.from_line_grapheme(0, 4));
        assert_eq!(from_rtl.grapheme, 3);
        assert_eq!(from_rtl.affinity, CaretAffinity::Downstream);
        assert_eq!(from_rtl.visual_col, 5);

        // Trailing RTL run in LTR paragraph: "ab אב"
        // Logical end g=5 is reachable both Upstream (col 3, left of 'ב') and
        // Downstream (col 5, visual right display edge).
        let r2 = rope("ab \u{05D0}\u{05D1}");
        let nav2 = CursorNavigator::new(&r2).with_visual_bidi(true);
        let end_up = nav2.from_line_grapheme_with_affinity(0, 5, CaretAffinity::Upstream);
        let end_down = nav2.from_line_grapheme_with_affinity(0, 5, CaretAffinity::Downstream);
        assert_eq!(end_up.visual_col, 3);
        assert_eq!(end_down.visual_col, 5);
        assert_eq!(nav2.move_right(end_up).grapheme, 4);
        assert_eq!(nav2.move_left(end_up).grapheme, 2);
        assert_eq!(nav2.move_left(end_down).grapheme, 4);
        assert_eq!(nav2.move_right(end_down), end_down);
    }

    #[cfg(feature = "bidi")]
    #[test]
    fn visual_bidi_zero_width_marks_distinct_stops_no_loops() {
        // RLM + 'a' + LRM + Hebrew Alef (4 graphemes, 5 distinct boundaries, total width 2).
        let text = "\u{200F}a\u{200E}\u{05D0}";
        let r = rope(text);
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);
        let mut pos = nav.line_start(nav.document_start());
        let mut forward = vec![pos];
        for _ in 0..10 {
            let next = nav.move_right(pos);
            if next == pos {
                break;
            }
            forward.push(next);
            pos = next;
        }
        assert_eq!(
            forward.len(),
            5,
            "all 5 boundaries around RLM/LRM must be distinct"
        );
        let mut backward = vec![pos];
        for _ in 0..10 {
            let prev = nav.move_left(pos);
            if prev == pos {
                break;
            }
            backward.push(prev);
            pos = prev;
        }
        backward.reverse();
        assert_eq!(
            backward, forward,
            "reverse traversal must round-trip identically"
        );
    }

    #[cfg(feature = "bidi")]
    #[test]
    fn visual_bidi_wide_cell_hit_test_snaps_to_left_edge() {
        // Mixed CJK + Hebrew + Emoji:
        // Visual cells: 0..2:世, 2..4:界, 4..5:' ', 5..6:ב, 6..7:א, 7..8:' ', 8..10:😀
        let r = rope("\u{4E16}\u{754C} \u{05D0}\u{05D1} \u{1F600}");
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);
        assert_eq!(nav.from_visual_col(0, 0).visual_col, 0);
        assert_eq!(nav.from_visual_col(0, 1).visual_col, 0); // inside 世 -> snap left
        assert_eq!(nav.from_visual_col(0, 2).visual_col, 2);
        assert_eq!(nav.from_visual_col(0, 3).visual_col, 2); // inside 界 -> snap left
        assert_eq!(nav.from_visual_col(0, 8).visual_col, 8);
        assert_eq!(nav.from_visual_col(0, 9).visual_col, 8); // inside 😀 -> snap left
        assert_eq!(nav.from_visual_col(0, 10).visual_col, 10);

        // Wide emoji embedded inside RTL run: "א😀ב"
        // Visual cells: 0..1:ב, 1..3:😀, 3..4:א
        let r_rtl = rope("\u{05D0}\u{1F600}\u{05D1}");
        let nav_rtl = CursorNavigator::new(&r_rtl).with_visual_bidi(true);
        assert_eq!(nav_rtl.from_visual_col(0, 1), nav_rtl.from_visual_col(0, 2));
        assert_eq!(nav_rtl.from_visual_col(0, 2).visual_col, 1);
    }

    #[cfg(feature = "bidi")]
    #[test]
    fn visual_line_layout_wrapped_slice_preserves_paragraph_direction() {
        // LTR paragraph wrapping onto an RTL-only second row.
        let full = "hello \u{05D0}\u{05D1}\u{05D2}";
        let split = "hello ".len();
        let row0 = VisualLineLayout::for_wrapped_slice(full, 0..split);
        let row1 = VisualLineLayout::for_wrapped_slice(full, split..full.len());
        assert!(!row0.base_is_rtl());
        assert!(!row1.base_is_rtl());
        let row1_glyphs: Vec<&str> = row1.visual_graphemes().iter().map(|vg| vg.text).collect();
        assert_eq!(row1_glyphs, vec!["\u{05D2}", "\u{05D1}", "\u{05D0}"]);
        // Byte offsets remain relative to the slice so callers map back to original line bytes.
        assert_eq!(
            &full[split + row1.visual_graphemes()[0].byte_start
                ..split + row1.visual_graphemes()[0].byte_end],
            "\u{05D2}"
        );
    }

    #[cfg(feature = "bidi")]
    #[test]
    fn visual_bidi_wraps_across_lines() {
        let r = rope("\u{05D0}\u{05D1}\nab");
        let nav = CursorNavigator::new(&r).with_visual_bidi(true);
        // Visual right edge of the RTL line is logical 0.
        let right_edge = nav.from_line_grapheme(0, 0);
        let next = nav.move_right(right_edge);
        assert_eq!((next.line, next.grapheme), (1, 0));
        let back = nav.move_left(next);
        assert_eq!((back.line, back.grapheme), (0, 0));
        // From the LTR line's end, right goes nowhere; left walks back over
        // "ab" and then onto the RTL line's right edge.
        let end = nav.document_end();
        assert_eq!(nav.move_right(end), end);
        let mut pos = end;
        for g in [1, 0] {
            pos = nav.move_left(pos);
            assert_eq!((pos.line, pos.grapheme), (1, g));
        }
        pos = nav.move_left(pos);
        assert_eq!((pos.line, pos.grapheme, pos.visual_col), (0, 0, 2));
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

#![forbid(unsafe_code)]

//! Rope-backed text storage with line/column helpers.
//!
//! This is a thin wrapper around `ropey::Rope` with a stable API and
//! convenience methods for line/column and grapheme-aware operations.

use std::borrow::Cow;
use std::fmt;
use std::ops::{Bound, RangeBounds};
use std::str::FromStr;

use ropey::{Rope as InnerRope, RopeSlice};
use unicode_segmentation::UnicodeSegmentation;

/// Rope-backed text storage.
#[derive(Clone, Debug, Default)]
pub struct Rope {
    rope: InnerRope,
}

impl Rope {
    /// Create an empty rope.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rope: InnerRope::new(),
        }
    }

    /// Create a rope from a string slice.
    ///
    /// This is a convenience method. You can also use `.parse()` or `From<&str>`.
    #[must_use]
    pub fn from_text(s: &str) -> Self {
        Self {
            rope: InnerRope::from_str(s),
        }
    }

    /// Total length in bytes.
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Total length in Unicode scalar values.
    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Total number of lines (newline count + 1).
    #[must_use]
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Returns `true` if the rope is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    /// Get a line by index.
    #[must_use]
    pub fn line(&self, idx: usize) -> Option<Cow<'_, str>> {
        if idx < self.len_lines() {
            Some(cow_from_slice(self.rope.line(idx)))
        } else {
            None
        }
    }

    /// Iterate over all lines.
    pub fn lines(&self) -> impl Iterator<Item = Cow<'_, str>> + '_ {
        self.rope.lines().map(cow_from_slice)
    }

    /// Get a slice of the rope by character range.
    #[must_use]
    pub fn slice<R>(&self, range: R) -> Cow<'_, str>
    where
        R: RangeBounds<usize>,
    {
        self.rope
            .get_slice(range)
            .map(cow_from_slice)
            .unwrap_or_else(|| Cow::Borrowed(""))
    }

    /// Insert text at a character index.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        if text.len() >= 10_000 {
            tracing::debug!(len = text.len(), "rope insert large text");
        }
        let idx = char_idx.min(self.len_chars());
        self.rope.insert(idx, text);
    }

    /// Insert text at a grapheme index.
    pub fn insert_grapheme(&mut self, grapheme_idx: usize, text: &str) {
        let char_idx = self.grapheme_to_char_idx(grapheme_idx);
        self.insert(char_idx, text);
    }

    /// Remove a character range.
    pub fn remove<R>(&mut self, range: R)
    where
        R: RangeBounds<usize>,
    {
        let (start, end) = normalize_range(range, self.len_chars());
        if start < end {
            self.rope.remove(start..end);
        }
    }

    /// Remove a grapheme range.
    pub fn remove_grapheme_range<R>(&mut self, range: R)
    where
        R: RangeBounds<usize>,
    {
        let (start, end) = normalize_range(range, self.grapheme_count());
        if start < end {
            let char_start = self.grapheme_to_char_idx(start);
            let char_end = self.grapheme_to_char_idx(end);
            self.rope.remove(char_start..char_end);
        }
    }

    /// Replace the entire contents.
    pub fn replace(&mut self, text: &str) {
        if text.len() >= 10_000 {
            tracing::debug!(len = text.len(), "rope replace large text");
        }
        self.rope = InnerRope::from(text);
    }

    /// Append text to the end.
    pub fn append(&mut self, text: &str) {
        let len = self.len_chars();
        self.insert(len, text);
    }

    /// Clear all content.
    pub fn clear(&mut self) {
        self.rope = InnerRope::new();
    }

    /// Convert a character index to a byte index.
    #[must_use]
    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        self.rope.char_to_byte(char_idx.min(self.len_chars()))
    }

    /// Convert a byte index to a character index.
    #[must_use]
    pub fn byte_to_char(&self, byte_idx: usize) -> usize {
        self.rope.byte_to_char(byte_idx.min(self.len_bytes()))
    }

    /// Convert a character index to a line index.
    #[must_use]
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx.min(self.len_chars()))
    }

    /// Get the character index at the start of a line.
    #[must_use]
    pub fn line_to_char(&self, line_idx: usize) -> usize {
        if line_idx >= self.len_lines() {
            self.len_chars()
        } else {
            self.rope.line_to_char(line_idx)
        }
    }

    /// Convert a byte index to (line, column) in characters.
    #[must_use]
    pub fn byte_to_line_col(&self, byte_idx: usize) -> (usize, usize) {
        let char_idx = self.byte_to_char(byte_idx);
        let line = self.char_to_line(char_idx);
        let line_start = self.line_to_char(line);
        (line, char_idx.saturating_sub(line_start))
    }

    /// Convert (line, column) in characters to a byte index.
    #[must_use]
    pub fn line_col_to_byte(&self, line_idx: usize, col: usize) -> usize {
        let line_start = self.line_to_char(line_idx);
        let char_idx = (line_start + col).min(self.len_chars());
        self.char_to_byte(char_idx)
    }

    /// Iterate over all characters.
    pub fn chars(&self) -> impl Iterator<Item = char> + '_ {
        self.rope.chars()
    }

    /// Return all graphemes as owned strings.
    #[must_use]
    pub fn graphemes(&self) -> Vec<String> {
        self.to_string()
            .graphemes(true)
            .map(str::to_string)
            .collect()
    }

    /// Count grapheme clusters.
    #[must_use]
    pub fn grapheme_count(&self) -> usize {
        self.to_string().graphemes(true).count()
    }

    fn grapheme_to_char_idx(&self, grapheme_idx: usize) -> usize {
        let snapshot = self.to_string();
        let mut char_idx = 0usize;
        let mut g_idx = 0usize;
        for grapheme in snapshot.graphemes(true) {
            if g_idx == grapheme_idx {
                return char_idx;
            }
            char_idx = char_idx.saturating_add(grapheme.chars().count());
            g_idx = g_idx.saturating_add(1);
        }
        self.len_chars()
    }
}

impl fmt::Display for Rope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.rope.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}

impl FromStr for Rope {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_text(s))
    }
}

impl From<&str> for Rope {
    fn from(s: &str) -> Self {
        Self::from_text(s)
    }
}

impl From<String> for Rope {
    fn from(s: String) -> Self {
        Self::from_text(&s)
    }
}

fn cow_from_slice(slice: RopeSlice<'_>) -> Cow<'_, str> {
    match slice.as_str() {
        Some(s) => Cow::Borrowed(s),
        None => Cow::Owned(slice.to_string()),
    }
}

fn normalize_range<R>(range: R, max: usize) -> (usize, usize)
where
    R: RangeBounds<usize>,
{
    let start = match range.start_bound() {
        Bound::Included(&s) => s,
        Bound::Excluded(&s) => s.saturating_add(1),
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&e) => e.saturating_add(1),
        Bound::Excluded(&e) => e,
        Bound::Unbounded => max,
    };

    let start = start.min(max);
    let end = end.min(max);
    if end < start {
        (start, start)
    } else {
        (start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rope_basic_counts() {
        let rope = Rope::from("Hello, world!");
        assert_eq!(rope.len_chars(), 13);
        assert_eq!(rope.len_lines(), 1);
    }

    #[test]
    fn rope_multiline_lines() {
        let rope = Rope::from("Line 1\nLine 2\nLine 3");
        assert_eq!(rope.len_lines(), 3);
        assert_eq!(rope.line(0).unwrap(), "Line 1\n");
        assert_eq!(rope.line(2).unwrap(), "Line 3");
    }

    #[test]
    fn rope_insert_remove_replace() {
        let mut rope = Rope::from("Hello!");
        rope.insert(5, ", world");
        assert_eq!(rope.to_string(), "Hello, world!");

        rope.remove(5..12);
        assert_eq!(rope.to_string(), "Hello!");

        rope.replace("Replaced");
        assert_eq!(rope.to_string(), "Replaced");
    }

    #[test]
    fn rope_append_clear() {
        let mut rope = Rope::from("Hi");
        rope.append(" there");
        assert_eq!(rope.to_string(), "Hi there");
        rope.clear();
        assert!(rope.is_empty());
        assert_eq!(rope.len_lines(), 1);
    }

    #[test]
    fn rope_char_byte_conversions() {
        let s = "a\u{1F600}b";
        let rope = Rope::from(s);
        assert_eq!(rope.len_chars(), 3);
        assert_eq!(rope.char_to_byte(0), 0);
        assert_eq!(rope.char_to_byte(1), "a".len());
        assert_eq!(rope.byte_to_char(rope.len_bytes()), 3);
    }

    #[test]
    fn rope_line_col_conversions() {
        let rope = Rope::from("ab\ncde\n");
        let (line, col) = rope.byte_to_line_col(4);
        assert_eq!(line, 1);
        assert_eq!(col, 1);

        let byte = rope.line_col_to_byte(1, 2);
        assert_eq!(byte, 5);
    }

    #[test]
    fn rope_grapheme_ops() {
        let mut rope = Rope::from("e\u{301}");
        assert_eq!(rope.grapheme_count(), 1);
        rope.insert_grapheme(1, "!");
        assert_eq!(rope.to_string(), "e\u{301}!");

        let mut rope = Rope::from("a\u{1F600}b");
        rope.remove_grapheme_range(1..2);
        assert_eq!(rope.to_string(), "ab");
    }

    proptest! {
        #[test]
        fn insert_remove_roundtrip(s in any::<String>(), insert in any::<String>(), idx in 0usize..200) {
            let mut rope = Rope::from(s.as_str());
            let insert_len = insert.chars().count();
            let pos = idx.min(rope.len_chars());
            rope.insert(pos, &insert);
            rope.remove(pos..pos.saturating_add(insert_len));
            prop_assert_eq!(rope.to_string(), s);
        }

        #[test]
        fn line_count_matches_newlines(s in "[^\r\u{000B}\u{000C}\u{0085}\u{2028}\u{2029}]*") {
            // Exclude all line separators except \n (CR, VT, FF, NEL, LS, PS)
            // ropey treats these as line breaks but we only count \n
            let rope = Rope::from(s.as_str());
            let newlines = s.as_bytes().iter().filter(|&&b| b == b'\n').count();
            prop_assert_eq!(rope.len_lines(), newlines + 1);
        }
    }
}

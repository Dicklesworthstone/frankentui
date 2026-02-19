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
    #[inline]
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Total length in Unicode scalar values.
    #[inline]
    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Total number of lines (newline count + 1).
    #[inline]
    #[must_use]
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Returns `true` if the rope is empty.
    #[inline]
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
    #[inline]
    #[must_use]
    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        self.rope.char_to_byte(char_idx.min(self.len_chars()))
    }

    /// Convert a byte index to a character index.
    #[inline]
    #[must_use]
    pub fn byte_to_char(&self, byte_idx: usize) -> usize {
        self.rope.byte_to_char(byte_idx.min(self.len_bytes()))
    }

    /// Convert a character index to a line index.
    #[inline]
    #[must_use]
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx.min(self.len_chars()))
    }

    /// Get the character index at the start of a line.
    #[inline]
    #[must_use]
    pub fn line_to_char(&self, line_idx: usize) -> usize {
        if line_idx >= self.len_lines() {
            self.len_chars()
        } else {
            self.rope.line_to_char(line_idx)
        }
    }

    /// Convert a byte index to (line, column) in characters.
    #[inline]
    #[must_use]
    pub fn byte_to_line_col(&self, byte_idx: usize) -> (usize, usize) {
        let char_idx = self.byte_to_char(byte_idx);
        let line = self.char_to_line(char_idx);
        let line_start = self.line_to_char(line);
        (line, char_idx.saturating_sub(line_start))
    }

    /// Convert (line, column) in characters to a byte index.
    #[inline]
    #[must_use]
    pub fn line_col_to_byte(&self, line_idx: usize, col: usize) -> usize {
        let line_start = self.line_to_char(line_idx);
        let char_idx = line_start.saturating_add(col).min(self.len_chars());
        self.char_to_byte(char_idx)
    }

    /// Iterate over all characters.
    pub fn chars(&self) -> impl Iterator<Item = char> + '_ {
        self.rope.chars()
    }

    /// Return all graphemes as owned strings.
    #[must_use]
    pub fn graphemes(&self) -> Vec<String> {
        let mut result = Vec::with_capacity(self.len_chars());
        for line in self.lines() {
            result.extend(line.graphemes(true).map(String::from));
        }
        result
    }

    /// Count grapheme clusters.
    #[must_use]
    pub fn grapheme_count(&self) -> usize {
        self.lines()
            .map(|line| line.graphemes(true).count())
            .sum()
    }

    fn grapheme_to_char_idx(&self, grapheme_idx: usize) -> usize {
        let mut g_count = 0;
        let mut char_count = 0;

        for line_slice in self.rope.lines() {
            let line = cow_from_slice(line_slice);
            let line_g_count = line.graphemes(true).count();
            if g_count + line_g_count > grapheme_idx {
                let offset = grapheme_idx - g_count;
                for (current_g, g) in line.graphemes(true).enumerate() {
                    if current_g == offset {
                        return char_count;
                    }
                    char_count += g.chars().count();
                }
            }
            g_count += line_g_count;
            char_count += line_slice.len_chars();
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

    // ====== Empty rope tests ======

    #[test]
    fn empty_rope_properties() {
        let rope = Rope::new();
        assert!(rope.is_empty());
        assert_eq!(rope.len_bytes(), 0);
        assert_eq!(rope.len_chars(), 0);
        assert_eq!(rope.len_lines(), 1); // ropey: empty string = 1 line
        assert_eq!(rope.grapheme_count(), 0);
        assert_eq!(rope.to_string(), "");
    }

    #[test]
    fn empty_rope_line_access() {
        let rope = Rope::new();
        assert!(rope.line(0).is_some()); // empty string is line 0
        assert!(rope.line(1).is_none());
    }

    #[test]
    fn empty_rope_slice() {
        let rope = Rope::new();
        assert_eq!(rope.slice(0..0), "");
        assert_eq!(rope.slice(..), "");
    }

    #[test]
    fn empty_rope_conversions() {
        let rope = Rope::new();
        assert_eq!(rope.char_to_byte(0), 0);
        assert_eq!(rope.byte_to_char(0), 0);
        assert_eq!(rope.char_to_line(0), 0);
        assert_eq!(rope.line_to_char(0), 0);
    }

    // ====== From impls ======

    #[test]
    fn from_str_impl() {
        let rope: Rope = "hello".into();
        assert_eq!(rope.to_string(), "hello");
    }

    #[test]
    fn from_string_impl() {
        let rope: Rope = String::from("hello").into();
        assert_eq!(rope.to_string(), "hello");
    }

    #[test]
    fn from_str_parse() {
        let rope: Rope = "hello".parse().unwrap();
        assert_eq!(rope.to_string(), "hello");
    }

    #[test]
    fn display_impl() {
        let rope = Rope::from("hello world");
        assert_eq!(format!("{rope}"), "hello world");
    }

    // ====== Line access ======

    #[test]
    fn line_out_of_bounds() {
        let rope = Rope::from("a\nb");
        assert!(rope.line(0).is_some());
        assert!(rope.line(1).is_some());
        assert!(rope.line(2).is_none());
        assert!(rope.line(100).is_none());
    }

    #[test]
    fn trailing_newline_creates_empty_last_line() {
        let rope = Rope::from("a\n");
        assert_eq!(rope.len_lines(), 2);
        assert_eq!(rope.line(0).unwrap(), "a\n");
        assert_eq!(rope.line(1).unwrap(), "");
    }

    #[test]
    fn multiple_newlines() {
        let rope = Rope::from("\n\n\n");
        assert_eq!(rope.len_lines(), 4);
    }

    #[test]
    fn lines_iterator() {
        let rope = Rope::from("a\nb\nc");
        let lines: Vec<String> = rope.lines().map(|c| c.to_string()).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "a\n");
        assert_eq!(lines[1], "b\n");
        assert_eq!(lines[2], "c");
    }

    // ====== Slice ======

    #[test]
    fn slice_basic() {
        let rope = Rope::from("hello world");
        assert_eq!(rope.slice(0..5), "hello");
        assert_eq!(rope.slice(6..11), "world");
        assert_eq!(rope.slice(6..), "world");
        assert_eq!(rope.slice(..5), "hello");
    }

    #[test]
    fn slice_out_of_bounds_returns_empty() {
        let rope = Rope::from("hi");
        assert_eq!(rope.slice(100..200), "");
    }

    // ====== Insert edge cases ======

    #[test]
    fn insert_at_beginning() {
        let mut rope = Rope::from("world");
        rope.insert(0, "hello ");
        assert_eq!(rope.to_string(), "hello world");
    }

    #[test]
    fn insert_at_end() {
        let mut rope = Rope::from("hello");
        rope.insert(5, " world");
        assert_eq!(rope.to_string(), "hello world");
    }

    #[test]
    fn insert_beyond_length_clamps() {
        let mut rope = Rope::from("hi");
        rope.insert(100, "!");
        assert_eq!(rope.to_string(), "hi!");
    }

    #[test]
    fn insert_empty_string() {
        let mut rope = Rope::from("hello");
        rope.insert(2, "");
        assert_eq!(rope.to_string(), "hello");
    }

    // ====== Remove edge cases ======

    #[test]
    fn remove_empty_range() {
        let mut rope = Rope::from("hello");
        rope.remove(2..2);
        assert_eq!(rope.to_string(), "hello");
    }

    #[test]
    fn remove_entire_content() {
        let mut rope = Rope::from("hello");
        rope.remove(..);
        assert!(rope.is_empty());
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn remove_inverted_range_is_noop() {
        let mut rope = Rope::from("hello");
        rope.remove(3..1); // end < start
        assert_eq!(rope.to_string(), "hello");
    }

    // ====== Grapheme operations ======

    #[test]
    fn grapheme_insert_at_beginning() {
        let mut rope = Rope::from("bc");
        rope.insert_grapheme(0, "a");
        assert_eq!(rope.to_string(), "abc");
    }

    #[test]
    fn grapheme_insert_with_combining() {
        let mut rope = Rope::from("e\u{301}x"); // é x
        assert_eq!(rope.grapheme_count(), 2);
        rope.insert_grapheme(1, "y");
        assert_eq!(rope.to_string(), "e\u{301}yx");
    }

    #[test]
    fn grapheme_remove_range() {
        let mut rope = Rope::from("abcd");
        rope.remove_grapheme_range(1..3);
        assert_eq!(rope.to_string(), "ad");
    }

    #[test]
    fn grapheme_remove_empty_range() {
        let mut rope = Rope::from("abc");
        rope.remove_grapheme_range(1..1);
        assert_eq!(rope.to_string(), "abc");
    }

    #[test]
    fn graphemes_returns_correct_list() {
        let rope = Rope::from("ae\u{301}b"); // a é b
        let gs = rope.graphemes();
        assert_eq!(gs.len(), 3);
        assert_eq!(gs[0], "a");
        assert_eq!(gs[1], "e\u{301}");
        assert_eq!(gs[2], "b");
    }

    // ====== Char/byte/line conversions ======

    #[test]
    fn char_to_byte_with_multibyte() {
        let rope = Rope::from("a\u{1F600}b"); // a 😀 b
        assert_eq!(rope.char_to_byte(0), 0); // 'a'
        assert_eq!(rope.char_to_byte(1), 1); // start of emoji
        assert_eq!(rope.char_to_byte(2), 5); // 'b' (1 + 4 bytes for emoji)
    }

    #[test]
    fn byte_to_char_clamps() {
        let rope = Rope::from("hi");
        assert_eq!(rope.byte_to_char(100), 2);
    }

    #[test]
    fn char_to_byte_clamps() {
        let rope = Rope::from("hi");
        assert_eq!(rope.char_to_byte(100), 2);
    }

    #[test]
    fn line_to_char_out_of_bounds() {
        let rope = Rope::from("a\nb");
        assert_eq!(rope.line_to_char(0), 0);
        assert_eq!(rope.line_to_char(1), 2);
        assert_eq!(rope.line_to_char(100), 3); // len_chars
    }

    #[test]
    fn byte_to_line_col_basic() {
        let rope = Rope::from("abc\ndef");
        let (line, col) = rope.byte_to_line_col(5); // 'e' in "def"
        assert_eq!(line, 1);
        assert_eq!(col, 1);
    }

    #[test]
    fn line_col_to_byte_basic() {
        let rope = Rope::from("abc\ndef");
        let byte = rope.line_col_to_byte(1, 1);
        assert_eq!(byte, 5); // 'e'
    }

    // ====== Chars iterator ======

    #[test]
    fn chars_iterator() {
        let rope = Rope::from("ab");
        let chars: Vec<char> = rope.chars().collect();
        assert_eq!(chars, vec!['a', 'b']);
    }

    // ====== normalize_range helper ======

    #[test]
    fn normalize_range_basic() {
        assert_eq!(normalize_range(2..5, 10), (2, 5));
        assert_eq!(normalize_range(0..10, 10), (0, 10));
        assert_eq!(normalize_range(.., 10), (0, 10));
    }

    #[test]
    fn normalize_range_clamps_to_max() {
        assert_eq!(normalize_range(0..100, 5), (0, 5));
        assert_eq!(normalize_range(50..100, 5), (5, 5));
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn normalize_range_inverted_becomes_empty() {
        assert_eq!(normalize_range(5..2, 10), (5, 5));
    }

    #[test]
    fn normalize_range_inclusive() {
        assert_eq!(normalize_range(1..=3, 10), (1, 4));
    }

    // ====== Property tests ======

    proptest! {
        #[test]
        fn append_then_len_grows(s in "\\PC{0,50}", suffix in "\\PC{0,50}") {
            let mut rope = Rope::from(s.as_str());
            let before = rope.len_chars();
            let suffix_len = suffix.chars().count();
            rope.append(&suffix);
            prop_assert_eq!(rope.len_chars(), before + suffix_len);
        }

        #[test]
        fn replace_yields_new_content(s in "\\PC{0,50}", replacement in "\\PC{0,50}") {
            let mut rope = Rope::from(s.as_str());
            rope.replace(&replacement);
            prop_assert_eq!(rope.to_string(), replacement);
        }

        #[test]
        fn clear_always_empty(s in "\\PC{0,100}") {
            let mut rope = Rope::from(s.as_str());
            rope.clear();
            prop_assert!(rope.is_empty());
            prop_assert_eq!(rope.len_bytes(), 0);
            prop_assert_eq!(rope.len_chars(), 0);
        }

        #[test]
        fn display_matches_to_string(s in "\\PC{0,100}") {
            let rope = Rope::from(s.as_str());
            prop_assert_eq!(format!("{rope}"), rope.to_string());
        }

        #[test]
        fn char_byte_roundtrip(s in "\\PC{1,50}", idx in 0usize..50) {
            let rope = Rope::from(s.as_str());
            let char_idx = idx.min(rope.len_chars());
            let byte_idx = rope.char_to_byte(char_idx);
            let back = rope.byte_to_char(byte_idx);
            prop_assert_eq!(back, char_idx);
        }

        #[test]
        fn grapheme_count_leq_char_count(s in "\\PC{0,100}") {
            let rope = Rope::from(s.as_str());
            prop_assert!(rope.grapheme_count() <= rope.len_chars());
        }
    }
}

#![forbid(unsafe_code)]

//! Segment system for styled text units.
//!
//! A Segment is the atomic unit of styled text that can be:
//! - Cheaply borrowed (`Cow<str>`) for string literals / static content
//! - Split at **cell positions** (not byte positions) for correct wrapping
//!
//! Segments bridge higher-level text/layout systems to the render pipeline.
//!
//! # Example
//! ```
//! use ftui_text::Segment;
//! use ftui_style::Style;
//!
//! // Static text (zero-copy)
//! let seg = Segment::text("Hello, world!");
//! assert_eq!(seg.cell_length(), 13);
//!
//! // Styled text
//! let styled = Segment::styled("Error!", Style::new().bold());
//!
//! // Split at cell position
//! let (left, right) = seg.split_at_cell(5);
//! assert_eq!(left.as_str(), "Hello");
//! assert_eq!(right.as_str(), ", world!");
//! ```

use ftui_style::Style;
use smallvec::SmallVec;
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Control codes that can be carried by a segment.
///
/// Control segments do not consume display width and are used for
/// non-textual actions like cursor movement or clearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlCode {
    /// Carriage return (move to start of line)
    CarriageReturn,
    /// Line feed (move to next line)
    LineFeed,
    /// Bell (audible alert)
    Bell,
    /// Backspace
    Backspace,
    /// Tab
    Tab,
    /// Home (move to start of line, used in some contexts)
    Home,
    /// Clear to end of line
    ClearToEndOfLine,
    /// Clear line
    ClearLine,
}

impl ControlCode {
    /// Whether this control code should cause a line break.
    #[inline]
    #[must_use]
    pub const fn is_newline(&self) -> bool {
        matches!(self, Self::LineFeed)
    }

    /// Whether this control code is a carriage return.
    #[inline]
    #[must_use]
    pub const fn is_cr(&self) -> bool {
        matches!(self, Self::CarriageReturn)
    }
}

/// A segment of styled text.
///
/// Segments are the atomic units of text rendering. They can contain:
/// - Regular text with optional styling
/// - Control codes for non-textual actions
///
/// Text is stored as `Cow<str>` to allow zero-copy for static strings
/// while still supporting owned data when needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment<'a> {
    /// The text content (may be empty for control-only segments).
    pub text: Cow<'a, str>,
    /// Optional style applied to this segment.
    pub style: Option<Style>,
    /// Optional control codes (stack-allocated for common cases).
    pub control: Option<SmallVec<[ControlCode; 2]>>,
}

impl<'a> Segment<'a> {
    /// Create a new text segment without styling.
    #[inline]
    #[must_use]
    pub fn text(s: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: s.into(),
            style: None,
            control: None,
        }
    }

    /// Create a new styled text segment.
    #[inline]
    #[must_use]
    pub fn styled(s: impl Into<Cow<'a, str>>, style: Style) -> Self {
        Self {
            text: s.into(),
            style: Some(style),
            control: None,
        }
    }

    /// Create a control segment (no text, just control codes).
    #[inline]
    #[must_use]
    pub fn control(code: ControlCode) -> Self {
        let mut codes = SmallVec::new();
        codes.push(code);
        Self {
            text: Cow::Borrowed(""),
            style: None,
            control: Some(codes),
        }
    }

    /// Create a newline segment.
    #[inline]
    #[must_use]
    pub fn newline() -> Self {
        Self::control(ControlCode::LineFeed)
    }

    /// Create an empty segment.
    #[inline]
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            text: Cow::Borrowed(""),
            style: None,
            control: None,
        }
    }

    /// Get the text as a string slice.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Check if this segment is empty (no text and no control codes).
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.control.is_none()
    }

    /// Check if this segment has text content.
    #[inline]
    #[must_use]
    pub fn has_text(&self) -> bool {
        !self.text.is_empty()
    }

    /// Check if this is a control-only segment.
    #[inline]
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.control.is_some() && self.text.is_empty()
    }

    /// Check if this segment contains a newline control code.
    #[inline]
    #[must_use]
    pub fn is_newline(&self) -> bool {
        self.control
            .as_ref()
            .is_some_and(|codes| codes.iter().any(|c| c.is_newline()))
    }

    /// Get the display width in terminal cells.
    ///
    /// Control segments have zero width.
    /// Text width is calculated using Unicode width rules.
    #[inline]
    #[must_use]
    pub fn cell_length(&self) -> usize {
        if self.is_control() {
            return 0;
        }
        crate::display_width(&self.text)
    }

    /// Calculate cell length with a specific width function.
    ///
    /// This allows custom width calculations (e.g., for testing or
    /// terminal-specific behavior).
    #[inline]
    #[must_use]
    pub fn cell_length_with<F>(&self, width_fn: F) -> usize
    where
        F: Fn(&str) -> usize,
    {
        if self.is_control() {
            return 0;
        }
        width_fn(&self.text)
    }

    /// Split the segment at a cell position.
    ///
    /// Returns `(left, right)` where:
    /// - `left` contains content up to (but not exceeding) `cell_pos` cells
    /// - `right` contains the remaining content
    ///
    /// The split respects grapheme cluster boundaries to avoid breaking
    /// emoji, combining characters, or other complex graphemes.
    ///
    /// # Panics
    /// Does not panic; if `cell_pos` is beyond the segment length,
    /// returns `(self, empty)`.
    #[must_use]
    pub fn split_at_cell(&self, cell_pos: usize) -> (Self, Self) {
        // Control segments cannot be split
        if self.is_control() {
            if cell_pos == 0 {
                return (Self::empty(), self.clone());
            }
            return (self.clone(), Self::empty());
        }

        // Empty text
        if self.text.is_empty() || cell_pos == 0 {
            return (
                Self {
                    text: Cow::Borrowed(""),
                    style: self.style,
                    control: None,
                },
                self.clone(),
            );
        }

        let total_width = self.cell_length();
        if cell_pos >= total_width {
            return (
                self.clone(),
                Self {
                    text: Cow::Borrowed(""),
                    style: self.style,
                    control: None,
                },
            );
        }

        // Find the byte position that corresponds to the cell position
        let (byte_pos, _actual_width) = find_cell_boundary(&self.text, cell_pos);

        let left_text = &self.text[..byte_pos];
        let right_text = &self.text[byte_pos..];

        (
            Self {
                text: Cow::Owned(left_text.to_string()),
                style: self.style,
                control: None,
            },
            Self {
                text: Cow::Owned(right_text.to_string()),
                style: self.style,
                control: None,
            },
        )
    }

    /// Apply a style to this segment.
    #[inline]
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = Some(style);
        self
    }

    /// Convert to an owned segment (no lifetime constraints).
    #[must_use]
    pub fn into_owned(self) -> Segment<'static> {
        Segment {
            text: Cow::Owned(self.text.into_owned()),
            style: self.style,
            control: self.control,
        }
    }

    /// Add a control code to this segment.
    #[must_use]
    pub fn with_control(mut self, code: ControlCode) -> Self {
        if let Some(ref mut codes) = self.control {
            codes.push(code);
        } else {
            let mut codes = SmallVec::new();
            codes.push(code);
            self.control = Some(codes);
        }
        self
    }
}

impl<'a> Default for Segment<'a> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<'a> From<&'a str> for Segment<'a> {
    fn from(s: &'a str) -> Self {
        Self::text(s)
    }
}

impl From<String> for Segment<'static> {
    fn from(s: String) -> Self {
        Self::text(s)
    }
}

/// Find the byte position that corresponds to a cell position.
///
/// Returns `(byte_pos, actual_cell_width)` where `actual_cell_width`
/// is the width up to `byte_pos` (may be less than target if we can't
/// reach it exactly without breaking a grapheme).
fn find_cell_boundary(text: &str, target_cells: usize) -> (usize, usize) {
    let mut current_cells = 0;
    let mut byte_pos = 0;

    for grapheme in text.graphemes(true) {
        let grapheme_width = grapheme.width();

        // Check if adding this grapheme would exceed the target
        if current_cells + grapheme_width > target_cells {
            // Stop before this grapheme
            break;
        }

        current_cells += grapheme_width;
        byte_pos += grapheme.len();

        if current_cells >= target_cells {
            break;
        }
    }

    (byte_pos, current_cells)
}

/// A line of segments.
///
/// Represents a single line of text that may contain multiple styled segments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentLine<'a> {
    segments: Vec<Segment<'a>>,
}

impl<'a> SegmentLine<'a> {
    /// Create an empty line.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Create a line from a vector of segments.
    #[inline]
    #[must_use]
    pub fn from_segments(segments: Vec<Segment<'a>>) -> Self {
        Self { segments }
    }

    /// Create a line from a single segment.
    #[inline]
    #[must_use]
    pub fn from_segment(segment: Segment<'a>) -> Self {
        Self {
            segments: vec![segment],
        }
    }

    /// Check if the line is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() || self.segments.iter().all(|s| s.is_empty())
    }

    /// Get the number of segments.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Get the total cell width of the line.
    #[must_use]
    pub fn cell_length(&self) -> usize {
        self.segments.iter().map(|s| s.cell_length()).sum()
    }

    /// Add a segment to the end of the line.
    #[inline]
    pub fn push(&mut self, segment: Segment<'a>) {
        self.segments.push(segment);
    }

    /// Get the segments as a slice.
    #[inline]
    #[must_use]
    pub fn segments(&self) -> &[Segment<'a>] {
        &self.segments
    }

    /// Get mutable access to segments.
    #[inline]
    pub fn segments_mut(&mut self) -> &mut Vec<Segment<'a>> {
        &mut self.segments
    }

    /// Iterate over segments.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &Segment<'a>> {
        self.segments.iter()
    }

    /// Split the line at a cell position.
    ///
    /// Returns `(left, right)` where the split respects grapheme boundaries.
    #[must_use]
    pub fn split_at_cell(&self, cell_pos: usize) -> (Self, Self) {
        if cell_pos == 0 {
            return (Self::new(), self.clone());
        }

        let total_width = self.cell_length();
        if cell_pos >= total_width {
            return (self.clone(), Self::new());
        }

        let mut left_segments = Vec::new();
        let mut right_segments = Vec::new();
        let mut consumed = 0;
        let mut found_split = false;

        for segment in &self.segments {
            if found_split {
                right_segments.push(segment.clone());
                continue;
            }

            let seg_width = segment.cell_length();
            if consumed + seg_width <= cell_pos {
                // Entire segment goes to left
                left_segments.push(segment.clone());
                consumed += seg_width;
            } else if consumed >= cell_pos {
                // Entire segment goes to right
                right_segments.push(segment.clone());
                found_split = true;
            } else {
                // Need to split this segment
                let split_at = cell_pos - consumed;
                let (left, right) = segment.split_at_cell(split_at);
                if left.has_text() {
                    left_segments.push(left);
                }
                if right.has_text() {
                    right_segments.push(right);
                }
                found_split = true;
            }
        }

        (
            Self::from_segments(left_segments),
            Self::from_segments(right_segments),
        )
    }

    /// Concatenate plain text from all segments.
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        self.segments.iter().map(|s| s.as_str()).collect()
    }

    /// Convert all segments to owned (remove lifetime constraints).
    #[must_use]
    pub fn into_owned(self) -> SegmentLine<'static> {
        SegmentLine {
            segments: self.segments.into_iter().map(|s| s.into_owned()).collect(),
        }
    }
}

impl<'a> IntoIterator for SegmentLine<'a> {
    type Item = Segment<'a>;
    type IntoIter = std::vec::IntoIter<Segment<'a>>;

    fn into_iter(self) -> Self::IntoIter {
        self.segments.into_iter()
    }
}

impl<'a, 'b> IntoIterator for &'b SegmentLine<'a> {
    type Item = &'b Segment<'a>;
    type IntoIter = std::slice::Iter<'b, Segment<'a>>;

    fn into_iter(self) -> Self::IntoIter {
        self.segments.iter()
    }
}

/// Collection of lines (multi-line text).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentLines<'a> {
    lines: Vec<SegmentLine<'a>>,
}

impl<'a> SegmentLines<'a> {
    /// Create empty lines collection.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self { lines: Vec::new() }
    }

    /// Create from a vector of lines.
    #[inline]
    #[must_use]
    pub fn from_lines(lines: Vec<SegmentLine<'a>>) -> Self {
        Self { lines }
    }

    /// Check if empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Get number of lines.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Add a line.
    #[inline]
    pub fn push(&mut self, line: SegmentLine<'a>) {
        self.lines.push(line);
    }

    /// Get lines as slice.
    #[inline]
    #[must_use]
    pub fn lines(&self) -> &[SegmentLine<'a>] {
        &self.lines
    }

    /// Iterate over lines.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &SegmentLine<'a>> {
        self.lines.iter()
    }

    /// Get the maximum cell width across all lines.
    #[must_use]
    pub fn max_width(&self) -> usize {
        self.lines
            .iter()
            .map(|l| l.cell_length())
            .max()
            .unwrap_or(0)
    }

    /// Convert to owned.
    #[must_use]
    pub fn into_owned(self) -> SegmentLines<'static> {
        SegmentLines {
            lines: self.lines.into_iter().map(|l| l.into_owned()).collect(),
        }
    }
}

impl<'a> IntoIterator for SegmentLines<'a> {
    type Item = SegmentLine<'a>;
    type IntoIter = std::vec::IntoIter<SegmentLine<'a>>;

    fn into_iter(self) -> Self::IntoIter {
        self.lines.into_iter()
    }
}

/// Split segments by newlines into lines.
///
/// This function processes a sequence of segments and splits them into
/// separate lines whenever a newline control code is encountered.
///
/// A newline creates a new line, so "a\nb" becomes ["a", "b"] (2 lines),
/// and "\n" becomes ["", ""] (2 empty lines).
#[must_use]
pub fn split_into_lines<'a>(segments: impl IntoIterator<Item = Segment<'a>>) -> SegmentLines<'a> {
    let mut lines = SegmentLines::new();
    let mut current_line = SegmentLine::new();
    let mut has_content = false;

    for segment in segments {
        has_content = true;
        if segment.is_newline() {
            lines.push(std::mem::take(&mut current_line));
        } else if segment.has_text() {
            // Check if text contains literal newlines
            let text = segment.as_str();
            if text.contains('\n') {
                // Split on newlines within the text
                let parts: Vec<&str> = text.split('\n').collect();
                for (i, part) in parts.iter().enumerate() {
                    if !part.is_empty() {
                        current_line.push(Segment {
                            text: Cow::Owned((*part).to_string()),
                            style: segment.style,
                            control: None,
                        });
                    }
                    // Push line after each newline (but not after the last part)
                    if i < parts.len() - 1 {
                        lines.push(std::mem::take(&mut current_line));
                    }
                }
            } else {
                current_line.push(segment);
            }
        } else if !segment.is_empty() {
            current_line.push(segment);
        }
    }

    // Always push the final line (even if empty, it represents content after last newline)
    // Only exception: if we had no segments at all, push one empty line
    if has_content || lines.is_empty() {
        lines.push(current_line);
    }

    lines
}

/// Join lines into a flat sequence of segments with newlines between.
pub fn join_lines<'a>(lines: &SegmentLines<'a>) -> Vec<Segment<'a>> {
    let mut result = Vec::new();
    let line_count = lines.len();

    for (i, line) in lines.iter().enumerate() {
        for segment in line.iter() {
            result.push(segment.clone());
        }
        if i < line_count - 1 {
            result.push(Segment::newline());
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // Basic Segment tests
    // ==========================================================================

    #[test]
    fn segment_text_creates_unstyled_segment() {
        let seg = Segment::text("hello");
        assert_eq!(seg.as_str(), "hello");
        assert!(seg.style.is_none());
        assert!(seg.control.is_none());
    }

    #[test]
    fn segment_styled_creates_styled_segment() {
        let style = Style::new().bold();
        let seg = Segment::styled("hello", style);
        assert_eq!(seg.as_str(), "hello");
        assert_eq!(seg.style, Some(style));
    }

    #[test]
    fn segment_control_creates_control_segment() {
        let seg = Segment::control(ControlCode::LineFeed);
        assert!(seg.is_control());
        assert!(seg.is_newline());
        assert_eq!(seg.cell_length(), 0);
    }

    #[test]
    fn segment_empty_is_empty() {
        let seg = Segment::empty();
        assert!(seg.is_empty());
        assert!(!seg.has_text());
        assert_eq!(seg.cell_length(), 0);
    }

    // ==========================================================================
    // cell_length tests
    // ==========================================================================

    #[test]
    fn cell_length_ascii() {
        let seg = Segment::text("hello");
        assert_eq!(seg.cell_length(), 5);
    }

    #[test]
    fn cell_length_cjk() {
        // CJK characters are typically 2 cells wide
        let seg = Segment::text("你好");
        assert_eq!(seg.cell_length(), 4); // 2 chars * 2 cells each
    }

    #[test]
    fn cell_length_mixed() {
        let seg = Segment::text("hi你好");
        assert_eq!(seg.cell_length(), 6); // 2 ASCII + 4 CJK
    }

    #[test]
    fn cell_length_emoji() {
        // Basic emoji (varies by terminal, but unicode-width treats it as 2)
        let seg = Segment::text("😀");
        // unicode-width may return 2 for emoji
        assert!(seg.cell_length() >= 1);
    }

    #[test]
    fn cell_length_zwj_sequence() {
        // Family emoji (ZWJ sequence)
        let seg = Segment::text("👨‍👩‍👧");
        // This is a complex case - just ensure it doesn't panic
        let _width = seg.cell_length();
    }

    #[test]
    fn cell_length_control_is_zero() {
        let seg = Segment::control(ControlCode::Bell);
        assert_eq!(seg.cell_length(), 0);
    }

    // ==========================================================================
    // split_at_cell tests
    // ==========================================================================

    #[test]
    fn split_at_cell_ascii() {
        let seg = Segment::text("hello world");
        let (left, right) = seg.split_at_cell(5);
        assert_eq!(left.as_str(), "hello");
        assert_eq!(right.as_str(), " world");
    }

    #[test]
    fn split_at_cell_zero() {
        let seg = Segment::text("hello");
        let (left, right) = seg.split_at_cell(0);
        assert_eq!(left.as_str(), "");
        assert_eq!(right.as_str(), "hello");
    }

    #[test]
    fn split_at_cell_beyond_length() {
        let seg = Segment::text("hi");
        let (left, right) = seg.split_at_cell(10);
        assert_eq!(left.as_str(), "hi");
        assert_eq!(right.as_str(), "");
    }

    #[test]
    fn split_at_cell_cjk() {
        // Each CJK char is 2 cells
        let seg = Segment::text("你好世界");
        let (left, right) = seg.split_at_cell(2);
        assert_eq!(left.as_str(), "你");
        assert_eq!(right.as_str(), "好世界");
    }

    #[test]
    fn split_at_cell_cjk_mid_char() {
        // Try to split at cell 1 (middle of a 2-cell char)
        // Should not break the char, so left should be empty
        let seg = Segment::text("你好");
        let (left, right) = seg.split_at_cell(1);
        // Can't include half a character, so left is empty
        assert_eq!(left.as_str(), "");
        assert_eq!(right.as_str(), "你好");
    }

    #[test]
    fn split_at_cell_mixed() {
        let seg = Segment::text("hi你");
        let (left, right) = seg.split_at_cell(2);
        assert_eq!(left.as_str(), "hi");
        assert_eq!(right.as_str(), "你");
    }

    #[test]
    fn split_at_cell_preserves_style() {
        let style = Style::new().bold();
        let seg = Segment::styled("hello", style);
        let (left, right) = seg.split_at_cell(2);
        assert_eq!(left.style, Some(style));
        assert_eq!(right.style, Some(style));
    }

    #[test]
    fn split_at_cell_control_segment() {
        let seg = Segment::control(ControlCode::LineFeed);
        let (left, right) = seg.split_at_cell(0);
        assert!(left.is_empty());
        assert!(right.is_control());
    }

    // ==========================================================================
    // SegmentLine tests
    // ==========================================================================

    #[test]
    fn segment_line_cell_length() {
        let mut line = SegmentLine::new();
        line.push(Segment::text("hello "));
        line.push(Segment::text("world"));
        assert_eq!(line.cell_length(), 11);
    }

    #[test]
    fn segment_line_split_at_cell() {
        let mut line = SegmentLine::new();
        line.push(Segment::text("hello "));
        line.push(Segment::text("world"));

        let (left, right) = line.split_at_cell(8);
        assert_eq!(left.to_plain_text(), "hello wo");
        assert_eq!(right.to_plain_text(), "rld");
    }

    #[test]
    fn segment_line_split_at_segment_boundary() {
        let mut line = SegmentLine::new();
        line.push(Segment::text("hello"));
        line.push(Segment::text(" world"));

        let (left, right) = line.split_at_cell(5);
        assert_eq!(left.to_plain_text(), "hello");
        assert_eq!(right.to_plain_text(), " world");
    }

    // ==========================================================================
    // Line splitting tests
    // ==========================================================================

    #[test]
    fn split_into_lines_single_line() {
        let segments = vec![Segment::text("hello world")];
        let lines = split_into_lines(segments);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines.lines()[0].to_plain_text(), "hello world");
    }

    #[test]
    fn split_into_lines_with_newline_control() {
        let segments = vec![
            Segment::text("line one"),
            Segment::newline(),
            Segment::text("line two"),
        ];
        let lines = split_into_lines(segments);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines.lines()[0].to_plain_text(), "line one");
        assert_eq!(lines.lines()[1].to_plain_text(), "line two");
    }

    #[test]
    fn split_into_lines_with_embedded_newline() {
        let segments = vec![Segment::text("line one\nline two")];
        let lines = split_into_lines(segments);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines.lines()[0].to_plain_text(), "line one");
        assert_eq!(lines.lines()[1].to_plain_text(), "line two");
    }

    #[test]
    fn split_into_lines_empty_input() {
        let segments: Vec<Segment> = vec![];
        let lines = split_into_lines(segments);
        assert_eq!(lines.len(), 1); // One empty line
        assert!(lines.lines()[0].is_empty());
    }

    // ==========================================================================
    // Join lines test
    // ==========================================================================

    #[test]
    fn join_lines_roundtrip() {
        let segments = vec![
            Segment::text("line one"),
            Segment::newline(),
            Segment::text("line two"),
        ];
        let lines = split_into_lines(segments);
        let joined = join_lines(&lines);

        // Should have: "line one", newline, "line two"
        assert_eq!(joined.len(), 3);
        assert_eq!(joined[0].as_str(), "line one");
        assert!(joined[1].is_newline());
        assert_eq!(joined[2].as_str(), "line two");
    }

    // ==========================================================================
    // Ownership tests
    // ==========================================================================

    #[test]
    fn segment_into_owned() {
        let s = String::from("hello");
        let seg: Segment = Segment::text(&s[..]);
        let owned: Segment<'static> = seg.into_owned();
        assert_eq!(owned.as_str(), "hello");
    }

    #[test]
    fn segment_from_string() {
        let seg: Segment<'static> = Segment::from(String::from("hello"));
        assert_eq!(seg.as_str(), "hello");
    }

    #[test]
    fn segment_from_str() {
        let seg: Segment = Segment::from("hello");
        assert_eq!(seg.as_str(), "hello");
    }

    // ==========================================================================
    // Control code tests
    // ==========================================================================

    #[test]
    fn control_code_is_newline() {
        assert!(ControlCode::LineFeed.is_newline());
        assert!(!ControlCode::CarriageReturn.is_newline());
        assert!(!ControlCode::Bell.is_newline());
    }

    #[test]
    fn control_code_is_cr() {
        assert!(ControlCode::CarriageReturn.is_cr());
        assert!(!ControlCode::LineFeed.is_cr());
    }

    #[test]
    fn segment_with_control() {
        let seg = Segment::text("hello").with_control(ControlCode::Bell);
        assert!(seg.control.is_some());
        assert_eq!(seg.control.as_ref().unwrap().len(), 1);
    }

    // ==========================================================================
    // Edge cases
    // ==========================================================================

    #[test]
    fn split_empty_segment() {
        let seg = Segment::text("");
        let (left, right) = seg.split_at_cell(5);
        assert_eq!(left.as_str(), "");
        assert_eq!(right.as_str(), "");
    }

    #[test]
    fn combining_characters() {
        // e followed by combining acute accent
        let seg = Segment::text("e\u{0301}"); // é as two code points
        // Should be treated as single grapheme, 1 cell wide
        let width = seg.cell_length();
        assert!(width >= 1);

        // Split should keep the grapheme together
        let (left, right) = seg.split_at_cell(1);
        assert_eq!(left.cell_length() + right.cell_length(), width);
    }

    #[test]
    fn segment_line_is_empty() {
        let line = SegmentLine::new();
        assert!(line.is_empty());

        let mut line2 = SegmentLine::new();
        line2.push(Segment::empty());
        assert!(line2.is_empty());

        let mut line3 = SegmentLine::new();
        line3.push(Segment::text("x"));
        assert!(!line3.is_empty());
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn split_preserves_total_width(s in "[a-zA-Z0-9 ]{1,100}", pos in 0usize..200) {
            let seg = Segment::text(s);
            let total = seg.cell_length();
            let (left, right) = seg.split_at_cell(pos);

            // Total width should be preserved
            prop_assert_eq!(left.cell_length() + right.cell_length(), total);
        }

        #[test]
        fn split_preserves_content(s in "[a-zA-Z0-9 ]{1,100}", pos in 0usize..200) {
            let seg = Segment::text(s.clone());
            let (left, right) = seg.split_at_cell(pos);

            // Concatenating left and right text should give original
            let combined = format!("{}{}", left.as_str(), right.as_str());
            prop_assert_eq!(combined, s);
        }

        #[test]
        fn cell_length_matches_unicode_width(s in "[a-zA-Z0-9 ]{1,100}") {
            let seg = Segment::text(s.clone());
            let expected = unicode_width::UnicodeWidthStr::width(s.as_str());
            prop_assert_eq!(seg.cell_length(), expected);
        }

        #[test]
        fn line_split_preserves_total_width(
            parts in prop::collection::vec("[a-z]{1,10}", 1..5),
            pos in 0usize..100
        ) {
            let mut line = SegmentLine::new();
            for part in &parts {
                line.push(Segment::text(part.as_str()));
            }

            let total = line.cell_length();
            let (left, right) = line.split_at_cell(pos);

            prop_assert_eq!(left.cell_length() + right.cell_length(), total);
        }

        #[test]
        fn split_into_lines_preserves_content(s in "[a-zA-Z0-9 \n]{1,200}") {
            let segments = vec![Segment::text(s.clone())];
            let lines = split_into_lines(segments);

            // Join all lines with newlines
            let mut result = String::new();
            for (i, line) in lines.lines().iter().enumerate() {
                if i > 0 {
                    result.push('\n');
                }
                result.push_str(&line.to_plain_text());
            }

            prop_assert_eq!(result, s);
        }
    }
}

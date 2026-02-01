#![forbid(unsafe_code)]

//! Text type for styled text collections.
//!
//! `Text` is a higher-level type that collects styled segments and provides
//! ergonomic APIs for text manipulation, styling, and rendering.
//!
//! # Example
//! ```
//! use ftui_text::{Text, Span};
//! use ftui_style::Style;
//!
//! // Simple construction
//! let text = Text::raw("Hello, world!");
//!
//! // Styled text
//! let styled = Text::styled("Error!", Style::new().bold());
//!
//! // Build from spans
//! let text = Text::from_spans([
//!     Span::raw("Normal "),
//!     Span::styled("bold", Style::new().bold()),
//!     Span::raw(" normal"),
//! ]);
//!
//! // Chain spans with builder pattern
//! let text = Text::raw("Status: ")
//!     .with_span(Span::styled("OK", Style::new().bold()));
//! ```

use crate::TextMeasurement;
use crate::segment::{Segment, SegmentLine, SegmentLines, split_into_lines};
use ftui_style::Style;
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

/// A styled span of text.
///
/// Span is a simple wrapper around text and optional style, providing
/// an ergonomic builder for creating styled text units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span<'a> {
    /// The text content.
    pub content: Cow<'a, str>,
    /// Optional style for this span.
    pub style: Option<Style>,
}

impl<'a> Span<'a> {
    /// Create an unstyled span.
    #[inline]
    #[must_use]
    pub fn raw(content: impl Into<Cow<'a, str>>) -> Self {
        Self {
            content: content.into(),
            style: None,
        }
    }

    /// Create a styled span.
    #[inline]
    #[must_use]
    pub fn styled(content: impl Into<Cow<'a, str>>, style: Style) -> Self {
        Self {
            content: content.into(),
            style: Some(style),
        }
    }

    /// Get the text content.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.content
    }

    /// Get the display width in cells.
    #[inline]
    #[must_use]
    pub fn width(&self) -> usize {
        crate::display_width(&self.content)
    }

    /// Return bounds-based measurement for this span.
    #[must_use]
    pub fn measurement(&self) -> TextMeasurement {
        let width = self.width();
        TextMeasurement {
            minimum: width,
            maximum: width,
        }
    }

    /// Check if the span is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Apply a style to this span.
    #[inline]
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = Some(style);
        self
    }

    /// Convert to a segment.
    #[inline]
    #[must_use]
    pub fn into_segment(self) -> Segment<'a> {
        match self.style {
            Some(style) => Segment::styled(self.content, style),
            None => Segment::text(self.content),
        }
    }

    /// Convert to an owned span.
    #[must_use]
    pub fn into_owned(self) -> Span<'static> {
        Span {
            content: Cow::Owned(self.content.into_owned()),
            style: self.style,
        }
    }
}

impl<'a> From<&'a str> for Span<'a> {
    fn from(s: &'a str) -> Self {
        Self::raw(s)
    }
}

impl From<String> for Span<'static> {
    fn from(s: String) -> Self {
        Self::raw(s)
    }
}

impl<'a> From<Segment<'a>> for Span<'a> {
    fn from(seg: Segment<'a>) -> Self {
        Self {
            content: seg.text,
            style: seg.style,
        }
    }
}

impl Default for Span<'_> {
    fn default() -> Self {
        Self::raw("")
    }
}

/// A collection of styled text spans.
///
/// `Text` provides a high-level interface for working with styled text.
/// It stores spans (styled text units) and provides operations for:
/// - Appending and building text
/// - Applying base styles
/// - Splitting into lines
/// - Truncating and wrapping to widths
///
/// # Ownership
/// `Text` uses `Cow<'static, str>` for storage, which means:
/// - String literals are stored by reference (zero-copy)
/// - Owned strings are stored inline
/// - The API is ergonomic (no lifetime parameters on Text)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    /// The lines of styled spans.
    lines: Vec<Line>,
}

/// A single line of styled spans.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    spans: Vec<Span<'static>>,
}

impl Line {
    /// Create an empty line.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self { spans: Vec::new() }
    }

    /// Create a line from spans.
    #[must_use]
    pub fn from_spans<'a>(spans: impl IntoIterator<Item = Span<'a>>) -> Self {
        Self {
            spans: spans.into_iter().map(|s| s.into_owned()).collect(),
        }
    }

    /// Create a line from a single raw string.
    #[inline]
    #[must_use]
    pub fn raw(content: impl Into<String>) -> Self {
        Self {
            spans: vec![Span::raw(content.into())],
        }
    }

    /// Create a line from a single styled string.
    #[inline]
    #[must_use]
    pub fn styled(content: impl Into<String>, style: Style) -> Self {
        Self {
            spans: vec![Span::styled(content.into(), style)],
        }
    }

    /// Check if the line is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty() || self.spans.iter().all(|s| s.is_empty())
    }

    /// Get the number of spans.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Get the display width in cells.
    #[must_use]
    pub fn width(&self) -> usize {
        self.spans.iter().map(|s| s.width()).sum()
    }

    /// Return bounds-based measurement for this line.
    #[must_use]
    pub fn measurement(&self) -> TextMeasurement {
        let width = self.width();
        TextMeasurement {
            minimum: width,
            maximum: width,
        }
    }

    /// Get the spans.
    #[inline]
    #[must_use]
    pub fn spans(&self) -> &[Span<'static>] {
        &self.spans
    }

    /// Add a span to the line.
    #[inline]
    pub fn push_span<'a>(&mut self, span: Span<'a>) {
        self.spans.push(span.into_owned());
    }

    /// Append a span (builder pattern).
    #[inline]
    #[must_use]
    pub fn with_span<'a>(mut self, span: Span<'a>) -> Self {
        self.push_span(span);
        self
    }

    /// Apply a base style to all spans.
    ///
    /// The base style is merged with each span's style, with the span's
    /// style taking precedence for conflicting properties.
    pub fn apply_base_style(&mut self, base: Style) {
        for span in &mut self.spans {
            span.style = Some(match span.style {
                Some(existing) => existing.merge(&base),
                None => base,
            });
        }
    }

    /// Get the plain text content.
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        self.spans.iter().map(|s| s.as_str()).collect()
    }

    /// Convert to segments.
    #[must_use]
    pub fn into_segments(self) -> Vec<Segment<'static>> {
        self.spans.into_iter().map(|s| s.into_segment()).collect()
    }

    /// Convert to a SegmentLine.
    #[must_use]
    pub fn into_segment_line(self) -> SegmentLine<'static> {
        SegmentLine::from_segments(self.into_segments())
    }

    /// Iterate over spans.
    pub fn iter(&self) -> impl Iterator<Item = &Span<'static>> {
        self.spans.iter()
    }
}

impl<'a> From<Span<'a>> for Line {
    fn from(span: Span<'a>) -> Self {
        Self {
            spans: vec![span.into_owned()],
        }
    }
}

impl From<&str> for Line {
    fn from(s: &str) -> Self {
        Self::raw(s)
    }
}

impl From<String> for Line {
    fn from(s: String) -> Self {
        Self::raw(s)
    }
}

impl IntoIterator for Line {
    type Item = Span<'static>;
    type IntoIter = std::vec::IntoIter<Span<'static>>;

    fn into_iter(self) -> Self::IntoIter {
        self.spans.into_iter()
    }
}

impl<'a> IntoIterator for &'a Line {
    type Item = &'a Span<'static>;
    type IntoIter = std::slice::Iter<'a, Span<'static>>;

    fn into_iter(self) -> Self::IntoIter {
        self.spans.iter()
    }
}

impl Text {
    /// Create an empty text.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self { lines: Vec::new() }
    }

    /// Create text from a raw string (may contain newlines).
    #[must_use]
    pub fn raw(content: impl AsRef<str>) -> Self {
        let content = content.as_ref();
        if content.is_empty() {
            return Self::new();
        }

        let lines: Vec<Line> = content.split('\n').map(Line::raw).collect();

        Self { lines }
    }

    /// Create styled text from a string (may contain newlines).
    #[must_use]
    pub fn styled(content: impl AsRef<str>, style: Style) -> Self {
        let content = content.as_ref();
        if content.is_empty() {
            return Self::new();
        }

        let lines: Vec<Line> = content
            .split('\n')
            .map(|s| Line::styled(s, style))
            .collect();

        Self { lines }
    }

    /// Create text from a single line.
    #[inline]
    #[must_use]
    pub fn from_line(line: Line) -> Self {
        Self { lines: vec![line] }
    }

    /// Create text from multiple lines.
    #[must_use]
    pub fn from_lines(lines: impl IntoIterator<Item = Line>) -> Self {
        Self {
            lines: lines.into_iter().collect(),
        }
    }

    /// Create text from spans (single line).
    #[must_use]
    pub fn from_spans<'a>(spans: impl IntoIterator<Item = Span<'a>>) -> Self {
        Self {
            lines: vec![Line::from_spans(spans)],
        }
    }

    /// Create text from segments.
    #[must_use]
    pub fn from_segments<'a>(segments: impl IntoIterator<Item = Segment<'a>>) -> Self {
        let segment_lines = split_into_lines(segments);
        let lines: Vec<Line> = segment_lines
            .into_iter()
            .map(|seg_line| Line::from_spans(seg_line.into_iter().map(Span::from)))
            .collect();

        Self { lines }
    }

    /// Check if empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() || self.lines.iter().all(|l| l.is_empty())
    }

    /// Get the number of lines.
    #[inline]
    #[must_use]
    pub fn height(&self) -> usize {
        self.lines.len()
    }

    /// Get the maximum width across all lines.
    #[must_use]
    pub fn width(&self) -> usize {
        self.lines.iter().map(|l| l.width()).max().unwrap_or(0)
    }

    /// Return bounds-based measurement for this text block.
    #[must_use]
    pub fn measurement(&self) -> TextMeasurement {
        let width = self.width();
        TextMeasurement {
            minimum: width,
            maximum: width,
        }
    }

    /// Get the lines.
    #[inline]
    #[must_use]
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Add a line.
    #[inline]
    pub fn push_line(&mut self, line: Line) {
        self.lines.push(line);
    }

    /// Append a line (builder pattern).
    #[inline]
    #[must_use]
    pub fn with_line(mut self, line: Line) -> Self {
        self.push_line(line);
        self
    }

    /// Add a span to the last line (or create new line if empty).
    pub fn push_span<'a>(&mut self, span: Span<'a>) {
        if self.lines.is_empty() {
            self.lines.push(Line::new());
        }
        if let Some(last) = self.lines.last_mut() {
            last.push_span(span);
        }
    }

    /// Append a span to the last line (builder pattern).
    #[must_use]
    pub fn with_span<'a>(mut self, span: Span<'a>) -> Self {
        self.push_span(span);
        self
    }

    /// Apply a base style to all lines and spans.
    ///
    /// The base style is merged with each span's style, with the span's
    /// style taking precedence for conflicting properties.
    pub fn apply_base_style(&mut self, base: Style) {
        for line in &mut self.lines {
            line.apply_base_style(base);
        }
    }

    /// Create a new Text with base style applied.
    #[must_use]
    pub fn with_base_style(mut self, base: Style) -> Self {
        self.apply_base_style(base);
        self
    }

    /// Get the plain text content (lines joined with newlines).
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.to_plain_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Convert to SegmentLines.
    #[must_use]
    pub fn into_segment_lines(self) -> SegmentLines<'static> {
        SegmentLines::from_lines(
            self.lines
                .into_iter()
                .map(|l| l.into_segment_line())
                .collect(),
        )
    }

    /// Iterate over lines.
    pub fn iter(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }

    /// Truncate all lines to a maximum width.
    ///
    /// Lines exceeding `max_width` are truncated. If `ellipsis` is provided,
    /// it replaces the end of truncated lines.
    pub fn truncate(&mut self, max_width: usize, ellipsis: Option<&str>) {
        let ellipsis_width = ellipsis.map(|e| e.width()).unwrap_or(0);

        for line in &mut self.lines {
            let line_width = line.width();
            if line_width <= max_width {
                continue;
            }

            // Calculate how much content we can keep
            let (content_width, use_ellipsis) = if ellipsis.is_some() && max_width >= ellipsis_width
            {
                (max_width - ellipsis_width, true)
            } else {
                (max_width, false)
            };

            // Truncate spans
            let mut remaining = content_width;
            let mut new_spans = Vec::new();

            for span in &line.spans {
                if remaining == 0 {
                    break;
                }

                let span_width = span.width();
                if span_width <= remaining {
                    new_spans.push(span.clone());
                    remaining -= span_width;
                } else {
                    // Need to truncate this span
                    let truncated = truncate_str(&span.content, remaining);
                    if !truncated.is_empty() {
                        new_spans.push(Span {
                            content: Cow::Owned(truncated),
                            style: span.style,
                        });
                    }
                    remaining = 0;
                }
            }

            // Add ellipsis if needed and we have space
            if use_ellipsis
                && line_width > max_width
                && let Some(e) = ellipsis
            {
                new_spans.push(Span::raw(e.to_string()));
            }

            line.spans = new_spans;
        }
    }

    /// Create a truncated copy.
    #[must_use]
    pub fn truncated(&self, max_width: usize, ellipsis: Option<&str>) -> Self {
        let mut text = self.clone();
        text.truncate(max_width, ellipsis);
        text
    }
}

/// Truncate a string to fit within `max_width` cells.
fn truncate_str(s: &str, max_width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;

    let mut result = String::new();
    let mut width = 0;

    for grapheme in s.graphemes(true) {
        let g_width = grapheme.width();
        if width + g_width > max_width {
            break;
        }
        result.push_str(grapheme);
        width += g_width;
    }

    result
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Self::raw(s)
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Self::raw(s)
    }
}

impl From<Line> for Text {
    fn from(line: Line) -> Self {
        Self::from_line(line)
    }
}

impl<'a> FromIterator<Span<'a>> for Text {
    fn from_iter<I: IntoIterator<Item = Span<'a>>>(iter: I) -> Self {
        Self::from_spans(iter)
    }
}

impl FromIterator<Line> for Text {
    fn from_iter<I: IntoIterator<Item = Line>>(iter: I) -> Self {
        Self::from_lines(iter)
    }
}

impl IntoIterator for Text {
    type Item = Line;
    type IntoIter = std::vec::IntoIter<Line>;

    fn into_iter(self) -> Self::IntoIter {
        self.lines.into_iter()
    }
}

impl<'a> IntoIterator for &'a Text {
    type Item = &'a Line;
    type IntoIter = std::slice::Iter<'a, Line>;

    fn into_iter(self) -> Self::IntoIter {
        self.lines.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_style::StyleFlags;

    // ==========================================================================
    // Span tests
    // ==========================================================================

    #[test]
    fn span_raw_creates_unstyled() {
        let span = Span::raw("hello");
        assert_eq!(span.as_str(), "hello");
        assert!(span.style.is_none());
    }

    #[test]
    fn span_styled_creates_styled() {
        let style = Style::new().bold();
        let span = Span::styled("hello", style);
        assert_eq!(span.as_str(), "hello");
        assert_eq!(span.style, Some(style));
    }

    #[test]
    fn span_width_ascii() {
        let span = Span::raw("hello");
        assert_eq!(span.width(), 5);
    }

    #[test]
    fn span_width_cjk() {
        let span = Span::raw("你好");
        assert_eq!(span.width(), 4);
    }

    #[test]
    fn span_into_segment() {
        let style = Style::new().bold();
        let span = Span::styled("hello", style);
        let seg = span.into_segment();
        assert_eq!(seg.as_str(), "hello");
        assert_eq!(seg.style, Some(style));
    }

    // ==========================================================================
    // Line tests
    // ==========================================================================

    #[test]
    fn line_empty() {
        let line = Line::new();
        assert!(line.is_empty());
        assert_eq!(line.width(), 0);
    }

    #[test]
    fn line_raw() {
        let line = Line::raw("hello world");
        assert_eq!(line.width(), 11);
        assert_eq!(line.to_plain_text(), "hello world");
    }

    #[test]
    fn line_styled() {
        let style = Style::new().bold();
        let line = Line::styled("hello", style);
        assert_eq!(line.spans()[0].style, Some(style));
    }

    #[test]
    fn line_from_spans() {
        let line = Line::from_spans([Span::raw("hello "), Span::raw("world")]);
        assert_eq!(line.len(), 2);
        assert_eq!(line.width(), 11);
        assert_eq!(line.to_plain_text(), "hello world");
    }

    #[test]
    fn line_push_span() {
        let mut line = Line::raw("hello ");
        line.push_span(Span::raw("world"));
        assert_eq!(line.len(), 2);
        assert_eq!(line.to_plain_text(), "hello world");
    }

    #[test]
    fn line_apply_base_style() {
        let base = Style::new().bold();
        let mut line = Line::from_spans([
            Span::raw("hello"),
            Span::styled("world", Style::new().italic()),
        ]);

        line.apply_base_style(base);

        // First span should have bold
        assert!(line.spans()[0].style.unwrap().has_attr(StyleFlags::BOLD));

        // Second span should have both bold and italic
        let second_style = line.spans()[1].style.unwrap();
        assert!(second_style.has_attr(StyleFlags::BOLD));
        assert!(second_style.has_attr(StyleFlags::ITALIC));
    }

    // ==========================================================================
    // Text tests
    // ==========================================================================

    #[test]
    fn text_empty() {
        let text = Text::new();
        assert!(text.is_empty());
        assert_eq!(text.height(), 0);
        assert_eq!(text.width(), 0);
    }

    #[test]
    fn text_raw_single_line() {
        let text = Text::raw("hello world");
        assert_eq!(text.height(), 1);
        assert_eq!(text.width(), 11);
        assert_eq!(text.to_plain_text(), "hello world");
    }

    #[test]
    fn text_raw_multiline() {
        let text = Text::raw("line 1\nline 2\nline 3");
        assert_eq!(text.height(), 3);
        assert_eq!(text.to_plain_text(), "line 1\nline 2\nline 3");
    }

    #[test]
    fn text_styled() {
        let style = Style::new().bold();
        let text = Text::styled("hello", style);
        assert_eq!(text.lines()[0].spans()[0].style, Some(style));
    }

    #[test]
    fn text_from_spans() {
        let text = Text::from_spans([Span::raw("hello "), Span::raw("world")]);
        assert_eq!(text.height(), 1);
        assert_eq!(text.to_plain_text(), "hello world");
    }

    #[test]
    fn text_from_lines() {
        let text = Text::from_lines([Line::raw("line 1"), Line::raw("line 2")]);
        assert_eq!(text.height(), 2);
        assert_eq!(text.to_plain_text(), "line 1\nline 2");
    }

    #[test]
    fn text_push_line() {
        let mut text = Text::raw("line 1");
        text.push_line(Line::raw("line 2"));
        assert_eq!(text.height(), 2);
    }

    #[test]
    fn text_push_span() {
        let mut text = Text::raw("hello ");
        text.push_span(Span::raw("world"));
        assert_eq!(text.to_plain_text(), "hello world");
    }

    #[test]
    fn text_apply_base_style() {
        let base = Style::new().bold();
        let mut text = Text::from_lines([
            Line::raw("line 1"),
            Line::styled("line 2", Style::new().italic()),
        ]);

        text.apply_base_style(base);

        // First line should have bold
        assert!(
            text.lines()[0].spans()[0]
                .style
                .unwrap()
                .has_attr(StyleFlags::BOLD)
        );

        // Second line should have both
        let second_style = text.lines()[1].spans()[0].style.unwrap();
        assert!(second_style.has_attr(StyleFlags::BOLD));
        assert!(second_style.has_attr(StyleFlags::ITALIC));
    }

    #[test]
    fn text_width_multiline() {
        let text = Text::raw("short\nlonger line\nmed");
        assert_eq!(text.width(), 11); // "longer line" is widest
    }

    // ==========================================================================
    // Truncation tests
    // ==========================================================================

    #[test]
    fn truncate_no_change_if_fits() {
        let mut text = Text::raw("hello");
        text.truncate(10, None);
        assert_eq!(text.to_plain_text(), "hello");
    }

    #[test]
    fn truncate_simple() {
        let mut text = Text::raw("hello world");
        text.truncate(5, None);
        assert_eq!(text.to_plain_text(), "hello");
    }

    #[test]
    fn truncate_with_ellipsis() {
        let mut text = Text::raw("hello world");
        text.truncate(8, Some("..."));
        assert_eq!(text.to_plain_text(), "hello...");
    }

    #[test]
    fn truncate_multiline() {
        let mut text = Text::raw("hello world\nfoo bar baz");
        text.truncate(8, Some("..."));
        assert_eq!(text.to_plain_text(), "hello...\nfoo b...");
    }

    #[test]
    fn truncate_preserves_style() {
        let style = Style::new().bold();
        let mut text = Text::styled("hello world", style);
        text.truncate(5, None);

        assert_eq!(text.lines()[0].spans()[0].style, Some(style));
    }

    #[test]
    fn truncate_cjk() {
        let mut text = Text::raw("你好世界"); // 8 cells
        text.truncate(4, None);
        assert_eq!(text.to_plain_text(), "你好");
    }

    #[test]
    fn truncate_cjk_odd_width() {
        let mut text = Text::raw("你好世界"); // 8 cells
        text.truncate(5, None); // Can't fit half a char, so only 4
        assert_eq!(text.to_plain_text(), "你好");
    }

    // ==========================================================================
    // Conversion tests
    // ==========================================================================

    #[test]
    fn text_from_str() {
        let text: Text = "hello".into();
        assert_eq!(text.to_plain_text(), "hello");
    }

    #[test]
    fn text_from_string() {
        let text: Text = String::from("hello").into();
        assert_eq!(text.to_plain_text(), "hello");
    }

    #[test]
    fn text_into_segment_lines() {
        let text = Text::raw("line 1\nline 2");
        let seg_lines = text.into_segment_lines();
        assert_eq!(seg_lines.len(), 2);
    }

    #[test]
    fn line_into_iter() {
        let line = Line::from_spans([Span::raw("a"), Span::raw("b")]);
        let collected: Vec<_> = line.into_iter().collect();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn text_into_iter() {
        let text = Text::from_lines([Line::raw("a"), Line::raw("b")]);
        let collected: Vec<_> = text.into_iter().collect();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn text_collect_from_spans() {
        let text: Text = [Span::raw("a"), Span::raw("b")].into_iter().collect();
        assert_eq!(text.height(), 1);
        assert_eq!(text.to_plain_text(), "ab");
    }

    #[test]
    fn text_collect_from_lines() {
        let text: Text = [Line::raw("a"), Line::raw("b")].into_iter().collect();
        assert_eq!(text.height(), 2);
    }

    // ==========================================================================
    // Edge cases
    // ==========================================================================

    #[test]
    fn empty_string_creates_empty_text() {
        let text = Text::raw("");
        assert!(text.is_empty());
    }

    #[test]
    fn single_newline_creates_two_empty_lines() {
        let text = Text::raw("\n");
        assert_eq!(text.height(), 2);
        assert!(text.lines()[0].is_empty());
        assert!(text.lines()[1].is_empty());
    }

    #[test]
    fn trailing_newline() {
        let text = Text::raw("hello\n");
        assert_eq!(text.height(), 2);
        assert_eq!(text.lines()[0].to_plain_text(), "hello");
        assert!(text.lines()[1].is_empty());
    }

    #[test]
    fn leading_newline() {
        let text = Text::raw("\nhello");
        assert_eq!(text.height(), 2);
        assert!(text.lines()[0].is_empty());
        assert_eq!(text.lines()[1].to_plain_text(), "hello");
    }

    #[test]
    fn line_with_span_ownership() {
        // Verify that spans are properly owned
        let s = String::from("hello");
        let line = Line::raw(&s);
        drop(s); // Original string dropped
        assert_eq!(line.to_plain_text(), "hello"); // Still works
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn raw_text_roundtrips(s in "[a-zA-Z0-9 \n]{0,100}") {
            let text = Text::raw(&s);
            let plain = text.to_plain_text();
            prop_assert_eq!(plain, s);
        }

        #[test]
        fn truncate_never_exceeds_width(s in "[a-zA-Z0-9]{1,50}", max_width in 1usize..20) {
            let mut text = Text::raw(&s);
            text.truncate(max_width, None);
            prop_assert!(text.width() <= max_width);
        }

        #[test]
        fn truncate_with_ellipsis_never_exceeds_width(s in "[a-zA-Z0-9]{1,50}", max_width in 4usize..20) {
            let mut text = Text::raw(&s);
            text.truncate(max_width, Some("..."));
            prop_assert!(text.width() <= max_width);
        }

        #[test]
        fn height_equals_newline_count_plus_one(s in "[a-zA-Z\n]{1,100}") {
            let text = Text::raw(&s);
            let newline_count = s.chars().filter(|&c| c == '\n').count();
            prop_assert_eq!(text.height(), newline_count + 1);
        }

        #[test]
        fn from_segments_preserves_content(
            parts in prop::collection::vec("[a-z]{1,10}", 1..5)
        ) {
            let segments: Vec<Segment> = parts.iter()
                .map(|s| Segment::text(s.as_str()))
                .collect();

            let text = Text::from_segments(segments);
            let plain = text.to_plain_text();
            let expected: String = parts.join("");

            prop_assert_eq!(plain, expected);
        }
    }
}

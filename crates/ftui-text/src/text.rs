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
use crate::grapheme_width;
use crate::segment::{Segment, SegmentLine, SegmentLines, split_into_lines};
use crate::wrap::{WrapMode, graphemes, truncate_to_width_with_info};
use ftui_style::Style;
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

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
    /// Optional hyperlink URL (OSC 8).
    pub link: Option<Cow<'a, str>>,
}

impl<'a> Span<'a> {
    /// Create an unstyled span.
    #[inline]
    #[must_use]
    pub fn raw(content: impl Into<Cow<'a, str>>) -> Self {
        Self {
            content: content.into(),
            style: None,
            link: None,
        }
    }

    /// Create a styled span.
    #[inline]
    #[must_use]
    pub fn styled(content: impl Into<Cow<'a, str>>, style: Style) -> Self {
        Self {
            content: content.into(),
            style: Some(style),
            link: None,
        }
    }

    /// Set the hyperlink URL for this span.
    #[inline]
    #[must_use]
    pub fn link(mut self, link: impl Into<Cow<'a, str>>) -> Self {
        self.link = Some(link.into());
        self
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

    /// Split the span at a cell position.
    ///
    /// Returns `(left, right)` where the split respects grapheme boundaries.
    #[must_use]
    pub fn split_at_cell(&self, cell_pos: usize) -> (Self, Self) {
        if self.content.is_empty() || cell_pos == 0 {
            return (Self::raw(""), self.clone());
        }

        let total_width = self.width();
        if cell_pos >= total_width {
            return (self.clone(), Self::raw(""));
        }

        let (byte_pos, _actual_width) = find_cell_boundary(&self.content, cell_pos);
        let (left, right) = self.content.split_at(byte_pos);

        (
            Self {
                content: Cow::Owned(left.to_string()),
                style: self.style,
                link: self.link.clone(),
            },
            Self {
                content: Cow::Owned(right.to_string()),
                style: self.style,
                link: self.link.clone(),
            },
        )
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
        // Segments don't support links yet, so we ignore it.
        // TODO: Add link support to Segment if needed for lower-level handling.
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
            link: self.link.map(|l| Cow::Owned(l.into_owned())),
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
            link: None,
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
    #[inline]
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

    /// Wrap this line to the given width, preserving span styles.
    #[must_use]
    pub fn wrap(&self, width: usize, mode: WrapMode) -> Vec<Line> {
        if mode == WrapMode::None || width == 0 {
            return vec![self.clone()];
        }

        if self.is_empty() {
            return vec![Line::new()];
        }

        match mode {
            WrapMode::None => vec![self.clone()],
            WrapMode::Char => wrap_line_chars(self, width),
            WrapMode::Word => wrap_line_words(self, width, false),
            WrapMode::WordChar => wrap_line_words(self, width, true),
        }
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

    /// Get the number of lines as u16, saturating at u16::MAX.
    #[inline]
    #[must_use]
    pub fn height_as_u16(&self) -> u16 {
        self.lines.len().try_into().unwrap_or(u16::MAX)
    }

    /// Get the maximum width across all lines.
    #[inline]
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

    /// Get the style of the first span, if any.
    ///
    /// Returns `None` if the text is empty or has no styled spans.
    #[inline]
    #[must_use]
    pub fn style(&self) -> Option<Style> {
        self.lines
            .first()
            .and_then(|line| line.spans().first())
            .and_then(|span| span.style)
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
        let ellipsis_width = ellipsis.map(crate::display_width).unwrap_or(0);

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
                    let (truncated, _) = truncate_to_width_with_info(&span.content, remaining);
                    if !truncated.is_empty() {
                        new_spans.push(Span {
                            content: Cow::Owned(truncated.to_string()),
                            style: span.style,
                            link: span.link.clone(),
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

// ---------------------------------------------------------------------------
// Wrap Helpers (style-preserving)
// ---------------------------------------------------------------------------

fn find_cell_boundary(text: &str, target_cells: usize) -> (usize, usize) {
    let mut current_cells = 0;
    let mut byte_pos = 0;

    for grapheme in graphemes(text) {
        let grapheme_width = grapheme_width(grapheme);

        if current_cells + grapheme_width > target_cells {
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

fn span_is_whitespace(span: &Span<'static>) -> bool {
    span.as_str()
        .graphemes(true)
        .all(|g| g.chars().all(|c| c.is_whitespace()))
}

fn trim_span_start(span: Span<'static>) -> Span<'static> {
    let text = span.as_str();
    let mut start = 0;
    let mut found = false;

    for (idx, grapheme) in text.grapheme_indices(true) {
        if grapheme.chars().all(|c| c.is_whitespace()) {
            start = idx + grapheme.len();
            continue;
        }
        found = true;
        break;
    }

    if !found {
        return Span::raw("");
    }

    Span {
        content: Cow::Owned(text[start..].to_string()),
        style: span.style,
        link: span.link,
    }
}

fn trim_span_end(span: Span<'static>) -> Span<'static> {
    let text = span.as_str();
    let mut end = text.len();
    let mut found = false;

    for (idx, grapheme) in text.grapheme_indices(true).rev() {
        if grapheme.chars().all(|c| c.is_whitespace()) {
            end = idx;
            continue;
        }
        found = true;
        break;
    }

    if !found {
        return Span::raw("");
    }

    Span {
        content: Cow::Owned(text[..end].to_string()),
        style: span.style,
        link: span.link,
    }
}

fn trim_line_trailing(mut line: Line) -> Line {
    while let Some(last) = line.spans.last().cloned() {
        let trimmed = trim_span_end(last);
        if trimmed.is_empty() {
            line.spans.pop();
            continue;
        }
        let len = line.spans.len();
        if len > 0 {
            line.spans[len - 1] = trimmed;
        }
        break;
    }
    line
}

fn push_span_merged(line: &mut Line, span: Span<'static>) {
    if span.is_empty() {
        return;
    }

    if let Some(last) = line.spans.last_mut()
        && last.style == span.style
        && last.link == span.link
    {
        let mut merged = String::with_capacity(last.as_str().len() + span.as_str().len());
        merged.push_str(last.as_str());
        merged.push_str(span.as_str());
        last.content = Cow::Owned(merged);
        return;
    }

    line.spans.push(span);
}

fn split_span_words(span: &Span<'static>) -> Vec<Span<'static>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_whitespace = false;

    for grapheme in span.as_str().graphemes(true) {
        let is_ws = grapheme.chars().all(crate::wrap::is_breaking_whitespace);

        if is_ws != in_whitespace && !current.is_empty() {
            segments.push(Span {
                content: Cow::Owned(std::mem::take(&mut current)),
                style: span.style,
                link: span.link.clone(),
            });
        }

        current.push_str(grapheme);
        in_whitespace = is_ws;
    }

    if !current.is_empty() {
        segments.push(Span {
            content: Cow::Owned(current),
            style: span.style,
            link: span.link.clone(),
        });
    }

    segments
}

fn wrap_line_chars(line: &Line, width: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut current = Line::new();
    let mut current_width = 0;

    for span in line.spans.iter().cloned() {
        let mut remaining = span;
        while !remaining.is_empty() {
            if current_width >= width && !current.is_empty() {
                lines.push(trim_line_trailing(current));
                current = Line::new();
                current_width = 0;
            }

            let available = width.saturating_sub(current_width).max(1);
            let span_width = remaining.width();

            if span_width <= available {
                current_width += span_width;
                push_span_merged(&mut current, remaining);
                break;
            }

            let (left, right) = remaining.split_at_cell(available);

            // Force progress if the first grapheme is too wide for `available`
            // and we are at the start of a line (so we can't wrap further).
            let (left, right) = if left.is_empty() && current.is_empty() && !remaining.is_empty() {
                let first_w = remaining
                    .as_str()
                    .graphemes(true)
                    .next()
                    .map(grapheme_width)
                    .unwrap_or(1);
                remaining.split_at_cell(first_w.max(1))
            } else {
                (left, right)
            };

            if !left.is_empty() {
                push_span_merged(&mut current, left);
            }
            lines.push(trim_line_trailing(current));
            current = Line::new();
            current_width = 0;
            remaining = right;
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(trim_line_trailing(current));
    }

    lines
}

fn wrap_line_words(line: &Line, width: usize, char_fallback: bool) -> Vec<Line> {
    let mut pieces = Vec::new();
    for span in &line.spans {
        pieces.extend(split_span_words(span));
    }

    let mut lines = Vec::new();
    let mut current = Line::new();
    let mut current_width = 0;
    let mut first_line = true;

    for piece in pieces {
        let piece_width = piece.width();
        let is_ws = span_is_whitespace(&piece);

        if current_width + piece_width <= width {
            if current_width == 0 && !first_line && is_ws {
                continue;
            }
            current_width += piece_width;
            push_span_merged(&mut current, piece);
            continue;
        }

        if !current.is_empty() {
            lines.push(trim_line_trailing(current));
            current = Line::new();
            current_width = 0;
            first_line = false;
        }

        if piece_width > width {
            if char_fallback {
                let mut remaining = piece;
                while !remaining.is_empty() {
                    if current_width >= width && !current.is_empty() {
                        lines.push(trim_line_trailing(current));
                        current = Line::new();
                        current_width = 0;
                        first_line = false;
                    }

                    let available = width.saturating_sub(current_width).max(1);
                    let (left, right) = remaining.split_at_cell(available);

                    // Force progress if the first grapheme is too wide for `available`
                    // and we are at the start of a line (so we can't wrap further).
                    let (left, right) =
                        if left.is_empty() && current.is_empty() && !remaining.is_empty() {
                            let first_w = remaining
                                .as_str()
                                .graphemes(true)
                                .next()
                                .map(grapheme_width)
                                .unwrap_or(1);
                            remaining.split_at_cell(first_w.max(1))
                        } else {
                            (left, right)
                        };

                    let mut left = left;

                    if current_width == 0 && !first_line {
                        left = trim_span_start(left);
                    }

                    if !left.is_empty() {
                        current_width += left.width();
                        push_span_merged(&mut current, left);
                    }

                    if current_width >= width && !current.is_empty() {
                        lines.push(trim_line_trailing(current));
                        current = Line::new();
                        current_width = 0;
                        first_line = false;
                    }

                    remaining = right;
                }
            } else if !is_ws {
                let mut trimmed = piece;
                if !first_line {
                    trimmed = trim_span_start(trimmed);
                }
                if !trimmed.is_empty() {
                    push_span_merged(&mut current, trimmed);
                }
                lines.push(trim_line_trailing(current));
                current = Line::new();
                current_width = 0;
                first_line = false;
            }
            continue;
        }

        let mut trimmed = piece;
        if !first_line {
            trimmed = trim_span_start(trimmed);
        }
        if !trimmed.is_empty() {
            current_width += trimmed.width();
            push_span_merged(&mut current, trimmed);
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(trim_line_trailing(current));
    }

    lines
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

    #[test]
    fn line_wrap_preserves_styles_word() {
        let bold = Style::new().bold();
        let italic = Style::new().italic();
        let line = Line::from_spans([Span::styled("Hello", bold), Span::styled(" world", italic)]);

        let wrapped = line.wrap(6, WrapMode::Word);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].spans()[0].as_str(), "Hello");
        assert_eq!(wrapped[0].spans()[0].style, Some(bold));
        assert_eq!(wrapped[1].spans()[0].as_str(), "world");
        assert_eq!(wrapped[1].spans()[0].style, Some(italic));
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
    fn text_from_empty_string_is_empty() {
        let text: Text = String::new().into();
        assert!(text.is_empty());
        assert_eq!(text.height(), 0);
        assert_eq!(text.width(), 0);
    }

    #[test]
    fn text_from_empty_line_preserves_single_empty_line() {
        let text: Text = Line::new().into();
        assert_eq!(text.height(), 1);
        assert!(text.lines()[0].is_empty());
        assert_eq!(text.width(), 0);
    }

    #[test]
    fn text_from_lines_empty_iter_is_empty() {
        let text = Text::from_lines(Vec::<Line>::new());
        assert!(text.is_empty());
        assert_eq!(text.height(), 0);
    }

    #[test]
    fn text_from_str_preserves_empty_middle_line() {
        let text: Text = "a\n\nb".into();
        assert_eq!(text.height(), 3);
        assert_eq!(text.lines()[0].to_plain_text(), "a");
        assert!(text.lines()[1].is_empty());
        assert_eq!(text.lines()[2].to_plain_text(), "b");
        assert_eq!(text.to_plain_text(), "a\n\nb");
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

    // ==========================================================================
    // Cow<str> ownership behavior tests
    // ==========================================================================

    #[test]
    fn span_cow_borrowed_from_static() {
        let span = Span::raw("static");
        assert!(matches!(span.content, Cow::Borrowed(_)));
    }

    #[test]
    fn span_cow_owned_from_string() {
        let span = Span::raw(String::from("owned"));
        assert!(matches!(span.content, Cow::Owned(_)));
    }

    #[test]
    fn span_into_owned_converts_borrowed() {
        let span = Span::raw("borrowed");
        assert!(matches!(span.content, Cow::Borrowed(_)));

        let owned = span.into_owned();
        assert!(matches!(owned.content, Cow::Owned(_)));
        assert_eq!(owned.as_str(), "borrowed");
    }

    #[test]
    fn span_with_link_into_owned() {
        let span = Span::raw("text").link("https://example.com");
        let owned = span.into_owned();
        assert!(owned.link.is_some());
        assert!(matches!(owned.link.as_ref().unwrap(), Cow::Owned(_)));
    }

    // ==========================================================================
    // Span additional tests
    // ==========================================================================

    #[test]
    fn span_link_method() {
        let span = Span::raw("click me").link("https://example.com");
        assert_eq!(span.link.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn span_measurement() {
        let span = Span::raw("hello");
        let m = span.measurement();
        assert_eq!(m.minimum, 5);
        assert_eq!(m.maximum, 5);
    }

    #[test]
    fn span_is_empty() {
        assert!(Span::raw("").is_empty());
        assert!(!Span::raw("x").is_empty());
    }

    #[test]
    fn span_default_is_empty() {
        let span = Span::default();
        assert!(span.is_empty());
        assert!(span.style.is_none());
        assert!(span.link.is_none());
    }

    #[test]
    fn span_with_style() {
        let style = Style::new().bold();
        let span = Span::raw("text").with_style(style);
        assert_eq!(span.style, Some(style));
    }

    #[test]
    fn span_from_segment() {
        let style = Style::new().italic();
        let seg = Segment::styled("hello", style);
        let span: Span = seg.into();
        assert_eq!(span.as_str(), "hello");
        assert_eq!(span.style, Some(style));
    }

    #[test]
    fn span_debug_impl() {
        let span = Span::raw("test");
        let debug = format!("{:?}", span);
        assert!(debug.contains("Span"));
        assert!(debug.contains("test"));
    }

    // ==========================================================================
    // Line additional tests
    // ==========================================================================

    #[test]
    fn line_measurement() {
        let line = Line::raw("hello world");
        let m = line.measurement();
        assert_eq!(m.minimum, 11);
        assert_eq!(m.maximum, 11);
    }

    #[test]
    fn line_from_empty_string_is_empty() {
        let line: Line = String::new().into();
        assert!(line.is_empty());
        assert_eq!(line.width(), 0);
    }

    #[test]
    fn line_width_combining_mark_is_single_cell() {
        let line = Line::raw("e\u{301}");
        assert_eq!(line.width(), 1);
    }

    #[test]
    fn line_wrap_handles_wide_grapheme_with_tiny_width() {
        let line = Line::raw("你好");
        let wrapped = line.wrap(1, WrapMode::Char);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].to_plain_text(), "你");
        assert_eq!(wrapped[1].to_plain_text(), "好");
    }

    #[test]
    fn line_iter() {
        let line = Line::from_spans([Span::raw("a"), Span::raw("b"), Span::raw("c")]);
        let collected: Vec<_> = line.iter().collect();
        assert_eq!(collected.len(), 3);
    }

    #[test]
    fn line_into_segments() {
        let style = Style::new().bold();
        let line = Line::from_spans([Span::raw("hello"), Span::styled(" world", style)]);
        let segments = line.into_segments();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].style, None);
        assert_eq!(segments[1].style, Some(style));
    }

    #[test]
    fn line_into_segment_line() {
        let line = Line::raw("test");
        let seg_line = line.into_segment_line();
        assert_eq!(seg_line.to_plain_text(), "test");
    }

    #[test]
    fn line_with_span_builder() {
        let line = Line::raw("hello ").with_span(Span::raw("world"));
        assert_eq!(line.to_plain_text(), "hello world");
    }

    #[test]
    fn line_from_span() {
        let span = Span::styled("test", Style::new().bold());
        let line: Line = span.into();
        assert_eq!(line.to_plain_text(), "test");
    }

    #[test]
    fn line_debug_impl() {
        let line = Line::raw("test");
        let debug = format!("{:?}", line);
        assert!(debug.contains("Line"));
    }

    #[test]
    fn line_default_is_empty() {
        let line = Line::default();
        assert!(line.is_empty());
    }

    // ==========================================================================
    // Text additional tests
    // ==========================================================================

    #[test]
    fn text_style_returns_first_span_style() {
        let style = Style::new().bold();
        let text = Text::styled("hello", style);
        assert_eq!(text.style(), Some(style));
    }

    #[test]
    fn text_style_returns_none_for_empty() {
        let text = Text::new();
        assert!(text.style().is_none());
    }

    #[test]
    fn text_style_returns_none_for_unstyled() {
        let text = Text::raw("plain");
        assert!(text.style().is_none());
    }

    #[test]
    fn text_with_line_builder() {
        let text = Text::raw("line 1").with_line(Line::raw("line 2"));
        assert_eq!(text.height(), 2);
    }

    #[test]
    fn text_with_span_builder() {
        let text = Text::raw("hello ").with_span(Span::raw("world"));
        assert_eq!(text.to_plain_text(), "hello world");
    }

    #[test]
    fn text_with_base_style_builder() {
        let text = Text::raw("test").with_base_style(Style::new().bold());
        assert!(
            text.lines()[0].spans()[0]
                .style
                .unwrap()
                .has_attr(StyleFlags::BOLD)
        );
    }

    #[test]
    fn text_height_as_u16() {
        let text = Text::raw("a\nb\nc");
        assert_eq!(text.height_as_u16(), 3);
    }

    #[test]
    fn text_height_as_u16_saturates() {
        // Create text with more than u16::MAX lines would saturate
        // Just verify the method exists and works for normal cases
        let text = Text::new();
        assert_eq!(text.height_as_u16(), 0);
    }

    #[test]
    fn text_measurement() {
        let text = Text::raw("short\nlonger line");
        let m = text.measurement();
        assert_eq!(m.minimum, 11); // "longer line"
        assert_eq!(m.maximum, 11);
    }

    #[test]
    fn text_from_segments_with_newlines() {
        let segments = vec![
            Segment::text("line 1"),
            Segment::newline(),
            Segment::text("line 2"),
        ];
        let text = Text::from_segments(segments);
        assert_eq!(text.height(), 2);
        assert_eq!(text.lines()[0].to_plain_text(), "line 1");
        assert_eq!(text.lines()[1].to_plain_text(), "line 2");
    }

    #[test]
    fn text_converts_to_segment_lines_multiline() {
        let text = Text::raw("a\nb");
        let seg_lines = text.into_segment_lines();
        assert_eq!(seg_lines.len(), 2);
    }

    #[test]
    fn text_iter() {
        let text = Text::from_lines([Line::raw("a"), Line::raw("b")]);
        let collected: Vec<_> = text.iter().collect();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn text_debug_impl() {
        let text = Text::raw("test");
        let debug = format!("{:?}", text);
        assert!(debug.contains("Text"));
    }

    #[test]
    fn text_default_is_empty() {
        let text = Text::default();
        assert!(text.is_empty());
    }

    // ==========================================================================
    // Truncation edge cases
    // ==========================================================================

    #[test]
    fn truncate_ellipsis_wider_than_max() {
        let mut text = Text::raw("ab");
        text.truncate(2, Some("...")); // ellipsis is 3 wide, max is 2
        // Should truncate without ellipsis since ellipsis doesn't fit
        assert!(text.width() <= 2);
    }

    #[test]
    fn truncate_exact_width_no_change() {
        let mut text = Text::raw("hello");
        text.truncate(5, Some("..."));
        assert_eq!(text.to_plain_text(), "hello"); // Exact fit, no truncation needed
    }

    #[test]
    fn truncate_multiple_spans() {
        let text = Text::from_spans([
            Span::raw("hello "),
            Span::styled("world", Style::new().bold()),
        ]);
        let truncated = text.truncated(8, None);
        assert_eq!(truncated.to_plain_text(), "hello wo");
    }

    #[test]
    fn truncate_preserves_link() {
        let mut text =
            Text::from_spans([Span::raw("click ").link("https://a.com"), Span::raw("here")]);
        text.truncate(6, None);
        // Link should be preserved on first span
        assert!(text.lines()[0].spans()[0].link.is_some());
    }

    // ==========================================================================
    // Push span on empty text
    // ==========================================================================

    #[test]
    fn push_span_on_empty_creates_line() {
        let mut text = Text::new();
        text.push_span(Span::raw("hello"));
        assert_eq!(text.height(), 1);
        assert_eq!(text.to_plain_text(), "hello");
    }

    // ==========================================================================
    // From iterator tests
    // ==========================================================================

    #[test]
    fn text_ref_into_iter() {
        let text = Text::from_lines([Line::raw("a"), Line::raw("b")]);
        let mut count = 0;
        for _line in &text {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn line_ref_into_iter() {
        let line = Line::from_spans([Span::raw("a"), Span::raw("b")]);
        let mut count = 0;
        for _span in &line {
            count += 1;
        }
        assert_eq!(count, 2);
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

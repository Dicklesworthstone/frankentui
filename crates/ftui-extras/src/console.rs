#![forbid(unsafe_code)]

//! Console abstraction for ergonomic styled output.
//!
//! The Console provides rich-style output helpers adapted to ftui's constraints:
//! - **Segment-first**: Works with structured text + style (not raw strings)
//! - **One-writer safe**: No ad-hoc terminal writes; all output goes through
//!   explicit sinks (buffers, log sinks, etc.)
//!
//! # Quick Start
//!
//! ```
//! use ftui_extras::console::{Console, ConsoleSink};
//! use ftui_text::Segment;
//! use ftui_style::Style;
//!
//! // Create a console with capture sink for testing
//! let sink = ConsoleSink::capture();
//! let mut console = Console::new(80, sink);
//!
//! // Print styled text
//! console.print(Segment::text("Hello, "));
//! console.print(Segment::styled("world!", Style::new().bold()));
//! console.newline();
//!
//! // Get captured output
//! let output = console.into_captured();
//! assert!(output.contains("Hello, world!"));
//! ```
//!
//! # Style Stack
//!
//! The Console maintains a style stack for nested styling:
//!
//! ```
//! use ftui_extras::console::{Console, ConsoleSink};
//! use ftui_render::cell::PackedRgba;
//! use ftui_text::Segment;
//! use ftui_style::Style;
//!
//! let sink = ConsoleSink::capture();
//! let mut console = Console::new(80, sink);
//!
//! // Push base style (blue foreground)
//! console.push_style(Style::new().fg(PackedRgba::rgb(0, 0, 255)));
//! console.print(Segment::text("Blue text"));
//!
//! // Nested style (inherits blue, adds bold)
//! console.push_style(Style::new().bold());
//! console.print(Segment::text(" + bold"));
//! console.pop_style();
//!
//! console.print(Segment::text(" back to just blue"));
//! console.pop_style();
//! ```
//!
//! # Width-Aware Wrapping
//!
//! Text is automatically wrapped at the configured width:
//!
//! ```
//! use ftui_extras::console::{Console, ConsoleSink, WrapMode};
//! use ftui_text::Segment;
//!
//! let sink = ConsoleSink::capture();
//! let mut console = Console::with_options(40, sink, WrapMode::Word);
//!
//! console.print(Segment::text("This is a long line that will wrap automatically when it exceeds the console width."));
//! ```

use std::borrow::Cow;
use std::io::{self, Write};

use ftui_style::Style;
use ftui_text::Segment;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[cfg(test)]
use ftui_render::cell::PackedRgba;
#[cfg(test)]
use ftui_style::StyleFlags;

// ============================================================================
// Wrap Mode
// ============================================================================

/// Text wrapping mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    /// No wrapping; text may overflow.
    None,
    /// Wrap at word boundaries.
    #[default]
    Word,
    /// Wrap at character boundaries (may break words).
    Character,
}

// ============================================================================
// Console Sink
// ============================================================================

/// Output sink for Console.
///
/// The Console never writes directly to stdout/stderr. Instead, output goes
/// through a sink that can be:
/// - A capture buffer (for testing)
/// - A writer (for output to files, logs, etc.)
/// - A custom implementation
pub enum ConsoleSink {
    /// Capture output in memory (for testing).
    Capture(Vec<CapturedLine>),
    /// Write to an io::Write implementation.
    Writer(Box<dyn Write + Send>),
}

impl ConsoleSink {
    /// Create a capture sink for testing.
    #[must_use]
    pub fn capture() -> Self {
        Self::Capture(Vec::new())
    }

    /// Create a writer sink.
    pub fn writer<W: Write + Send + 'static>(w: W) -> Self {
        Self::Writer(Box::new(w))
    }

    /// Write a line to the sink.
    fn write_line(&mut self, line: &ConsoleBuffer) -> io::Result<()> {
        match self {
            Self::Capture(lines) => {
                lines.push(line.to_captured());
                Ok(())
            }
            Self::Writer(w) => {
                // Write plain text (styles are captured but not rendered)
                for seg in &line.segments {
                    w.write_all(seg.text.as_bytes())?;
                }
                w.write_all(b"\n")?;
                Ok(())
            }
        }
    }

    /// Flush the sink.
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Capture(_) => Ok(()),
            Self::Writer(w) => w.flush(),
        }
    }

    /// Get captured lines (only valid for Capture sink).
    #[must_use]
    pub fn captured(&self) -> Option<&[CapturedLine]> {
        match self {
            Self::Capture(lines) => Some(lines),
            Self::Writer(_) => None,
        }
    }
}

impl std::fmt::Debug for ConsoleSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capture(lines) => f.debug_tuple("Capture").field(&lines.len()).finish(),
            Self::Writer(_) => f.debug_tuple("Writer").finish(),
        }
    }
}

// ============================================================================
// Captured Output
// ============================================================================

/// A captured line of console output.
#[derive(Debug, Clone)]
pub struct CapturedLine {
    /// Segments in this line.
    pub segments: Vec<CapturedSegment>,
}

impl CapturedLine {
    /// Get the plain text of this line.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect()
    }

    /// Get the total display width.
    #[must_use]
    pub fn width(&self) -> usize {
        self.segments.iter().map(|s| s.text.width()).sum()
    }
}

/// A captured segment of styled text.
#[derive(Debug, Clone)]
pub struct CapturedSegment {
    /// The text content.
    pub text: String,
    /// The applied style (merged from style stack).
    pub style: Style,
}

// ============================================================================
// Console Buffer
// ============================================================================

/// Internal line buffer.
#[derive(Debug, Clone, Default)]
struct ConsoleBuffer {
    segments: Vec<BufferSegment>,
    width: usize,
}

#[derive(Debug, Clone)]
struct BufferSegment {
    text: String,
    style: Style,
}

impl ConsoleBuffer {
    fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, text: &str, style: Style) {
        let width = text.width();
        self.segments.push(BufferSegment {
            text: text.to_string(),
            style,
        });
        self.width += width;
    }

    fn clear(&mut self) {
        self.segments.clear();
        self.width = 0;
    }

    fn is_empty(&self) -> bool {
        self.segments.is_empty() || self.segments.iter().all(|s| s.text.is_empty())
    }

    fn to_captured(&self) -> CapturedLine {
        CapturedLine {
            segments: self
                .segments
                .iter()
                .map(|s| CapturedSegment {
                    text: s.text.clone(),
                    style: s.style.clone(),
                })
                .collect(),
        }
    }
}

// ============================================================================
// Console
// ============================================================================

/// Ergonomic console output helper.
///
/// Provides rich-style output adapted to ftui's constraints:
/// - Segment-first (structured text + style)
/// - One-writer safe (no ad-hoc terminal writes)
/// - Width-aware wrapping
/// - Style stack for nested styling
pub struct Console {
    width: usize,
    sink: ConsoleSink,
    wrap_mode: WrapMode,
    style_stack: Vec<Style>,
    current_line: ConsoleBuffer,
    line_count: usize,
}

impl Console {
    /// Create a new console with the specified width and sink.
    #[must_use]
    pub fn new(width: usize, sink: ConsoleSink) -> Self {
        Self::with_options(width, sink, WrapMode::Word)
    }

    /// Create a new console with custom wrap mode.
    #[must_use]
    pub fn with_options(width: usize, sink: ConsoleSink, wrap_mode: WrapMode) -> Self {
        Self {
            width,
            sink,
            wrap_mode,
            style_stack: Vec::new(),
            current_line: ConsoleBuffer::new(),
            line_count: 0,
        }
    }

    /// Get the console width.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Get the number of lines written.
    #[must_use]
    pub const fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the current merged style from the style stack.
    #[must_use]
    pub fn current_style(&self) -> Style {
        let mut merged = Style::default();
        for style in &self.style_stack {
            merged = merged.merge(style);
        }
        merged
    }

    /// Push a style onto the stack.
    ///
    /// The style will be merged with existing styles for all subsequent output
    /// until `pop_style` is called.
    pub fn push_style(&mut self, style: Style) {
        self.style_stack.push(style);
    }

    /// Pop a style from the stack.
    ///
    /// Returns the popped style, or `None` if the stack was empty.
    pub fn pop_style(&mut self) -> Option<Style> {
        self.style_stack.pop()
    }

    /// Clear the style stack.
    pub fn clear_styles(&mut self) {
        self.style_stack.clear();
    }

    /// Print a segment.
    ///
    /// The segment's style is merged with the current style stack.
    pub fn print(&mut self, segment: Segment<'_>) {
        let base_style = self.current_style();
        let style = if let Some(seg_style) = segment.style {
            base_style.merge(&seg_style)
        } else {
            base_style
        };

        let text = segment.text.as_ref();

        // Handle control codes
        if let Some(controls) = &segment.control {
            for control in controls {
                if control.is_newline() {
                    self.newline();
                } else if control.is_cr() {
                    // Carriage return: clear current line
                    self.current_line.clear();
                }
            }
        }

        if text.is_empty() {
            return;
        }

        match self.wrap_mode {
            WrapMode::None => {
                self.current_line.push(text, style);
            }
            WrapMode::Word => {
                self.print_word_wrapped(text, style);
            }
            WrapMode::Character => {
                self.print_char_wrapped(text, style);
            }
        }
    }

    /// Print text with word wrapping.
    fn print_word_wrapped(&mut self, text: &str, style: Style) {
        let mut remaining = text;

        while !remaining.is_empty() {
            let remaining_width = self.width.saturating_sub(self.current_line.width);

            if remaining_width == 0 {
                self.flush_line();
                continue;
            }

            // Find next word boundary
            let (word, rest) = split_next_word(remaining);

            if word.is_empty() {
                // Only whitespace left
                if !rest.is_empty() {
                    self.current_line.push(rest, style.clone());
                }
                break;
            }

            let word_width = word.width();

            if word_width <= remaining_width {
                // Word fits on current line
                self.current_line.push(word, style.clone());
                remaining = rest;
            } else if self.current_line.is_empty() {
                // Word doesn't fit but line is empty - char wrap
                let (fits, _overflow) = split_at_width(word, remaining_width);
                if !fits.is_empty() {
                    self.current_line.push(fits, style.clone());
                    self.flush_line();
                    // Continue with overflow + rest (everything after fits)
                    remaining = &remaining[fits.len()..];
                } else {
                    // First character is too wide to fit - push it anyway to avoid infinite loop
                    let first_grapheme_end = word
                        .grapheme_indices(true)
                        .nth(1)
                        .map(|(i, _)| i)
                        .unwrap_or(word.len());
                    self.current_line
                        .push(&word[..first_grapheme_end], style.clone());
                    self.flush_line();
                    // Continue after the first char (in remaining, not word, to preserve rest)
                    remaining = &remaining[first_grapheme_end..];
                }
            } else {
                // Word doesn't fit - wrap to next line
                self.flush_line();
            }
        }
    }

    /// Print text with character wrapping.
    fn print_char_wrapped(&mut self, text: &str, style: Style) {
        let mut remaining = text;

        while !remaining.is_empty() {
            let remaining_width = self.width.saturating_sub(self.current_line.width);

            if remaining_width == 0 {
                self.flush_line();
                continue;
            }

            let (fits, overflow) = split_at_width(remaining, remaining_width);
            if !fits.is_empty() {
                self.current_line.push(fits, style.clone());
                remaining = overflow;
            } else {
                // First character is too wide to fit - flush and try again
                // If line is empty and still can't fit, push it anyway to avoid infinite loop
                if self.current_line.is_empty() {
                    // Push first char even if too wide
                    let first_grapheme_end = remaining
                        .grapheme_indices(true)
                        .nth(1)
                        .map(|(i, _)| i)
                        .unwrap_or(remaining.len());
                    self.current_line
                        .push(&remaining[..first_grapheme_end], style.clone());
                    remaining = &remaining[first_grapheme_end..];
                }
                self.flush_line();
            }
        }
    }

    /// Print styled text.
    ///
    /// Convenience method that creates a segment and prints it.
    pub fn print_styled(&mut self, text: impl Into<Cow<'static, str>>, style: Style) {
        self.print(Segment::styled(text, style));
    }

    /// Print plain text.
    ///
    /// Convenience method that creates a segment and prints it.
    pub fn print_text(&mut self, text: impl Into<Cow<'static, str>>) {
        self.print(Segment::text(text));
    }

    /// Print a newline.
    pub fn newline(&mut self) {
        self.flush_line();
    }

    /// Print text followed by a newline.
    pub fn println(&mut self, segment: Segment<'_>) {
        self.print(segment);
        self.newline();
    }

    /// Print styled text followed by a newline.
    pub fn println_styled(&mut self, text: impl Into<Cow<'static, str>>, style: Style) {
        self.print_styled(text, style);
        self.newline();
    }

    /// Print plain text followed by a newline.
    pub fn println_text(&mut self, text: impl Into<Cow<'static, str>>) {
        self.print_text(text);
        self.newline();
    }

    /// Print a blank line.
    pub fn blank_line(&mut self) {
        self.flush_line();
        let _ = self.sink.write_line(&ConsoleBuffer::new());
        self.line_count += 1;
    }

    /// Print a horizontal rule.
    pub fn rule(&mut self, char: char) {
        self.flush_line();
        let rule_text: String = std::iter::repeat(char).take(self.width).collect();
        self.current_line.push(&rule_text, self.current_style());
        self.flush_line();
    }

    /// Flush the current line to the sink.
    fn flush_line(&mut self) {
        if !self.current_line.is_empty() {
            let _ = self.sink.write_line(&self.current_line);
            self.line_count += 1;
        }
        self.current_line.clear();
    }

    /// Flush any remaining output.
    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_line();
        self.sink.flush()
    }

    /// Consume the console and return captured output (if using capture sink).
    #[must_use]
    pub fn into_captured(mut self) -> String {
        self.flush_line();
        match self.sink {
            ConsoleSink::Capture(lines) => lines
                .iter()
                .map(CapturedLine::plain_text)
                .collect::<Vec<_>>()
                .join("\n"),
            ConsoleSink::Writer(_) => String::new(),
        }
    }

    /// Consume the console and return captured lines (if using capture sink).
    #[must_use]
    pub fn into_captured_lines(mut self) -> Vec<CapturedLine> {
        self.flush_line();
        match self.sink {
            ConsoleSink::Capture(lines) => lines,
            ConsoleSink::Writer(_) => Vec::new(),
        }
    }

    /// Get access to the sink.
    #[must_use]
    pub fn sink(&self) -> &ConsoleSink {
        &self.sink
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Split text at the next word boundary.
///
/// Returns (word_including_trailing_space, rest).
fn split_next_word(text: &str) -> (&str, &str) {
    // Skip leading whitespace
    let start = text
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    if start == text.len() {
        return (text, "");
    }

    // Find end of word
    let end = text[start..]
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| start + i)
        .unwrap_or(text.len());

    // Include one trailing space if present
    let end_with_space = if end < text.len() && text[end..].starts_with(' ') {
        end + 1
    } else {
        end
    };

    (&text[..end_with_space], &text[end_with_space..])
}

/// Split text at approximately the given display width.
///
/// Returns (fits, overflow) where `fits.width() <= max_width`.
fn split_at_width(text: &str, max_width: usize) -> (&str, &str) {
    if text.width() <= max_width {
        return (text, "");
    }

    let mut width = 0;
    let mut split_idx = 0;

    for grapheme in text.graphemes(true) {
        let g_width = grapheme.width();
        if width + g_width > max_width {
            break;
        }
        width += g_width;
        split_idx += grapheme.len();
    }

    (&text[..split_idx], &text[split_idx..])
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const RED: PackedRgba = PackedRgba::rgb(255, 0, 0);
    const BLUE: PackedRgba = PackedRgba::rgb(0, 0, 255);

    #[test]
    fn console_basic_output() {
        let sink = ConsoleSink::capture();
        let mut console = Console::new(80, sink);

        console.print_text("Hello, world!");
        console.newline();

        let output = console.into_captured();
        assert_eq!(output, "Hello, world!");
    }

    #[test]
    fn console_styled_output() {
        let sink = ConsoleSink::capture();
        let mut console = Console::new(80, sink);

        console.print(Segment::styled("Bold", Style::new().bold()));
        console.newline();

        let lines = console.into_captured_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].segments[0].text, "Bold");
        assert!(lines[0].segments[0].style.has_attr(StyleFlags::BOLD));
    }

    #[test]
    fn console_style_stack() {
        let sink = ConsoleSink::capture();
        // Use WrapMode::None to avoid word-splitting segments
        let mut console = Console::with_options(80, sink, WrapMode::None);

        console.push_style(Style::new().fg(RED));
        console.print_text("Red");
        console.push_style(Style::new().bold());
        console.print_text("Bold");
        console.pop_style();
        console.print_text("Red");
        console.newline();

        let lines = console.into_captured_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].segments.len(), 3);

        // First segment: red
        assert_eq!(lines[0].segments[0].style.fg, Some(RED));
        assert!(!lines[0].segments[0].style.has_attr(StyleFlags::BOLD));

        // Second segment: red + bold
        assert_eq!(lines[0].segments[1].style.fg, Some(RED));
        assert!(lines[0].segments[1].style.has_attr(StyleFlags::BOLD));

        // Third segment: red only
        assert_eq!(lines[0].segments[2].style.fg, Some(RED));
        assert!(!lines[0].segments[2].style.has_attr(StyleFlags::BOLD));
    }

    #[test]
    fn console_word_wrap() {
        let sink = ConsoleSink::capture();
        let mut console = Console::with_options(20, sink, WrapMode::Word);

        console.print_text("This is a test of word wrapping in the console.");
        console.flush().unwrap();

        let lines = console.into_captured_lines();
        assert!(lines.len() > 1);

        // Each line should be <= 20 chars wide
        for line in &lines {
            assert!(line.width() <= 20, "Line too wide: {:?}", line.plain_text());
        }
    }

    #[test]
    fn console_word_wrap_long_word_with_rest() {
        // Regression test: long word followed by more text should not lose the rest
        let sink = ConsoleSink::capture();
        let mut console = Console::with_options(10, sink, WrapMode::Word);

        console.print_text("superlongword more text");
        console.flush().unwrap();

        let lines = console.into_captured_lines();
        let all_text: String = lines
            .iter()
            .map(|l| l.plain_text())
            .collect::<Vec<_>>()
            .join("");
        // Verify no text was lost
        assert_eq!(all_text.replace(" ", ""), "superlongwordmoretext");

        // Verify all lines fit within width
        for line in &lines {
            assert!(line.width() <= 10, "Line too wide: {:?}", line.plain_text());
        }
    }

    #[test]
    fn console_word_wrap_wide_char_boundary() {
        // Test word wrap with wide characters that need char-wrapping
        let sink = ConsoleSink::capture();
        let mut console = Console::with_options(3, sink, WrapMode::Word);

        console.print_text("中文 test");
        console.flush().unwrap();

        let lines = console.into_captured_lines();
        let all_text: String = lines
            .iter()
            .map(|l| l.plain_text())
            .collect::<Vec<_>>()
            .join("");
        // Verify no text was lost (including space handling)
        assert!(all_text.contains("中") && all_text.contains("文") && all_text.contains("test"));
    }

    #[test]
    fn console_char_wrap() {
        let sink = ConsoleSink::capture();
        let mut console = Console::with_options(10, sink, WrapMode::Character);

        console.print_text("HelloWorld123456");
        console.flush().unwrap();

        let lines = console.into_captured_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].plain_text(), "HelloWorld");
        assert_eq!(lines[1].plain_text(), "123456");
    }

    #[test]
    fn console_no_wrap() {
        let sink = ConsoleSink::capture();
        let mut console = Console::with_options(10, sink, WrapMode::None);

        console.print_text("This text is longer than 10 chars");
        console.newline();

        let lines = console.into_captured_lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].width() > 10);
    }

    #[test]
    fn console_rule() {
        let sink = ConsoleSink::capture();
        let mut console = Console::new(10, sink);

        console.rule('-');

        let lines = console.into_captured_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].plain_text(), "----------");
    }

    #[test]
    fn console_blank_line() {
        let sink = ConsoleSink::capture();
        let mut console = Console::new(80, sink);

        console.print_text("Before");
        console.newline();
        console.blank_line();
        console.print_text("After");
        console.newline();

        let output = console.into_captured();
        assert_eq!(output, "Before\n\nAfter");
    }

    #[test]
    fn console_line_count() {
        let sink = ConsoleSink::capture();
        let mut console = Console::new(80, sink);

        assert_eq!(console.line_count(), 0);

        console.println_text("Line 1");
        assert_eq!(console.line_count(), 1);

        console.println_text("Line 2");
        assert_eq!(console.line_count(), 2);
    }

    #[test]
    fn split_next_word_basic() {
        assert_eq!(split_next_word("hello world"), ("hello ", "world"));
        assert_eq!(split_next_word("hello"), ("hello", ""));
        assert_eq!(split_next_word("  hello"), ("  hello", ""));
        assert_eq!(split_next_word(""), ("", ""));
    }

    #[test]
    fn split_at_width_basic() {
        assert_eq!(split_at_width("hello", 10), ("hello", ""));
        assert_eq!(split_at_width("hello", 3), ("hel", "lo"));
        assert_eq!(split_at_width("hello", 0), ("", "hello"));
    }

    #[test]
    fn split_at_width_wide_chars() {
        // Wide char '中' has width 2
        assert_eq!(split_at_width("中文", 2), ("中", "文"));
        assert_eq!(split_at_width("中文", 1), ("", "中文")); // Can't fit
        assert_eq!(split_at_width("中文", 4), ("中文", ""));
    }

    #[test]
    fn console_wide_char_wrap() {
        let sink = ConsoleSink::capture();
        let mut console = Console::with_options(5, sink, WrapMode::Character);

        console.print_text("中文测试");
        console.flush().unwrap();

        let lines = console.into_captured_lines();
        // Each wide char is 2 cells, so "中文" = 4, "测试" = 4
        // Width 5 means "中文" fits (4 <= 5), then "测" fits (2 <= 5)
        assert_eq!(lines[0].plain_text(), "中文");
        assert_eq!(lines[1].plain_text(), "测试");
    }

    #[test]
    fn console_segment_with_newline() {
        let sink = ConsoleSink::capture();
        let mut console = Console::new(80, sink);

        console.print(Segment::text("Line 1"));
        console.print(Segment::newline());
        console.print(Segment::text("Line 2"));
        console.newline();

        let output = console.into_captured();
        assert_eq!(output, "Line 1\nLine 2");
    }

    #[test]
    fn console_clear_styles() {
        let sink = ConsoleSink::capture();
        let mut console = Console::new(80, sink);

        console.push_style(Style::new().bold());
        console.push_style(Style::new().italic());
        assert_eq!(console.style_stack.len(), 2);

        console.clear_styles();
        assert_eq!(console.style_stack.len(), 0);
    }

    #[test]
    fn console_current_style_merges() {
        let sink = ConsoleSink::capture();
        let mut console = Console::new(80, sink);

        console.push_style(Style::new().fg(RED));
        console.push_style(Style::new().bg(BLUE).bold());

        let current = console.current_style();
        assert_eq!(current.fg, Some(RED));
        assert_eq!(current.bg, Some(BLUE));
        assert!(current.has_attr(StyleFlags::BOLD));
    }
}

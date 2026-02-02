#![forbid(unsafe_code)]

//! Traceback renderable for displaying error stacks.
//!
//! Renders formatted error tracebacks with optional source context,
//! syntax highlighting, and line numbers.
//!
//! # Example
//! ```ignore
//! use ftui_extras::traceback::{Traceback, TracebackFrame};
//!
//! let traceback = Traceback::new(
//!     vec![
//!         TracebackFrame::new("main", 42)
//!             .filename("src/main.rs")
//!             .source_context("fn main() {\n    run();\n}", 41),
//!     ],
//!     "PanicError",
//!     "something went wrong",
//! );
//! ```

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_style::Style;

/// A single traceback frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracebackFrame {
    /// Source filename (optional).
    pub filename: Option<String>,
    /// Function or scope name.
    pub name: String,
    /// Line number (1-indexed) of the error within the source.
    pub line: usize,
    /// Optional source code snippet for this frame.
    pub source_context: Option<String>,
    /// Line number of the first line in `source_context`.
    pub source_first_line: usize,
}

impl TracebackFrame {
    /// Create a new frame with a function name and line number.
    #[must_use]
    pub fn new(name: impl Into<String>, line: usize) -> Self {
        Self {
            filename: None,
            name: name.into(),
            line,
            source_context: None,
            source_first_line: 1,
        }
    }

    /// Set the source filename.
    #[must_use]
    pub fn filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    /// Provide source context lines directly.
    ///
    /// # Arguments
    /// * `source` - Source code snippet (may contain multiple lines)
    /// * `first_line` - Line number of the first line in the snippet
    #[must_use]
    pub fn source_context(mut self, source: impl Into<String>, first_line: usize) -> Self {
        self.source_context = Some(source.into());
        self.source_first_line = first_line.max(1);
        self
    }
}

/// Style configuration for traceback rendering.
#[derive(Debug, Clone)]
pub struct TracebackStyle {
    /// Style for the title line.
    pub title: Style,
    /// Style for the border.
    pub border: Style,
    /// Style for filename text.
    pub filename: Style,
    /// Style for function name.
    pub function: Style,
    /// Style for line numbers.
    pub lineno: Style,
    /// Style for the error indicator arrow.
    pub indicator: Style,
    /// Style for source code (non-error lines).
    pub source: Style,
    /// Style for the error line in source context.
    pub error_line: Style,
    /// Style for exception type.
    pub exception_type: Style,
    /// Style for exception message.
    pub exception_message: Style,
}

impl Default for TracebackStyle {
    fn default() -> Self {
        Self {
            title: Style::new().fg(PackedRgba::rgb(255, 100, 100)).bold(),
            border: Style::new().fg(PackedRgba::rgb(255, 100, 100)),
            filename: Style::new().fg(PackedRgba::rgb(100, 200, 255)),
            function: Style::new().fg(PackedRgba::rgb(100, 255, 100)),
            lineno: Style::new().fg(PackedRgba::rgb(200, 200, 100)).dim(),
            indicator: Style::new().fg(PackedRgba::rgb(255, 80, 80)).bold(),
            source: Style::new().fg(PackedRgba::rgb(180, 180, 180)),
            error_line: Style::new().fg(PackedRgba::rgb(255, 255, 255)).bold(),
            exception_type: Style::new().fg(PackedRgba::rgb(255, 80, 80)).bold(),
            exception_message: Style::new().fg(PackedRgba::rgb(255, 200, 200)),
        }
    }
}

/// A traceback renderable for displaying error stacks.
#[derive(Debug, Clone)]
pub struct Traceback {
    frames: Vec<TracebackFrame>,
    exception_type: String,
    exception_message: String,
    title: String,
    style: TracebackStyle,
}

impl Traceback {
    /// Create a new traceback.
    #[must_use]
    pub fn new(
        frames: impl Into<Vec<TracebackFrame>>,
        exception_type: impl Into<String>,
        exception_message: impl Into<String>,
    ) -> Self {
        Self {
            frames: frames.into(),
            exception_type: exception_type.into(),
            exception_message: exception_message.into(),
            title: "Traceback (most recent call last)".to_string(),
            style: TracebackStyle::default(),
        }
    }

    /// Override the title.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Override the style.
    #[must_use]
    pub fn style(mut self, style: TracebackStyle) -> Self {
        self.style = style;
        self
    }

    /// Push a frame.
    pub fn push_frame(&mut self, frame: TracebackFrame) {
        self.frames.push(frame);
    }

    /// Access frames.
    #[must_use]
    pub fn frames(&self) -> &[TracebackFrame] {
        &self.frames
    }

    /// Access exception type.
    #[must_use]
    pub fn exception_type(&self) -> &str {
        &self.exception_type
    }

    /// Access exception message.
    #[must_use]
    pub fn exception_message(&self) -> &str {
        &self.exception_message
    }

    /// Compute the number of lines needed to render this traceback.
    #[must_use]
    pub fn line_count(&self) -> usize {
        let mut count = 0;
        // Title line (border top)
        count += 1;
        // Each frame
        for frame in &self.frames {
            // Location line: "  File "filename", line N, in function"
            count += 1;
            // Source context lines
            if let Some(ref ctx) = frame.source_context {
                count += ctx.lines().count();
            }
        }
        // Exception line
        count += 1;
        count
    }

    /// Render the traceback into a frame.
    pub fn render(&self, area: Rect, frame: &mut Frame) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let width = area.width as usize;
        let mut y = area.y;
        let max_y = area.y.saturating_add(area.height);

        // Title line
        if y < max_y {
            let title_line = format!("── {} ──", self.title);
            draw_line(
                &mut frame.buffer,
                area.x,
                y,
                &title_line,
                self.style.title,
                width,
            );
            y += 1;
        }

        // Frames
        for f in &self.frames {
            if y >= max_y {
                break;
            }

            // Location line
            let location = format_location(f);
            draw_line(
                &mut frame.buffer,
                area.x,
                y,
                &location,
                self.style.filename,
                width,
            );
            y += 1;

            // Source context
            if let Some(ref ctx) = f.source_context {
                let lineno_width = lineno_column_width(f);
                for (i, line) in ctx.lines().enumerate() {
                    if y >= max_y {
                        break;
                    }
                    let current_lineno = f.source_first_line + i;
                    let is_error_line = current_lineno == f.line;

                    let indicator = if is_error_line { "❱" } else { " " };
                    let formatted = format!(
                        " {indicator} {lineno:>w$} │ {line}",
                        indicator = indicator,
                        lineno = current_lineno,
                        w = lineno_width,
                        line = line,
                    );

                    let line_style = if is_error_line {
                        self.style.error_line
                    } else {
                        self.style.source
                    };

                    draw_line(&mut frame.buffer, area.x, y, &formatted, line_style, width);

                    // Draw indicator in its own style if error line
                    if is_error_line {
                        draw_styled_char(
                            &mut frame.buffer,
                            area.x + 1,
                            y,
                            '❱',
                            self.style.indicator,
                        );
                    }

                    y += 1;
                }
            }
        }

        // Exception line
        if y < max_y {
            let exception = format!("{}: {}", self.exception_type, self.exception_message);
            // Draw type in exception_type style, message in exception_message style
            let type_end =
                unicode_width::UnicodeWidthStr::width(self.exception_type.as_str()).min(width);
            draw_line(
                &mut frame.buffer,
                area.x,
                y,
                &exception,
                self.style.exception_message,
                width,
            );
            // Overlay the type portion with exception_type style
            draw_line_partial(
                &mut frame.buffer,
                area.x,
                y,
                &self.exception_type,
                self.style.exception_type,
                type_end,
            );
        }
    }
}

/// Format the location line for a frame.
fn format_location(frame: &TracebackFrame) -> String {
    match &frame.filename {
        Some(filename) => format!(
            "  File \"{}\", line {}, in {}",
            filename, frame.line, frame.name
        ),
        None => format!("  line {}, in {}", frame.line, frame.name),
    }
}

/// Compute the width of the line number column for a frame's source context.
fn lineno_column_width(frame: &TracebackFrame) -> usize {
    if let Some(ref ctx) = frame.source_context {
        let last_line = frame.source_first_line + ctx.lines().count().saturating_sub(1);
        digit_count(last_line)
    } else {
        1
    }
}

/// Count digits in a number.
fn digit_count(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut count = 0;
    let mut v = n;
    while v > 0 {
        count += 1;
        v /= 10;
    }
    count
}

/// Draw a single line of text into the buffer, truncating and padding to width.
fn draw_line(buffer: &mut Buffer, x: u16, y: u16, text: &str, style: Style, width: usize) {
    let mut col = 0;
    for ch in text.chars() {
        if col >= width {
            break;
        }
        let cell_x = x.saturating_add(col as u16);
        let mut cell = Cell::from_char(ch);
        apply_style(&mut cell, style);
        buffer.set(cell_x, y, cell);
        col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
    }
    // Fill remaining with spaces
    while col < width {
        let cell_x = x.saturating_add(col as u16);
        let mut cell = Cell::from_char(' ');
        apply_style(&mut cell, style);
        buffer.set(cell_x, y, cell);
        col += 1;
    }
}

/// Draw a partial line (for overlaying styled substrings).
fn draw_line_partial(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    text: &str,
    style: Style,
    max_col: usize,
) {
    let mut col = 0;
    for ch in text.chars() {
        if col >= max_col {
            break;
        }
        let cell_x = x.saturating_add(col as u16);
        let mut cell = Cell::from_char(ch);
        apply_style(&mut cell, style);
        buffer.set(cell_x, y, cell);
        col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
    }
}

/// Draw a single styled character.
fn draw_styled_char(buffer: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    let mut cell = Cell::from_char(ch);
    apply_style(&mut cell, style);
    buffer.set(x, y, cell);
}

/// Apply a style to a cell.
fn apply_style(cell: &mut Cell, style: Style) {
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg {
        cell.bg = bg;
    }
    if let Some(attrs) = style.attrs {
        let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
        cell.attrs = cell.attrs.with_flags(cell_flags);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn traceback_new() {
        let tb = Traceback::new(Vec::new(), "Error", "something failed");
        assert_eq!(tb.exception_type(), "Error");
        assert_eq!(tb.exception_message(), "something failed");
        assert!(tb.frames().is_empty());
    }

    #[test]
    fn traceback_with_frames() {
        let tb = Traceback::new(
            vec![
                TracebackFrame::new("main", 10).filename("src/main.rs"),
                TracebackFrame::new("run", 25).filename("src/lib.rs"),
            ],
            "PanicError",
            "oops",
        );
        assert_eq!(tb.frames().len(), 2);
        assert_eq!(tb.frames()[0].name, "main");
        assert_eq!(tb.frames()[1].name, "run");
    }

    #[test]
    fn traceback_push_frame() {
        let mut tb = Traceback::new(Vec::new(), "Error", "msg");
        tb.push_frame(TracebackFrame::new("foo", 1));
        assert_eq!(tb.frames().len(), 1);
    }

    #[test]
    fn traceback_title() {
        let tb = Traceback::new(Vec::new(), "Error", "msg").title("Custom Title");
        assert_eq!(tb.title, "Custom Title");
    }

    #[test]
    fn frame_builder() {
        let f = TracebackFrame::new("test_fn", 42)
            .filename("test.rs")
            .source_context("line1\nline2\nline3", 40);
        assert_eq!(f.name, "test_fn");
        assert_eq!(f.line, 42);
        assert_eq!(f.filename.as_deref(), Some("test.rs"));
        assert_eq!(f.source_first_line, 40);
        assert!(f.source_context.is_some());
    }

    #[test]
    fn frame_source_context_first_line_min() {
        let f = TracebackFrame::new("f", 1).source_context("x", 0);
        assert_eq!(f.source_first_line, 1); // clamped to 1
    }

    #[test]
    fn format_location_with_filename() {
        let f = TracebackFrame::new("main", 42).filename("src/main.rs");
        let loc = format_location(&f);
        assert_eq!(loc, "  File \"src/main.rs\", line 42, in main");
    }

    #[test]
    fn format_location_without_filename() {
        let f = TracebackFrame::new("anon", 7);
        let loc = format_location(&f);
        assert_eq!(loc, "  line 7, in anon");
    }

    #[test]
    fn digit_count_works() {
        assert_eq!(digit_count(0), 1);
        assert_eq!(digit_count(1), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(99), 2);
        assert_eq!(digit_count(100), 3);
        assert_eq!(digit_count(999), 3);
        assert_eq!(digit_count(1000), 4);
    }

    #[test]
    fn lineno_column_width_single_line() {
        let f = TracebackFrame::new("f", 5).source_context("hello", 5);
        assert_eq!(lineno_column_width(&f), 1);
    }

    #[test]
    fn lineno_column_width_multi_line() {
        let f = TracebackFrame::new("f", 100).source_context("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk", 95);
        // Lines 95..105 => last is 105 => 3 digits
        assert_eq!(lineno_column_width(&f), 3);
    }

    #[test]
    fn lineno_column_width_no_context() {
        let f = TracebackFrame::new("f", 5);
        assert_eq!(lineno_column_width(&f), 1);
    }

    #[test]
    fn line_count_empty() {
        let tb = Traceback::new(Vec::new(), "E", "m");
        // title + exception = 2
        assert_eq!(tb.line_count(), 2);
    }

    #[test]
    fn line_count_with_frame() {
        let tb = Traceback::new(vec![TracebackFrame::new("f", 1)], "E", "m");
        // title(1) + location(1) + exception(1) = 3
        assert_eq!(tb.line_count(), 3);
    }

    #[test]
    fn line_count_with_source() {
        let tb = Traceback::new(
            vec![TracebackFrame::new("f", 2).source_context("a\nb\nc", 1)],
            "E",
            "m",
        );
        // title(1) + location(1) + 3 source lines + exception(1) = 6
        assert_eq!(tb.line_count(), 6);
    }

    #[test]
    fn render_zero_area() {
        let tb = Traceback::new(Vec::new(), "E", "m");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        tb.render(Rect::new(0, 0, 0, 0), &mut frame);
        // Should not panic
    }

    #[test]
    fn render_basic() {
        let tb = Traceback::new(
            vec![TracebackFrame::new("main", 5).filename("src/main.rs")],
            "PanicError",
            "test failure",
        );
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 10, &mut pool);
        tb.render(Rect::new(0, 0, 60, 10), &mut frame);

        // Verify title line contains title text
        let title_text = read_line(&frame.buffer, 0, 60);
        assert!(
            title_text.contains("Traceback"),
            "Title should contain 'Traceback', got: {title_text}"
        );

        // Verify location line
        let loc_text = read_line(&frame.buffer, 1, 60);
        assert!(
            loc_text.contains("src/main.rs"),
            "Location should contain filename, got: {loc_text}"
        );
        assert!(
            loc_text.contains("main"),
            "Location should contain function name"
        );

        // Verify exception line
        let exc_text = read_line(&frame.buffer, 2, 60);
        assert!(
            exc_text.contains("PanicError"),
            "Exception line should contain type, got: {exc_text}"
        );
        assert!(
            exc_text.contains("test failure"),
            "Exception line should contain message"
        );
    }

    #[test]
    fn render_with_source_context() {
        let tb = Traceback::new(
            vec![
                TracebackFrame::new("run", 3)
                    .filename("lib.rs")
                    .source_context("fn run() {\n    let x = 1;\n    panic!(\"oops\");\n}", 1),
            ],
            "PanicError",
            "oops",
        );
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 20, &mut pool);
        tb.render(Rect::new(0, 0, 60, 20), &mut frame);

        // Line 0: title
        let title = read_line(&frame.buffer, 0, 60);
        assert!(title.contains("Traceback"));

        // Line 1: location
        let loc = read_line(&frame.buffer, 1, 60);
        assert!(loc.contains("lib.rs"));

        // Lines 2-5: source context (4 lines)
        let line2 = read_line(&frame.buffer, 2, 60);
        assert!(line2.contains("fn run()"), "Source line 1: {line2}");

        let line4 = read_line(&frame.buffer, 4, 60);
        assert!(
            line4.contains("panic!"),
            "Error line should contain panic: {line4}"
        );
        assert!(
            line4.contains("❱"),
            "Error line should have indicator: {line4}"
        );

        // Exception line
        let exc = read_line(&frame.buffer, 6, 60);
        assert!(exc.contains("PanicError"));
    }

    #[test]
    fn render_truncated_height() {
        let tb = Traceback::new(
            vec![
                TracebackFrame::new("a", 1).source_context("line1\nline2\nline3", 1),
                TracebackFrame::new("b", 1).source_context("line4\nline5", 1),
            ],
            "Error",
            "msg",
        );
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 4, &mut pool);
        // Only 4 rows available - should render partially without panic
        tb.render(Rect::new(0, 0, 40, 4), &mut frame);
    }

    #[test]
    fn render_narrow_width() {
        let tb = Traceback::new(
            vec![
                TracebackFrame::new("function_with_long_name", 100)
                    .filename("very/long/path/to/source/file.rs"),
            ],
            "LongExceptionTypeName",
            "a very long error message that should be truncated",
        );
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        tb.render(Rect::new(0, 0, 20, 10), &mut frame);
        // Should not panic, content just gets truncated
    }

    #[test]
    fn default_style_is_readable() {
        let style = TracebackStyle::default();
        // Just verify non-default styles are set
        assert_ne!(style.title.fg, None);
        assert_ne!(style.exception_type.fg, None);
        assert_ne!(style.filename.fg, None);
    }

    /// Helper: read a line from the buffer as text.
    fn read_line(buffer: &Buffer, y: u16, width: u16) -> String {
        let mut s = String::new();
        for x in 0..width {
            if let Some(cell) = buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                s.push(ch);
            }
        }
        s
    }
}

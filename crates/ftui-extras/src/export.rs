#![forbid(unsafe_code)]

//! Export adapters for converting terminal buffers to external formats.
//!
//! This module provides exporters that convert an [`ftui_render::buffer::Buffer`]
//! and its companion [`ftui_render::grapheme_pool::GraphemePool`] into HTML, SVG,
//! or plain text.
//!
//! # Feature Gate
//!
//! Enabled via `export` feature in `ftui-extras`.
//!
//! # Supported Formats
//!
//! - [`HtmlExporter`]: Generates `<pre>` blocks with `<span>` elements and
//!   inline CSS styles (or CSS classes).
//! - [`SvgExporter`]: Generates an SVG document with positioned `<text>` elements.
//! - [`TextExporter`]: Generates plain text, optionally including ANSI escape codes.
//!
//! # Usage
//!
//! ```no_run
//! use ftui_extras::export::{HtmlExporter, TextExporter};
//! use ftui_render::buffer::Buffer;
//! use ftui_render::grapheme_pool::GraphemePool;
//!
//! let buffer = Buffer::new(80, 24);
//! let pool = GraphemePool::new();
//!
//! let html = HtmlExporter::default().export(&buffer, &pool);
//! let text = TextExporter::plain().export(&buffer, &pool);
//! ```

use std::fmt::Write;

use ftui_render::buffer::Buffer;
use ftui_render::cell::{CellAttrs, CellContent, PackedRgba, StyleFlags};
use ftui_render::grapheme_pool::GraphemePool;

// ---------------------------------------------------------------------------
// HTML Exporter
// ---------------------------------------------------------------------------

/// Configuration for HTML export.
#[derive(Debug, Clone)]
pub struct HtmlExporter {
    /// CSS class prefix for generated elements.
    pub class_prefix: String,
    /// Font family for the `<pre>` wrapper.
    pub font_family: String,
    /// Font size (CSS value).
    pub font_size: String,
    /// Whether to use inline styles (true) or CSS classes (false).
    pub inline_styles: bool,
}

impl Default for HtmlExporter {
    fn default() -> Self {
        Self {
            class_prefix: "ftui".into(),
            font_family: "monospace".into(),
            font_size: "14px".into(),
            inline_styles: true,
        }
    }
}

impl HtmlExporter {
    /// Export a buffer to an HTML string.
    ///
    /// Each cell's foreground color, background color, and style flags are
    /// converted to inline CSS or class attributes on `<span>` elements.
    /// Continuation cells (wide character tails) are skipped.
    pub fn export(&self, buffer: &Buffer, pool: &GraphemePool) -> String {
        let mut out = String::with_capacity(buffer.len() * 20);

        write!(
            out,
            "<pre class=\"{}\" style=\"font-family:{};font-size:{};line-height:1.2;\">",
            self.class_prefix, self.font_family, self.font_size,
        )
        .unwrap();

        for y in 0..buffer.height() {
            if y > 0 {
                out.push('\n');
            }
            for x in 0..buffer.width() {
                let cell = buffer.get(x, y).unwrap();

                // Skip continuation cells (part of a wide character).
                if cell.is_continuation() {
                    continue;
                }

                let content = cell_content_str(cell.content, pool);
                if content.is_empty() {
                    out.push(' ');
                    continue;
                }

                let has_style = cell.fg != PackedRgba::WHITE
                    || cell.bg != PackedRgba::TRANSPARENT
                    || cell.attrs != CellAttrs::NONE;

                if has_style {
                    out.push_str("<span style=\"");
                    self.write_inline_style(&mut out, cell.fg, cell.bg, cell.attrs);
                    out.push_str("\">");
                }

                html_escape_into(&mut out, &content);

                if has_style {
                    out.push_str("</span>");
                }
            }
        }

        out.push_str("</pre>");
        out
    }

    fn write_inline_style(
        &self,
        out: &mut String,
        fg: PackedRgba,
        bg: PackedRgba,
        attrs: CellAttrs,
    ) {
        if fg != PackedRgba::WHITE {
            write!(out, "color:#{:02x}{:02x}{:02x};", fg.r(), fg.g(), fg.b()).unwrap();
        }
        if bg != PackedRgba::TRANSPARENT && bg.a() > 0 {
            write!(
                out,
                "background:#{:02x}{:02x}{:02x};",
                bg.r(),
                bg.g(),
                bg.b()
            )
            .unwrap();
        }

        let flags = attrs.flags();
        if flags.contains(StyleFlags::BOLD) {
            out.push_str("font-weight:bold;");
        }
        if flags.contains(StyleFlags::DIM) {
            out.push_str("opacity:0.5;");
        }
        if flags.contains(StyleFlags::ITALIC) {
            out.push_str("font-style:italic;");
        }

        let mut decorations = Vec::new();
        if flags.contains(StyleFlags::UNDERLINE) {
            decorations.push("underline");
        }
        if flags.contains(StyleFlags::STRIKETHROUGH) {
            decorations.push("line-through");
        }
        if !decorations.is_empty() {
            write!(out, "text-decoration:{};", decorations.join(" ")).unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// SVG Exporter
// ---------------------------------------------------------------------------

/// Configuration for SVG export.
#[derive(Debug, Clone)]
pub struct SvgExporter {
    /// Width of a single cell in pixels.
    pub cell_width: f32,
    /// Height of a single cell in pixels.
    pub cell_height: f32,
    /// Font size in pixels.
    pub font_size: f32,
    /// Font family.
    pub font_family: String,
    /// Background color for the SVG.
    pub background: PackedRgba,
}

impl Default for SvgExporter {
    fn default() -> Self {
        Self {
            cell_width: 8.4,
            cell_height: 17.0,
            font_size: 14.0,
            font_family: "monospace".into(),
            background: PackedRgba::BLACK,
        }
    }
}

impl SvgExporter {
    /// Export a buffer to an SVG string.
    ///
    /// Each cell becomes a `<text>` element positioned at the correct
    /// (x, y) coordinate. Adjacent cells with identical styles are merged
    /// into single `<text>` elements for compactness.
    pub fn export(&self, buffer: &Buffer, pool: &GraphemePool) -> String {
        let svg_width = f32::from(buffer.width()) * self.cell_width;
        let svg_height = f32::from(buffer.height()) * self.cell_height;

        let mut out = String::with_capacity(buffer.len() * 40);

        write!(
            out,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" \
             width=\"{svg_width}\" height=\"{svg_height}\" \
             viewBox=\"0 0 {svg_width} {svg_height}\">"
        )
        .unwrap();

        // Background rectangle.
        if self.background.a() > 0 {
            write!(
                out,
                "<rect width=\"100%\" height=\"100%\" fill=\"#{:02x}{:02x}{:02x}\"/>",
                self.background.r(),
                self.background.g(),
                self.background.b(),
            )
            .unwrap();
        }

        write!(
            out,
            "<g font-family=\"{}\" font-size=\"{}\">",
            self.font_family, self.font_size,
        )
        .unwrap();

        for y in 0..buffer.height() {
            for x in 0..buffer.width() {
                let cell = buffer.get(x, y).unwrap();

                if cell.is_continuation() || cell.is_empty() {
                    continue;
                }

                let content = cell_content_str(cell.content, pool);
                if content.is_empty() {
                    continue;
                }

                // Cell background (only if non-transparent).
                if cell.bg != PackedRgba::TRANSPARENT && cell.bg.a() > 0 {
                    let bx = f32::from(x) * self.cell_width;
                    let by = f32::from(y) * self.cell_height;
                    let bw = self.cell_width * content.len().max(1) as f32;
                    write!(
                        out,
                        "<rect x=\"{bx}\" y=\"{by}\" width=\"{bw}\" height=\"{}\" \
                         fill=\"#{:02x}{:02x}{:02x}\"/>",
                        self.cell_height,
                        cell.bg.r(),
                        cell.bg.g(),
                        cell.bg.b(),
                    )
                    .unwrap();
                }

                let tx = f32::from(x) * self.cell_width;
                let ty = f32::from(y) * self.cell_height + self.font_size;

                out.push_str("<text");
                write!(out, " x=\"{tx}\" y=\"{ty}\"").unwrap();

                // Foreground color.
                if cell.fg != PackedRgba::WHITE {
                    write!(
                        out,
                        " fill=\"#{:02x}{:02x}{:02x}\"",
                        cell.fg.r(),
                        cell.fg.g(),
                        cell.fg.b()
                    )
                    .unwrap();
                }

                // Style attributes.
                let flags = cell.attrs.flags();
                if flags.contains(StyleFlags::BOLD) {
                    out.push_str(" font-weight=\"bold\"");
                }
                if flags.contains(StyleFlags::ITALIC) {
                    out.push_str(" font-style=\"italic\"");
                }
                if flags.contains(StyleFlags::DIM) {
                    out.push_str(" opacity=\"0.5\"");
                }
                if flags.contains(StyleFlags::UNDERLINE) {
                    out.push_str(" text-decoration=\"underline\"");
                }
                if flags.contains(StyleFlags::STRIKETHROUGH) {
                    out.push_str(" text-decoration=\"line-through\"");
                }

                out.push('>');
                svg_escape_into(&mut out, &content);
                out.push_str("</text>");
            }
        }

        out.push_str("</g></svg>");
        out
    }
}

// ---------------------------------------------------------------------------
// Text Exporter
// ---------------------------------------------------------------------------

/// Configuration for plain text export.
#[derive(Debug, Clone)]
pub struct TextExporter {
    /// Include ANSI escape codes for colors and styles.
    pub include_ansi: bool,
    /// Trim trailing whitespace from each line.
    pub trim_trailing: bool,
}

impl TextExporter {
    /// Create a plain-text exporter (no ANSI codes, trimmed lines).
    #[must_use]
    pub fn plain() -> Self {
        Self {
            include_ansi: false,
            trim_trailing: true,
        }
    }

    /// Create an ANSI-enabled exporter.
    #[must_use]
    pub fn ansi() -> Self {
        Self {
            include_ansi: true,
            trim_trailing: true,
        }
    }

    /// Export a buffer to a plain text (or ANSI) string.
    pub fn export(&self, buffer: &Buffer, pool: &GraphemePool) -> String {
        let mut out = String::with_capacity(buffer.len() + buffer.height() as usize);

        for y in 0..buffer.height() {
            if y > 0 {
                out.push('\n');
            }

            let mut line = String::with_capacity(buffer.width() as usize);

            for x in 0..buffer.width() {
                let cell = buffer.get(x, y).unwrap();

                if cell.is_continuation() {
                    continue;
                }

                let content = cell_content_str(cell.content, pool);

                if self.include_ansi {
                    let has_style = cell.fg != PackedRgba::WHITE
                        || cell.bg != PackedRgba::TRANSPARENT
                        || cell.attrs != CellAttrs::NONE;

                    if has_style {
                        write_ansi_style(&mut line, cell.fg, cell.bg, cell.attrs);
                    }

                    if content.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(&content);
                    }

                    if has_style {
                        line.push_str("\x1b[0m");
                    }
                } else if content.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&content);
                }
            }

            if self.trim_trailing {
                let trimmed = line.trim_end();
                out.push_str(trimmed);
            } else {
                out.push_str(&line);
            }
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve cell content to a displayable string.
fn cell_content_str(content: CellContent, pool: &GraphemePool) -> String {
    if content.is_empty() || content.is_continuation() {
        return String::new();
    }

    if let Some(c) = content.as_char() {
        return c.to_string();
    }

    if let Some(id) = content.grapheme_id()
        && let Some(s) = pool.get(id)
    {
        return s.to_string();
    }

    String::new()
}

/// HTML-escape a string into the output buffer.
fn html_escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

/// SVG-escape a string into the output buffer.
fn svg_escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(c),
        }
    }
}

/// Write ANSI SGR escape codes for the given style.
fn write_ansi_style(out: &mut String, fg: PackedRgba, bg: PackedRgba, attrs: CellAttrs) {
    out.push_str("\x1b[");
    let mut first = true;
    let mut sep = |out: &mut String| {
        if first {
            first = false;
        } else {
            out.push(';');
        }
    };

    let flags = attrs.flags();

    if flags.contains(StyleFlags::BOLD) {
        sep(out);
        out.push('1');
    }
    if flags.contains(StyleFlags::DIM) {
        sep(out);
        out.push('2');
    }
    if flags.contains(StyleFlags::ITALIC) {
        sep(out);
        out.push('3');
    }
    if flags.contains(StyleFlags::UNDERLINE) {
        sep(out);
        out.push('4');
    }
    if flags.contains(StyleFlags::BLINK) {
        sep(out);
        out.push('5');
    }
    if flags.contains(StyleFlags::REVERSE) {
        sep(out);
        out.push('7');
    }
    if flags.contains(StyleFlags::HIDDEN) {
        sep(out);
        out.push('8');
    }
    if flags.contains(StyleFlags::STRIKETHROUGH) {
        sep(out);
        out.push('9');
    }

    // Foreground: 24-bit color.
    if fg != PackedRgba::WHITE && fg.a() > 0 {
        sep(out);
        write!(out, "38;2;{};{};{}", fg.r(), fg.g(), fg.b()).unwrap();
    }

    // Background: 24-bit color.
    if bg != PackedRgba::TRANSPARENT && bg.a() > 0 {
        sep(out);
        write!(out, "48;2;{};{};{}", bg.r(), bg.g(), bg.b()).unwrap();
    }

    out.push('m');
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::{Cell, CellAttrs, PackedRgba, StyleFlags};

    fn make_buffer(text: &str, width: u16) -> (Buffer, GraphemePool) {
        let height = 1;
        let mut buf = Buffer::new(width, height);
        let pool = GraphemePool::new();

        for (i, ch) in text.chars().enumerate() {
            if (i as u16) < width {
                buf.set(i as u16, 0, Cell::from_char(ch));
            }
        }

        (buf, pool)
    }

    // --- HTML exporter tests ---

    #[test]
    fn html_basic_text() {
        let (buf, pool) = make_buffer("Hi", 5);
        let html = HtmlExporter::default().export(&buf, &pool);
        assert!(html.contains("Hi"));
        assert!(html.starts_with("<pre"));
        assert!(html.ends_with("</pre>"));
    }

    #[test]
    fn html_escapes_special_chars() {
        let (buf, pool) = make_buffer("<>&", 5);
        let html = HtmlExporter::default().export(&buf, &pool);
        assert!(html.contains("&lt;"));
        assert!(html.contains("&gt;"));
        assert!(html.contains("&amp;"));
        assert!(!html.contains("<>&"));
    }

    #[test]
    fn html_includes_color_styles() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('R').with_fg(PackedRgba::rgb(255, 0, 0));
        buf.set(0, 0, cell);

        let html = HtmlExporter::default().export(&buf, &pool);
        assert!(html.contains("color:#ff0000"));
    }

    #[test]
    fn html_includes_bg_color() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('B').with_bg(PackedRgba::rgb(0, 0, 255));
        buf.set(0, 0, cell);

        let html = HtmlExporter::default().export(&buf, &pool);
        assert!(html.contains("background:#0000ff"));
    }

    #[test]
    fn html_includes_bold_style() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('B').with_attrs(CellAttrs::new(StyleFlags::BOLD, 0));
        buf.set(0, 0, cell);

        let html = HtmlExporter::default().export(&buf, &pool);
        assert!(html.contains("font-weight:bold"));
    }

    #[test]
    fn html_includes_italic_style() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('I').with_attrs(CellAttrs::new(StyleFlags::ITALIC, 0));
        buf.set(0, 0, cell);

        let html = HtmlExporter::default().export(&buf, &pool);
        assert!(html.contains("font-style:italic"));
    }

    #[test]
    fn html_includes_underline_style() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('U').with_attrs(CellAttrs::new(StyleFlags::UNDERLINE, 0));
        buf.set(0, 0, cell);

        let html = HtmlExporter::default().export(&buf, &pool);
        assert!(html.contains("text-decoration:underline"));
    }

    #[test]
    fn html_empty_buffer() {
        let buf = Buffer::new(3, 2);
        let pool = GraphemePool::new();
        let html = HtmlExporter::default().export(&buf, &pool);
        assert!(html.starts_with("<pre"));
        assert!(html.ends_with("</pre>"));
    }

    #[test]
    fn html_multiline() {
        let mut buf = Buffer::new(3, 2);
        let pool = GraphemePool::new();
        buf.set(0, 0, Cell::from_char('A'));
        buf.set(0, 1, Cell::from_char('B'));

        let html = HtmlExporter::default().export(&buf, &pool);
        assert!(html.contains("A"));
        assert!(html.contains("B"));
        assert!(html.contains('\n'));
    }

    // --- SVG exporter tests ---

    #[test]
    fn svg_basic_structure() {
        let (buf, pool) = make_buffer("Hi", 5);
        let svg = SvgExporter::default().export(&buf, &pool);
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains("xmlns"));
    }

    #[test]
    fn svg_contains_text_elements() {
        let (buf, pool) = make_buffer("X", 3);
        let svg = SvgExporter::default().export(&buf, &pool);
        assert!(svg.contains("<text"));
        assert!(svg.contains(">X</text>"));
    }

    #[test]
    fn svg_escapes_special_chars() {
        let (buf, pool) = make_buffer("<", 3);
        let svg = SvgExporter::default().export(&buf, &pool);
        assert!(svg.contains("&lt;"));
    }

    #[test]
    fn svg_includes_color() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('R').with_fg(PackedRgba::rgb(255, 0, 0));
        buf.set(0, 0, cell);

        let svg = SvgExporter::default().export(&buf, &pool);
        assert!(svg.contains("fill=\"#ff0000\""));
    }

    #[test]
    fn svg_includes_bold() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('B').with_attrs(CellAttrs::new(StyleFlags::BOLD, 0));
        buf.set(0, 0, cell);

        let svg = SvgExporter::default().export(&buf, &pool);
        assert!(svg.contains("font-weight=\"bold\""));
    }

    #[test]
    fn svg_dimensions() {
        let buf = Buffer::new(10, 5);
        let pool = GraphemePool::new();
        let exporter = SvgExporter {
            cell_width: 10.0,
            cell_height: 20.0,
            ..SvgExporter::default()
        };

        let svg = exporter.export(&buf, &pool);
        assert!(svg.contains("width=\"100\""));
        assert!(svg.contains("height=\"100\""));
    }

    #[test]
    fn svg_has_background_rect() {
        let buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();
        let svg = SvgExporter::default().export(&buf, &pool);
        assert!(svg.contains("<rect width=\"100%\" height=\"100%\""));
    }

    #[test]
    fn svg_empty_buffer() {
        let buf = Buffer::new(3, 2);
        let pool = GraphemePool::new();
        let svg = SvgExporter::default().export(&buf, &pool);
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>"));
    }

    // --- Text exporter tests ---

    #[test]
    fn text_plain_basic() {
        let (buf, pool) = make_buffer("Hello", 5);
        let text = TextExporter::plain().export(&buf, &pool);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn text_plain_trims_trailing() {
        let (buf, pool) = make_buffer("Hi", 10);
        let text = TextExporter::plain().export(&buf, &pool);
        assert_eq!(text, "Hi");
    }

    #[test]
    fn text_plain_no_trim() {
        let (buf, pool) = make_buffer("Hi", 5);
        let exporter = TextExporter {
            include_ansi: false,
            trim_trailing: false,
        };
        let text = exporter.export(&buf, &pool);
        assert_eq!(text.len(), 5); // "Hi" + 3 spaces
    }

    #[test]
    fn text_plain_multiline() {
        let mut buf = Buffer::new(3, 2);
        let pool = GraphemePool::new();
        buf.set(0, 0, Cell::from_char('A'));
        buf.set(0, 1, Cell::from_char('B'));

        let text = TextExporter::plain().export(&buf, &pool);
        assert!(text.contains('A'));
        assert!(text.contains('B'));
        assert!(text.contains('\n'));
    }

    #[test]
    fn text_plain_empty_buffer() {
        let buf = Buffer::new(3, 2);
        let pool = GraphemePool::new();
        let text = TextExporter::plain().export(&buf, &pool);
        // All empty cells become spaces, then trimmed.
        for line in text.lines() {
            assert!(line.is_empty());
        }
    }

    #[test]
    fn text_ansi_includes_escape_codes() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('R').with_fg(PackedRgba::rgb(255, 0, 0));
        buf.set(0, 0, cell);

        let text = TextExporter::ansi().export(&buf, &pool);
        assert!(text.contains("\x1b["));
        assert!(text.contains("38;2;255;0;0"));
        assert!(text.contains("\x1b[0m"));
    }

    #[test]
    fn text_ansi_bold() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('B').with_attrs(CellAttrs::new(StyleFlags::BOLD, 0));
        buf.set(0, 0, cell);

        let text = TextExporter::ansi().export(&buf, &pool);
        assert!(text.contains("\x1b[1m"));
    }

    #[test]
    fn text_ansi_multiple_styles() {
        let mut buf = Buffer::new(3, 1);
        let pool = GraphemePool::new();

        let cell = Cell::from_char('X')
            .with_fg(PackedRgba::rgb(0, 255, 0))
            .with_attrs(CellAttrs::new(StyleFlags::BOLD | StyleFlags::ITALIC, 0));
        buf.set(0, 0, cell);

        let text = TextExporter::ansi().export(&buf, &pool);
        assert!(text.contains("\x1b["));
        assert!(text.contains('1')); // bold
        assert!(text.contains('3')); // italic
        assert!(text.contains("38;2;0;255;0")); // green fg
    }

    // --- Helper tests ---

    #[test]
    fn html_escape_handles_all_special_chars() {
        let mut out = String::new();
        html_escape_into(&mut out, "<script>alert(\"hi&bye\")</script>");
        assert_eq!(
            out,
            "&lt;script&gt;alert(&quot;hi&amp;bye&quot;)&lt;/script&gt;"
        );
    }

    #[test]
    fn html_escape_passthrough_normal() {
        let mut out = String::new();
        html_escape_into(&mut out, "Hello World 123");
        assert_eq!(out, "Hello World 123");
    }

    #[test]
    fn svg_escape_handles_special_chars() {
        let mut out = String::new();
        svg_escape_into(&mut out, "a < b & c > d");
        assert_eq!(out, "a &lt; b &amp; c &gt; d");
    }

    #[test]
    fn ansi_style_bold_only() {
        let mut out = String::new();
        write_ansi_style(
            &mut out,
            PackedRgba::WHITE,
            PackedRgba::TRANSPARENT,
            CellAttrs::new(StyleFlags::BOLD, 0),
        );
        assert_eq!(out, "\x1b[1m");
    }

    #[test]
    fn ansi_style_fg_only() {
        let mut out = String::new();
        write_ansi_style(
            &mut out,
            PackedRgba::rgb(255, 0, 0),
            PackedRgba::TRANSPARENT,
            CellAttrs::NONE,
        );
        assert_eq!(out, "\x1b[38;2;255;0;0m");
    }

    #[test]
    fn ansi_style_combined() {
        let mut out = String::new();
        write_ansi_style(
            &mut out,
            PackedRgba::rgb(0, 255, 0),
            PackedRgba::rgb(0, 0, 128),
            CellAttrs::new(StyleFlags::BOLD | StyleFlags::UNDERLINE, 0),
        );
        assert!(out.starts_with("\x1b["));
        assert!(out.ends_with('m'));
        assert!(out.contains('1')); // bold
        assert!(out.contains('4')); // underline
        assert!(out.contains("38;2;0;255;0")); // fg
        assert!(out.contains("48;2;0;0;128")); // bg
    }

    #[test]
    fn ansi_style_empty() {
        let mut out = String::new();
        write_ansi_style(
            &mut out,
            PackedRgba::WHITE,
            PackedRgba::TRANSPARENT,
            CellAttrs::NONE,
        );
        // No attributes, just ESC [ m
        assert_eq!(out, "\x1b[m");
    }
}

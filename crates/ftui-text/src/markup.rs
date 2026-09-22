//! BBCode-style markup parsing for styled text.
//!
//! This module provides a parser for creating styled [`Text`] from markup strings.
//! The syntax is inspired by BBCode and supports nesting.
//!
//! # Syntax
//!
//! ## Style Tags
//! - `[bold]text[/bold]` - Bold text
//! - `[italic]text[/italic]` - Italic text
//! - `[underline]text[/underline]` - Underlined text
//! - `[dim]text[/dim]` - Dim text
//! - `[reverse]text[/reverse]` - Reverse video
//! - `[strikethrough]text[/strikethrough]` - Strikethrough
//! - `[blink]text[/blink]` - Blinking text
//!
//! ## Colors
//! - `[fg=red]text[/fg]` - Named foreground color
//! - `[bg=#00ff00]text[/bg]` - Hex background color
//! - `[fg=rgb(255,128,0)]text[/fg]` - RGB foreground color
//!
//! ## Links
//! - `[link=https://example.com]Click here[/link]` - Hyperlink (stored as metadata)
//!
//! ## Escaping
//! - `\[` - Literal `[` character
//! - `\\` - Literal `\` character
//!
//! # Example
//! ```
//! use ftui_text::markup::{MarkupParser, parse_markup};
//!
//! let text = parse_markup("[bold]Hello[/bold] [fg=red]world[/fg]!").unwrap();
//! assert_eq!(text.to_plain_text(), "Hello world!");
//! ```

use crate::text::{Line, Span, Text};
use ftui_render::cell::PackedRgba;
use ftui_style::Style;

/// Errors that can occur during markup parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkupError {
    /// A closing tag doesn't match the expected opening tag.
    UnmatchedTag {
        expected: Option<String>,
        found: String,
        position: usize,
    },
    /// An opening tag was never closed.
    UnclosedTag { tag: String, position: usize },
    /// Invalid color specification.
    InvalidColor { value: String, position: usize },
    /// Invalid attribute in a tag.
    InvalidAttribute { name: String, position: usize },
    /// Links cannot be nested.
    NestedLinkNotAllowed { position: usize },
    /// Empty tag name.
    EmptyTag { position: usize },
    /// Malformed tag syntax.
    MalformedTag { position: usize },
    /// Nesting depth limit exceeded.
    DepthLimitExceeded { position: usize },
}

impl std::fmt::Display for MarkupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnmatchedTag {
                expected,
                found,
                position,
            } => {
                if let Some(exp) = expected {
                    write!(
                        f,
                        "unmatched tag at position {}: expected [/{}], found [/{}]",
                        position, exp, found
                    )
                } else {
                    write!(
                        f,
                        "unexpected closing tag [/{}] at position {} with no matching opening tag",
                        found, position
                    )
                }
            }
            Self::UnclosedTag { tag, position } => {
                write!(f, "unclosed tag [{}] opened at position {}", tag, position)
            }
            Self::InvalidColor { value, position } => {
                write!(f, "invalid color '{}' at position {}", value, position)
            }
            Self::InvalidAttribute { name, position } => {
                write!(f, "invalid attribute '{}' at position {}", name, position)
            }
            Self::NestedLinkNotAllowed { position } => {
                write!(f, "nested links not allowed at position {}", position)
            }
            Self::EmptyTag { position } => {
                write!(f, "empty tag at position {}", position)
            }
            Self::MalformedTag { position } => {
                write!(f, "malformed tag at position {}", position)
            }
            Self::DepthLimitExceeded { position } => {
                write!(f, "nesting depth limit exceeded at position {}", position)
            }
        }
    }
}

impl std::error::Error for MarkupError {}

/// Entry on the style stack tracking open tags.
#[derive(Debug, Clone)]
struct StyleEntry {
    /// The tag name (e.g., "bold", "fg").
    tag: String,
    /// Position where this tag was opened.
    position: usize,
    /// The style before this tag was applied.
    previous_style: Style,
    /// The style delta applied by this tag.
    style_delta: Style,
    /// URL for link tags.
    link_url: Option<String>,
}

/// Parse markup string into styled Text.
///
/// This is a convenience function that creates a parser and parses the input.
///
/// # Example
/// ```
/// use ftui_text::markup::parse_markup;
///
/// let text = parse_markup("[bold]Hello[/bold]!").unwrap();
/// assert_eq!(text.to_plain_text(), "Hello!");
/// ```
pub fn parse_markup(input: &str) -> Result<Text<'static>, MarkupError> {
    let mut parser = MarkupParser::new();
    let parsed: Text<'static> = parser.parse(input)?;
    Ok(parsed)
}

/// A parser for BBCode-style markup.
///
/// The parser maintains a style stack to handle nested tags correctly.
/// Each opening tag pushes the current style onto the stack and modifies it.
/// Each closing tag pops the stack to restore the previous style.
#[derive(Debug, Default)]
pub struct MarkupParser {
    /// Stack of open style entries.
    style_stack: Vec<StyleEntry>,
    /// Current accumulated style.
    current_style: Style,
    /// Whether we're inside a link (links can't nest).
    in_link: bool,
    /// Current link URL if in a link.
    current_link: Option<String>,
}

impl MarkupParser {
    /// Create a new parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset the parser state.
    pub fn reset(&mut self) {
        self.style_stack.clear();
        self.current_style = Style::default();
        self.in_link = false;
        self.current_link = None;
    }

    /// Parse a markup string into styled Text.
    pub fn parse(&mut self, input: &str) -> Result<Text<'static>, MarkupError> {
        self.reset();

        // A newline starts a new line, as in `Text::raw`. The spans of a
        // multi-line input used to go into one line, with the newlines inside
        // them, so the text rendered on a single row.
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_text = String::new();

        let mut chars = input.char_indices().peekable();

        while let Some((pos, ch)) = chars.next() {
            match ch {
                '\n' => {
                    if !current_text.is_empty() {
                        spans.push(self.make_span(std::mem::take(&mut current_text)));
                    }
                    lines.push(Line::from_spans(std::mem::take(&mut spans)));
                }
                '\\' => {
                    // Escape sequence
                    if let Some(&(_, next_ch)) = chars.peek()
                        && (next_ch == '[' || next_ch == ']' || next_ch == '\\')
                    {
                        chars.next();
                        current_text.push(next_ch);
                        continue;
                    }
                    current_text.push(ch);
                }
                '[' => {
                    // Potential tag - find the closing ]
                    let tag_start = pos;
                    let mut tag_content = String::new();
                    let mut found_close = false;

                    // Newlines are not allowed in tags. One ends the scan but
                    // stays in the input: it used to be consumed and lost.
                    while let Some((_, tag_ch)) = chars.next_if(|&(_, c)| c != '\n') {
                        if tag_ch == ']' {
                            found_close = true;
                            break;
                        }
                        tag_content.push(tag_ch);
                    }

                    if !found_close {
                        // Not a valid tag, treat as literal
                        current_text.push('[');
                        current_text.push_str(&tag_content);
                        continue;
                    }

                    // Parse the tag
                    let tag_content = tag_content.trim();
                    if tag_content.is_empty() {
                        return Err(MarkupError::EmptyTag {
                            position: tag_start,
                        });
                    }

                    if let Some(tag_name) = tag_content.strip_prefix('/') {
                        // Closing tag
                        let tag_name = tag_name.trim();
                        if tag_name.is_empty() {
                            return Err(MarkupError::EmptyTag {
                                position: tag_start,
                            });
                        }

                        // Flush current text
                        if !current_text.is_empty() {
                            spans.push(self.make_span(std::mem::take(&mut current_text)));
                        }

                        // Pop the style stack
                        self.pop_style(tag_name, tag_start)?;
                    } else {
                        // Opening tag
                        // Flush current text first
                        if !current_text.is_empty() {
                            spans.push(self.make_span(std::mem::take(&mut current_text)));
                        }

                        // Parse tag name and optional value
                        let (name, value) = if let Some(eq_pos) = tag_content.find('=') {
                            let name = tag_content[..eq_pos].trim();
                            let value = tag_content[eq_pos + 1..].trim();
                            (name, Some(value))
                        } else {
                            (tag_content, None)
                        };

                        if name.is_empty() {
                            return Err(MarkupError::EmptyTag {
                                position: tag_start,
                            });
                        }

                        self.push_style(name, value, tag_start)?;
                    }
                }
                _ => {
                    current_text.push(ch);
                }
            }
        }

        // Flush remaining text
        if !current_text.is_empty() {
            spans.push(self.make_span(std::mem::take(&mut current_text)));
        }
        lines.push(Line::from_spans(spans));

        // Check for unclosed tags
        if let Some(entry) = self.style_stack.first() {
            return Err(MarkupError::UnclosedTag {
                tag: entry.tag.clone(),
                position: entry.position,
            });
        }

        Ok(Text::from_lines(lines))
    }

    /// Create a span with the current style and link.
    fn make_span(&self, text: String) -> Span<'static> {
        let span = if self.current_style.is_empty() {
            Span::raw(text)
        } else {
            Span::styled(text, self.current_style)
        };

        // Attach link if present
        if let Some(url) = &self.current_link {
            span.link(url.clone())
        } else {
            span
        }
    }

    /// Push a new style onto the stack.
    fn push_style(
        &mut self,
        name: &str,
        value: Option<&str>,
        position: usize,
    ) -> Result<(), MarkupError> {
        if self.style_stack.len() >= 50 {
            return Err(MarkupError::DepthLimitExceeded { position });
        }

        // Apply the new style to get the delta
        let style_delta = self.apply_tag(name, value, position)?;

        // Save current state
        let entry = StyleEntry {
            tag: name.to_lowercase(),
            position,
            previous_style: self.current_style,
            style_delta,
            link_url: self.current_link.clone(),
        };

        self.current_style = self.current_style.merge(&style_delta);

        self.style_stack.push(entry);
        Ok(())
    }

    /// Pop a style from the stack.
    fn pop_style(&mut self, tag_name: &str, position: usize) -> Result<(), MarkupError> {
        let tag_lower = tag_name.to_lowercase();

        // Find the matching opening tag
        let entry_idx = self
            .style_stack
            .iter()
            .rposition(|e| e.tag == tag_lower)
            .ok_or_else(|| MarkupError::UnmatchedTag {
                expected: self.style_stack.last().map(|e| e.tag.clone()),
                found: tag_name.to_string(),
                position,
            })?;

        let entry = self.style_stack.remove(entry_idx);

        // Restore the style to what it was before this tag was pushed
        self.current_style = entry.previous_style;

        // Re-apply styles from any stack entries that were above the removed
        // entry (now shifted down to entry_idx..).
        for remaining in &self.style_stack[entry_idx..] {
            // Because we removed an entry below this one, we must rebuild the
            // accumulated style chain. We use the stored style_delta to exactly
            // replay the effect of each remaining tag.
            self.current_style = self.current_style.merge(&remaining.style_delta);
        }

        // Derive link state from remaining entries on the stack.
        // We cannot use entry.link_url because it may be stale if the link
        // was closed before this entry was popped (interleaved tags).
        self.in_link = false;
        self.current_link = None;
        for remaining in &self.style_stack {
            if remaining.tag == "link" {
                self.in_link = true;
                self.current_link = remaining.link_url.clone();
                // Links cannot nest, so there's at most one
                break;
            }
        }

        Ok(())
    }

    /// Apply a tag and return the style delta.
    fn apply_tag(
        &mut self,
        name: &str,
        value: Option<&str>,
        position: usize,
    ) -> Result<Style, MarkupError> {
        let name_lower = name.to_lowercase();
        match name_lower.as_str() {
            // Simple style attributes
            "bold" | "b" => Ok(Style::new().bold()),
            "italic" | "i" => Ok(Style::new().italic()),
            "underline" | "u" => Ok(Style::new().underline()),
            "dim" => Ok(Style::new().dim()),
            "reverse" => Ok(Style::new().reverse()),
            "strikethrough" | "s" => Ok(Style::new().strikethrough()),
            "blink" => Ok(Style::new().blink()),
            "hidden" => Ok(Style::new().hidden()),

            // Color attributes
            "fg" | "color" => {
                let color_str = value.ok_or_else(|| MarkupError::InvalidAttribute {
                    name: name.to_string(),
                    position,
                })?;
                let color = parse_color(color_str, position)?;
                Ok(Style::new().fg(color))
            }
            "bg" | "background" => {
                let color_str = value.ok_or_else(|| MarkupError::InvalidAttribute {
                    name: name.to_string(),
                    position,
                })?;
                let color = parse_color(color_str, position)?;
                Ok(Style::new().bg(color))
            }

            // Link
            "link" => {
                if self.in_link {
                    return Err(MarkupError::NestedLinkNotAllowed { position });
                }
                self.in_link = true;
                self.current_link = value.map(|s| s.to_string());
                // Links are typically rendered with underline
                Ok(Style::new().underline())
            }

            // Unknown tag - treat as no-op but still track for closing
            _ => Ok(Style::new()),
        }
    }
}

/// Parse a color specification.
///
/// Supports:
/// - Named colors: red, green, blue, etc.
/// - Hex: #rgb, #rrggbb
/// - RGB: rgb(r, g, b)
fn parse_color(s: &str, position: usize) -> Result<PackedRgba, MarkupError> {
    let s = s.trim();

    // Hex color
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex, position);
    }

    // RGB function
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        return parse_rgb_function(inner, position);
    }

    // Named color
    parse_named_color(s, position)
}

/// Parse a hex color (#rgb or #rrggbb).
fn parse_hex_color(hex: &str, position: usize) -> Result<PackedRgba, MarkupError> {
    let hex = hex.trim();
    let make_err = || MarkupError::InvalidColor {
        value: format!("#{}", hex),
        position,
    };

    // Ensure valid hex characters to prevent panics on slicing and invalid values
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(make_err());
    }

    match hex.len() {
        3 => {
            // #rgb -> #rrggbb
            let mut chars = hex.chars();
            let r = chars
                .next()
                .and_then(|c| c.to_digit(16))
                .ok_or_else(make_err)? as u8;
            let g = chars
                .next()
                .and_then(|c| c.to_digit(16))
                .ok_or_else(make_err)? as u8;
            let b = chars
                .next()
                .and_then(|c| c.to_digit(16))
                .ok_or_else(make_err)? as u8;
            Ok(PackedRgba::rgb(r * 17, g * 17, b * 17))
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| make_err())?;
            let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| make_err())?;
            let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| make_err())?;
            Ok(PackedRgba::rgb(r, g, b))
        }
        _ => Err(make_err()),
    }
}

/// Parse rgb(r, g, b) function.
fn parse_rgb_function(inner: &str, position: usize) -> Result<PackedRgba, MarkupError> {
    let make_err = || MarkupError::InvalidColor {
        value: format!("rgb({})", inner),
        position,
    };

    let parts: Vec<&str> = inner.split(',').collect();
    if parts.len() != 3 {
        return Err(make_err());
    }

    let r: u8 = parts[0].trim().parse().map_err(|_| make_err())?;
    let g: u8 = parts[1].trim().parse().map_err(|_| make_err())?;
    let b: u8 = parts[2].trim().parse().map_err(|_| make_err())?;

    Ok(PackedRgba::rgb(r, g, b))
}

/// Parse a named color.
fn parse_named_color(name: &str, position: usize) -> Result<PackedRgba, MarkupError> {
    let name_lower = name.to_lowercase();
    match name_lower.as_str() {
        // Basic colors
        "black" => Ok(PackedRgba::rgb(0, 0, 0)),
        "red" => Ok(PackedRgba::rgb(255, 0, 0)),
        "green" => Ok(PackedRgba::rgb(0, 255, 0)),
        "yellow" => Ok(PackedRgba::rgb(255, 255, 0)),
        "blue" => Ok(PackedRgba::rgb(0, 0, 255)),
        "magenta" | "purple" => Ok(PackedRgba::rgb(255, 0, 255)),
        "cyan" => Ok(PackedRgba::rgb(0, 255, 255)),
        "white" => Ok(PackedRgba::rgb(255, 255, 255)),

        // Bright variants
        "bright_black" | "gray" | "grey" => Ok(PackedRgba::rgb(128, 128, 128)),
        "bright_red" => Ok(PackedRgba::rgb(255, 85, 85)),
        "bright_green" => Ok(PackedRgba::rgb(85, 255, 85)),
        "bright_yellow" => Ok(PackedRgba::rgb(255, 255, 85)),
        "bright_blue" => Ok(PackedRgba::rgb(85, 85, 255)),
        "bright_magenta" => Ok(PackedRgba::rgb(255, 85, 255)),
        "bright_cyan" => Ok(PackedRgba::rgb(85, 255, 255)),
        "bright_white" => Ok(PackedRgba::rgb(255, 255, 255)),

        // Extended colors
        "orange" => Ok(PackedRgba::rgb(255, 165, 0)),
        "pink" => Ok(PackedRgba::rgb(255, 192, 203)),
        "brown" => Ok(PackedRgba::rgb(165, 42, 42)),
        "gold" => Ok(PackedRgba::rgb(255, 215, 0)),
        "silver" => Ok(PackedRgba::rgb(192, 192, 192)),
        "navy" => Ok(PackedRgba::rgb(0, 0, 128)),
        "teal" => Ok(PackedRgba::rgb(0, 128, 128)),
        "olive" => Ok(PackedRgba::rgb(128, 128, 0)),
        "maroon" => Ok(PackedRgba::rgb(128, 0, 0)),
        "lime" => Ok(PackedRgba::rgb(0, 255, 0)),
        "aqua" => Ok(PackedRgba::rgb(0, 255, 255)),
        "fuchsia" => Ok(PackedRgba::rgb(255, 0, 255)),

        _ => Err(MarkupError::InvalidColor {
            value: name.to_string(),
            position,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_style::StyleFlags;

    // =========================================================================
    // Basic parsing tests
    // =========================================================================

    #[test]
    fn parse_plain_text() {
        let text = parse_markup("Hello, world!").unwrap();
        assert_eq!(text.to_plain_text(), "Hello, world!");
        assert_eq!(text.height(), 1);
    }

    #[test]
    fn parse_bold() {
        let text = parse_markup("[bold]Hello[/bold]").unwrap();
        assert_eq!(text.to_plain_text(), "Hello");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::BOLD));
    }

    #[test]
    fn parse_italic() {
        let text = parse_markup("[italic]Hello[/italic]").unwrap();
        assert_eq!(text.to_plain_text(), "Hello");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::ITALIC));
    }

    #[test]
    fn parse_underline() {
        let text = parse_markup("[underline]Hello[/underline]").unwrap();
        assert_eq!(text.to_plain_text(), "Hello");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::UNDERLINE));
    }

    #[test]
    fn parse_short_tags() {
        let text = parse_markup("[b]Bold[/b] [i]Italic[/i] [u]Underline[/u]").unwrap();
        assert_eq!(text.to_plain_text(), "Bold Italic Underline");
    }

    // =========================================================================
    // Color parsing tests
    // =========================================================================

    #[test]
    fn parse_fg_named_color() {
        let text = parse_markup("[fg=red]Red text[/fg]").unwrap();
        assert_eq!(text.to_plain_text(), "Red text");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(255, 0, 0)));
    }

    #[test]
    fn parse_bg_named_color() {
        let text = parse_markup("[bg=blue]Blue background[/bg]").unwrap();
        assert_eq!(text.to_plain_text(), "Blue background");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.bg, Some(PackedRgba::rgb(0, 0, 255)));
    }

    #[test]
    fn parse_hex_color_short() {
        let text = parse_markup("[fg=#f00]Red[/fg]").unwrap();
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(255, 0, 0)));
    }

    #[test]
    fn parse_hex_color_long() {
        let text = parse_markup("[fg=#00ff00]Green[/fg]").unwrap();
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(0, 255, 0)));
    }

    #[test]
    fn parse_rgb_function() {
        let text = parse_markup("[fg=rgb(128, 64, 255)]Custom[/fg]").unwrap();
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(128, 64, 255)));
    }

    // =========================================================================
    // Nested tags tests
    // =========================================================================

    #[test]
    fn parse_interleaved_tags_with_values() {
        // [fg=red][bg=blue]text[/fg]more[/bg]
        // Push fg=red (Current: red)
        // Push bg=blue (Current: red on blue)
        // Pop fg (restore bg=blue prev... wait, bg=blue prev was red. Restoring fg's prev (empty). Reapply bg=blue.)
        // Result: "text" is red on blue. "more" should be blue background (default fg).
        let text = parse_markup("[fg=red][bg=blue]text[/fg]more[/bg]").unwrap();

        assert_eq!(text.to_plain_text(), "textmore");

        // "text" span
        let style1 = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style1.fg, Some(PackedRgba::rgb(255, 0, 0))); // Red
        assert_eq!(style1.bg, Some(PackedRgba::rgb(0, 0, 255))); // Blue

        // "more" span
        let style2 = text.lines()[0].spans()[1].style.unwrap();
        assert_eq!(style2.fg, None); // Default fg
        assert_eq!(style2.bg, Some(PackedRgba::rgb(0, 0, 255))); // Blue
    }

    #[test]
    fn parse_nested_tags() {
        let text = parse_markup("[bold][italic]Bold and italic[/italic][/bold]").unwrap();
        assert_eq!(text.to_plain_text(), "Bold and italic");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::BOLD));
        assert!(style.has_attr(StyleFlags::ITALIC));
    }

    #[test]
    fn parse_adjacent_styled_spans() {
        let text = parse_markup("[bold]Bold[/bold] [italic]Italic[/italic]").unwrap();
        assert_eq!(text.to_plain_text(), "Bold Italic");
        assert_eq!(text.lines()[0].spans().len(), 3); // Bold, space, Italic
    }

    #[test]
    fn parse_mixed_styles() {
        let text =
            parse_markup("Normal [bold]bold [fg=red]bold+red[/fg] bold[/bold] normal").unwrap();
        assert_eq!(text.to_plain_text(), "Normal bold bold+red bold normal");
    }

    // =========================================================================
    // Escape sequence tests
    // =========================================================================

    #[test]
    fn parse_escaped_bracket() {
        let text = parse_markup(r"Hello \[world\]").unwrap();
        assert_eq!(text.to_plain_text(), "Hello [world]");
    }

    #[test]
    fn parse_escaped_backslash() {
        let text = parse_markup(r"Hello \\world").unwrap();
        assert_eq!(text.to_plain_text(), r"Hello \world");
    }

    #[test]
    fn parse_escape_in_tag() {
        let text = parse_markup(r"[bold]Hello \[tag\][/bold]").unwrap();
        assert_eq!(text.to_plain_text(), "Hello [tag]");
    }

    // =========================================================================
    // Error handling tests
    // =========================================================================

    #[test]
    fn error_unclosed_tag() {
        let result = parse_markup("[bold]Hello");
        assert!(matches!(result, Err(MarkupError::UnclosedTag { .. })));
    }

    #[test]
    fn error_unmatched_closing_tag() {
        let result = parse_markup("Hello[/bold]");
        assert!(matches!(result, Err(MarkupError::UnmatchedTag { .. })));
    }

    #[test]
    fn error_empty_tag() {
        let result = parse_markup("Hello[]world");
        assert!(matches!(result, Err(MarkupError::EmptyTag { .. })));
    }

    #[test]
    fn error_invalid_color() {
        let result = parse_markup("[fg=notacolor]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidColor { .. })));
    }

    #[test]
    fn error_nested_links() {
        let result = parse_markup("[link=a][link=b]text[/link][/link]");
        assert!(matches!(
            result,
            Err(MarkupError::NestedLinkNotAllowed { .. })
        ));
    }

    #[test]
    fn error_fg_without_value() {
        let result = parse_markup("[fg]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidAttribute { .. })));
    }

    // =========================================================================
    // Edge case tests
    // =========================================================================

    #[test]
    fn parse_empty_string() {
        let text = parse_markup("").unwrap();
        assert!(text.is_empty());
    }

    #[test]
    fn parse_only_tags() {
        let text = parse_markup("[bold][/bold]").unwrap();
        assert!(text.is_empty() || text.to_plain_text().is_empty());
    }

    #[test]
    fn parse_unclosed_bracket_literal() {
        // An unclosed [ is treated as literal text
        let text = parse_markup("Hello [world").unwrap();
        assert_eq!(text.to_plain_text(), "Hello [world");
    }

    #[test]
    fn parse_link() {
        let text = parse_markup("[link=https://example.com]Click here[/link]").unwrap();
        assert_eq!(text.to_plain_text(), "Click here");
        // Link text should be underlined
        let span = &text.lines()[0].spans()[0];
        let style = span.style.unwrap();
        assert!(style.has_attr(StyleFlags::UNDERLINE));
        // Link URL should be set on the span
        assert_eq!(span.link.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn parse_link_preserves_url_after_close() {
        // Text after link close should NOT have the link
        let text = parse_markup("[link=https://a.com]Link[/link] Normal").unwrap();
        let spans = text.lines()[0].spans();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].link.as_deref(), Some("https://a.com"));
        assert_eq!(spans[1].link, None);
    }

    #[test]
    fn parse_link_with_nested_tag_outlasting_link() {
        // Interleaved case: [link=a][bold]text[/link]more[/bold]after
        // Link is closed before bold. Text after link close should NOT have the link,
        // even though bold was opened inside the link.
        let text = parse_markup("[link=https://a.com][bold]text[/link]more[/bold]after").unwrap();
        let spans = text.lines()[0].spans();

        // "text" should have link (inside both link and bold)
        assert_eq!(spans[0].link.as_deref(), Some("https://a.com"));

        // "more" should NOT have link (link was closed, even though bold is still open)
        assert!(
            spans[1].link.is_none(),
            "span 'more' should NOT have link after [/link], got {:?}",
            spans[1].link
        );

        // "after" should NOT have link (both tags closed)
        if let Some(span) = spans.get(2) {
            assert!(
                span.link.is_none(),
                "span 'after' should NOT have link, got {:?}",
                span.link
            );
        }
    }

    #[test]
    fn parse_case_insensitive_tags() {
        let text = parse_markup("[BOLD]Hello[/bold]").unwrap();
        assert_eq!(text.to_plain_text(), "Hello");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::BOLD));
    }

    #[test]
    fn parse_whitespace_in_tags() {
        let text = parse_markup("[ bold ]Hello[ / bold ]").unwrap();
        assert_eq!(text.to_plain_text(), "Hello");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::BOLD));
    }

    // =========================================================================
    // Named color tests
    // =========================================================================

    #[test]
    fn parse_all_basic_colors() {
        let colors = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        for color in colors {
            let input = format!("[fg={}]text[/fg]", color);
            let result = parse_markup(&input);
            assert!(result.is_ok(), "Failed to parse color: {}", color);
        }
    }

    #[test]
    fn parse_extended_colors() {
        let colors = [
            "orange", "pink", "brown", "gold", "silver", "navy", "teal", "olive", "maroon", "lime",
        ];
        for color in colors {
            let input = format!("[fg={}]text[/fg]", color);
            let result = parse_markup(&input);
            assert!(result.is_ok(), "Failed to parse color: {}", color);
        }
    }

    // =========================================================================
    // Integration tests
    // =========================================================================

    #[test]
    fn parse_complex_markup() {
        let input = r#"[bold]Title[/bold]

[fg=blue]Info:[/fg] This is [italic]important[/italic] text.
[bg=yellow][fg=black]Warning![/fg][/bg]"#;

        let text = parse_markup(input).unwrap();
        assert_eq!(
            text.to_plain_text(),
            "Title\n\nInfo: This is important text.\nWarning!"
        );
    }

    #[test]
    fn parser_reuse() {
        let mut parser = MarkupParser::new();

        let text1 = parser.parse("[bold]First[/bold]").unwrap();
        let text2 = parser.parse("[italic]Second[/italic]").unwrap();

        assert_eq!(text1.to_plain_text(), "First");
        assert_eq!(text2.to_plain_text(), "Second");
    }

    // =========================================================================
    // Remaining style tags (dim, reverse, strikethrough, blink, hidden)
    // =========================================================================

    #[test]
    fn parse_dim() {
        let text = parse_markup("[dim]Faded[/dim]").unwrap();
        assert_eq!(text.to_plain_text(), "Faded");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::DIM));
    }

    #[test]
    fn parse_reverse() {
        let text = parse_markup("[reverse]Inverted[/reverse]").unwrap();
        assert_eq!(text.to_plain_text(), "Inverted");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::REVERSE));
    }

    #[test]
    fn parse_strikethrough() {
        let text = parse_markup("[strikethrough]Deleted[/strikethrough]").unwrap();
        assert_eq!(text.to_plain_text(), "Deleted");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::STRIKETHROUGH));
    }

    #[test]
    fn parse_strikethrough_short_tag() {
        let text = parse_markup("[s]Deleted[/s]").unwrap();
        assert_eq!(text.to_plain_text(), "Deleted");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::STRIKETHROUGH));
    }

    #[test]
    fn parse_blink() {
        let text = parse_markup("[blink]Flashy[/blink]").unwrap();
        assert_eq!(text.to_plain_text(), "Flashy");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::BLINK));
    }

    #[test]
    fn parse_hidden() {
        let text = parse_markup("[hidden]Secret[/hidden]").unwrap();
        assert_eq!(text.to_plain_text(), "Secret");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::HIDDEN));
    }

    // =========================================================================
    // Color/background alias tags
    // =========================================================================

    #[test]
    fn parse_color_alias_for_fg() {
        let text = parse_markup("[color=green]Colored[/color]").unwrap();
        assert_eq!(text.to_plain_text(), "Colored");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(0, 255, 0)));
    }

    #[test]
    fn parse_background_alias_for_bg() {
        let text = parse_markup("[background=yellow]Highlighted[/background]").unwrap();
        assert_eq!(text.to_plain_text(), "Highlighted");
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.bg, Some(PackedRgba::rgb(255, 255, 0)));
    }

    // =========================================================================
    // Unknown tag passthrough
    // =========================================================================

    #[test]
    fn parse_unknown_tag_no_style() {
        let text = parse_markup("[custom]text[/custom]").unwrap();
        assert_eq!(text.to_plain_text(), "text");
        // Unknown tags apply no style (Style::new() delta)
        let span = &text.lines()[0].spans()[0];
        assert!(
            span.style.is_none() || span.style.unwrap().is_empty(),
            "unknown tag should not apply any style"
        );
    }

    // =========================================================================
    // Depth limit
    // =========================================================================

    #[test]
    fn error_depth_limit_exceeded() {
        // 51 nested bold tags should exceed the limit of 50
        let mut input = String::new();
        for _ in 0..51 {
            input.push_str("[bold]");
        }
        input.push_str("deep");
        for _ in 0..51 {
            input.push_str("[/bold]");
        }
        let result = parse_markup(&input);
        assert!(
            matches!(result, Err(MarkupError::DepthLimitExceeded { .. })),
            "51 nested tags should exceed depth limit"
        );
    }

    #[test]
    fn parse_at_depth_limit_ok() {
        // Exactly 50 nested tags should be fine
        let mut input = String::new();
        for _ in 0..50 {
            input.push_str("[bold]");
        }
        input.push_str("deep");
        for _ in 0..50 {
            input.push_str("[/bold]");
        }
        let result = parse_markup(&input);
        assert!(result.is_ok(), "exactly 50 nested tags should succeed");
    }

    // =========================================================================
    // MarkupError Display
    // =========================================================================

    #[test]
    fn error_display_unmatched_tag_with_expected() {
        let err = MarkupError::UnmatchedTag {
            expected: Some("bold".into()),
            found: "italic".into(),
            position: 10,
        };
        let msg = err.to_string();
        assert!(msg.contains("position 10"));
        assert!(msg.contains("[/bold]"));
        assert!(msg.contains("[/italic]"));
    }

    #[test]
    fn error_display_unmatched_tag_no_expected() {
        let err = MarkupError::UnmatchedTag {
            expected: None,
            found: "bold".into(),
            position: 5,
        };
        let msg = err.to_string();
        assert!(msg.contains("position 5"));
        assert!(msg.contains("[/bold]"));
        assert!(msg.contains("no matching"));
    }

    #[test]
    fn error_display_unclosed_tag() {
        let err = MarkupError::UnclosedTag {
            tag: "italic".into(),
            position: 0,
        };
        let msg = err.to_string();
        assert!(msg.contains("[italic]"));
        assert!(msg.contains("position 0"));
    }

    #[test]
    fn error_display_invalid_color() {
        let err = MarkupError::InvalidColor {
            value: "nope".into(),
            position: 3,
        };
        let msg = err.to_string();
        assert!(msg.contains("nope"));
        assert!(msg.contains("position 3"));
    }

    #[test]
    fn error_display_invalid_attribute() {
        let err = MarkupError::InvalidAttribute {
            name: "fg".into(),
            position: 7,
        };
        let msg = err.to_string();
        assert!(msg.contains("fg"));
        assert!(msg.contains("position 7"));
    }

    #[test]
    fn error_display_nested_link() {
        let err = MarkupError::NestedLinkNotAllowed { position: 20 };
        let msg = err.to_string();
        assert!(msg.contains("nested"));
        assert!(msg.contains("position 20"));
    }

    #[test]
    fn error_display_empty_tag() {
        let err = MarkupError::EmptyTag { position: 4 };
        let msg = err.to_string();
        assert!(msg.contains("empty"));
        assert!(msg.contains("position 4"));
    }

    #[test]
    fn error_display_malformed_tag() {
        let err = MarkupError::MalformedTag { position: 15 };
        let msg = err.to_string();
        assert!(msg.contains("malformed"));
        assert!(msg.contains("position 15"));
    }

    #[test]
    fn error_display_depth_limit() {
        let err = MarkupError::DepthLimitExceeded { position: 100 };
        let msg = err.to_string();
        assert!(msg.contains("depth"));
        assert!(msg.contains("position 100"));
    }

    // =========================================================================
    // Hex color edge cases
    // =========================================================================

    #[test]
    fn error_hex_invalid_chars() {
        let result = parse_markup("[fg=#gghhii]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidColor { .. })));
    }

    #[test]
    fn error_hex_wrong_length_2() {
        let result = parse_markup("[fg=#ff]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidColor { .. })));
    }

    #[test]
    fn error_hex_wrong_length_4() {
        let result = parse_markup("[fg=#ffff]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidColor { .. })));
    }

    #[test]
    fn error_hex_wrong_length_5() {
        let result = parse_markup("[fg=#fffff]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidColor { .. })));
    }

    // =========================================================================
    // RGB function edge cases
    // =========================================================================

    #[test]
    fn error_rgb_too_few_args() {
        let result = parse_markup("[fg=rgb(255,128)]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidColor { .. })));
    }

    #[test]
    fn error_rgb_too_many_args() {
        let result = parse_markup("[fg=rgb(255,128,0,1)]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidColor { .. })));
    }

    #[test]
    fn error_rgb_overflow_value() {
        let result = parse_markup("[fg=rgb(999,0,0)]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidColor { .. })));
    }

    #[test]
    fn error_rgb_negative_value() {
        let result = parse_markup("[fg=rgb(-1,0,0)]text[/fg]");
        assert!(matches!(result, Err(MarkupError::InvalidColor { .. })));
    }

    #[test]
    fn parse_rgb_with_spaces() {
        let text = parse_markup("[fg=rgb( 10 , 20 , 30 )]text[/fg]").unwrap();
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(10, 20, 30)));
    }

    #[test]
    fn parse_rgb_boundary_values() {
        let text = parse_markup("[fg=rgb(0,0,0)]min[/fg]").unwrap();
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(0, 0, 0)));

        let text = parse_markup("[fg=rgb(255,255,255)]max[/fg]").unwrap();
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(255, 255, 255)));
    }

    // =========================================================================
    // Newline in tag
    // =========================================================================

    #[test]
    fn parse_newline_in_tag_treated_as_literal() {
        // Newline inside a tag breaks the tag → [ and content become literal text.
        // The newline still ends the line, and the ] after it is a regular
        // character. It used to be dropped, which made `[bold]` out of it.
        let text = parse_markup("Hello [bold\n]world").unwrap();
        assert_eq!(text.to_plain_text(), "Hello [bold\n]world");
        assert_eq!(text.height(), 2);
    }

    #[test]
    fn parse_newlines_start_lines_and_keep_open_styles() {
        let text = parse_markup("[bold]one\ntwo[/bold]\n\nthree\n").unwrap();
        let lines: Vec<String> = text.lines().iter().map(Line::to_plain_text).collect();
        assert_eq!(lines, ["one", "two", "", "three", ""]);
        assert_eq!(text.height(), Text::raw("one\ntwo\n\nthree\n").height());
        for line in &text.lines()[..2] {
            let style = line.spans()[0].style.unwrap();
            assert!(style.has_attr(StyleFlags::BOLD));
        }
        assert!(text.lines()[3].spans()[0].style.is_none());
        assert_eq!(parse_markup("").unwrap().height(), 1);
    }

    // =========================================================================
    // Empty closing tag [/]
    // =========================================================================

    #[test]
    fn error_empty_closing_tag() {
        let result = parse_markup("[bold]text[/]");
        assert!(matches!(result, Err(MarkupError::EmptyTag { .. })));
    }

    // =========================================================================
    // Backslash edge cases
    // =========================================================================

    #[test]
    fn parse_backslash_before_non_escapable() {
        // \a is not an escape → backslash is kept as-is
        let text = parse_markup(r"Hello \a world").unwrap();
        assert_eq!(text.to_plain_text(), "Hello \\a world");
    }

    #[test]
    fn parse_trailing_backslash() {
        let text = parse_markup(r"Hello\").unwrap();
        assert_eq!(text.to_plain_text(), "Hello\\");
    }

    #[test]
    fn parse_escape_closing_bracket() {
        let text = parse_markup(r"Hello \] world").unwrap();
        assert_eq!(text.to_plain_text(), "Hello ] world");
    }

    // =========================================================================
    // Named color variants and aliases
    // =========================================================================

    #[test]
    fn parse_bright_color_variants() {
        let bright_colors = [
            ("bright_black", PackedRgba::rgb(128, 128, 128)),
            ("bright_red", PackedRgba::rgb(255, 85, 85)),
            ("bright_green", PackedRgba::rgb(85, 255, 85)),
            ("bright_yellow", PackedRgba::rgb(255, 255, 85)),
            ("bright_blue", PackedRgba::rgb(85, 85, 255)),
            ("bright_magenta", PackedRgba::rgb(255, 85, 255)),
            ("bright_cyan", PackedRgba::rgb(85, 255, 255)),
            ("bright_white", PackedRgba::rgb(255, 255, 255)),
        ];
        for (name, expected) in bright_colors {
            let input = format!("[fg={name}]text[/fg]");
            let text = parse_markup(&input).unwrap();
            let style = text.lines()[0].spans()[0].style.unwrap();
            assert_eq!(style.fg, Some(expected), "color mismatch for {name}");
        }
    }

    #[test]
    fn parse_gray_grey_aliases() {
        let text1 = parse_markup("[fg=gray]text[/fg]").unwrap();
        let text2 = parse_markup("[fg=grey]text[/fg]").unwrap();
        let c1 = text1.lines()[0].spans()[0].style.unwrap().fg;
        let c2 = text2.lines()[0].spans()[0].style.unwrap().fg;
        assert_eq!(c1, c2, "gray and grey should be the same color");
        assert_eq!(c1, Some(PackedRgba::rgb(128, 128, 128)));
    }

    #[test]
    fn parse_purple_alias_for_magenta() {
        let text1 = parse_markup("[fg=magenta]text[/fg]").unwrap();
        let text2 = parse_markup("[fg=purple]text[/fg]").unwrap();
        let c1 = text1.lines()[0].spans()[0].style.unwrap().fg;
        let c2 = text2.lines()[0].spans()[0].style.unwrap().fg;
        assert_eq!(c1, c2, "magenta and purple should be the same color");
    }

    #[test]
    fn parse_aqua_fuchsia_colors() {
        let text = parse_markup("[fg=aqua]text[/fg]").unwrap();
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(0, 255, 255)));

        let text = parse_markup("[fg=fuchsia]text[/fg]").unwrap();
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert_eq!(style.fg, Some(PackedRgba::rgb(255, 0, 255)));
    }

    // =========================================================================
    // Link edge cases
    // =========================================================================

    #[test]
    fn parse_link_without_url() {
        // [link] without =value should still parse (url is None)
        let text = parse_markup("[link]Click[/link]").unwrap();
        assert_eq!(text.to_plain_text(), "Click");
        let span = &text.lines()[0].spans()[0];
        assert!(
            span.link.is_none(),
            "link without value should have None URL"
        );
        // Should still have underline style
        let style = span.style.unwrap();
        assert!(style.has_attr(StyleFlags::UNDERLINE));
    }

    // =========================================================================
    // Tag with equals but empty value
    // =========================================================================

    #[test]
    fn error_fg_with_empty_value() {
        let result = parse_markup("[fg=]text[/fg]");
        assert!(
            result.is_err(),
            "fg with empty value should produce an error"
        );
    }

    #[test]
    fn error_bg_without_value() {
        let result = parse_markup("[bg]text[/bg]");
        assert!(matches!(result, Err(MarkupError::InvalidAttribute { .. })));
    }

    // =========================================================================
    // MarkupError is std::error::Error
    // =========================================================================

    #[test]
    fn markup_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(MarkupError::EmptyTag { position: 0 });
        assert!(!err.to_string().is_empty());
    }
}

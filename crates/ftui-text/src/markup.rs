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

use crate::text::{Span, Text};
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
pub fn parse_markup(input: &str) -> Result<Text, MarkupError> {
    let mut parser = MarkupParser::new();
    parser.parse(input)
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
    pub fn parse(&mut self, input: &str) -> Result<Text, MarkupError> {
        self.reset();

        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_text = String::new();

        let mut chars = input.char_indices().peekable();

        while let Some((pos, ch)) = chars.next() {
            match ch {
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

                    for (_, tag_ch) in chars.by_ref() {
                        if tag_ch == ']' {
                            found_close = true;
                            break;
                        }
                        if tag_ch == '\n' {
                            // Newlines not allowed in tags
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

        // Check for unclosed tags
        if let Some(entry) = self.style_stack.first() {
            return Err(MarkupError::UnclosedTag {
                tag: entry.tag.clone(),
                position: entry.position,
            });
        }

        Ok(Text::from_spans(spans))
    }

    /// Create a span with the current style.
    fn make_span(&self, text: String) -> Span<'static> {
        if self.current_style.is_empty() {
            Span::raw(text)
        } else {
            Span::styled(text, self.current_style)
        }
    }

    /// Push a new style onto the stack.
    fn push_style(
        &mut self,
        name: &str,
        value: Option<&str>,
        position: usize,
    ) -> Result<(), MarkupError> {
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
        self.current_link = entry.link_url;

        if tag_lower == "link" {
            self.in_link = false;
            self.current_link = None;
        }

        // Re-apply styles from any stack entries that were above the removed
        // entry (now shifted down to entry_idx..).
        for remaining in &self.style_stack[entry_idx..] {
            // Because we removed an entry below this one, we must rebuild the
            // accumulated style chain. We use the stored style_delta to exactly
            // replay the effect of each remaining tag.
            self.current_style = self.current_style.merge(&remaining.style_delta);

            // Restore link state from remaining entries
            if remaining.tag == "link" {
                self.in_link = true;
                self.current_link = remaining.link_url.clone();
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
        let style = text.lines()[0].spans()[0].style.unwrap();
        assert!(style.has_attr(StyleFlags::UNDERLINE));
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
}

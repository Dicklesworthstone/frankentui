//! JSON view widget for pretty-printing JSON text.
//!
//! Renders formatted JSON with indentation and optional syntax highlighting.
//! Does not depend on serde; operates on raw JSON strings with a minimal
//! tokenizer.
//!
//! # Example
//!
//! ```
//! use ftui_widgets::json_view::JsonView;
//!
//! let json = r#"{"name": "Alice", "age": 30}"#;
//! let view = JsonView::new(json);
//! let lines = view.formatted_lines();
//! assert!(lines.len() > 1); // Pretty-printed across multiple lines
//! ```

use crate::{Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;


/// A classified JSON token for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonToken {
    /// Object key (string before colon).
    Key(String),
    /// String value.
    StringVal(String),
    /// Number value.
    Number(String),
    /// Boolean or null literal.
    Literal(String),
    /// Structural character: `{`, `}`, `[`, `]`, `:`, `,`.
    Punctuation(String),
    /// Whitespace / indentation.
    Whitespace(String),
    /// Newline.
    Newline,
    /// Error text (invalid JSON portion).
    Error(String),
}

/// Widget that renders pretty-printed JSON with syntax coloring.
#[derive(Debug, Clone)]
pub struct JsonView {
    source: String,
    indent: usize,
    key_style: Style,
    string_style: Style,
    number_style: Style,
    literal_style: Style,
    punct_style: Style,
    error_style: Style,
}

impl Default for JsonView {
    fn default() -> Self {
        Self::new("")
    }
}

impl JsonView {
    /// Create a new JSON view from a raw JSON string.
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            indent: 2,
            key_style: Style::new().bold(),
            string_style: Style::default(),
            number_style: Style::default(),
            literal_style: Style::default(),
            punct_style: Style::default(),
            error_style: Style::default(),
        }
    }

    /// Set the indentation width.
    #[must_use]
    pub fn with_indent(mut self, indent: usize) -> Self {
        self.indent = indent;
        self
    }

    /// Set style for object keys.
    #[must_use]
    pub fn with_key_style(mut self, style: Style) -> Self {
        self.key_style = style;
        self
    }

    /// Set style for string values.
    #[must_use]
    pub fn with_string_style(mut self, style: Style) -> Self {
        self.string_style = style;
        self
    }

    /// Set style for numbers.
    #[must_use]
    pub fn with_number_style(mut self, style: Style) -> Self {
        self.number_style = style;
        self
    }

    /// Set style for boolean/null literals.
    #[must_use]
    pub fn with_literal_style(mut self, style: Style) -> Self {
        self.literal_style = style;
        self
    }

    /// Set style for punctuation.
    #[must_use]
    pub fn with_punct_style(mut self, style: Style) -> Self {
        self.punct_style = style;
        self
    }

    /// Set style for error text.
    #[must_use]
    pub fn with_error_style(mut self, style: Style) -> Self {
        self.error_style = style;
        self
    }

    /// Set the source JSON.
    pub fn set_source(&mut self, source: impl Into<String>) {
        self.source = source.into();
    }

    /// Get the source JSON.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Pretty-format the JSON into lines of tokens for rendering.
    #[must_use]
    pub fn formatted_lines(&self) -> Vec<Vec<JsonToken>> {
        let trimmed = self.source.trim();
        if trimmed.is_empty() {
            return vec![];
        }

        let mut lines: Vec<Vec<JsonToken>> = Vec::new();
        let mut current_line: Vec<JsonToken> = Vec::new();
        let mut depth: usize = 0;
        let mut chars = trimmed.chars().peekable();

        while let Some(&ch) = chars.peek() {
            match ch {
                '{' | '[' => {
                    chars.next();
                    current_line.push(JsonToken::Punctuation(ch.to_string()));
                    // Check if next non-whitespace is closing bracket
                    skip_ws(&mut chars);
                    let next = chars.peek().copied();
                    if next == Some('}') || next == Some(']') {
                        // Empty object/array
                        let closing = chars.next().unwrap();
                        current_line.push(JsonToken::Punctuation(closing.to_string()));
                        // Check for comma
                        skip_ws(&mut chars);
                        if chars.peek() == Some(&',') {
                            chars.next();
                            current_line.push(JsonToken::Punctuation(",".to_string()));
                        }
                    } else {
                        depth += 1;
                        lines.push(current_line);
                        current_line = vec![JsonToken::Whitespace(make_indent(depth, self.indent))];
                    }
                }
                '}' | ']' => {
                    chars.next();
                    depth = depth.saturating_sub(1);
                    lines.push(current_line);
                    current_line = vec![
                        JsonToken::Whitespace(make_indent(depth, self.indent)),
                        JsonToken::Punctuation(ch.to_string()),
                    ];
                    // Check for comma
                    skip_ws(&mut chars);
                    if chars.peek() == Some(&',') {
                        chars.next();
                        current_line.push(JsonToken::Punctuation(",".to_string()));
                    }
                }
                '"' => {
                    let s = read_string(&mut chars);
                    skip_ws(&mut chars);
                    if chars.peek() == Some(&':') {
                        // This is a key
                        current_line.push(JsonToken::Key(s));
                        chars.next();
                        current_line.push(JsonToken::Punctuation(": ".to_string()));
                        skip_ws(&mut chars);
                        } else {
                        current_line.push(JsonToken::StringVal(s));
                        // Check for comma
                        skip_ws(&mut chars);
                        if chars.peek() == Some(&',') {
                            chars.next();
                            current_line.push(JsonToken::Punctuation(",".to_string()));
                            lines.push(current_line);
                            current_line = vec![JsonToken::Whitespace(make_indent(depth, self.indent))];
                                }
                    }
                }
                ',' => {
                    chars.next();
                    current_line.push(JsonToken::Punctuation(",".to_string()));
                    lines.push(current_line);
                    current_line = vec![JsonToken::Whitespace(make_indent(depth, self.indent))];
                }
                ':' => {
                    chars.next();
                    current_line.push(JsonToken::Punctuation(": ".to_string()));
                    skip_ws(&mut chars);
                }
                ' ' | '\t' | '\r' | '\n' => {
                    chars.next();
                }
                _ => {
                    // Number, boolean, null, or error
                    let token = read_literal(&mut chars);
                    let tok = classify_literal(&token);
                    current_line.push(tok);
                    // Check for comma
                    skip_ws(&mut chars);
                    if chars.peek() == Some(&',') {
                        chars.next();
                        current_line.push(JsonToken::Punctuation(",".to_string()));
                        lines.push(current_line);
                        current_line = vec![JsonToken::Whitespace(make_indent(depth, self.indent))];
                    }
                }
            }
        }

        if !current_line.is_empty() {
            lines.push(current_line);
        }

        lines
    }
}

fn make_indent(depth: usize, width: usize) -> String {
    " ".repeat(depth * width)
}

fn skip_ws(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&ch) = chars.peek() {
        if ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n' {
            chars.next();
        } else {
            break;
        }
    }
}

fn read_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    s.push('"');
    chars.next(); // consume opening quote
    let mut escaped = false;
    for ch in chars.by_ref() {
        s.push(ch);
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            break;
        }
    }
    s
}

fn read_literal(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&ch) = chars.peek() {
        if ch == ',' || ch == '}' || ch == ']' || ch == ':' || ch == ' ' || ch == '\n' || ch == '\r' || ch == '\t' {
            break;
        }
        s.push(ch);
        chars.next();
    }
    s
}

fn classify_literal(s: &str) -> JsonToken {
    match s {
        "true" | "false" | "null" => JsonToken::Literal(s.to_string()),
        _ => {
            // Try as number
            if s.bytes().all(|b| b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'+' || b == b'e' || b == b'E') && !s.is_empty() {
                JsonToken::Number(s.to_string())
            } else {
                JsonToken::Error(s.to_string())
            }
        }
    }
}

impl Widget for JsonView {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        let lines = self.formatted_lines();
        let max_x = area.right();

        for (row_idx, tokens) in lines.iter().enumerate() {
            if row_idx as u16 >= area.height {
                break;
            }

            let y = area.y + row_idx as u16;
            let mut x = area.x;

            for token in tokens {
                let (text, style) = match token {
                    JsonToken::Key(s) => (s.as_str(), self.key_style),
                    JsonToken::StringVal(s) => (s.as_str(), self.string_style),
                    JsonToken::Number(s) => (s.as_str(), self.number_style),
                    JsonToken::Literal(s) => (s.as_str(), self.literal_style),
                    JsonToken::Punctuation(s) => (s.as_str(), self.punct_style),
                    JsonToken::Whitespace(s) => (s.as_str(), Style::default()),
                    JsonToken::Error(s) => (s.as_str(), self.error_style),
                    JsonToken::Newline => continue,
                };

                if deg.apply_styling() {
                    x = draw_text_span(frame, x, y, text, style, max_x);
                } else {
                    x = draw_text_span(frame, x, y, text, Style::default(), max_x);
                }
            }
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn empty_source() {
        let view = JsonView::new("");
        assert!(view.formatted_lines().is_empty());
    }

    #[test]
    fn simple_object() {
        let view = JsonView::new(r#"{"a": 1}"#);
        let lines = view.formatted_lines();
        assert!(lines.len() >= 3); // { + content + }
    }

    #[test]
    fn nested_object() {
        let view = JsonView::new(r#"{"a": {"b": 2}}"#);
        let lines = view.formatted_lines();
        assert!(lines.len() >= 3);
    }

    #[test]
    fn array() {
        let view = JsonView::new(r#"[1, 2, 3]"#);
        let lines = view.formatted_lines();
        assert!(lines.len() >= 3);
    }

    #[test]
    fn empty_object() {
        let view = JsonView::new(r#"{}"#);
        let lines = view.formatted_lines();
        assert!(!lines.is_empty());
        // Should be compact: single line with {}
    }

    #[test]
    fn empty_array() {
        let view = JsonView::new(r#"[]"#);
        let lines = view.formatted_lines();
        assert!(!lines.is_empty());
    }

    #[test]
    fn string_values() {
        let view = JsonView::new(r#"{"msg": "hello world"}"#);
        let lines = view.formatted_lines();
        // Should contain StringVal token with quoted string
        let has_string = lines.iter().any(|line| {
            line.iter().any(|t| matches!(t, JsonToken::StringVal(s) if s.contains("hello")))
        });
        assert!(has_string);
    }

    #[test]
    fn boolean_and_null() {
        let view = JsonView::new(r#"{"a": true, "b": false, "c": null}"#);
        let lines = view.formatted_lines();
        let has_literal = lines.iter().any(|line| {
            line.iter().any(|t| matches!(t, JsonToken::Literal(s) if s == "true"))
        });
        assert!(has_literal);
    }

    #[test]
    fn numbers() {
        let view = JsonView::new(r#"{"x": 42, "y": -3.14}"#);
        let lines = view.formatted_lines();
        let has_number = lines.iter().any(|line| {
            line.iter().any(|t| matches!(t, JsonToken::Number(s) if s == "42"))
        });
        assert!(has_number);
    }

    #[test]
    fn escaped_string() {
        let view = JsonView::new(r#"{"msg": "hello \"world\""}"#);
        let lines = view.formatted_lines();
        let has_escaped = lines.iter().any(|line| {
            line.iter().any(|t| matches!(t, JsonToken::StringVal(s) if s.contains("\\\"")))
        });
        assert!(has_escaped);
    }

    #[test]
    fn indent_width() {
        let view = JsonView::new(r#"{"a": 1}"#).with_indent(4);
        let lines = view.formatted_lines();
        let has_4_indent = lines.iter().any(|line| {
            line.iter().any(|t| matches!(t, JsonToken::Whitespace(s) if s == "    "))
        });
        assert!(has_4_indent);
    }

    #[test]
    fn render_basic() {
        let view = JsonView::new(r#"{"key": "value"}"#);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);
        view.render(area, &mut frame);

        // First char should be '{'
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('{'));
    }

    #[test]
    fn render_zero_area() {
        let view = JsonView::new(r#"{"a": 1}"#);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        view.render(Rect::new(0, 0, 0, 0), &mut frame); // No panic
    }

    #[test]
    fn render_truncated_height() {
        let view = JsonView::new(r#"{"a": 1, "b": 2, "c": 3}"#);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 2, &mut pool);
        let area = Rect::new(0, 0, 40, 2);
        view.render(area, &mut frame); // Only first 2 lines, no panic
    }

    #[test]
    fn is_not_essential() {
        let view = JsonView::new("");
        assert!(!view.is_essential());
    }

    #[test]
    fn default_impl() {
        let view = JsonView::default();
        assert!(view.source().is_empty());
    }

    #[test]
    fn set_source() {
        let mut view = JsonView::new("");
        view.set_source(r#"{"a": 1}"#);
        assert!(!view.formatted_lines().is_empty());
    }

    #[test]
    fn plain_literal() {
        let view = JsonView::new("42");
        let lines = view.formatted_lines();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn classify_literal_types() {
        assert_eq!(classify_literal("true"), JsonToken::Literal("true".to_string()));
        assert_eq!(classify_literal("false"), JsonToken::Literal("false".to_string()));
        assert_eq!(classify_literal("null"), JsonToken::Literal("null".to_string()));
        assert_eq!(classify_literal("42"), JsonToken::Number("42".to_string()));
        assert_eq!(classify_literal("-3.14"), JsonToken::Number("-3.14".to_string()));
        assert!(matches!(classify_literal("invalid!"), JsonToken::Error(_)));
    }
}

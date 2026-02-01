#![forbid(unsafe_code)]

//! Syntax tokenization engine for highlighting.
//!
//! This module provides a token model, tokenizer trait, registry, and a generic
//! tokenizer that handles common patterns (strings, comments, numbers, keywords).
//! Language-specific tokenizers are delegated to `bd-3ky.13`.
//!
//! Feature-gated behind `syntax`. Zero impact on core rendering when disabled.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Token kinds
// ---------------------------------------------------------------------------

/// Semantic token categories for syntax highlighting.
///
/// Sub-categories (e.g., `KeywordControl` vs `Keyword`) allow themes to assign
/// different styles to different semantic roles while keeping a flat enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // Keywords
    Keyword,
    KeywordControl,
    KeywordType,
    KeywordModifier,

    // Literals
    String,
    StringEscape,
    Number,
    Boolean,

    // Identifiers
    Identifier,
    Type,
    Constant,
    Function,
    Macro,

    // Comments
    Comment,
    CommentBlock,
    CommentDoc,

    // Operators and punctuation
    Operator,
    Punctuation,
    Delimiter,

    // Special
    Attribute,
    Lifetime,
    Label,

    // Markup
    Heading,
    Link,
    Emphasis,

    // Whitespace and errors
    Whitespace,
    Error,

    // Default / plain text
    Text,
}

impl TokenKind {
    /// Whether this kind is a comment variant.
    pub fn is_comment(self) -> bool {
        matches!(self, Self::Comment | Self::CommentBlock | Self::CommentDoc)
    }

    /// Whether this kind is a string variant.
    pub fn is_string(self) -> bool {
        matches!(self, Self::String | Self::StringEscape)
    }

    /// Whether this kind is a keyword variant.
    pub fn is_keyword(self) -> bool {
        matches!(
            self,
            Self::Keyword | Self::KeywordControl | Self::KeywordType | Self::KeywordModifier
        )
    }
}

// ---------------------------------------------------------------------------
// Token
// ---------------------------------------------------------------------------

/// A token with a kind and byte range in the source text.
///
/// Ranges are always byte offsets into the source. Tokens must satisfy:
/// - `range.start <= range.end`
/// - `range.end <= source.len()`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range<usize>,
    pub meta: Option<TokenMeta>,
}

/// Optional metadata attached to a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenMeta {
    /// Nesting depth (e.g., bracket depth, comment nesting level).
    pub nesting: u16,
}

impl Token {
    /// Create a token. Panics in debug builds if the range is inverted.
    pub fn new(kind: TokenKind, range: Range<usize>) -> Self {
        debug_assert!(range.start <= range.end, "token range must be ordered");
        Self {
            kind,
            range,
            meta: None,
        }
    }

    /// Create a token with nesting metadata.
    pub fn with_nesting(kind: TokenKind, range: Range<usize>, nesting: u16) -> Self {
        debug_assert!(range.start <= range.end, "token range must be ordered");
        Self {
            kind,
            range,
            meta: Some(TokenMeta { nesting }),
        }
    }

    /// Token length in bytes.
    pub fn len(&self) -> usize {
        self.range.end.saturating_sub(self.range.start)
    }

    /// Whether the token is empty.
    pub fn is_empty(&self) -> bool {
        self.range.start >= self.range.end
    }

    /// Extract the token's text from a source string.
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.range.clone()]
    }
}

// ---------------------------------------------------------------------------
// Line state
// ---------------------------------------------------------------------------

/// Lexical state carried across lines for multi-line constructs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineState {
    /// Normal code context.
    #[default]
    Normal,
    /// Inside a string literal.
    InString(StringKind),
    /// Inside a comment.
    InComment(CommentKind),
    /// Inside a raw string (the u8 is the delimiter count, e.g., `r###"`).
    InRawString(u8),
}

/// String literal variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringKind {
    Double,
    Single,
    Backtick,
    Triple,
}

/// Comment variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommentKind {
    Block,
    Doc,
    /// Nested block comment with depth counter.
    Nested(u8),
}

// ---------------------------------------------------------------------------
// Tokenizer trait
// ---------------------------------------------------------------------------

/// Core tokenizer abstraction.
///
/// Implementors produce tokens for a single line given the state from the
/// previous line. The default `tokenize()` method threads state across all
/// lines and adjusts byte offsets.
pub trait Tokenizer: Send + Sync {
    /// Human-readable name (e.g., "Rust", "Python").
    fn name(&self) -> &'static str;

    /// File extensions this tokenizer handles (without dots).
    fn extensions(&self) -> &'static [&'static str];

    /// Tokenize a single line. Returns `(tokens, state_after)`.
    ///
    /// Token ranges are byte offsets within `line` (not the full source).
    fn tokenize_line(&self, line: &str, state: LineState) -> (Vec<Token>, LineState);

    /// Tokenize a full text buffer.
    ///
    /// The default implementation splits on lines, calls `tokenize_line` for
    /// each, and adjusts token ranges to be offsets into the full source.
    /// Handles LF, CRLF, and bare CR line endings.
    fn tokenize(&self, text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut state = LineState::Normal;
        let mut offset = 0usize;
        let bytes = text.as_bytes();

        for line in text.lines() {
            let (line_tokens, new_state) = self.tokenize_line(line, state);
            for mut token in line_tokens {
                token.range.start += offset;
                token.range.end += offset;
                tokens.push(token);
            }

            offset += line.len();

            // Advance past line ending.
            if offset < bytes.len() {
                if bytes[offset] == b'\r' && offset + 1 < bytes.len() && bytes[offset + 1] == b'\n'
                {
                    offset += 2; // CRLF
                } else if bytes[offset] == b'\n' || bytes[offset] == b'\r' {
                    offset += 1; // LF or bare CR
                }
            }

            state = new_state;
        }

        tokens
    }
}

// ---------------------------------------------------------------------------
// TokenizerRegistry
// ---------------------------------------------------------------------------

/// Registry for looking up tokenizers by file extension or name.
#[derive(Default)]
pub struct TokenizerRegistry {
    tokenizers: Vec<Arc<dyn Tokenizer>>,
    by_extension: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
}

impl TokenizerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tokenizer. Later registrations for the same extension or
    /// name override earlier ones.
    pub fn register(&mut self, tokenizer: Box<dyn Tokenizer>) {
        let tokenizer: Arc<dyn Tokenizer> = Arc::from(tokenizer);
        let index = self.tokenizers.len();
        self.by_name
            .insert(tokenizer.name().to_ascii_lowercase(), index);
        for ext in tokenizer.extensions() {
            let key = ext.trim_start_matches('.').to_ascii_lowercase();
            if !key.is_empty() {
                self.by_extension.insert(key, index);
            }
        }
        self.tokenizers.push(tokenizer);
    }

    /// Look up a tokenizer by file extension (case-insensitive, dot optional).
    pub fn for_extension(&self, ext: &str) -> Option<&dyn Tokenizer> {
        let key = ext.trim_start_matches('.').to_ascii_lowercase();
        let index = self.by_extension.get(&key)?;
        self.tokenizers.get(*index).map(AsRef::as_ref)
    }

    /// Look up a tokenizer by name (case-insensitive).
    pub fn by_name(&self, name: &str) -> Option<&dyn Tokenizer> {
        let key = name.to_ascii_lowercase();
        let index = self.by_name.get(&key)?;
        self.tokenizers.get(*index).map(AsRef::as_ref)
    }

    /// Number of registered tokenizers.
    pub fn len(&self) -> usize {
        self.tokenizers.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.tokenizers.is_empty()
    }
}

// ---------------------------------------------------------------------------
// TokenizedText (incremental updates)
// ---------------------------------------------------------------------------

/// Per-line tokenization result with the ending state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenLine {
    pub tokens: Vec<Token>,
    pub state_after: LineState,
}

/// Cached tokenization for a multi-line text buffer.
#[derive(Debug, Clone, Default)]
pub struct TokenizedText {
    lines: Vec<TokenLine>,
}

impl TokenizedText {
    /// Tokenize an entire buffer from scratch (using `text.lines()`).
    pub fn from_text<T: Tokenizer>(tokenizer: &T, text: &str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        Self::from_lines(tokenizer, &lines)
    }

    /// Tokenize an explicit slice of lines (preserves empty lines).
    pub fn from_lines<T: Tokenizer>(tokenizer: &T, lines: &[&str]) -> Self {
        let mut state = LineState::Normal;
        let mut out = Vec::with_capacity(lines.len());
        for line in lines {
            let (tokens, state_after) = tokenizer.tokenize_line(line, state);
            debug_assert!(validate_tokens(line, &tokens));
            out.push(TokenLine {
                tokens,
                state_after,
            });
            state = state_after;
        }
        Self { lines: out }
    }

    /// Access tokenized lines.
    pub fn lines(&self) -> &[TokenLine] {
        &self.lines
    }

    /// Return tokens on a line that overlap the given byte range.
    pub fn tokens_in_range(&self, line_index: usize, range: Range<usize>) -> Vec<&Token> {
        let Some(line) = self.lines.get(line_index) else {
            return Vec::new();
        };
        line.tokens
            .iter()
            .filter(|token| token.range.start < range.end && token.range.end > range.start)
            .collect()
    }

    /// Incrementally re-tokenize starting at a single line edit.
    ///
    /// This re-tokenizes the edited line and continues until the line's
    /// `state_after` matches the previous cached state (no further impact).
    /// If line counts change, it falls back to full re-tokenization.
    pub fn update_line<T: Tokenizer>(&mut self, tokenizer: &T, lines: &[&str], line_index: usize) {
        if line_index >= lines.len() {
            return;
        }

        if self.lines.len() != lines.len() {
            *self = Self::from_lines(tokenizer, lines);
            return;
        }

        let mut state = if line_index == 0 {
            LineState::Normal
        } else {
            self.lines[line_index - 1].state_after
        };

        #[allow(clippy::needless_range_loop)] // idx needed to index both `lines` and `self.lines`
        for idx in line_index..lines.len() {
            let (tokens, state_after) = tokenizer.tokenize_line(lines[idx], state);
            debug_assert!(validate_tokens(lines[idx], &tokens));

            let unchanged =
                self.lines[idx].state_after == state_after && self.lines[idx].tokens == tokens;

            self.lines[idx] = TokenLine {
                tokens,
                state_after,
            };

            if unchanged {
                break;
            }

            state = state_after;
        }
    }
}

// ---------------------------------------------------------------------------
// GenericTokenizer
// ---------------------------------------------------------------------------

/// Configuration for a [`GenericTokenizer`].
pub struct GenericTokenizerConfig {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub keywords: &'static [&'static str],
    pub control_keywords: &'static [&'static str],
    pub type_keywords: &'static [&'static str],
    pub line_comment: &'static str,
    pub block_comment_start: &'static str,
    pub block_comment_end: &'static str,
}

/// A configurable tokenizer for C-family languages.
///
/// Handles the most common lexical patterns:
/// - Line comments (`//`) and block comments (`/* */`)
/// - Double-quoted and single-quoted strings with backslash escapes
/// - Decimal and hex numbers
/// - Configurable keyword sets
///
/// Language-specific tokenizers (bd-3ky.13) can use this as a base or
/// implement `Tokenizer` directly.
pub struct GenericTokenizer {
    config: GenericTokenizerConfig,
}

impl GenericTokenizer {
    /// Create a generic tokenizer with the given configuration.
    pub const fn new(config: GenericTokenizerConfig) -> Self {
        Self { config }
    }

    /// Scan a word (identifier or keyword) starting at `pos`.
    fn scan_word(&self, bytes: &[u8], pos: usize) -> (TokenKind, usize) {
        let start = pos;
        let mut end = pos;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        let word = std::str::from_utf8(&bytes[start..end]).unwrap_or("");
        let kind = if self.config.keywords.contains(&word) {
            TokenKind::Keyword
        } else if self.config.control_keywords.contains(&word) {
            TokenKind::KeywordControl
        } else if self.config.type_keywords.contains(&word) {
            TokenKind::KeywordType
        } else if word == "true" || word == "false" {
            TokenKind::Boolean
        } else if word.chars().next().is_some_and(|c| c.is_uppercase()) {
            TokenKind::Type
        } else {
            TokenKind::Identifier
        };
        (kind, end)
    }

    /// Scan a number starting at `pos`.
    fn scan_number(&self, bytes: &[u8], pos: usize) -> usize {
        let mut end = pos;
        // Hex prefix
        if end + 1 < bytes.len() && bytes[end] == b'0' && (bytes[end + 1] | 0x20) == b'x' {
            end += 2;
            while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                end += 1;
            }
            return end;
        }
        // Decimal (with optional dot and exponent)
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end < bytes.len()
            && bytes[end] == b'.'
            && end + 1 < bytes.len()
            && bytes[end + 1].is_ascii_digit()
        {
            end += 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
        }
        // Type suffix (e.g., u32, f64)
        if end < bytes.len() && bytes[end].is_ascii_alphabetic() {
            while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
                end += 1;
            }
        }
        end
    }

    /// Scan a string literal starting at `pos` (the opening quote).
    fn scan_string(&self, bytes: &[u8], pos: usize) -> (usize, bool) {
        let quote = bytes[pos];
        let mut end = pos + 1;
        while end < bytes.len() {
            if bytes[end] == b'\\' {
                // Skip escaped character, but don't go past end of line
                end = (end + 2).min(bytes.len());
            } else if bytes[end] == quote {
                return (end + 1, true); // closed
            } else {
                end += 1;
            }
        }
        (end, false) // unclosed (continues on next line)
    }

    /// Continue scanning a block comment.
    fn continue_block_comment(&self, line: &str) -> (Vec<Token>, LineState) {
        let end_pat = self.config.block_comment_end;
        if let Some(end_pos) = line.find(end_pat) {
            let comment_end = end_pos + end_pat.len();
            let mut tokens = vec![Token::new(TokenKind::CommentBlock, 0..comment_end)];
            // Tokenize the rest of the line normally.
            let rest = &line[comment_end..];
            let (rest_tokens, rest_state) = self.tokenize_normal(rest, comment_end);
            tokens.extend(rest_tokens);
            (tokens, rest_state)
        } else {
            // Whole line is still inside the block comment.
            (
                vec![Token::new(TokenKind::CommentBlock, 0..line.len())],
                LineState::InComment(CommentKind::Block),
            )
        }
    }

    /// Continue scanning an unclosed string.
    fn continue_string(&self, line: &str, kind: StringKind) -> (Vec<Token>, LineState) {
        let quote = match kind {
            StringKind::Double => b'"',
            StringKind::Single => b'\'',
            _ => b'"',
        };
        let bytes = line.as_bytes();
        let mut end = 0;
        while end < bytes.len() {
            if bytes[end] == b'\\' {
                // Skip escaped character, but don't go past end of line
                end = (end + 2).min(bytes.len());
            } else if bytes[end] == quote {
                let tokens = vec![Token::new(TokenKind::String, 0..end + 1)];
                let rest = &line[end + 1..];
                let (mut rest_tokens, rest_state) = self.tokenize_normal(rest, end + 1);
                let mut all = tokens;
                all.append(&mut rest_tokens);
                return (all, rest_state);
            } else {
                end += 1;
            }
        }
        (
            vec![Token::new(TokenKind::String, 0..line.len())],
            LineState::InString(kind),
        )
    }

    /// Tokenize a line in normal (non-continuation) context.
    fn tokenize_normal(&self, line: &str, base_offset: usize) -> (Vec<Token>, LineState) {
        let bytes = line.as_bytes();
        let mut tokens = Vec::new();
        let mut pos = 0;

        while pos < bytes.len() {
            let ch = bytes[pos];

            // Whitespace run
            if ch.is_ascii_whitespace() {
                let start = pos;
                while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                tokens.push(Token::new(
                    TokenKind::Whitespace,
                    base_offset + start..base_offset + pos,
                ));
                continue;
            }

            // Line comment
            if !self.config.line_comment.is_empty()
                && line[pos..].starts_with(self.config.line_comment)
            {
                tokens.push(Token::new(
                    TokenKind::Comment,
                    base_offset + pos..base_offset + bytes.len(),
                ));
                return (tokens, LineState::Normal);
            }

            // Block comment start
            if !self.config.block_comment_start.is_empty()
                && line[pos..].starts_with(self.config.block_comment_start)
            {
                let start = pos;
                let after_open = pos + self.config.block_comment_start.len();
                let rest = &line[after_open..];
                if let Some(end_pos) = rest.find(self.config.block_comment_end) {
                    let comment_end = after_open + end_pos + self.config.block_comment_end.len();
                    tokens.push(Token::new(
                        TokenKind::CommentBlock,
                        base_offset + start..base_offset + comment_end,
                    ));
                    pos = comment_end;
                } else {
                    tokens.push(Token::new(
                        TokenKind::CommentBlock,
                        base_offset + start..base_offset + bytes.len(),
                    ));
                    return (tokens, LineState::InComment(CommentKind::Block));
                }
                continue;
            }

            // String literals
            if ch == b'"' || ch == b'\'' {
                let start = pos;
                let kind = if ch == b'"' {
                    StringKind::Double
                } else {
                    StringKind::Single
                };
                let (end, closed) = self.scan_string(bytes, pos);
                tokens.push(Token::new(
                    TokenKind::String,
                    base_offset + start..base_offset + end,
                ));
                if !closed {
                    return (tokens, LineState::InString(kind));
                }
                pos = end;
                continue;
            }

            // Numbers
            if ch.is_ascii_digit() {
                let start = pos;
                let end = self.scan_number(bytes, pos);
                tokens.push(Token::new(
                    TokenKind::Number,
                    base_offset + start..base_offset + end,
                ));
                pos = end;
                continue;
            }

            // Identifiers and keywords
            if ch.is_ascii_alphabetic() || ch == b'_' {
                let start = pos;
                let (kind, end) = self.scan_word(bytes, pos);
                tokens.push(Token::new(kind, base_offset + start..base_offset + end));
                pos = end;
                continue;
            }

            // Attribute (#[...] or @...)
            if ch == b'#' || ch == b'@' {
                let start = pos;
                pos += 1;
                if pos < bytes.len() && bytes[pos] == b'[' {
                    // Scan until closing ]
                    while pos < bytes.len() && bytes[pos] != b']' {
                        pos += 1;
                    }
                    if pos < bytes.len() {
                        pos += 1;
                    }
                }
                tokens.push(Token::new(
                    TokenKind::Attribute,
                    base_offset + start..base_offset + pos,
                ));
                continue;
            }

            // Delimiters
            if matches!(ch, b'(' | b')' | b'[' | b']' | b'{' | b'}') {
                tokens.push(Token::new(
                    TokenKind::Delimiter,
                    base_offset + pos..base_offset + pos + 1,
                ));
                pos += 1;
                continue;
            }

            // Operators (multi-char)
            if is_operator_byte(ch) {
                let start = pos;
                while pos < bytes.len() && is_operator_byte(bytes[pos]) {
                    pos += 1;
                }
                tokens.push(Token::new(
                    TokenKind::Operator,
                    base_offset + start..base_offset + pos,
                ));
                continue;
            }

            // Punctuation (everything else: commas, semicolons, dots, etc.)
            tokens.push(Token::new(
                TokenKind::Punctuation,
                base_offset + pos..base_offset + pos + 1,
            ));
            pos += 1;
        }

        (tokens, LineState::Normal)
    }
}

fn is_operator_byte(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'!' | b'<' | b'>' | b'&' | b'|' | b'^' | b'~'
    )
}

impl Tokenizer for GenericTokenizer {
    fn name(&self) -> &'static str {
        self.config.name
    }

    fn extensions(&self) -> &'static [&'static str] {
        self.config.extensions
    }

    fn tokenize_line(&self, line: &str, state: LineState) -> (Vec<Token>, LineState) {
        match state {
            LineState::InComment(CommentKind::Block | CommentKind::Nested(_)) => {
                self.continue_block_comment(line)
            }
            LineState::InString(kind) => self.continue_string(line, kind),
            _ => self.tokenize_normal(line, 0),
        }
    }
}

// ---------------------------------------------------------------------------
// PlainTokenizer (trivial fallback)
// ---------------------------------------------------------------------------

/// Tokenizer that treats each line as a single `Text` token.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainTokenizer;

impl Tokenizer for PlainTokenizer {
    fn name(&self) -> &'static str {
        "Plain"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["txt"]
    }

    fn tokenize_line(&self, line: &str, state: LineState) -> (Vec<Token>, LineState) {
        if line.is_empty() {
            return (Vec::new(), state);
        }
        (vec![Token::new(TokenKind::Text, 0..line.len())], state)
    }
}

// ---------------------------------------------------------------------------
// Built-in language configurations
// ---------------------------------------------------------------------------

/// Create a generic tokenizer configured for Rust.
pub fn rust_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Rust",
        extensions: &["rs"],
        keywords: &[
            "fn", "let", "mut", "const", "static", "use", "mod", "pub", "crate", "self", "super",
            "impl", "trait", "struct", "enum", "type", "where", "as", "in", "ref", "move",
            "unsafe", "extern", "async", "await", "dyn", "macro",
        ],
        control_keywords: &[
            "if", "else", "match", "for", "while", "loop", "break", "continue", "return", "yield",
        ],
        type_keywords: &[
            "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32",
            "i64", "i128", "isize", "f32", "f64", "Self", "String", "Vec", "Option", "Result",
            "Box", "Rc", "Arc",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for Python.
pub fn python_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Python",
        extensions: &["py", "pyi"],
        keywords: &[
            "def", "class", "import", "from", "as", "global", "nonlocal", "lambda", "with",
            "assert", "del", "in", "is", "not", "and", "or",
        ],
        control_keywords: &[
            "if", "elif", "else", "for", "while", "break", "continue", "return", "yield", "try",
            "except", "finally", "raise", "pass",
        ],
        type_keywords: &[
            "int", "float", "str", "bool", "list", "dict", "tuple", "set", "None", "type",
        ],
        line_comment: "#",
        block_comment_start: "",
        block_comment_end: "",
    })
}

/// Create a generic tokenizer configured for JavaScript/TypeScript.
pub fn javascript_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "JavaScript",
        extensions: &["js", "jsx", "ts", "tsx", "mjs", "cjs"],
        keywords: &[
            "function",
            "var",
            "let",
            "const",
            "class",
            "new",
            "delete",
            "typeof",
            "instanceof",
            "void",
            "this",
            "super",
            "import",
            "export",
            "default",
            "from",
            "as",
            "of",
            "in",
            "async",
            "await",
        ],
        control_keywords: &[
            "if", "else", "switch", "case", "for", "while", "do", "break", "continue", "return",
            "throw", "try", "catch", "finally", "yield",
        ],
        type_keywords: &[
            "number",
            "string",
            "boolean",
            "object",
            "symbol",
            "bigint",
            "undefined",
            "null",
            "Array",
            "Promise",
            "Map",
            "Set",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate that all token ranges are in-bounds and non-overlapping.
pub fn validate_tokens(source: &str, tokens: &[Token]) -> bool {
    let len = source.len();
    let mut prev_end = 0;
    for token in tokens {
        if token.range.start > token.range.end {
            return false;
        }
        if token.range.end > len {
            return false;
        }
        if token.range.start < prev_end {
            return false; // overlapping
        }
        prev_end = token.range.end;
    }
    true
}

// ---------------------------------------------------------------------------
// Highlight Themes
// ---------------------------------------------------------------------------

use ftui_style::Style;

/// A theme that maps token kinds to styles for syntax highlighting.
///
/// # Example
/// ```ignore
/// use ftui_extras::syntax::{HighlightTheme, TokenKind, rust_tokenizer};
/// use ftui_style::Style;
/// use ftui_render::cell::PackedRgba;
///
/// let theme = HighlightTheme::dark();
/// let tokenizer = rust_tokenizer();
/// let tokens = tokenizer.tokenize("let x = 42;");
///
/// for token in &tokens {
///     let style = theme.style_for(token.kind);
///     // Apply style to render the token...
/// }
/// ```
#[derive(Debug, Clone, Default)]
pub struct HighlightTheme {
    /// Style for keywords (`fn`, `let`, `pub`, etc.)
    pub keyword: Style,
    /// Style for control flow keywords (`if`, `else`, `return`, etc.)
    pub keyword_control: Style,
    /// Style for type keywords (`u32`, `String`, `bool`, etc.)
    pub keyword_type: Style,
    /// Style for modifier keywords
    pub keyword_modifier: Style,
    /// Style for string literals
    pub string: Style,
    /// Style for escape sequences in strings
    pub string_escape: Style,
    /// Style for numeric literals
    pub number: Style,
    /// Style for boolean literals
    pub boolean: Style,
    /// Style for identifiers
    pub identifier: Style,
    /// Style for type names
    pub type_name: Style,
    /// Style for constants
    pub constant: Style,
    /// Style for function names
    pub function: Style,
    /// Style for macros
    pub macro_name: Style,
    /// Style for line comments
    pub comment: Style,
    /// Style for block comments
    pub comment_block: Style,
    /// Style for doc comments
    pub comment_doc: Style,
    /// Style for operators
    pub operator: Style,
    /// Style for punctuation
    pub punctuation: Style,
    /// Style for delimiters (brackets, braces, parens)
    pub delimiter: Style,
    /// Style for attributes (`#[...]`)
    pub attribute: Style,
    /// Style for lifetimes (`'a`)
    pub lifetime: Style,
    /// Style for labels
    pub label: Style,
    /// Style for headings (markup)
    pub heading: Style,
    /// Style for links (markup)
    pub link: Style,
    /// Style for emphasis (markup)
    pub emphasis: Style,
    /// Style for whitespace (usually empty)
    pub whitespace: Style,
    /// Style for errors
    pub error: Style,
    /// Style for plain text (fallback)
    pub text: Style,
}

impl HighlightTheme {
    /// Create a new theme with all empty styles (inherit from parent).
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the style for a given token kind.
    #[must_use]
    pub fn style_for(&self, kind: TokenKind) -> Style {
        match kind {
            TokenKind::Keyword => self.keyword,
            TokenKind::KeywordControl => self.keyword_control,
            TokenKind::KeywordType => self.keyword_type,
            TokenKind::KeywordModifier => self.keyword_modifier,
            TokenKind::String => self.string,
            TokenKind::StringEscape => self.string_escape,
            TokenKind::Number => self.number,
            TokenKind::Boolean => self.boolean,
            TokenKind::Identifier => self.identifier,
            TokenKind::Type => self.type_name,
            TokenKind::Constant => self.constant,
            TokenKind::Function => self.function,
            TokenKind::Macro => self.macro_name,
            TokenKind::Comment => self.comment,
            TokenKind::CommentBlock => self.comment_block,
            TokenKind::CommentDoc => self.comment_doc,
            TokenKind::Operator => self.operator,
            TokenKind::Punctuation => self.punctuation,
            TokenKind::Delimiter => self.delimiter,
            TokenKind::Attribute => self.attribute,
            TokenKind::Lifetime => self.lifetime,
            TokenKind::Label => self.label,
            TokenKind::Heading => self.heading,
            TokenKind::Link => self.link,
            TokenKind::Emphasis => self.emphasis,
            TokenKind::Whitespace => self.whitespace,
            TokenKind::Error => self.error,
            TokenKind::Text => self.text,
        }
    }

    /// Create a dark theme with sensible defaults.
    ///
    /// Colors are chosen for readability on dark backgrounds.
    #[must_use]
    pub fn dark() -> Self {
        use ftui_render::cell::PackedRgba;

        // Color palette for dark theme
        let purple = PackedRgba::rgb(198, 120, 221); // Keywords
        let blue = PackedRgba::rgb(97, 175, 239); // Types, functions
        let cyan = PackedRgba::rgb(86, 182, 194); // Strings
        let green = PackedRgba::rgb(152, 195, 121); // Comments
        let orange = PackedRgba::rgb(209, 154, 102); // Numbers, constants
        let red = PackedRgba::rgb(224, 108, 117); // Errors, control
        let yellow = PackedRgba::rgb(229, 192, 123); // Attributes, macros
        let gray = PackedRgba::rgb(92, 99, 112); // Punctuation

        Self {
            keyword: Style::new().fg(purple).bold(),
            keyword_control: Style::new().fg(red),
            keyword_type: Style::new().fg(blue),
            keyword_modifier: Style::new().fg(purple),
            string: Style::new().fg(cyan),
            string_escape: Style::new().fg(orange),
            number: Style::new().fg(orange),
            boolean: Style::new().fg(orange),
            identifier: Style::new(),
            type_name: Style::new().fg(blue),
            constant: Style::new().fg(orange),
            function: Style::new().fg(blue),
            macro_name: Style::new().fg(yellow),
            comment: Style::new().fg(green).italic(),
            comment_block: Style::new().fg(green).italic(),
            comment_doc: Style::new().fg(green).italic(),
            operator: Style::new().fg(gray),
            punctuation: Style::new().fg(gray),
            delimiter: Style::new().fg(gray),
            attribute: Style::new().fg(yellow),
            lifetime: Style::new().fg(orange),
            label: Style::new().fg(orange),
            heading: Style::new().fg(blue).bold(),
            link: Style::new().fg(cyan).underline(),
            emphasis: Style::new().italic(),
            whitespace: Style::new(),
            error: Style::new().fg(red).bold(),
            text: Style::new(),
        }
    }

    /// Create a light theme with sensible defaults.
    ///
    /// Colors are chosen for readability on light backgrounds.
    #[must_use]
    pub fn light() -> Self {
        use ftui_render::cell::PackedRgba;

        // Color palette for light theme (darker, more saturated)
        let purple = PackedRgba::rgb(136, 57, 169); // Keywords
        let blue = PackedRgba::rgb(0, 92, 197); // Types, functions
        let cyan = PackedRgba::rgb(0, 128, 128); // Strings
        let green = PackedRgba::rgb(80, 120, 60); // Comments
        let orange = PackedRgba::rgb(152, 104, 1); // Numbers, constants
        let red = PackedRgba::rgb(193, 52, 52); // Errors, control
        let yellow = PackedRgba::rgb(133, 100, 4); // Attributes, macros
        let gray = PackedRgba::rgb(95, 99, 104); // Punctuation

        Self {
            keyword: Style::new().fg(purple).bold(),
            keyword_control: Style::new().fg(red),
            keyword_type: Style::new().fg(blue),
            keyword_modifier: Style::new().fg(purple),
            string: Style::new().fg(cyan),
            string_escape: Style::new().fg(orange),
            number: Style::new().fg(orange),
            boolean: Style::new().fg(orange),
            identifier: Style::new(),
            type_name: Style::new().fg(blue),
            constant: Style::new().fg(orange),
            function: Style::new().fg(blue),
            macro_name: Style::new().fg(yellow),
            comment: Style::new().fg(green).italic(),
            comment_block: Style::new().fg(green).italic(),
            comment_doc: Style::new().fg(green).italic(),
            operator: Style::new().fg(gray),
            punctuation: Style::new().fg(gray),
            delimiter: Style::new().fg(gray),
            attribute: Style::new().fg(yellow),
            lifetime: Style::new().fg(orange),
            label: Style::new().fg(orange),
            heading: Style::new().fg(blue).bold(),
            link: Style::new().fg(cyan).underline(),
            emphasis: Style::new().italic(),
            whitespace: Style::new(),
            error: Style::new().fg(red).bold(),
            text: Style::new(),
        }
    }

    /// Create a builder for constructing a custom theme.
    pub fn builder() -> HighlightThemeBuilder {
        HighlightThemeBuilder::new()
    }
}

/// Builder for constructing custom highlight themes.
#[derive(Debug, Clone, Default)]
pub struct HighlightThemeBuilder {
    theme: HighlightTheme,
}

impl HighlightThemeBuilder {
    /// Create a new builder with empty styles.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start from an existing theme.
    pub fn from_theme(theme: HighlightTheme) -> Self {
        Self { theme }
    }

    /// Set the keyword style.
    pub fn keyword(mut self, style: Style) -> Self {
        self.theme.keyword = style;
        self
    }

    /// Set the control keyword style.
    pub fn keyword_control(mut self, style: Style) -> Self {
        self.theme.keyword_control = style;
        self
    }

    /// Set the type keyword style.
    pub fn keyword_type(mut self, style: Style) -> Self {
        self.theme.keyword_type = style;
        self
    }

    /// Set the string literal style.
    pub fn string(mut self, style: Style) -> Self {
        self.theme.string = style;
        self
    }

    /// Set the number literal style.
    pub fn number(mut self, style: Style) -> Self {
        self.theme.number = style;
        self
    }

    /// Set the comment style (applies to all comment variants).
    pub fn comment(mut self, style: Style) -> Self {
        self.theme.comment = style;
        self.theme.comment_block = style;
        self.theme.comment_doc = style;
        self
    }

    /// Set the type name style.
    pub fn type_name(mut self, style: Style) -> Self {
        self.theme.type_name = style;
        self
    }

    /// Set the function name style.
    pub fn function(mut self, style: Style) -> Self {
        self.theme.function = style;
        self
    }

    /// Set the operator style.
    pub fn operator(mut self, style: Style) -> Self {
        self.theme.operator = style;
        self
    }

    /// Set the error style.
    pub fn error(mut self, style: Style) -> Self {
        self.theme.error = style;
        self
    }

    /// Build the final theme.
    pub fn build(self) -> HighlightTheme {
        self.theme
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Token basics -------------------------------------------------------

    #[test]
    fn token_new_and_accessors() {
        let t = Token::new(TokenKind::Keyword, 2..8);
        assert_eq!(t.kind, TokenKind::Keyword);
        assert_eq!(t.range, 2..8);
        assert_eq!(t.len(), 6);
        assert!(!t.is_empty());
        assert!(t.meta.is_none());
    }

    #[test]
    fn token_empty() {
        let t = Token::new(TokenKind::Text, 5..5);
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn token_with_nesting() {
        let t = Token::with_nesting(TokenKind::Delimiter, 0..1, 3);
        assert_eq!(t.meta.unwrap().nesting, 3);
    }

    #[test]
    fn token_text_extraction() {
        let source = "let x = 42;";
        let t = Token::new(TokenKind::Identifier, 4..5);
        assert_eq!(t.text(source), "x");
    }

    // -- TokenKind predicates -----------------------------------------------

    #[test]
    fn token_kind_predicates() {
        assert!(TokenKind::Comment.is_comment());
        assert!(TokenKind::CommentBlock.is_comment());
        assert!(TokenKind::CommentDoc.is_comment());
        assert!(!TokenKind::Keyword.is_comment());

        assert!(TokenKind::String.is_string());
        assert!(TokenKind::StringEscape.is_string());
        assert!(!TokenKind::Number.is_string());

        assert!(TokenKind::Keyword.is_keyword());
        assert!(TokenKind::KeywordControl.is_keyword());
        assert!(TokenKind::KeywordType.is_keyword());
        assert!(!TokenKind::Identifier.is_keyword());
    }

    // -- PlainTokenizer -----------------------------------------------------

    #[test]
    fn plain_tokenizer_single_text_token() {
        let t = PlainTokenizer;
        let (tokens, state) = t.tokenize_line("hello", LineState::Normal);
        assert_eq!(state, LineState::Normal);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Text);
        assert_eq!(tokens[0].range, 0..5);
    }

    #[test]
    fn plain_tokenizer_empty_line() {
        let t = PlainTokenizer;
        let (tokens, _) = t.tokenize_line("", LineState::Normal);
        assert!(tokens.is_empty());
    }

    #[test]
    fn plain_tokenizer_full_text() {
        let t = PlainTokenizer;
        let tokens = t.tokenize("one\ntwo\nthree");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].range, 0..3);
        assert_eq!(tokens[1].range, 4..7);
        assert_eq!(tokens[2].range, 8..13);
    }

    // -- GenericTokenizer: Rust ---------------------------------------------

    #[test]
    fn rust_keywords() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("fn main let", LineState::Normal);
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Keyword,
                TokenKind::Identifier,
                TokenKind::Keyword
            ]
        );
    }

    #[test]
    fn rust_control_keywords() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("if else return", LineState::Normal);
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(kinds, vec![TokenKind::KeywordControl; 3]);
    }

    #[test]
    fn rust_type_keywords() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("u32 String", LineState::Normal);
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(kinds, vec![TokenKind::KeywordType, TokenKind::KeywordType]);
    }

    #[test]
    fn rust_uppercase_is_type() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("MyStruct", LineState::Normal);
        assert_eq!(tokens[0].kind, TokenKind::Type);
    }

    #[test]
    fn rust_booleans() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("true false", LineState::Normal);
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(kinds, vec![TokenKind::Boolean, TokenKind::Boolean]);
    }

    // -- Numbers ------------------------------------------------------------

    #[test]
    fn numbers_decimal() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("42 3.14 0xff", LineState::Normal);
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(kinds, vec![TokenKind::Number; 3]);
    }

    #[test]
    fn number_with_suffix() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("42u32", LineState::Normal);
        assert_eq!(tokens[0].kind, TokenKind::Number);
        assert_eq!(tokens[0].range, 0..5);
    }

    // -- Strings ------------------------------------------------------------

    #[test]
    fn string_double_quoted() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line(r#""hello""#, LineState::Normal);
        assert_eq!(tokens[0].kind, TokenKind::String);
        assert_eq!(tokens[0].range, 0..7);
    }

    #[test]
    fn string_with_escape() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line(r#""he\"llo""#, LineState::Normal);
        assert_eq!(tokens[0].kind, TokenKind::String);
        // The escaped quote should not end the string.
        assert_eq!(
            tokens
                .iter()
                .filter(|t| t.kind == TokenKind::String)
                .count(),
            1
        );
    }

    #[test]
    fn string_unclosed_continues_next_line() {
        let t = rust_tokenizer();
        let (tokens, state) = t.tokenize_line(r#""hello"#, LineState::Normal);
        assert_eq!(tokens[0].kind, TokenKind::String);
        assert_eq!(state, LineState::InString(StringKind::Double));

        // Continue on next line
        let (tokens2, state2) = t.tokenize_line(r#"world""#, state);
        assert_eq!(tokens2[0].kind, TokenKind::String);
        assert_eq!(state2, LineState::Normal);
    }

    #[test]
    fn string_trailing_backslash_at_eol() {
        // Edge case: string ends with backslash at end of line
        let t = rust_tokenizer();
        let input = r#""hello\"#; // unclosed string ending with backslash
        let (tokens, state) = t.tokenize_line(input, LineState::Normal);

        // Token range must not exceed input length
        assert!(
            tokens[0].range.end <= input.len(),
            "Token range {:?} exceeds input length {}",
            tokens[0].range,
            input.len()
        );
        assert_eq!(tokens[0].kind, TokenKind::String);
        assert!(matches!(state, LineState::InString(_)));

        // Should be able to extract text without panic
        let _ = tokens[0].text(input);
    }

    #[test]
    fn string_continuation_trailing_backslash() {
        // Edge case: continued string line ends with backslash
        let t = rust_tokenizer();
        let (_, state) = t.tokenize_line(r#""start"#, LineState::Normal);

        let continued = r#"middle\"#; // continuation ending with backslash
        let (tokens, state2) = t.tokenize_line(continued, state);

        assert!(
            tokens[0].range.end <= continued.len(),
            "Token range {:?} exceeds input length {}",
            tokens[0].range,
            continued.len()
        );
        assert!(matches!(state2, LineState::InString(_)));
    }

    // -- Comments -----------------------------------------------------------

    #[test]
    fn line_comment() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("x // comment", LineState::Normal);
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(kinds, vec![TokenKind::Identifier, TokenKind::Comment]);
    }

    #[test]
    fn block_comment_single_line() {
        let t = rust_tokenizer();
        let (tokens, state) = t.tokenize_line("x /* comment */ y", LineState::Normal);
        assert_eq!(state, LineState::Normal);
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Identifier,
                TokenKind::CommentBlock,
                TokenKind::Identifier
            ]
        );
    }

    #[test]
    fn block_comment_multiline() {
        let t = rust_tokenizer();

        // Line 1: opens block comment
        let (tokens1, state1) = t.tokenize_line("x /* start", LineState::Normal);
        assert_eq!(state1, LineState::InComment(CommentKind::Block));
        assert_eq!(tokens1.last().unwrap().kind, TokenKind::CommentBlock);

        // Line 2: still in block comment
        let (tokens2, state2) = t.tokenize_line("middle", state1);
        assert_eq!(state2, LineState::InComment(CommentKind::Block));
        assert_eq!(tokens2[0].kind, TokenKind::CommentBlock);

        // Line 3: closes block comment
        let (tokens3, state3) = t.tokenize_line("end */ y", state2);
        assert_eq!(state3, LineState::Normal);
        assert_eq!(tokens3[0].kind, TokenKind::CommentBlock);
    }

    // -- Python comments use # ----------------------------------------------

    #[test]
    fn python_line_comment() {
        let t = python_tokenizer();
        let (tokens, _) = t.tokenize_line("x = 1 # comment", LineState::Normal);
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert!(kinds.contains(&TokenKind::Comment));
    }

    // -- Operators and delimiters -------------------------------------------

    #[test]
    fn operators_and_delimiters() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("a + b()", LineState::Normal);
        let kinds: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Identifier,
                TokenKind::Operator,
                TokenKind::Identifier,
                TokenKind::Delimiter,
                TokenKind::Delimiter,
            ]
        );
    }

    #[test]
    fn multi_char_operator() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("a >= b", LineState::Normal);
        let op_tokens: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Operator)
            .collect();
        assert_eq!(op_tokens.len(), 1);
        assert_eq!(op_tokens[0].range.end - op_tokens[0].range.start, 2);
    }

    // -- Attributes ---------------------------------------------------------

    #[test]
    fn attribute_hash_bracket() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("#[derive(Debug)]", LineState::Normal);
        assert_eq!(tokens[0].kind, TokenKind::Attribute);
    }

    // -- Validation ---------------------------------------------------------

    #[test]
    fn validate_tokens_accepts_valid() {
        let source = "let x = 42;";
        let tokens = vec![
            Token::new(TokenKind::Keyword, 0..3),
            Token::new(TokenKind::Whitespace, 3..4),
            Token::new(TokenKind::Identifier, 4..5),
        ];
        assert!(validate_tokens(source, &tokens));
    }

    #[test]
    fn validate_tokens_rejects_out_of_bounds() {
        let source = "abc";
        let tokens = vec![Token::new(TokenKind::Text, 0..10)];
        assert!(!validate_tokens(source, &tokens));
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn validate_tokens_rejects_inverted_range() {
        let source = "abc";
        let tokens = vec![Token {
            kind: TokenKind::Text,
            range: 3..1, // intentionally invalid
            meta: None,
        }];
        assert!(!validate_tokens(source, &tokens));
    }

    #[test]
    fn validate_tokens_rejects_overlap() {
        let source = "abcdef";
        let tokens = vec![
            Token::new(TokenKind::Text, 0..4),
            Token::new(TokenKind::Text, 2..6),
        ];
        assert!(!validate_tokens(source, &tokens));
    }

    // -- Full tokenize (multi-line) -----------------------------------------

    #[test]
    fn full_tokenize_threads_state() {
        let t = rust_tokenizer();
        let tokens = t.tokenize("fn main() {\n    42\n}");
        assert!(validate_tokens("fn main() {\n    42\n}", &tokens));
        // Should contain at least: keyword, identifier, delimiters, number
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert!(kinds.contains(&TokenKind::Keyword));
        assert!(kinds.contains(&TokenKind::Number));
        assert!(kinds.contains(&TokenKind::Delimiter));
    }

    #[test]
    fn full_tokenize_crlf() {
        let t = rust_tokenizer();
        let source = "let\r\nx";
        let tokens = t.tokenize(source);
        assert!(validate_tokens(source, &tokens));
        let non_ws: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .collect();
        assert_eq!(non_ws.len(), 2);
        assert_eq!(non_ws[0].text(source), "let");
        assert_eq!(non_ws[1].text(source), "x");
    }

    #[test]
    fn full_tokenize_empty_lines() {
        let t = PlainTokenizer;
        let tokens = t.tokenize("a\n\nb");
        assert_eq!(tokens.len(), 2); // empty line produces no token
        assert_eq!(tokens[0].range, 0..1);
        assert_eq!(tokens[1].range, 3..4);
    }

    // -- Registry -----------------------------------------------------------

    #[test]
    fn registry_register_and_lookup() {
        let mut reg = TokenizerRegistry::new();
        assert!(reg.is_empty());
        reg.register(Box::new(PlainTokenizer));
        assert_eq!(reg.len(), 1);
        assert!(reg.for_extension("txt").is_some());
        assert!(reg.for_extension(".TXT").is_some());
        assert!(reg.by_name("plain").is_some());
        assert!(reg.by_name("PLAIN").is_some());
        assert!(reg.for_extension("rs").is_none());
    }

    #[test]
    fn registry_override() {
        let mut reg = TokenizerRegistry::new();
        reg.register(Box::new(PlainTokenizer));
        // Register Rust tokenizer, which doesn't handle "txt"
        reg.register(Box::new(rust_tokenizer()));
        assert!(reg.for_extension("rs").is_some());
        assert!(reg.for_extension("txt").is_some()); // Plain still registered
        assert_eq!(reg.len(), 2);
    }

    // -- TokenizedText ------------------------------------------------------

    #[test]
    fn tokenized_text_tokens_in_range() {
        let t = rust_tokenizer();
        let lines = ["let x = 1", "x"];
        let cache = TokenizedText::from_lines(&t, &lines);
        let hits = cache.tokens_in_range(0, 4..5);
        assert!(hits.iter().any(|token| token.kind == TokenKind::Identifier));
    }

    #[test]
    fn tokenized_text_update_line_propagates_state() {
        let t = rust_tokenizer();
        let lines = ["\"hello", "world"];
        let mut cache = TokenizedText::from_lines(&t, &lines);
        assert!(matches!(
            cache.lines()[0].state_after,
            LineState::InString(_)
        ));

        let updated = ["\"hello\"", "world"];
        cache.update_line(&t, &updated, 0);
        assert_eq!(cache.lines()[0].state_after, LineState::Normal);

        let kinds: Vec<_> = cache.lines()[1]
            .tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| t.kind)
            .collect();
        assert_eq!(kinds, vec![TokenKind::Identifier]);
    }

    // -- Trait bounds -------------------------------------------------------

    #[test]
    fn tokenizer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PlainTokenizer>();
        assert_send_sync::<GenericTokenizer>();
    }

    #[test]
    fn token_kind_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<TokenKind>();
        assert_copy::<LineState>();
        assert_copy::<StringKind>();
        assert_copy::<CommentKind>();
    }

    // -- Edge cases ---------------------------------------------------------

    #[test]
    fn empty_input() {
        let t = rust_tokenizer();
        let tokens = t.tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn whitespace_only_line() {
        let t = rust_tokenizer();
        let (tokens, _) = t.tokenize_line("   \t  ", LineState::Normal);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Whitespace);
    }

    #[test]
    fn all_tokens_have_valid_ranges() {
        let t = rust_tokenizer();
        let source = r#"
fn main() {
    let x: u32 = 42; // answer
    let s = "hello \"world\"";
    /* block
       comment */
    if x > 0 {
        println!("yes");
    }
}
"#;
        let tokens = t.tokenize(source);
        assert!(
            validate_tokens(source, &tokens),
            "Token validation failed for complex Rust source"
        );
        // Every token range should extract valid UTF-8
        for token in &tokens {
            let _ = token.text(source);
        }
    }

    // -- HighlightTheme tests -----------------------------------------------

    #[test]
    fn highlight_theme_dark_returns_all_token_kinds() {
        let theme = HighlightTheme::dark();

        // Verify all token kinds return a style (no panics)
        for kind in [
            TokenKind::Keyword,
            TokenKind::KeywordControl,
            TokenKind::KeywordType,
            TokenKind::KeywordModifier,
            TokenKind::String,
            TokenKind::StringEscape,
            TokenKind::Number,
            TokenKind::Boolean,
            TokenKind::Identifier,
            TokenKind::Type,
            TokenKind::Constant,
            TokenKind::Function,
            TokenKind::Macro,
            TokenKind::Comment,
            TokenKind::CommentBlock,
            TokenKind::CommentDoc,
            TokenKind::Operator,
            TokenKind::Punctuation,
            TokenKind::Delimiter,
            TokenKind::Attribute,
            TokenKind::Lifetime,
            TokenKind::Label,
            TokenKind::Heading,
            TokenKind::Link,
            TokenKind::Emphasis,
            TokenKind::Whitespace,
            TokenKind::Error,
            TokenKind::Text,
        ] {
            let _ = theme.style_for(kind);
        }
    }

    #[test]
    fn highlight_theme_light_returns_all_token_kinds() {
        let theme = HighlightTheme::light();

        // All kinds should work
        let _ = theme.style_for(TokenKind::Keyword);
        let _ = theme.style_for(TokenKind::String);
        let _ = theme.style_for(TokenKind::Comment);
        let _ = theme.style_for(TokenKind::Error);
    }

    #[test]
    fn highlight_theme_dark_keywords_are_styled() {
        let theme = HighlightTheme::dark();

        // Keywords should have some styling (fg color or attrs)
        let keyword_style = theme.style_for(TokenKind::Keyword);
        assert!(
            keyword_style.fg.is_some() || keyword_style.attrs.is_some(),
            "Keyword style should have fg or attrs"
        );
    }

    #[test]
    fn highlight_theme_builder_works() {
        use ftui_render::cell::PackedRgba;

        let theme = HighlightTheme::builder()
            .keyword(Style::new().fg(PackedRgba::rgb(255, 0, 0)).bold())
            .string(Style::new().fg(PackedRgba::rgb(0, 255, 0)))
            .comment(Style::new().fg(PackedRgba::rgb(128, 128, 128)).italic())
            .build();

        // Verify the styles were applied
        assert!(theme.keyword.fg.is_some());
        assert!(theme.string.fg.is_some());
        assert!(theme.comment.fg.is_some());
    }

    #[test]
    fn highlight_theme_builder_from_existing() {
        let base = HighlightTheme::dark();
        let theme = HighlightThemeBuilder::from_theme(base.clone())
            .error(Style::new().bold())
            .build();

        // Error was customized
        assert!(theme.error.attrs.is_some());
        // Other styles preserved from base
        assert_eq!(theme.keyword.fg, base.keyword.fg);
    }

    #[test]
    fn highlight_theme_new_is_empty() {
        let theme = HighlightTheme::new();

        // All styles should be default (empty)
        assert!(theme.keyword.fg.is_none());
        assert!(theme.keyword.bg.is_none());
        assert!(theme.keyword.attrs.is_none());
    }

    #[test]
    fn highlight_theme_style_for_covers_all_variants() {
        // This test ensures the match in style_for is exhaustive
        // If a TokenKind variant is added but not handled, this won't compile
        let theme = HighlightTheme::new();
        let kind = TokenKind::Text; // arbitrary
        let _ = theme.style_for(kind);
    }

    #[test]
    fn highlight_theme_integration_with_tokenizer() {
        let theme = HighlightTheme::dark();
        let tokenizer = rust_tokenizer();

        let source = "fn main() { let x = 42; }";
        let tokens = tokenizer.tokenize(source);

        // Should be able to get styles for all tokens
        for token in &tokens {
            let style = theme.style_for(token.kind);
            // Style shouldn't panic
            let _ = style;
        }
    }
}

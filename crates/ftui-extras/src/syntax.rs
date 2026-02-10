#![forbid(unsafe_code)]

//! Syntax tokenization engine for highlighting.
//!
//! This module provides a token model, tokenizer trait, registry, and a generic
//! tokenizer that handles common patterns (strings, comments, numbers, keywords).
//! Language-specific tokenizers are delegated to `bd-3ky.13`.
//!
//! Feature-gated behind `syntax`. Zero impact on core rendering when disabled.

use std::collections::HashMap;
use std::fmt;
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

    /// Get all registered tokenizer names.
    pub fn names(&self) -> Vec<&str> {
        self.tokenizers.iter().map(|t| t.name()).collect()
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
            while end < bytes.len() && (bytes[end].is_ascii_hexdigit() || bytes[end] == b'_') {
                end += 1;
            }
            return end;
        }
        // Decimal (with optional dot and exponent)
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'_') {
            end += 1;
        }
        if end < bytes.len()
            && bytes[end] == b'.'
            && end + 1 < bytes.len()
            && bytes[end + 1].is_ascii_digit()
        {
            end += 1;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'_') {
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
            // Advance by full UTF-8 character width, not just one byte.
            let char_len = line[pos..].chars().next().map_or(1, |c| c.len_utf8());
            tokens.push(Token::new(
                TokenKind::Punctuation,
                base_offset + pos..base_offset + pos + char_len,
            ));
            pos += char_len;
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
// DiffTokenizer (unified diff highlights)
// ---------------------------------------------------------------------------

/// Tokenizer for unified diff/patch output.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiffTokenizer;

impl Tokenizer for DiffTokenizer {
    fn name(&self) -> &'static str {
        "Diff"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["diff", "patch"]
    }

    fn tokenize_line(&self, line: &str, state: LineState) -> (Vec<Token>, LineState) {
        if line.is_empty() {
            return (Vec::new(), state);
        }

        let kind = if line.starts_with("+++") || line.starts_with("---") {
            TokenKind::Attribute
        } else if line.starts_with("@@") {
            TokenKind::KeywordControl
        } else if line.starts_with('+') {
            TokenKind::Keyword
        } else if line.starts_with('-') {
            TokenKind::Error
        } else if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
        {
            TokenKind::Comment
        } else {
            TokenKind::Text
        };

        (vec![Token::new(kind, 0..line.len())], state)
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
        extensions: &["js", "jsx", "mjs", "cjs"],
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

/// Create a generic tokenizer configured for C++.
pub fn cpp_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "C++",
        extensions: &["cpp", "cc", "cxx", "hpp", "hxx", "hh"],
        keywords: &[
            "namespace",
            "using",
            "class",
            "struct",
            "template",
            "typename",
            "constexpr",
            "consteval",
            "constinit",
            "auto",
            "decltype",
            "noexcept",
            "friend",
            "public",
            "private",
            "protected",
            "virtual",
            "override",
            "final",
            "operator",
            "new",
            "delete",
            "this",
            "sizeof",
            "alignof",
            "static_assert",
            "mutable",
            "volatile",
            "explicit",
            "inline",
            "concept",
            "requires",
            "co_await",
            "co_yield",
            "co_return",
            "import",
            "module",
            "export",
        ],
        control_keywords: &[
            "if", "else", "switch", "case", "for", "while", "do", "break", "continue", "return",
            "try", "catch", "throw", "goto",
        ],
        type_keywords: &[
            "int",
            "long",
            "short",
            "float",
            "double",
            "char",
            "bool",
            "void",
            "wchar_t",
            "char16_t",
            "char32_t",
            "size_t",
            "string",
            "vector",
            "map",
            "unordered_map",
            "optional",
            "variant",
            "span",
            "unique_ptr",
            "shared_ptr",
            "weak_ptr",
            "nullptr_t",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for Bash.
pub fn bash_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Bash",
        extensions: &["sh", "bash", "zsh"],
        keywords: &[
            "if", "then", "fi", "for", "in", "do", "done", "case", "esac", "while", "until",
            "function", "select", "time", "coproc", "local", "export", "readonly", "declare",
            "typeset", "unset", "shift", "break", "continue", "return", "trap", "source", "eval",
        ],
        control_keywords: &[],
        type_keywords: &[],
        line_comment: "#",
        block_comment_start: "",
        block_comment_end: "",
    })
}

/// Create a generic tokenizer configured for Kotlin.
pub fn kotlin_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Kotlin",
        extensions: &["kt", "kts"],
        keywords: &[
            "package",
            "import",
            "class",
            "interface",
            "object",
            "fun",
            "val",
            "var",
            "typealias",
            "data",
            "sealed",
            "enum",
            "annotation",
            "inline",
            "reified",
            "companion",
            "override",
            "open",
            "final",
            "internal",
            "public",
            "private",
            "protected",
            "tailrec",
            "suspend",
            "operator",
            "infix",
            "this",
            "super",
            "where",
            "by",
            "constructor",
            "init",
            "as",
            "is",
            "when",
        ],
        control_keywords: &[
            "if", "else", "for", "while", "do", "break", "continue", "return", "throw", "try",
            "catch", "finally", "yield",
        ],
        type_keywords: &[
            "Int",
            "Long",
            "Short",
            "Float",
            "Double",
            "Char",
            "Boolean",
            "Unit",
            "Any",
            "Nothing",
            "String",
            "List",
            "Map",
            "Set",
            "Array",
            "MutableList",
            "MutableMap",
            "MutableSet",
            "Sequence",
            "Result",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for PowerShell.
pub fn powershell_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "PowerShell",
        extensions: &["ps1", "psm1", "psd1"],
        keywords: &[
            "function",
            "param",
            "begin",
            "process",
            "end",
            "class",
            "enum",
            "interface",
            "using",
            "module",
            "import",
            "export",
        ],
        control_keywords: &[
            "if", "elseif", "else", "switch", "for", "foreach", "while", "do", "until", "break",
            "continue", "return", "throw", "try", "catch", "finally", "in",
        ],
        type_keywords: &[
            "string",
            "int",
            "int64",
            "bool",
            "datetime",
            "guid",
            "hashtable",
            "psobject",
        ],
        line_comment: "#",
        block_comment_start: "",
        block_comment_end: "",
    })
}

/// Create a generic tokenizer configured for C#.
pub fn csharp_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "C#",
        extensions: &["cs"],
        keywords: &[
            "namespace",
            "using",
            "class",
            "struct",
            "record",
            "interface",
            "enum",
            "public",
            "private",
            "protected",
            "internal",
            "static",
            "readonly",
            "volatile",
            "async",
            "await",
            "var",
            "new",
            "override",
            "virtual",
            "sealed",
            "partial",
            "unsafe",
            "fixed",
            "stackalloc",
            "nameof",
            "typeof",
            "is",
            "as",
            "switch",
            "case",
            "default",
            "when",
            "yield",
            "get",
            "set",
            "init",
        ],
        control_keywords: &[
            "if", "else", "for", "foreach", "while", "do", "break", "continue", "return", "try",
            "catch", "finally", "throw", "lock",
        ],
        type_keywords: &[
            "int",
            "long",
            "short",
            "float",
            "double",
            "decimal",
            "bool",
            "char",
            "string",
            "object",
            "byte",
            "uint",
            "ulong",
            "ushort",
            "sbyte",
            "nint",
            "nuint",
            "Task",
            "ValueTask",
            "List",
            "Dictionary",
            "Span",
            "ReadOnlySpan",
            "Guid",
            "DateTime",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for Ruby.
pub fn ruby_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Ruby",
        extensions: &["rb"],
        keywords: &[
            "def",
            "class",
            "module",
            "require",
            "include",
            "extend",
            "attr_reader",
            "attr_writer",
            "attr_accessor",
            "private",
            "protected",
            "public",
            "yield",
            "self",
            "super",
            "alias",
            "undef",
            "begin",
            "rescue",
            "ensure",
            "end",
            "return",
            "lambda",
        ],
        control_keywords: &[
            "if", "elsif", "else", "unless", "case", "when", "while", "until", "for", "break",
            "next", "redo", "retry", "in",
        ],
        type_keywords: &[
            "String",
            "Array",
            "Hash",
            "Symbol",
            "Integer",
            "Float",
            "Time",
            "Regexp",
            "Proc",
            "NilClass",
            "TrueClass",
            "FalseClass",
        ],
        line_comment: "#",
        block_comment_start: "",
        block_comment_end: "",
    })
}

/// Create a generic tokenizer configured for Java.
pub fn java_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Java",
        extensions: &["java"],
        keywords: &[
            "package",
            "import",
            "class",
            "interface",
            "enum",
            "record",
            "public",
            "private",
            "protected",
            "static",
            "final",
            "abstract",
            "sealed",
            "non-sealed",
            "extends",
            "implements",
            "new",
            "this",
            "super",
            "synchronized",
            "volatile",
            "transient",
            "native",
            "strictfp",
            "throws",
            "module",
            "requires",
            "exports",
            "opens",
        ],
        control_keywords: &[
            "if", "else", "switch", "case", "for", "while", "do", "break", "continue", "return",
            "throw", "try", "catch", "finally", "yield",
        ],
        type_keywords: &[
            "int",
            "long",
            "short",
            "byte",
            "float",
            "double",
            "boolean",
            "char",
            "void",
            "String",
            "Object",
            "List",
            "Map",
            "Set",
            "Optional",
            "Stream",
            "CompletableFuture",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for C.
pub fn c_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "C",
        extensions: &["c"],
        keywords: &[
            "auto", "break", "case", "char", "const", "continue", "default", "do", "double",
            "else", "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long",
            "register", "restrict", "return", "short", "signed", "sizeof", "static", "struct",
            "switch", "typedef", "union", "unsigned", "void", "volatile", "while",
        ],
        control_keywords: &[
            "if", "else", "switch", "case", "for", "while", "do", "break",
        ],
        type_keywords: &[
            "int",
            "long",
            "short",
            "float",
            "double",
            "char",
            "size_t",
            "ptrdiff_t",
            "uint8_t",
            "uint32_t",
            "uint64_t",
            "int8_t",
            "int32_t",
            "int64_t",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for Swift.
pub fn swift_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Swift",
        extensions: &["swift"],
        keywords: &[
            "import",
            "class",
            "struct",
            "enum",
            "protocol",
            "extension",
            "func",
            "let",
            "var",
            "public",
            "private",
            "fileprivate",
            "internal",
            "open",
            "static",
            "final",
            "mutating",
            "nonmutating",
            "override",
            "lazy",
            "init",
            "deinit",
            "associatedtype",
            "where",
            "as",
            "is",
            "try",
            "await",
            "async",
            "throws",
            "rethrows",
            "some",
            "any",
        ],
        control_keywords: &[
            "if", "else", "switch", "case", "for", "while", "repeat", "break", "continue",
            "return", "throw", "do", "catch", "guard", "defer",
        ],
        type_keywords: &[
            "Int",
            "Int64",
            "UInt",
            "Double",
            "Float",
            "Bool",
            "String",
            "Character",
            "Array",
            "Dictionary",
            "Set",
            "Optional",
            "Result",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for PHP.
pub fn php_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "PHP",
        extensions: &["php"],
        keywords: &[
            "namespace",
            "use",
            "class",
            "interface",
            "trait",
            "function",
            "public",
            "private",
            "protected",
            "static",
            "final",
            "abstract",
            "extends",
            "implements",
            "new",
            "clone",
            "yield",
            "include",
            "require",
            "declare",
            "global",
            "const",
            "return",
        ],
        control_keywords: &[
            "if", "elseif", "else", "switch", "case", "for", "foreach", "while", "do", "break",
            "continue", "try", "catch", "finally", "throw",
        ],
        type_keywords: &[
            "int", "float", "string", "bool", "array", "callable", "iterable", "object", "mixed",
            "void", "never",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for HTML.
pub fn html_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "HTML",
        extensions: &["html", "htm"],
        keywords: &[
            "doctype", "html", "head", "body", "meta", "link", "script", "style", "div", "span",
            "section", "header", "footer", "nav", "main", "button", "input", "form", "label",
            "canvas",
        ],
        control_keywords: &[],
        type_keywords: &[],
        line_comment: "",
        block_comment_start: "<!--",
        block_comment_end: "-->",
    })
}

/// Create a generic tokenizer configured for CSS.
pub fn css_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "CSS",
        extensions: &["css"],
        keywords: &[
            "@media",
            "@supports",
            "@keyframes",
            "@layer",
            "@import",
            "@font-face",
            "display",
            "position",
            "grid",
            "flex",
            "transform",
            "transition",
            "animation",
            "color",
            "background",
            "border",
            "padding",
            "margin",
            "font",
            "opacity",
        ],
        control_keywords: &[],
        type_keywords: &[],
        line_comment: "",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for Fish shell.
pub fn fish_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Fish",
        extensions: &["fish"],
        keywords: &[
            "function",
            "set",
            "set_color",
            "if",
            "else",
            "end",
            "for",
            "in",
            "while",
            "switch",
            "case",
            "break",
            "continue",
            "return",
            "and",
            "or",
            "not",
            "begin",
        ],
        control_keywords: &[],
        type_keywords: &[],
        line_comment: "#",
        block_comment_start: "",
        block_comment_end: "",
    })
}

/// Create a generic tokenizer configured for Lua.
pub fn lua_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Lua",
        extensions: &["lua"],
        keywords: &[
            "function", "local", "end", "then", "elseif", "for", "in", "do", "repeat", "until",
            "return", "break", "goto",
        ],
        control_keywords: &[
            "if", "else", "while", "for", "repeat", "until", "break", "return",
        ],
        type_keywords: &["nil", "true", "false"],
        line_comment: "--",
        block_comment_start: "--[[",
        block_comment_end: "]]",
    })
}

/// Create a generic tokenizer configured for R.
pub fn r_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "R",
        extensions: &["r"],
        keywords: &[
            "function", "library", "require", "data", "set.seed", "if", "else", "for", "while",
            "repeat", "break", "next", "return", "TRUE", "FALSE", "NULL",
        ],
        control_keywords: &[
            "if", "else", "for", "while", "repeat", "break", "next", "return",
        ],
        type_keywords: &[
            "numeric",
            "integer",
            "character",
            "logical",
            "list",
            "data.frame",
        ],
        line_comment: "#",
        block_comment_start: "",
        block_comment_end: "",
    })
}

/// Create a generic tokenizer configured for Elixir.
pub fn elixir_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Elixir",
        extensions: &["ex", "exs"],
        keywords: &[
            "def",
            "defp",
            "defmodule",
            "defmacro",
            "defguard",
            "defprotocol",
            "defimpl",
            "defstruct",
            "use",
            "import",
            "alias",
            "require",
            "quote",
            "unquote",
            "fn",
            "end",
            "do",
            "when",
            "with",
            "receive",
            "try",
            "catch",
            "rescue",
            "after",
            "raise",
        ],
        control_keywords: &[
            "if", "unless", "case", "cond", "for", "while", "try", "catch", "rescue", "after",
            "with", "receive", "do", "end",
        ],
        type_keywords: &[
            "integer",
            "float",
            "boolean",
            "atom",
            "binary",
            "bitstring",
            "list",
            "map",
            "tuple",
            "pid",
            "port",
            "reference",
            "any",
            "term",
        ],
        line_comment: "#",
        block_comment_start: "",
        block_comment_end: "",
    })
}

/// Create a generic tokenizer configured for Haskell.
pub fn haskell_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Haskell",
        extensions: &["hs", "lhs"],
        keywords: &[
            "module",
            "import",
            "qualified",
            "as",
            "hiding",
            "where",
            "data",
            "type",
            "newtype",
            "class",
            "instance",
            "deriving",
            "default",
            "infix",
            "infixl",
            "infixr",
            "family",
            "role",
            "pattern",
            "foreign",
        ],
        control_keywords: &[
            "if", "then", "else", "case", "of", "do", "let", "in", "where",
        ],
        type_keywords: &[
            "Int", "Integer", "Float", "Double", "Bool", "Char", "String", "IO", "Maybe", "Either",
            "Ordering",
        ],
        line_comment: "--",
        block_comment_start: "{-",
        block_comment_end: "-}",
    })
}

/// Create a generic tokenizer configured for Zig.
pub fn zig_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Zig",
        extensions: &["zig"],
        keywords: &[
            "const",
            "var",
            "fn",
            "struct",
            "enum",
            "union",
            "opaque",
            "error",
            "test",
            "comptime",
            "usingnamespace",
            "pub",
            "export",
            "extern",
            "packed",
            "inline",
            "noinline",
            "anytype",
            "anyframe",
            "asm",
            "nosuspend",
            "suspend",
            "resume",
            "await",
            "try",
            "catch",
            "defer",
            "errdefer",
        ],
        control_keywords: &[
            "if", "else", "switch", "while", "for", "break", "continue", "return", "try", "catch",
            "defer", "errdefer",
        ],
        type_keywords: &[
            "bool", "void", "noreturn", "usize", "isize", "u8", "u16", "u32", "u64", "u128", "i8",
            "i16", "i32", "i64", "i128", "f16", "f32", "f64", "f80", "f128", "anytype",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for Mermaid diagrams.
pub fn mermaid_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Mermaid",
        extensions: &["mmd", "mermaid"],
        keywords: &[
            "graph",
            "flowchart",
            "sequenceDiagram",
            "classDiagram",
            "stateDiagram",
            "erDiagram",
            "journey",
            "gantt",
            "pie",
            "mindmap",
            "timeline",
            "gitGraph",
            "C4Context",
            "C4Container",
            "C4Component",
            "C4Dynamic",
            "C4Deployment",
        ],
        control_keywords: &[
            "subgraph",
            "end",
            "direction",
            "click",
            "style",
            "linkStyle",
            "classDef",
            "class",
            "state",
            "note",
            "participant",
            "actor",
            "loop",
            "alt",
            "opt",
            "par",
            "and",
            "else",
            "rect",
        ],
        type_keywords: &["TB", "BT", "LR", "RL"],
        line_comment: "%%",
        block_comment_start: "%%{",
        block_comment_end: "}%%",
    })
}

/// Create a generic tokenizer configured for TypeScript.
pub fn typescript_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "TypeScript",
        extensions: &["ts", "tsx"],
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
            "interface",
            "type",
            "implements",
            "extends",
            "enum",
            "namespace",
            "module",
            "declare",
            "readonly",
            "public",
            "private",
            "protected",
            "abstract",
            "override",
            "satisfies",
            "keyof",
            "infer",
            "asserts",
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
            "unknown",
            "never",
            "any",
            "void",
            "Array",
            "ReadonlyArray",
            "Promise",
            "Map",
            "Set",
            "Record",
            "Partial",
            "Required",
            "Pick",
            "Omit",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for Go.
pub fn go_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "Go",
        extensions: &["go"],
        keywords: &[
            "package",
            "import",
            "func",
            "var",
            "const",
            "type",
            "struct",
            "interface",
            "map",
            "chan",
            "go",
            "defer",
            "range",
            "select",
            "switch",
            "case",
            "default",
            "fallthrough",
        ],
        control_keywords: &["if", "else", "for", "break", "continue", "return"],
        type_keywords: &[
            "string",
            "bool",
            "int",
            "int64",
            "uint64",
            "float64",
            "byte",
            "rune",
            "error",
            "uintptr",
            "any",
            "comparable",
            "context",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for SQL.
pub fn sql_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "SQL",
        extensions: &["sql"],
        keywords: &[
            "SELECT",
            "FROM",
            "WHERE",
            "JOIN",
            "LEFT",
            "RIGHT",
            "INNER",
            "OUTER",
            "FULL",
            "CROSS",
            "ON",
            "WITH",
            "AS",
            "INSERT",
            "UPDATE",
            "DELETE",
            "VALUES",
            "INTO",
            "CREATE",
            "ALTER",
            "DROP",
            "TABLE",
            "VIEW",
            "INDEX",
            "AND",
            "OR",
            "NOT",
            "NULL",
            "IS",
            "IN",
            "EXISTS",
            "DISTINCT",
            "GROUP",
            "BY",
            "ORDER",
            "HAVING",
            "LIMIT",
            "OFFSET",
            "UNION",
            "ALL",
            "CASE",
            "WHEN",
            "THEN",
            "ELSE",
            "END",
            "OVER",
            "PARTITION",
            "WINDOW",
            "FILTER",
            "LATERAL",
            "RETURNING",
            "COALESCE",
            "CAST",
        ],
        control_keywords: &["BEGIN", "COMMIT", "ROLLBACK"],
        type_keywords: &[
            "INT",
            "BIGINT",
            "SMALLINT",
            "TEXT",
            "UUID",
            "JSON",
            "JSONB",
            "TIMESTAMP",
            "DATE",
            "BOOLEAN",
            "NUMERIC",
        ],
        line_comment: "--",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Create a generic tokenizer configured for YAML.
pub fn yaml_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "YAML",
        extensions: &["yaml", "yml"],
        keywords: &["null", "NULL", "yes", "no", "on", "off"],
        control_keywords: &[],
        type_keywords: &[],
        line_comment: "#",
        block_comment_start: "",
        block_comment_end: "",
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
#[must_use]
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
// SyntaxHighlighter
// ---------------------------------------------------------------------------

use ftui_text::{Line, Span, Text};

/// High-level syntax highlighter that converts code into styled [`Text`].
///
/// Combines tokenizer registry + theme to produce highlighted output.
///
/// # Example
/// ```ignore
/// use ftui_extras::syntax::SyntaxHighlighter;
///
/// let hl = SyntaxHighlighter::new();
/// let text = hl.highlight("let x = 42;", "rs");
/// // `text` is a styled Text with colored spans
/// ```
pub struct SyntaxHighlighter {
    registry: TokenizerRegistry,
    theme: HighlightTheme,
}

impl fmt::Debug for SyntaxHighlighter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyntaxHighlighter")
            .field("tokenizer_count", &self.registry.tokenizers.len())
            .field("theme", &self.theme)
            .finish()
    }
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxHighlighter {
    /// Create a highlighter with all built-in tokenizers and the dark theme.
    #[must_use]
    pub fn new() -> Self {
        let mut registry = TokenizerRegistry::new();
        registry.register(Box::new(rust_tokenizer()));
        registry.register(Box::new(python_tokenizer()));
        registry.register(Box::new(javascript_tokenizer()));
        registry.register(Box::new(typescript_tokenizer()));
        registry.register(Box::new(go_tokenizer()));
        registry.register(Box::new(sql_tokenizer()));
        registry.register(Box::new(yaml_tokenizer()));
        registry.register(Box::new(bash_tokenizer()));
        registry.register(Box::new(cpp_tokenizer()));
        registry.register(Box::new(kotlin_tokenizer()));
        registry.register(Box::new(powershell_tokenizer()));
        registry.register(Box::new(csharp_tokenizer()));
        registry.register(Box::new(ruby_tokenizer()));
        registry.register(Box::new(java_tokenizer()));
        registry.register(Box::new(c_tokenizer()));
        registry.register(Box::new(swift_tokenizer()));
        registry.register(Box::new(php_tokenizer()));
        registry.register(Box::new(html_tokenizer()));
        registry.register(Box::new(css_tokenizer()));
        registry.register(Box::new(fish_tokenizer()));
        registry.register(Box::new(lua_tokenizer()));
        registry.register(Box::new(r_tokenizer()));
        registry.register(Box::new(elixir_tokenizer()));
        registry.register(Box::new(haskell_tokenizer()));
        registry.register(Box::new(zig_tokenizer()));
        registry.register(Box::new(mermaid_tokenizer()));
        registry.register(Box::new(JsonTokenizer));
        registry.register(Box::new(TomlTokenizer));
        registry.register(Box::new(MarkdownTokenizer));
        registry.register(Box::new(DiffTokenizer));
        registry.register(Box::new(PlainTokenizer));
        Self {
            registry,
            theme: HighlightTheme::dark(),
        }
    }

    /// Create a highlighter with a custom theme and the default tokenizers.
    #[must_use]
    pub fn with_theme(theme: HighlightTheme) -> Self {
        let mut hl = Self::new();
        hl.theme = theme;
        hl
    }

    /// Set the theme.
    pub fn set_theme(&mut self, theme: HighlightTheme) {
        self.theme = theme;
    }

    /// Get a reference to the current theme.
    #[must_use]
    pub fn theme(&self) -> &HighlightTheme {
        &self.theme
    }

    /// Register an additional tokenizer.
    pub fn register_tokenizer(&mut self, tokenizer: Box<dyn Tokenizer>) {
        self.registry.register(tokenizer);
    }

    /// Get the list of supported languages (tokenizer names).
    #[must_use]
    pub fn languages(&self) -> Vec<&str> {
        self.registry.names()
    }

    /// Return true if the highlighter recognizes the language identifier.
    ///
    /// The identifier can be a file extension (e.g. "rs") or tokenizer name.
    #[must_use]
    pub fn supports_language(&self, lang: &str) -> bool {
        self.registry.for_extension(lang).is_some() || self.registry.by_name(lang).is_some()
    }

    /// Highlight code using a language identifier (extension or name).
    ///
    /// Falls back to plain text if the language is not recognized.
    #[must_use]
    pub fn highlight(&self, code: &str, lang: &str) -> Text {
        let tokenizer = self
            .registry
            .for_extension(lang)
            .or_else(|| self.registry.by_name(lang))
            .unwrap_or_else(|| self.registry.for_extension("txt").expect("PlainTokenizer"));

        let mut lines = Vec::new();
        let mut state = LineState::Normal;

        for source_line in code.split('\n') {
            let (tokens, next_state) = tokenizer.tokenize_line(source_line, state);
            state = next_state;

            let spans = self.tokens_to_spans(source_line, &tokens);
            lines.push(Line::from_spans(spans));
        }

        Text::from_lines(lines)
    }

    /// Highlight code with line numbers prepended.
    #[must_use]
    pub fn highlight_numbered(&self, code: &str, lang: &str, start_line: usize) -> Text {
        let tokenizer = self
            .registry
            .for_extension(lang)
            .or_else(|| self.registry.by_name(lang))
            .unwrap_or_else(|| self.registry.for_extension("txt").expect("PlainTokenizer"));

        let source_lines: Vec<&str> = code.split('\n').collect();
        let total = source_lines.len() + start_line;
        let gutter_width = total.to_string().len();

        let mut lines = Vec::new();
        let mut state = LineState::Normal;
        let gutter_style = self.theme.punctuation;

        for (i, source_line) in source_lines.iter().enumerate() {
            let line_num = start_line + i + 1;
            let gutter = format!("{:>width$} ", line_num, width = gutter_width);

            let (tokens, next_state) = tokenizer.tokenize_line(source_line, state);
            state = next_state;

            let mut spans = vec![Span::styled(gutter, gutter_style)];
            spans.extend(self.tokens_to_spans(source_line, &tokens));
            lines.push(Line::from_spans(spans));
        }

        Text::from_lines(lines)
    }

    /// Convert tokens into styled spans for a single line.
    fn tokens_to_spans<'a>(&self, source: &'a str, tokens: &[Token]) -> Vec<Span<'a>> {
        let mut spans = Vec::with_capacity(tokens.len());
        let mut last_end = 0;

        for token in tokens {
            // Fill gaps between tokens with unstyled text
            if token.range.start > last_end
                && let Some(gap) = source.get(last_end..token.range.start)
            {
                spans.push(Span::raw(gap));
            }

            let style = self.theme.style_for(token.kind);
            if let Some(text) = source.get(token.range.clone()) {
                spans.push(Span::styled(text, style));
            }
            last_end = token.range.end;
        }

        // Trailing text after last token
        if last_end < source.len()
            && let Some(tail) = source.get(last_end..)
        {
            spans.push(Span::raw(tail));
        }

        spans
    }
}

// ---------------------------------------------------------------------------
// JSON Tokenizer
// ---------------------------------------------------------------------------

/// Tokenizer for JSON files.
///
/// Handles: strings, numbers, booleans, null, structural punctuation.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonTokenizer;

impl Tokenizer for JsonTokenizer {
    fn name(&self) -> &'static str {
        "JSON"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json", "jsonl", "geojson"]
    }

    fn tokenize_line(&self, line: &str, state: LineState) -> (Vec<Token>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut tokens = Vec::new();
        let mut pos = 0;
        let mut current_state = state;

        while pos < len {
            match current_state {
                LineState::InString(StringKind::Double) => {
                    let start = pos;
                    while pos < len {
                        if bytes[pos] == b'\\' && pos + 1 < len {
                            // Skip escape sequence
                            pos += 2;
                        } else if bytes[pos] == b'"' {
                            pos += 1;
                            current_state = LineState::Normal;
                            break;
                        } else {
                            pos += 1;
                        }
                    }
                    tokens.push(Token::new(TokenKind::String, start..pos));
                }
                _ => {
                    let b = bytes[pos];
                    match b {
                        b' ' | b'\t' | b'\r' => {
                            let start = pos;
                            while pos < len
                                && (bytes[pos] == b' '
                                    || bytes[pos] == b'\t'
                                    || bytes[pos] == b'\r')
                            {
                                pos += 1;
                            }
                            tokens.push(Token::new(TokenKind::Whitespace, start..pos));
                        }
                        b'"' => {
                            let start = pos;
                            pos += 1; // skip opening quote
                            while pos < len {
                                if bytes[pos] == b'\\' && pos + 1 < len {
                                    pos += 2;
                                } else if bytes[pos] == b'"' {
                                    pos += 1;
                                    break;
                                } else {
                                    pos += 1;
                                }
                            }
                            if pos <= len && pos > start + 1 && bytes[pos - 1] == b'"' {
                                tokens.push(Token::new(TokenKind::String, start..pos));
                            } else {
                                // Unterminated string continues on next line
                                tokens.push(Token::new(TokenKind::String, start..pos));
                                current_state = LineState::InString(StringKind::Double);
                            }
                        }
                        b'{' | b'}' | b'[' | b']' => {
                            tokens.push(Token::new(TokenKind::Delimiter, pos..pos + 1));
                            pos += 1;
                        }
                        b':' | b',' => {
                            tokens.push(Token::new(TokenKind::Punctuation, pos..pos + 1));
                            pos += 1;
                        }
                        b'-' | b'0'..=b'9' => {
                            let start = pos;
                            if b == b'-' {
                                pos += 1;
                            }
                            while pos < len && bytes[pos].is_ascii_digit() {
                                pos += 1;
                            }
                            // Decimal part
                            if pos < len && bytes[pos] == b'.' {
                                pos += 1;
                                while pos < len && bytes[pos].is_ascii_digit() {
                                    pos += 1;
                                }
                            }
                            // Exponent
                            if pos < len && (bytes[pos] == b'e' || bytes[pos] == b'E') {
                                pos += 1;
                                if pos < len && (bytes[pos] == b'+' || bytes[pos] == b'-') {
                                    pos += 1;
                                }
                                while pos < len && bytes[pos].is_ascii_digit() {
                                    pos += 1;
                                }
                            }
                            tokens.push(Token::new(TokenKind::Number, start..pos));
                        }
                        b't' if line.get(pos..pos + 4) == Some("true") => {
                            tokens.push(Token::new(TokenKind::Boolean, pos..pos + 4));
                            pos += 4;
                        }
                        b'f' if line.get(pos..pos + 5) == Some("false") => {
                            tokens.push(Token::new(TokenKind::Boolean, pos..pos + 5));
                            pos += 5;
                        }
                        b'n' if line.get(pos..pos + 4) == Some("null") => {
                            tokens.push(Token::new(TokenKind::Constant, pos..pos + 4));
                            pos += 4;
                        }
                        _ => {
                            tokens.push(Token::new(TokenKind::Error, pos..pos + 1));
                            pos += 1;
                        }
                    }
                }
            }
        }

        (tokens, current_state)
    }
}

// ---------------------------------------------------------------------------
// TOML Tokenizer
// ---------------------------------------------------------------------------

/// Tokenizer for TOML files.
///
/// Handles: table headers, keys, strings (basic/literal), numbers,
/// booleans, dates, comments.
#[derive(Debug, Clone, Copy, Default)]
pub struct TomlTokenizer;

impl Tokenizer for TomlTokenizer {
    fn name(&self) -> &'static str {
        "TOML"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
    }

    fn tokenize_line(&self, line: &str, state: LineState) -> (Vec<Token>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut tokens = Vec::new();
        let mut pos = 0;
        let mut current_state = state;

        // Handle multi-line string continuation
        match current_state {
            LineState::InString(StringKind::Triple) => {
                let start = pos;
                while pos < len {
                    if bytes[pos] == b'\\' && pos + 1 < len {
                        pos += 2;
                    } else if pos + 2 < len
                        && bytes[pos] == b'"'
                        && bytes[pos + 1] == b'"'
                        && bytes[pos + 2] == b'"'
                    {
                        pos += 3;
                        current_state = LineState::Normal;
                        break;
                    } else {
                        pos += 1;
                    }
                }
                tokens.push(Token::new(TokenKind::String, start..pos));
                if matches!(current_state, LineState::InString(_)) {
                    return (tokens, current_state);
                }
            }
            LineState::InString(StringKind::Single) => {
                // Multi-line literal string (''')
                let start = pos;
                while pos < len {
                    if pos + 2 < len
                        && bytes[pos] == b'\''
                        && bytes[pos + 1] == b'\''
                        && bytes[pos + 2] == b'\''
                    {
                        pos += 3;
                        current_state = LineState::Normal;
                        break;
                    } else {
                        pos += 1;
                    }
                }
                tokens.push(Token::new(TokenKind::String, start..pos));
                if matches!(current_state, LineState::InString(_)) {
                    return (tokens, current_state);
                }
            }
            _ => {}
        }

        while pos < len {
            let b = bytes[pos];
            match b {
                b' ' | b'\t' | b'\r' => {
                    let start = pos;
                    while pos < len
                        && (bytes[pos] == b' ' || bytes[pos] == b'\t' || bytes[pos] == b'\r')
                    {
                        pos += 1;
                    }
                    tokens.push(Token::new(TokenKind::Whitespace, start..pos));
                }
                b'#' => {
                    tokens.push(Token::new(TokenKind::Comment, pos..len));
                    pos = len;
                }
                b'[' => {
                    // Table header or array of tables
                    let start = pos;
                    if pos + 1 < len && bytes[pos + 1] == b'[' {
                        // Array of tables [[...]]
                        pos += 2;
                        while pos + 1 < len && !(bytes[pos] == b']' && bytes[pos + 1] == b']') {
                            pos += 1;
                        }
                        if pos + 1 < len {
                            pos += 2; // skip ]]
                        }
                    } else {
                        pos += 1;
                        while pos < len && bytes[pos] != b']' {
                            pos += 1;
                        }
                        if pos < len {
                            pos += 1; // skip ]
                        }
                    }
                    tokens.push(Token::new(TokenKind::Heading, start..pos));
                }
                b'"' => {
                    let start = pos;
                    if pos + 2 < len && bytes[pos + 1] == b'"' && bytes[pos + 2] == b'"' {
                        // Multi-line basic string
                        pos += 3;
                        while pos < len {
                            if bytes[pos] == b'\\' && pos + 1 < len {
                                pos += 2;
                            } else if pos + 2 < len
                                && bytes[pos] == b'"'
                                && bytes[pos + 1] == b'"'
                                && bytes[pos + 2] == b'"'
                            {
                                pos += 3;
                                break;
                            } else {
                                pos += 1;
                            }
                        }
                        if pos >= len
                            && (pos < 3
                                || bytes[pos.saturating_sub(1)] != b'"'
                                || bytes[pos.saturating_sub(2)] != b'"'
                                || bytes[pos.saturating_sub(3)] != b'"')
                        {
                            current_state = LineState::InString(StringKind::Triple);
                        }
                        tokens.push(Token::new(TokenKind::String, start..pos));
                    } else {
                        // Basic string
                        pos += 1;
                        while pos < len {
                            if bytes[pos] == b'\\' && pos + 1 < len {
                                pos += 2;
                            } else if bytes[pos] == b'"' {
                                pos += 1;
                                break;
                            } else {
                                pos += 1;
                            }
                        }
                        tokens.push(Token::new(TokenKind::String, start..pos));
                    }
                }
                b'\'' => {
                    let start = pos;
                    if pos + 2 < len && bytes[pos + 1] == b'\'' && bytes[pos + 2] == b'\'' {
                        // Multi-line literal string
                        pos += 3;
                        while pos < len {
                            if pos + 2 < len
                                && bytes[pos] == b'\''
                                && bytes[pos + 1] == b'\''
                                && bytes[pos + 2] == b'\''
                            {
                                pos += 3;
                                break;
                            } else {
                                pos += 1;
                            }
                        }
                        if pos >= len
                            && (pos < 3
                                || bytes[pos.saturating_sub(1)] != b'\''
                                || bytes[pos.saturating_sub(2)] != b'\''
                                || bytes[pos.saturating_sub(3)] != b'\'')
                        {
                            current_state = LineState::InString(StringKind::Single);
                        }
                        tokens.push(Token::new(TokenKind::String, start..pos));
                    } else {
                        // Literal string (no escapes)
                        pos += 1;
                        while pos < len && bytes[pos] != b'\'' {
                            pos += 1;
                        }
                        if pos < len {
                            pos += 1;
                        }
                        tokens.push(Token::new(TokenKind::String, start..pos));
                    }
                }
                b'=' => {
                    tokens.push(Token::new(TokenKind::Operator, pos..pos + 1));
                    pos += 1;
                }
                b',' => {
                    tokens.push(Token::new(TokenKind::Punctuation, pos..pos + 1));
                    pos += 1;
                }
                b'{' | b'}' => {
                    tokens.push(Token::new(TokenKind::Delimiter, pos..pos + 1));
                    pos += 1;
                }
                b't' if line.get(pos..pos + 4) == Some("true")
                    && !Self::continues_ident(bytes, pos + 4) =>
                {
                    tokens.push(Token::new(TokenKind::Boolean, pos..pos + 4));
                    pos += 4;
                }
                b'f' if line.get(pos..pos + 5) == Some("false")
                    && !Self::continues_ident(bytes, pos + 5) =>
                {
                    tokens.push(Token::new(TokenKind::Boolean, pos..pos + 5));
                    pos += 5;
                }
                b'-' | b'+' | b'0'..=b'9' => {
                    let start = pos;
                    // Could be number or date
                    if b == b'-' || b == b'+' {
                        pos += 1;
                    }
                    while pos < len
                        && (bytes[pos].is_ascii_alphanumeric()
                            || bytes[pos] == b'_'
                            || bytes[pos] == b'.'
                            || bytes[pos] == b'-'
                            || bytes[pos] == b'+'
                            || bytes[pos] == b':'
                            || bytes[pos] == b'T'
                            || bytes[pos] == b'Z')
                    {
                        pos += 1;
                    }
                    tokens.push(Token::new(TokenKind::Number, start..pos));
                }
                _ if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' => {
                    let start = pos;
                    while pos < len
                        && (bytes[pos].is_ascii_alphanumeric()
                            || bytes[pos] == b'_'
                            || bytes[pos] == b'-'
                            || bytes[pos] == b'.')
                    {
                        pos += 1;
                    }
                    tokens.push(Token::new(TokenKind::Identifier, start..pos));
                }
                _ => {
                    tokens.push(Token::new(TokenKind::Text, pos..pos + 1));
                    pos += 1;
                }
            }
        }

        (tokens, current_state)
    }
}

impl TomlTokenizer {
    fn continues_ident(bytes: &[u8], pos: usize) -> bool {
        pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_')
    }
}

// ---------------------------------------------------------------------------
// Markdown Tokenizer
// ---------------------------------------------------------------------------

/// Tokenizer for Markdown files.
///
/// Handles: headings, emphasis, links, code spans, code fences, lists.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownTokenizer;

impl Tokenizer for MarkdownTokenizer {
    fn name(&self) -> &'static str {
        "Markdown"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["md", "markdown", "mdx"]
    }

    fn tokenize_line(&self, line: &str, state: LineState) -> (Vec<Token>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut tokens = Vec::new();
        let mut pos = 0;
        // Handle fenced code block continuation
        if matches!(state, LineState::InComment(CommentKind::Block)) {
            // Check if this line closes the fence
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                tokens.push(Token::new(TokenKind::Delimiter, 0..len));
                return (tokens, LineState::Normal);
            }
            tokens.push(Token::new(TokenKind::String, 0..len));
            return (tokens, state);
        }

        // Check for fenced code block start
        {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                tokens.push(Token::new(TokenKind::Delimiter, 0..len));
                return (tokens, LineState::InComment(CommentKind::Block));
            }
        }

        // Check for ATX heading at start of line
        if pos < len && bytes[pos] == b'#' {
            let start = pos;
            while pos < len && bytes[pos] == b'#' {
                pos += 1;
            }
            if pos < len && bytes[pos] == b' ' {
                tokens.push(Token::new(TokenKind::Heading, start..len));
                return (tokens, state);
            }
            // Not a valid heading, reset
            pos = start;
        }

        // Inline tokenization
        while pos < len {
            let b = bytes[pos];
            match b {
                b'`' => {
                    // Code span
                    let start = pos;
                    pos += 1;
                    while pos < len && bytes[pos] != b'`' {
                        pos += 1;
                    }
                    if pos < len {
                        pos += 1; // closing backtick
                    }
                    tokens.push(Token::new(TokenKind::String, start..pos));
                }
                b'*' | b'_' => {
                    // Emphasis marker
                    let start = pos;
                    let marker = b;
                    while pos < len && bytes[pos] == marker {
                        pos += 1;
                    }
                    tokens.push(Token::new(TokenKind::Emphasis, start..pos));
                }
                b'[' => {
                    // Possible link: [text](url) or [text][ref]
                    let start = pos;
                    pos += 1;
                    while pos < len && bytes[pos] != b']' {
                        pos += 1;
                    }
                    if pos < len {
                        pos += 1; // skip ]
                    }
                    if pos < len && (bytes[pos] == b'(' || bytes[pos] == b'[') {
                        let close = if bytes[pos] == b'(' { b')' } else { b']' };
                        pos += 1;
                        while pos < len && bytes[pos] != close {
                            pos += 1;
                        }
                        if pos < len {
                            pos += 1;
                        }
                        tokens.push(Token::new(TokenKind::Link, start..pos));
                    } else {
                        tokens.push(Token::new(TokenKind::Text, start..pos));
                    }
                }
                b'!' if pos + 1 < len && bytes[pos + 1] == b'[' => {
                    // Image: ![alt](url)
                    let start = pos;
                    pos += 2; // skip ![
                    while pos < len && bytes[pos] != b']' {
                        pos += 1;
                    }
                    if pos < len {
                        pos += 1;
                    }
                    if pos < len && bytes[pos] == b'(' {
                        pos += 1;
                        while pos < len && bytes[pos] != b')' {
                            pos += 1;
                        }
                        if pos < len {
                            pos += 1;
                        }
                    }
                    tokens.push(Token::new(TokenKind::Link, start..pos));
                }
                b'-' | b'+' if pos == 0 && pos + 1 < len && bytes[pos + 1] == b' ' => {
                    // List item marker
                    tokens.push(Token::new(TokenKind::Punctuation, pos..pos + 1));
                    pos += 1;
                }
                b'>' if pos == 0 => {
                    // Block quote
                    tokens.push(Token::new(TokenKind::Punctuation, pos..pos + 1));
                    pos += 1;
                }
                _ => {
                    // Regular text
                    let start = pos;
                    while pos < len {
                        let c = bytes[pos];
                        if c == b'`'
                            || c == b'*'
                            || c == b'_'
                            || c == b'['
                            || (c == b'!' && pos + 1 < len && bytes[pos + 1] == b'[')
                        {
                            break;
                        }
                        pos += 1;
                    }
                    if pos > start {
                        tokens.push(Token::new(TokenKind::Text, start..pos));
                    }
                }
            }
        }

        (tokens, state)
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

    // -- JSON Tokenizer -------------------------------------------------------

    #[test]
    fn json_tokenizes_object() {
        let t = JsonTokenizer;
        let tokens = t.tokenize(r#"{"key": "value"}"#);
        assert!(validate_tokens(r#"{"key": "value"}"#, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Delimiter));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Punctuation));
    }

    #[test]
    fn json_tokenizes_numbers() {
        let t = JsonTokenizer;
        for input in ["42", "-3.14", "1e10", "2.5E-3"] {
            let tokens = t.tokenize(input);
            assert!(validate_tokens(input, &tokens));
            assert!(
                tokens.iter().any(|t| t.kind == TokenKind::Number),
                "Expected Number token for: {input}"
            );
        }
    }

    #[test]
    fn json_tokenizes_booleans_and_null() {
        let t = JsonTokenizer;
        let tokens = t.tokenize("[true, false, null]");
        assert!(validate_tokens("[true, false, null]", &tokens));
        assert_eq!(
            tokens
                .iter()
                .filter(|t| t.kind == TokenKind::Boolean)
                .count(),
            2
        );
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Constant)); // null
    }

    #[test]
    fn json_string_with_escapes() {
        let t = JsonTokenizer;
        let input = r#""hello \"world\" \n""#;
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String));
    }

    #[test]
    fn json_nested_structure() {
        let t = JsonTokenizer;
        let input = r#"{"a": [1, {"b": true}]}"#;
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        // Should not panic on complex nesting
        assert!(!tokens.is_empty());
    }

    #[test]
    fn json_empty_input() {
        let t = JsonTokenizer;
        let tokens = t.tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn json_malformed_no_panic() {
        let t = JsonTokenizer;
        for input in ["{invalid}", "{{{}}}!!!", "[,,,]", "trufalse"] {
            let tokens = t.tokenize(input);
            assert!(
                validate_tokens(input, &tokens),
                "Invalid tokens for: {input}"
            );
        }
    }

    // -- TOML Tokenizer -------------------------------------------------------

    #[test]
    fn toml_table_header() {
        let t = TomlTokenizer;
        let tokens = t.tokenize("[package]");
        assert!(validate_tokens("[package]", &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Heading));
    }

    #[test]
    fn toml_array_of_tables() {
        let t = TomlTokenizer;
        let tokens = t.tokenize("[[dependencies]]");
        assert!(validate_tokens("[[dependencies]]", &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Heading));
    }

    #[test]
    fn toml_key_value_string() {
        let t = TomlTokenizer;
        let input = r#"name = "ftui-text""#;
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Identifier)); // key
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Operator)); // =
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String)); // value
    }

    #[test]
    fn toml_booleans() {
        let t = TomlTokenizer;
        let input = "flag = true";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Boolean));
    }

    #[test]
    fn toml_comment() {
        let t = TomlTokenizer;
        let input = "# This is a comment";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Comment));
    }

    #[test]
    fn toml_number() {
        let t = TomlTokenizer;
        let input = "port = 8080";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Number));
    }

    #[test]
    fn toml_literal_string() {
        let t = TomlTokenizer;
        let input = "path = 'C:\\Users\\me'";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String));
    }

    #[test]
    fn toml_inline_table() {
        let t = TomlTokenizer;
        let input = r#"dep = { version = "1.0", features = ["a"] }"#;
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Delimiter));
    }

    #[test]
    fn toml_multiline_basic_string() {
        let t = TomlTokenizer;
        let lines = [r#"desc = """"#, "hello", "world", r#"""""#];
        let mut state = LineState::Normal;
        for line in &lines {
            let (tokens, new_state) = t.tokenize_line(line, state);
            assert!(validate_tokens(line, &tokens));
            state = new_state;
        }
        assert!(
            matches!(state, LineState::Normal),
            "Should end in Normal state"
        );
    }

    #[test]
    fn toml_malformed_no_panic() {
        let t = TomlTokenizer;
        for input in ["[[[nested]]]", "===", "true_ish", ""] {
            let tokens = t.tokenize(input);
            assert!(
                validate_tokens(input, &tokens),
                "Invalid tokens for: {input}"
            );
        }
    }

    // -- Markdown Tokenizer ---------------------------------------------------

    #[test]
    fn markdown_heading() {
        let t = MarkdownTokenizer;
        let input = "# Hello World";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Heading));
    }

    #[test]
    fn markdown_h2_heading() {
        let t = MarkdownTokenizer;
        let input = "## Section Two";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Heading));
    }

    #[test]
    fn markdown_emphasis() {
        let t = MarkdownTokenizer;
        let input = "some *bold* text";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Emphasis));
    }

    #[test]
    fn markdown_inline_code() {
        let t = MarkdownTokenizer;
        let input = "use `println!` here";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String));
    }

    #[test]
    fn markdown_link() {
        let t = MarkdownTokenizer;
        let input = "click [here](https://example.com)";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Link));
    }

    #[test]
    fn markdown_image() {
        let t = MarkdownTokenizer;
        let input = "![alt](image.png)";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Link));
    }

    #[test]
    fn markdown_fenced_code_block() {
        let t = MarkdownTokenizer;
        let lines = ["```rust", "fn main() {}", "```"];
        let mut state = LineState::Normal;
        for line in &lines {
            let (tokens, new_state) = t.tokenize_line(line, state);
            assert!(validate_tokens(line, &tokens), "Failed for line: {line}");
            state = new_state;
        }
        assert!(
            matches!(state, LineState::Normal),
            "Should end in Normal after closing fence"
        );
    }

    #[test]
    fn markdown_code_block_state_tracking() {
        let t = MarkdownTokenizer;
        let (_, state) = t.tokenize_line("```", LineState::Normal);
        assert!(matches!(state, LineState::InComment(CommentKind::Block)));

        let (tokens, state) = t.tokenize_line("code here", state);
        assert!(matches!(state, LineState::InComment(CommentKind::Block)));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String));

        let (_, state) = t.tokenize_line("```", state);
        assert!(matches!(state, LineState::Normal));
    }

    #[test]
    fn markdown_list_item() {
        let t = MarkdownTokenizer;
        let input = "- item one";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Punctuation));
    }

    #[test]
    fn markdown_blockquote() {
        let t = MarkdownTokenizer;
        let input = "> quoted text";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Punctuation));
    }

    #[test]
    fn markdown_empty_input() {
        let t = MarkdownTokenizer;
        let tokens = t.tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn markdown_plain_text() {
        let t = MarkdownTokenizer;
        let input = "Just plain text here.";
        let tokens = t.tokenize(input);
        assert!(validate_tokens(input, &tokens));
        assert!(tokens.iter().all(|t| t.kind == TokenKind::Text));
    }

    #[test]
    fn markdown_malformed_no_panic() {
        let t = MarkdownTokenizer;
        for input in ["[unclosed link", "![broken", "***", "```"] {
            let tokens = t.tokenize(input);
            assert!(
                validate_tokens(input, &tokens),
                "Invalid tokens for: {input}"
            );
        }
    }

    // -- Cross-tokenizer registry integration ---------------------------------

    #[test]
    fn registry_with_all_tokenizers() {
        let mut reg = TokenizerRegistry::new();
        reg.register(Box::new(rust_tokenizer()));
        reg.register(Box::new(python_tokenizer()));
        reg.register(Box::new(javascript_tokenizer()));
        reg.register(Box::new(typescript_tokenizer()));
        reg.register(Box::new(go_tokenizer()));
        reg.register(Box::new(sql_tokenizer()));
        reg.register(Box::new(yaml_tokenizer()));
        reg.register(Box::new(bash_tokenizer()));
        reg.register(Box::new(cpp_tokenizer()));
        reg.register(Box::new(kotlin_tokenizer()));
        reg.register(Box::new(powershell_tokenizer()));
        reg.register(Box::new(csharp_tokenizer()));
        reg.register(Box::new(ruby_tokenizer()));
        reg.register(Box::new(java_tokenizer()));
        reg.register(Box::new(c_tokenizer()));
        reg.register(Box::new(swift_tokenizer()));
        reg.register(Box::new(php_tokenizer()));
        reg.register(Box::new(html_tokenizer()));
        reg.register(Box::new(css_tokenizer()));
        reg.register(Box::new(fish_tokenizer()));
        reg.register(Box::new(lua_tokenizer()));
        reg.register(Box::new(r_tokenizer()));
        reg.register(Box::new(elixir_tokenizer()));
        reg.register(Box::new(haskell_tokenizer()));
        reg.register(Box::new(zig_tokenizer()));
        reg.register(Box::new(mermaid_tokenizer()));
        reg.register(Box::new(JsonTokenizer));
        reg.register(Box::new(TomlTokenizer));
        reg.register(Box::new(MarkdownTokenizer));
        reg.register(Box::new(DiffTokenizer));
        reg.register(Box::new(PlainTokenizer));

        assert!(reg.for_extension("rs").is_some());
        assert!(reg.for_extension("py").is_some());
        assert!(reg.for_extension("js").is_some());
        assert!(reg.for_extension("ts").is_some());
        assert!(reg.for_extension("go").is_some());
        assert!(reg.for_extension("sql").is_some());
        assert!(reg.for_extension("yaml").is_some());
        assert!(reg.for_extension("sh").is_some());
        assert!(reg.for_extension("cpp").is_some());
        assert!(reg.for_extension("kt").is_some());
        assert!(reg.for_extension("ps1").is_some());
        assert!(reg.for_extension("cs").is_some());
        assert!(reg.for_extension("rb").is_some());
        assert!(reg.for_extension("java").is_some());
        assert!(reg.for_extension("c").is_some());
        assert!(reg.for_extension("swift").is_some());
        assert!(reg.for_extension("php").is_some());
        assert!(reg.for_extension("html").is_some());
        assert!(reg.for_extension("css").is_some());
        assert!(reg.for_extension("fish").is_some());
        assert!(reg.for_extension("lua").is_some());
        assert!(reg.for_extension("r").is_some());
        assert!(reg.for_extension("ex").is_some());
        assert!(reg.for_extension("hs").is_some());
        assert!(reg.for_extension("zig").is_some());
        assert!(reg.for_extension("mmd").is_some());
        assert!(reg.for_extension("json").is_some());
        assert!(reg.for_extension("toml").is_some());
        assert!(reg.for_extension("md").is_some());
        assert!(reg.for_extension("diff").is_some());
        assert!(reg.for_extension("txt").is_some());
        assert_eq!(reg.len(), 31);
    }

    // -- Token range validation across all tokenizers -------------------------

    #[test]
    fn all_tokenizers_produce_valid_ranges() {
        let snippets: &[(&str, &dyn Tokenizer)] = &[
            (r#"{"a": 1, "b": [true, null]}"#, &JsonTokenizer),
            (
                "[package]\nname = \"test\"\nversion = \"0.1.0\"",
                &TomlTokenizer,
            ),
            (
                "# Heading\n\n```rust\nlet x = 1;\n```\n\n[link](url)",
                &MarkdownTokenizer,
            ),
            ("@@ -1,2 +1,2 @@\n-old line\n+new line\n", &DiffTokenizer),
            ("graph LR\nA-->B\n", &mermaid_tokenizer()),
            ("fn main() { let x = 42; }", &rust_tokenizer()),
            ("def foo():\n    return 42", &python_tokenizer()),
            (
                "const x = async () => await fetch()",
                &javascript_tokenizer(),
            ),
            (
                "type Result<T> = { ok: true; value: T } | { ok: false; error: string };",
                &typescript_tokenizer(),
            ),
            ("func Map[T any](in []T) []T { return in }", &go_tokenizer()),
            (
                "SELECT id FROM users WHERE active = true;",
                &sql_tokenizer(),
            ),
            ("service:\n  enabled: true\n  retries: 3", &yaml_tokenizer()),
            ("#!/usr/bin/env bash\nset -euo pipefail", &bash_tokenizer()),
            (
                "template <typename T> T add(T a, T b) { return a + b; }",
                &cpp_tokenizer(),
            ),
            (
                "data class User(val id: Int, val name: String)",
                &kotlin_tokenizer(),
            ),
            ("$ErrorActionPreference = \"Stop\"", &powershell_tokenizer()),
            ("record User(Guid Id, string Name);", &csharp_tokenizer()),
            (
                "class User; def initialize(id); @id = id; end; end",
                &ruby_tokenizer(),
            ),
            (
                "public record User(String id, int age) {}",
                &java_tokenizer(),
            ),
            ("typedef struct { int id; } user_t;", &c_tokenizer()),
            ("struct User: Codable { let id: UUID }", &swift_tokenizer()),
            (
                "<?php final class User { public function __construct(public string $id) {} }",
                &php_tokenizer(),
            ),
            (
                "<!doctype html><main><h1>Hello</h1></main>",
                &html_tokenizer(),
            ),
            (
                "@media (min-width: 768px) { .app { display: grid; } }",
                &css_tokenizer(),
            ),
            ("function foo; set -l x 1; end", &fish_tokenizer()),
            (
                "local function add(a, b) return a + b end",
                &lua_tokenizer(),
            ),
            ("x <- c(1, 2, 3); mean(x)", &r_tokenizer()),
        ];

        for (source, tokenizer) in snippets {
            let tokens = tokenizer.tokenize(source);
            assert!(
                validate_tokens(source, &tokens),
                "Invalid token ranges for {} tokenizer on: {}",
                tokenizer.name(),
                source
            );
        }
    }

    // -- SyntaxHighlighter ---------------------------------------------------

    #[test]
    fn highlighter_creates_with_defaults() {
        let hl = SyntaxHighlighter::new();
        let langs = hl.languages();
        assert!(langs.contains(&"Rust"));
        assert!(langs.contains(&"Python"));
        assert!(langs.contains(&"JavaScript"));
        assert!(langs.contains(&"TypeScript"));
        assert!(langs.contains(&"Go"));
        assert!(langs.contains(&"SQL"));
        assert!(langs.contains(&"YAML"));
        assert!(langs.contains(&"Bash"));
        assert!(langs.contains(&"C++"));
        assert!(langs.contains(&"Kotlin"));
        assert!(langs.contains(&"PowerShell"));
        assert!(langs.contains(&"C#"));
        assert!(langs.contains(&"Ruby"));
        assert!(langs.contains(&"Java"));
        assert!(langs.contains(&"C"));
        assert!(langs.contains(&"Swift"));
        assert!(langs.contains(&"PHP"));
        assert!(langs.contains(&"HTML"));
        assert!(langs.contains(&"CSS"));
        assert!(langs.contains(&"Fish"));
        assert!(langs.contains(&"Lua"));
        assert!(langs.contains(&"R"));
        assert!(langs.contains(&"JSON"));
        assert!(langs.contains(&"TOML"));
        assert!(langs.contains(&"Markdown"));
        assert!(langs.contains(&"Mermaid"));
        assert!(langs.contains(&"Diff"));
        assert!(langs.contains(&"Plain"));
    }

    #[test]
    fn highlighter_rust_code() {
        let hl = SyntaxHighlighter::new();
        let text = hl.highlight("fn main() {\n    let x = 42;\n}", "rs");
        assert_eq!(text.height(), 3);
        let plain = text.to_plain_text();
        assert!(plain.contains("fn main()"));
        assert!(plain.contains("let x = 42"));
    }

    #[test]
    fn highlighter_python_code() {
        let hl = SyntaxHighlighter::new();
        let text = hl.highlight("def hello():\n    return 42", "py");
        assert_eq!(text.height(), 2);
        let plain = text.to_plain_text();
        assert!(plain.contains("def hello()"));
    }

    #[test]
    fn highlighter_json() {
        let hl = SyntaxHighlighter::new();
        let text = hl.highlight(r#"{"key": "value", "num": 42}"#, "json");
        assert_eq!(text.height(), 1);
    }

    #[test]
    fn highlighter_unknown_language_falls_back() {
        let hl = SyntaxHighlighter::new();
        let text = hl.highlight("just some text", "zzz_unknown_zzz");
        assert_eq!(text.height(), 1);
        assert_eq!(text.to_plain_text(), "just some text");
    }

    #[test]
    fn highlighter_by_name_works() {
        let hl = SyntaxHighlighter::new();
        let text = hl.highlight("let x = 1;", "Rust");
        let plain = text.to_plain_text();
        assert!(plain.contains("let x = 1;"));
    }

    #[test]
    fn highlighter_with_line_numbers() {
        let hl = SyntaxHighlighter::new();
        let text = hl.highlight_numbered("line one\nline two\nline three", "txt", 0);
        assert_eq!(text.height(), 3);
        let plain = text.to_plain_text();
        assert!(plain.contains("1 "));
        assert!(plain.contains("2 "));
        assert!(plain.contains("3 "));
    }

    #[test]
    fn highlighter_numbered_with_offset() {
        let hl = SyntaxHighlighter::new();
        let text = hl.highlight_numbered("a\nb", "txt", 98);
        let plain = text.to_plain_text();
        assert!(plain.contains("99"));
        assert!(plain.contains("100"));
    }

    #[test]
    fn highlighter_theme_switching() {
        let mut hl = SyntaxHighlighter::new();
        let dark = hl.highlight("let x = 1;", "rs");

        hl.set_theme(HighlightTheme::light());
        let light = hl.highlight("let x = 1;", "rs");

        // Both should produce valid text with same content
        assert_eq!(dark.to_plain_text(), light.to_plain_text());
    }

    #[test]
    fn highlighter_empty_input() {
        let hl = SyntaxHighlighter::new();
        let text = hl.highlight("", "rs");
        assert_eq!(text.height(), 1); // One empty line
    }

    #[test]
    fn highlighter_multiline_preserves_content() {
        let hl = SyntaxHighlighter::new();
        let code = "fn foo() {}\nfn bar() {}\nfn baz() {}";
        let text = hl.highlight(code, "rs");
        assert_eq!(text.height(), 3);
        assert_eq!(text.to_plain_text(), code);
    }

    #[test]
    fn highlighter_custom_theme() {
        let theme = HighlightThemeBuilder::from_theme(HighlightTheme::dark())
            .keyword(Style::new().bold())
            .build();
        let hl = SyntaxHighlighter::with_theme(theme);
        let text = hl.highlight("let x = 1;", "rs");
        assert!(!text.to_plain_text().is_empty());
    }

    #[test]
    fn highlighter_register_custom_tokenizer() {
        let mut hl = SyntaxHighlighter::new();
        hl.register_tokenizer(Box::new(PlainTokenizer));
        // Should still work
        let text = hl.highlight("hello", "txt");
        assert_eq!(text.to_plain_text(), "hello");
    }

    #[test]
    fn registry_names_returns_all() {
        let mut reg = TokenizerRegistry::new();
        reg.register(Box::new(rust_tokenizer()));
        reg.register(Box::new(PlainTokenizer));
        let names = reg.names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"Rust"));
        assert!(names.contains(&"Plain"));
    }
}

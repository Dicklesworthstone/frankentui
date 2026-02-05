#![forbid(unsafe_code)]

//! GitHub-Flavored Markdown renderer for FrankenTUI.
//!
//! Converts Markdown text into styled [`Text`] for rendering in terminal UIs.
//! Uses [pulldown-cmark] for parsing with full GFM support including:
//!
//! - Tables, strikethrough, task lists
//! - Math expressions (`$inline$` and `$$block$$`) rendered as Unicode
//! - Footnotes with `[^id]` syntax
//! - Admonitions (`[!NOTE]`, `[!WARNING]`, etc.)
//!
//! # Auto-Detection
//!
//! Use [`is_likely_markdown`] for efficient detection of text that appears to be
//! Markdown or GFM. This is useful for automatically rendering markdown when
//! displaying user-provided text.
//!
//! # Streaming / Fragment Support
//!
//! Use [`render_streaming`] or [`MarkdownRenderer::render_streaming`] to render
//! incomplete markdown fragments gracefully. This handles:
//! - Unclosed code blocks (renders content with code style)
//! - Incomplete inline formatting (renders partial bold/italic)
//! - Incomplete links and math expressions
//!
//! # Example
//! ```
//! use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme, is_likely_markdown};
//!
//! let text = "# Hello\n\nSome **bold** text with $E=mc^2$.";
//!
//! // Auto-detect if this looks like markdown
//! if is_likely_markdown(text).is_likely() {
//!     let renderer = MarkdownRenderer::new(MarkdownTheme::default());
//!     let styled = renderer.render(text);
//!     assert!(styled.height() > 0);
//! }
//! ```

#[cfg(feature = "diagram")]
use crate::diagram;
#[cfg(feature = "syntax")]
use crate::syntax::{HighlightTheme, SyntaxHighlighter};
use ftui_render::cell::PackedRgba;
use ftui_style::{Style, TableEffectScope, TableSection, TableTheme};
use ftui_text::text::{Line, Span, Text};
use pulldown_cmark::{
    Alignment, BlockQuoteKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
#[cfg(feature = "syntax")]
use std::sync::Arc;

// ---------------------------------------------------------------------------
// GFM Auto-Detection
// ---------------------------------------------------------------------------

/// Result of markdown detection analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownDetection {
    /// Number of markdown indicators found.
    pub indicators: u8,
    /// Whether the text appears to be markdown (2+ indicators).
    likely: bool,
}

impl MarkdownDetection {
    /// Returns true if the text is likely markdown (2+ indicators found).
    #[must_use]
    pub const fn is_likely(self) -> bool {
        self.likely
    }

    /// Returns true if the text is definitely markdown (4+ indicators).
    #[must_use]
    pub const fn is_confident(self) -> bool {
        self.indicators >= 4
    }

    /// Returns a confidence score from 0.0 to 1.0.
    #[must_use]
    pub fn confidence(self) -> f32 {
        (self.indicators as f32 / 6.0).min(1.0)
    }
}

/// Fast, efficient detection of text that looks like GitHub-Flavored Markdown.
///
/// Uses simple byte-level pattern matching for maximum speed. Looks for:
/// - Headings (`#`)
/// - Bold/italic (`**`, `*`, `__`, `_`)
/// - Code (`` ` ``, ` ``` `)
/// - Links (`[`, `](`)
/// - Lists (`-`, `*`, `1.`)
/// - Math (`$`)
/// - Tables (`|`)
/// - Task lists (`[ ]`, `[x]`)
/// - Blockquotes (`>`)
///
/// This is designed to be called on every piece of text with minimal overhead.
/// Returns a [`MarkdownDetection`] with indicator count and likelihood assessment.
///
/// # Performance
///
/// This function scans bytes directly without regex or parsing, making it
/// suitable for high-frequency calls. Typical execution is under 100ns for
/// short strings.
///
/// # Example
/// ```
/// use ftui_extras::markdown::is_likely_markdown;
///
/// assert!(is_likely_markdown("# Hello\n**bold**").is_likely());
/// assert!(!is_likely_markdown("just plain text").is_likely());
/// assert!(is_likely_markdown("```rust\ncode\n```").is_confident());
/// ```
#[must_use]
pub fn is_likely_markdown(text: &str) -> MarkdownDetection {
    let bytes = text.as_bytes();
    let len = bytes.len();

    if len == 0 {
        return MarkdownDetection {
            indicators: 0,
            likely: false,
        };
    }

    let mut indicators: u8 = 0;

    // Check first bytes for line-start patterns
    let first_byte = bytes[0];

    // Heading at start of text
    if first_byte == b'#' {
        indicators = indicators.saturating_add(1);
    }

    // Blockquote at start
    if first_byte == b'>' {
        indicators = indicators.saturating_add(1);
    }

    // List item at start (-, *, digit followed by .)
    if (first_byte == b'-' || first_byte == b'*') && len > 1 && bytes[1] == b' ' {
        indicators = indicators.saturating_add(1);
    }
    if first_byte.is_ascii_digit() && len > 1 && bytes[1] == b'.' {
        indicators = indicators.saturating_add(1);
    }

    // Scan through bytes looking for patterns
    let mut i = 0;
    while i < len {
        let b = bytes[i];

        // After newline, check for line-start patterns
        if b == b'\n' && i + 1 < len {
            let next = bytes[i + 1];
            // Heading
            if next == b'#' {
                indicators = indicators.saturating_add(1);
            }
            // Blockquote
            if next == b'>' {
                indicators = indicators.saturating_add(1);
            }
            // List item
            if (next == b'-' || next == b'*') && i + 2 < len && bytes[i + 2] == b' ' {
                indicators = indicators.saturating_add(1);
            }
            if next.is_ascii_digit() && i + 2 < len && bytes[i + 2] == b'.' {
                indicators = indicators.saturating_add(1);
            }
            // Table row
            if next == b'|' {
                indicators = indicators.saturating_add(1);
            }
        }

        // Code fence (```)
        if b == b'`' && i + 2 < len && bytes[i + 1] == b'`' && bytes[i + 2] == b'`' {
            indicators = indicators.saturating_add(2); // Strong indicator
            i += 3;
            continue;
        }

        // Inline code (single backtick not followed by more backticks)
        if b == b'`' && (i + 1 >= len || bytes[i + 1] != b'`') {
            indicators = indicators.saturating_add(1);
            i += 1;
            continue;
        }

        // Bold (**) or italic (*)
        if b == b'*' && i + 1 < len && bytes[i + 1] == b'*' {
            indicators = indicators.saturating_add(1);
            i += 2;
            continue;
        }
        if b == b'*' {
            // Single * could be italic or list, check context
            if i > 0 && !bytes[i - 1].is_ascii_whitespace() {
                indicators = indicators.saturating_add(1);
            }
        }

        // Bold (__) or italic (_)
        if b == b'_' && i + 1 < len && bytes[i + 1] == b'_' {
            indicators = indicators.saturating_add(1);
            i += 2;
            continue;
        }

        // Link/image start
        if b == b'[' {
            // Look for ]( pattern ahead
            let mut j = i + 1;
            while j < len && j < i + 100 {
                if bytes[j] == b']' && j + 1 < len && bytes[j + 1] == b'(' {
                    indicators = indicators.saturating_add(1);
                    break;
                }
                if bytes[j] == b'\n' {
                    break;
                }
                j += 1;
            }
        }

        // Math expressions ($)
        if b == b'$' {
            indicators = indicators.saturating_add(1);
            // Display math ($$)
            if i + 1 < len && bytes[i + 1] == b'$' {
                indicators = indicators.saturating_add(1);
                i += 2;
                continue;
            }
        }

        // Task list checkbox [ ] or [x]
        if b == b'[' && i + 2 < len && bytes[i + 2] == b']' {
            let middle = bytes[i + 1];
            if middle == b' ' || middle == b'x' || middle == b'X' {
                indicators = indicators.saturating_add(1);
                i += 3;
                continue;
            }
        }

        // Strikethrough (~~)
        if b == b'~' && i + 1 < len && bytes[i + 1] == b'~' {
            indicators = indicators.saturating_add(1);
            i += 2;
            continue;
        }

        // Table separator (|)
        if b == b'|' {
            indicators = indicators.saturating_add(1);
        }

        // HTML tags that GFM supports (<kbd>, <sub>, <sup>, <br>)
        if b == b'<'
            && i + 3 < len
            && (bytes[i + 1..].starts_with(b"kbd")
                || bytes[i + 1..].starts_with(b"sub")
                || bytes[i + 1..].starts_with(b"sup")
                || bytes[i + 1..].starts_with(b"br")
                || bytes[i + 1..].starts_with(b"hr"))
        {
            indicators = indicators.saturating_add(1);
        }

        // Footnote reference [^
        if b == b'[' && i + 1 < len && bytes[i + 1] == b'^' {
            indicators = indicators.saturating_add(1);
        }

        // Horizontal rule (--- at line start, checked above in newline handler)
        if b == b'-' && i + 2 < len && bytes[i + 1] == b'-' && bytes[i + 2] == b'-' {
            // Check if at line start
            if i == 0 || bytes[i - 1] == b'\n' {
                indicators = indicators.saturating_add(1);
            }
        }

        i += 1;

        // Early exit if we've found enough indicators
        if indicators >= 6 {
            break;
        }
    }

    MarkdownDetection {
        indicators,
        likely: indicators >= 2,
    }
}

/// Complete any unclosed markdown constructs in a fragment.
///
/// This is used internally by streaming render to handle incomplete input.
fn complete_fragment(text: &str) -> String {
    let mut result = text.to_string();

    // Count backticks to close any unclosed code
    let backtick_count = text.bytes().filter(|&b| b == b'`').count();

    // Check for unclosed code fence
    let fence_count = text.matches("```").count();
    if fence_count % 2 == 1 {
        // Odd number of fences means one is unclosed
        result.push_str("\n```");
    } else if backtick_count % 2 == 1 {
        // Odd backticks means unclosed inline code
        result.push('`');
    }

    // Check for unclosed bold (**)
    let bold_count = text.matches("**").count();
    if bold_count % 2 == 1 {
        result.push_str("**");
    }

    // Check for unclosed italic (*) - tricky because * is also used for lists
    // Only close if there's clearly an unclosed italic (not at line start)
    let asterisk_count = text.bytes().filter(|&b| b == b'*').count();
    let bold_asterisks = bold_count * 2;
    let remaining = asterisk_count.saturating_sub(bold_asterisks);
    if remaining % 2 == 1
        && let Some(pos) = text.rfind('*')
        && pos > 0
        && !text.as_bytes()[pos - 1].is_ascii_whitespace()
    {
        result.push('*');
    }

    // Check for unclosed math
    let dollar_count = text.bytes().filter(|&b| b == b'$').count();
    let display_math_count = text.matches("$$").count();
    if display_math_count % 2 == 1 {
        result.push_str("$$");
    } else {
        let inline_math = dollar_count.saturating_sub(display_math_count * 2);
        if inline_math % 2 == 1 {
            result.push('$');
        }
    }

    // Check for unclosed link - look for [ without matching ]()
    // State machine: track whether we're in [text] or (url) part
    #[derive(Clone, Copy, PartialEq)]
    enum LinkState {
        None,
        InBracket,    // Inside [...]
        AfterBracket, // Saw ], waiting for (
        InParen,      // Inside (...)
    }
    let mut state = LinkState::None;
    let mut bracket_depth = 0i32;
    let mut paren_depth = 0i32;

    for c in text.chars() {
        match (state, c) {
            (LinkState::None, '[') => {
                state = LinkState::InBracket;
                bracket_depth = 1;
            }
            (LinkState::InBracket, '[') => {
                bracket_depth += 1;
            }
            (LinkState::InBracket, ']') => {
                bracket_depth -= 1;
                if bracket_depth == 0 {
                    state = LinkState::AfterBracket;
                }
            }
            (LinkState::AfterBracket, '(') => {
                state = LinkState::InParen;
                paren_depth = 1;
            }
            (LinkState::AfterBracket, '[') => {
                // New link started
                state = LinkState::InBracket;
                bracket_depth = 1;
            }
            (LinkState::AfterBracket, _) => {
                // Not a link, reset
                state = LinkState::None;
            }
            (LinkState::InParen, '(') => {
                paren_depth += 1;
            }
            (LinkState::InParen, ')') => {
                paren_depth -= 1;
                if paren_depth == 0 {
                    state = LinkState::None;
                }
            }
            _ => {}
        }
    }

    // Close unclosed constructs based on final state
    match state {
        LinkState::InBracket => {
            result.push_str("](...)");
        }
        LinkState::AfterBracket => {
            result.push_str("(...)");
        }
        LinkState::InParen => {
            result.push(')');
        }
        LinkState::None => {}
    }

    result
}

/// Render markdown text that may be incomplete (streaming/fragment mode).
///
/// This function handles incomplete markdown gracefully by auto-closing
/// unclosed constructs like code blocks, bold, italic, and math expressions.
/// Useful for rendering markdown as it streams in character by character.
///
/// # Example
/// ```
/// use ftui_extras::markdown::{render_streaming, MarkdownTheme};
///
/// // Incomplete code block
/// let text = render_streaming("```rust\nfn main()", &MarkdownTheme::default());
/// assert!(text.height() > 0);
///
/// // Incomplete bold
/// let text = render_streaming("Some **bold text", &MarkdownTheme::default());
/// assert!(text.height() > 0);
/// ```
#[must_use]
pub fn render_streaming(fragment: &str, theme: &MarkdownTheme) -> Text {
    let completed = complete_fragment(fragment);
    let renderer = MarkdownRenderer::new(theme.clone());
    renderer.render(&completed)
}

/// Convenience function to auto-detect and render text.
///
/// If the text looks like markdown (2+ indicators), renders it as markdown.
/// Otherwise returns the text as plain [`Text`].
///
/// # Example
/// ```
/// use ftui_extras::markdown::{auto_render, MarkdownTheme};
///
/// let theme = MarkdownTheme::default();
///
/// // Markdown text gets rendered
/// let md = auto_render("# Hello\n**world**", &theme);
///
/// // Plain text stays plain
/// let plain = auto_render("just some text", &theme);
/// ```
#[must_use]
pub fn auto_render(text: &str, theme: &MarkdownTheme) -> Text {
    if is_likely_markdown(text).is_likely() {
        let renderer = MarkdownRenderer::new(theme.clone());
        renderer.render(text)
    } else {
        Text::raw(text)
    }
}

/// Convenience function to auto-detect and render potentially incomplete text.
///
/// Combines [`is_likely_markdown`] detection with [`render_streaming`] for
/// handling partial markdown fragments that arrive during streaming.
///
/// # Example
/// ```
/// use ftui_extras::markdown::{auto_render_streaming, MarkdownTheme};
///
/// let theme = MarkdownTheme::default();
///
/// // Incomplete markdown fragment
/// let text = auto_render_streaming("# Hello\n**bold", &theme);
/// assert!(text.height() > 0);
/// ```
#[must_use]
pub fn auto_render_streaming(fragment: &str, theme: &MarkdownTheme) -> Text {
    if is_likely_markdown(fragment).is_likely() {
        render_streaming(fragment, theme)
    } else {
        Text::raw(fragment)
    }
}

// ---------------------------------------------------------------------------
// LaTeX to Unicode conversion
// ---------------------------------------------------------------------------

/// Convert LaTeX math expression to Unicode approximation.
///
/// Uses the `unicodeit` crate for symbol conversion, with fallbacks for
/// unsupported constructs.
fn latex_to_unicode(latex: &str) -> String {
    // Use unicodeit for the heavy lifting
    let mut result = unicodeit::replace(latex);

    // Clean up any remaining backslash commands that weren't converted
    // by applying some common fallbacks
    result = apply_latex_fallbacks(&result);

    result
}

/// Apply fallback conversions for LaTeX constructs not handled by unicodeit.
fn apply_latex_fallbacks(text: &str) -> String {
    let mut result = text.to_string();

    // Common fraction fallbacks
    let fractions = [
        (r"\frac{1}{2}", "½"),
        (r"\frac{1}{3}", "⅓"),
        (r"\frac{2}{3}", "⅔"),
        (r"\frac{1}{4}", "¼"),
        (r"\frac{3}{4}", "¾"),
        (r"\frac{1}{5}", "⅕"),
        (r"\frac{2}{5}", "⅖"),
        (r"\frac{3}{5}", "⅗"),
        (r"\frac{4}{5}", "⅘"),
        (r"\frac{1}{6}", "⅙"),
        (r"\frac{5}{6}", "⅚"),
        (r"\frac{1}{7}", "⅐"),
        (r"\frac{1}{8}", "⅛"),
        (r"\frac{3}{8}", "⅜"),
        (r"\frac{5}{8}", "⅝"),
        (r"\frac{7}{8}", "⅞"),
        (r"\frac{1}{9}", "⅑"),
        (r"\frac{1}{10}", "⅒"),
    ];

    for (latex_frac, unicode) in fractions {
        result = result.replace(latex_frac, unicode);
    }

    // Handle generic \frac{a}{b} -> a/b
    while let Some(start) = result.find(r"\frac{") {
        if let Some(end) = find_matching_brace(&result[start + 6..]) {
            let num_end = start + 6 + end;
            let numerator = &result[start + 6..num_end];

            // Look for denominator
            if result[num_end + 1..].starts_with('{')
                && let Some(denom_end) = find_matching_brace(&result[num_end + 2..])
            {
                let denominator = &result[num_end + 2..num_end + 2 + denom_end];
                let replacement = format!("{numerator}/{denominator}");
                let full_end = num_end + 3 + denom_end;
                result = format!("{}{}{}", &result[..start], replacement, &result[full_end..]);
                continue;
            }
        }
        break;
    }

    // Square root: \sqrt{x} -> √x
    while let Some(start) = result.find(r"\sqrt{") {
        if let Some(end) = find_matching_brace(&result[start + 6..]) {
            let content = &result[start + 6..start + 6 + end];
            let replacement = format!("√{content}");
            result = format!(
                "{}{}{}",
                &result[..start],
                replacement,
                &result[start + 7 + end..]
            );
        } else {
            break;
        }
    }

    // \sqrt without braces -> √
    result = result.replace(r"\sqrt", "√");

    // Common operators and symbols not in unicodeit
    let symbols = [
        (r"\cdot", "·"),
        (r"\times", "×"),
        (r"\div", "÷"),
        (r"\pm", "±"),
        (r"\mp", "∓"),
        (r"\neq", "≠"),
        (r"\approx", "≈"),
        (r"\equiv", "≡"),
        (r"\leq", "≤"),
        (r"\geq", "≥"),
        (r"\ll", "≪"),
        (r"\gg", "≫"),
        (r"\subset", "⊂"),
        (r"\supset", "⊃"),
        (r"\subseteq", "⊆"),
        (r"\supseteq", "⊇"),
        (r"\cup", "∪"),
        (r"\cap", "∩"),
        (r"\emptyset", "∅"),
        (r"\forall", "∀"),
        (r"\exists", "∃"),
        (r"\nexists", "∄"),
        (r"\neg", "¬"),
        (r"\land", "∧"),
        (r"\lor", "∨"),
        (r"\oplus", "⊕"),
        (r"\otimes", "⊗"),
        (r"\perp", "⊥"),
        (r"\parallel", "∥"),
        (r"\angle", "∠"),
        (r"\triangle", "△"),
        (r"\square", "□"),
        (r"\diamond", "◇"),
        (r"\star", "⋆"),
        (r"\circ", "∘"),
        (r"\bullet", "•"),
        (r"\nabla", "∇"),
        (r"\partial", "∂"),
        (r"\hbar", "ℏ"),
        (r"\ell", "ℓ"),
        (r"\Re", "ℜ"),
        (r"\Im", "ℑ"),
        (r"\wp", "℘"),
        (r"\aleph", "ℵ"),
        (r"\beth", "ℶ"),
        (r"\gimel", "ℷ"),
        (r"\daleth", "ℸ"),
    ];

    for (latex_sym, unicode) in symbols {
        result = result.replace(latex_sym, unicode);
    }

    // Clean up extra whitespace
    result = result.split_whitespace().collect::<Vec<_>>().join(" ");

    result
}

/// Find the position of the matching closing brace.
fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 1;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Convert text to Unicode subscript characters where possible.
fn to_unicode_subscript(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '0' => '₀',
            '1' => '₁',
            '2' => '₂',
            '3' => '₃',
            '4' => '₄',
            '5' => '₅',
            '6' => '₆',
            '7' => '₇',
            '8' => '₈',
            '9' => '₉',
            '+' => '₊',
            '-' => '₋',
            '=' => '₌',
            '(' => '₍',
            ')' => '₎',
            'a' => 'ₐ',
            'e' => 'ₑ',
            'h' => 'ₕ',
            'i' => 'ᵢ',
            'j' => 'ⱼ',
            'k' => 'ₖ',
            'l' => 'ₗ',
            'm' => 'ₘ',
            'n' => 'ₙ',
            'o' => 'ₒ',
            'p' => 'ₚ',
            'r' => 'ᵣ',
            's' => 'ₛ',
            't' => 'ₜ',
            'u' => 'ᵤ',
            'v' => 'ᵥ',
            'x' => 'ₓ',
            _ => c, // Keep unsupported chars as-is
        })
        .collect()
}

/// Convert text to Unicode superscript characters where possible.
fn to_unicode_superscript(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '0' => '⁰',
            '1' => '¹',
            '2' => '²',
            '3' => '³',
            '4' => '⁴',
            '5' => '⁵',
            '6' => '⁶',
            '7' => '⁷',
            '8' => '⁸',
            '9' => '⁹',
            '+' => '⁺',
            '-' => '⁻',
            '=' => '⁼',
            '(' => '⁽',
            ')' => '⁾',
            'a' => 'ᵃ',
            'b' => 'ᵇ',
            'c' => 'ᶜ',
            'd' => 'ᵈ',
            'e' => 'ᵉ',
            'f' => 'ᶠ',
            'g' => 'ᵍ',
            'h' => 'ʰ',
            'i' => 'ⁱ',
            'j' => 'ʲ',
            'k' => 'ᵏ',
            'l' => 'ˡ',
            'm' => 'ᵐ',
            'n' => 'ⁿ',
            'o' => 'ᵒ',
            'p' => 'ᵖ',
            'r' => 'ʳ',
            's' => 'ˢ',
            't' => 'ᵗ',
            'u' => 'ᵘ',
            'v' => 'ᵛ',
            'w' => 'ʷ',
            'x' => 'ˣ',
            'y' => 'ʸ',
            'z' => 'ᶻ',
            _ => c, // Keep unsupported chars as-is
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Theme for Markdown rendering.
///
/// Each field controls the style applied to the corresponding Markdown element.
/// The default theme uses a carefully curated color palette designed for
/// excellent readability in terminal environments.
#[derive(Debug, Clone)]
pub struct MarkdownTheme {
    // Headings - gradient from bright to muted
    pub h1: Style,
    pub h2: Style,
    pub h3: Style,
    pub h4: Style,
    pub h5: Style,
    pub h6: Style,

    // Code
    pub code_inline: Style,
    pub code_block: Style,

    // Text formatting
    pub blockquote: Style,
    pub link: Style,
    pub emphasis: Style,
    pub strong: Style,
    pub strikethrough: Style,

    // Lists
    pub list_bullet: Style,
    pub horizontal_rule: Style,

    // Tables
    pub table_theme: TableTheme,

    // Task lists
    pub task_done: Style,
    pub task_todo: Style,

    // Math (LaTeX)
    pub math_inline: Style,
    pub math_block: Style,

    // Footnotes
    pub footnote_ref: Style,
    pub footnote_def: Style,

    // Admonitions (GitHub alerts)
    pub admonition_note: Style,
    pub admonition_tip: Style,
    pub admonition_important: Style,
    pub admonition_warning: Style,
    pub admonition_caution: Style,
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        let table_theme = default_markdown_table_theme();
        Self {
            // Headings: bright white -> soft lavender gradient
            h1: Style::new().fg(PackedRgba::rgb(255, 255, 255)).bold(),
            h2: Style::new().fg(PackedRgba::rgb(200, 200, 255)).bold(),
            h3: Style::new().fg(PackedRgba::rgb(180, 180, 230)).bold(),
            h4: Style::new().fg(PackedRgba::rgb(160, 160, 210)).bold(),
            h5: Style::new().fg(PackedRgba::rgb(140, 140, 190)).bold(),
            h6: Style::new().fg(PackedRgba::rgb(120, 120, 170)).bold(),

            // Code: warm amber for inline, soft gray for blocks
            code_inline: Style::new().fg(PackedRgba::rgb(230, 180, 80)),
            code_block: Style::new().fg(PackedRgba::rgb(200, 200, 200)),

            // Text formatting
            blockquote: Style::new().fg(PackedRgba::rgb(150, 150, 150)).italic(),
            link: Style::new().fg(PackedRgba::rgb(100, 150, 255)).underline(),
            emphasis: Style::new().italic(),
            strong: Style::new().bold(),
            strikethrough: Style::new().strikethrough(),

            // Lists: warm gold bullets
            list_bullet: Style::new().fg(PackedRgba::rgb(180, 180, 100)),
            horizontal_rule: Style::new().fg(PackedRgba::rgb(100, 100, 100)).dim(),

            // Tables: cool borders with subtle zebra rows
            table_theme,

            // Task lists: green for done, cyan for todo
            task_done: Style::new().fg(PackedRgba::rgb(120, 220, 120)),
            task_todo: Style::new().fg(PackedRgba::rgb(150, 200, 220)),

            // Math: elegant purple/magenta for mathematical expressions
            math_inline: Style::new().fg(PackedRgba::rgb(220, 150, 255)).italic(),
            math_block: Style::new().fg(PackedRgba::rgb(200, 140, 240)).bold(),

            // Footnotes: subtle teal
            footnote_ref: Style::new().fg(PackedRgba::rgb(100, 180, 180)).dim(),
            footnote_def: Style::new().fg(PackedRgba::rgb(120, 160, 160)),

            // Admonitions: semantic colors matching their meaning
            admonition_note: Style::new().fg(PackedRgba::rgb(100, 150, 255)).bold(), // Blue - informational
            admonition_tip: Style::new().fg(PackedRgba::rgb(100, 200, 100)).bold(), // Green - helpful
            admonition_important: Style::new().fg(PackedRgba::rgb(180, 130, 255)).bold(), // Purple - important
            admonition_warning: Style::new().fg(PackedRgba::rgb(255, 200, 80)).bold(), // Yellow/amber - warning
            admonition_caution: Style::new().fg(PackedRgba::rgb(255, 100, 100)).bold(), // Red - danger
        }
    }
}

fn default_markdown_table_theme() -> TableTheme {
    let border = Style::new().fg(PackedRgba::rgb(145, 160, 190));
    let header = Style::new()
        .fg(PackedRgba::rgb(250, 250, 255))
        .bg(PackedRgba::rgb(55, 75, 115))
        .bold();
    let row = Style::new().fg(PackedRgba::rgb(225, 230, 240));
    let row_alt = Style::new()
        .fg(PackedRgba::rgb(225, 230, 240))
        .bg(PackedRgba::rgb(30, 35, 50));
    let divider = Style::new().fg(PackedRgba::rgb(120, 135, 165));

    TableTheme {
        border,
        header,
        row,
        row_alt,
        row_selected: header,
        row_hover: row_alt,
        divider,
        padding: 1,
        column_gap: 1,
        row_height: 1,
        effects: Vec::new(),
        preset_id: None,
    }
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

/// Markdown renderer that converts Markdown text into styled [`Text`].
///
/// Supports GitHub-Flavored Markdown including math expressions, task lists,
/// footnotes, and admonitions.
#[derive(Debug, Clone)]
pub struct MarkdownRenderer {
    theme: MarkdownTheme,
    rule_width: u16,
    table_max_width: Option<u16>,
    table_effect_phase: Option<f32>,
    #[cfg(feature = "syntax")]
    syntax_highlighter: Option<Arc<SyntaxHighlighter>>,
}

impl MarkdownRenderer {
    /// Create a new renderer with the given theme.
    #[must_use]
    pub fn new(theme: MarkdownTheme) -> Self {
        Self {
            theme,
            rule_width: 40,
            table_max_width: None,
            table_effect_phase: None,
            #[cfg(feature = "syntax")]
            syntax_highlighter: None,
        }
    }

    /// Set the width for horizontal rules.
    #[must_use]
    pub fn rule_width(mut self, width: u16) -> Self {
        self.rule_width = width;
        self
    }

    /// Set a maximum width for table rendering (including borders).
    #[must_use]
    pub fn table_max_width(mut self, width: u16) -> Self {
        self.table_max_width = Some(width);
        self
    }

    /// Set an optional phase for table effect animation (0.0..1.0).
    ///
    /// When not set, table effects are not applied.
    #[must_use]
    pub fn table_effect_phase(mut self, phase: f32) -> Self {
        self.table_effect_phase = Some(phase);
        self
    }

    /// Enable syntax highlighting for code blocks with a custom theme.
    #[cfg(feature = "syntax")]
    #[must_use]
    pub fn with_syntax_theme(mut self, theme: HighlightTheme) -> Self {
        self.syntax_highlighter = Some(Arc::new(SyntaxHighlighter::with_theme(theme)));
        self
    }

    /// Enable syntax highlighting for code blocks with a shared highlighter.
    #[cfg(feature = "syntax")]
    #[must_use]
    pub fn with_syntax_highlighter(mut self, highlighter: Arc<SyntaxHighlighter>) -> Self {
        self.syntax_highlighter = Some(highlighter);
        self
    }

    /// Render a Markdown string into styled [`Text`].
    ///
    /// Parses the input as GitHub-Flavored Markdown with all extensions enabled:
    /// tables, strikethrough, task lists, math, footnotes, and admonitions.
    #[must_use]
    pub fn render(&self, markdown: &str) -> Text {
        let options = Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TABLES
            | Options::ENABLE_HEADING_ATTRIBUTES
            | Options::ENABLE_MATH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_GFM;
        let parser = Parser::new_ext(markdown, options);

        let mut builder = RenderState::new(
            &self.theme,
            self.rule_width,
            self.table_max_width,
            self.table_effect_phase,
        );
        #[cfg(feature = "syntax")]
        {
            builder.syntax_highlighter = self.syntax_highlighter.as_deref();
        }
        builder.process(parser);
        builder.finish()
    }

    /// Render a potentially incomplete markdown fragment.
    ///
    /// This method handles streaming scenarios where markdown arrives
    /// piece by piece and may have unclosed constructs. It automatically
    /// closes unclosed code blocks, bold/italic markers, math expressions,
    /// and links before rendering.
    ///
    /// # Example
    /// ```
    /// use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
    ///
    /// let renderer = MarkdownRenderer::new(MarkdownTheme::default());
    ///
    /// // Render incomplete code block - will be closed automatically
    /// let text = renderer.render_streaming("```rust\nfn main()");
    /// assert!(text.height() > 0);
    /// ```
    #[must_use]
    pub fn render_streaming(&self, fragment: &str) -> Text {
        let completed = complete_fragment(fragment);
        self.render(&completed)
    }

    /// Check if text appears to be markdown and render appropriately.
    ///
    /// Returns rendered markdown if the text looks like markdown (2+ indicators),
    /// otherwise returns the text as plain [`Text`].
    #[must_use]
    pub fn auto_render(&self, text: &str) -> Text {
        if is_likely_markdown(text).is_likely() {
            self.render(text)
        } else {
            Text::raw(text)
        }
    }

    /// Check if text appears to be markdown and render as streaming fragment.
    ///
    /// Combines auto-detection with streaming fragment handling.
    #[must_use]
    pub fn auto_render_streaming(&self, fragment: &str) -> Text {
        if is_likely_markdown(fragment).is_likely() {
            self.render_streaming(fragment)
        } else {
            Text::raw(fragment)
        }
    }
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new(MarkdownTheme::default())
    }
}

// ---------------------------------------------------------------------------
// Internal render state machine
// ---------------------------------------------------------------------------

/// Style stack entry tracking what Markdown context is active.
#[derive(Debug, Clone)]
enum StyleContext {
    Heading(HeadingLevel),
    Emphasis,
    Strong,
    Strikethrough,
    CodeBlock,
    Blockquote,
    Link(String),
    FootnoteDefinition,
}

/// Tracks list nesting and numbering.
#[derive(Debug, Clone)]
struct ListState {
    ordered: bool,
    next_number: u64,
}

/// Admonition type from GFM blockquote tags.
#[derive(Debug, Clone, Copy)]
enum AdmonitionKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AdmonitionKind {
    fn from_blockquote_kind(kind: Option<BlockQuoteKind>) -> Option<Self> {
        match kind? {
            BlockQuoteKind::Note => Some(Self::Note),
            BlockQuoteKind::Tip => Some(Self::Tip),
            BlockQuoteKind::Important => Some(Self::Important),
            BlockQuoteKind::Warning => Some(Self::Warning),
            BlockQuoteKind::Caution => Some(Self::Caution),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Note => "ℹ️ ",
            Self::Tip => "💡",
            Self::Important => "❗",
            Self::Warning => "⚠️ ",
            Self::Caution => "🔴",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Note => "NOTE",
            Self::Tip => "TIP",
            Self::Important => "IMPORTANT",
            Self::Warning => "WARNING",
            Self::Caution => "CAUTION",
        }
    }
}

#[derive(Debug, Clone)]
struct TableRow {
    cells: Vec<Vec<Span<'static>>>,
    is_header: bool,
}

struct TableRowContext<'a> {
    widths: &'a [usize],
    alignments: &'a [Alignment],
    base_style: Style,
    border_style: Style,
    section: TableSection,
    effect_phase: Option<f32>,
    resolver: &'a ftui_style::TableEffectResolver<'a>,
}

#[derive(Debug, Clone)]
struct TableState {
    alignments: Vec<Alignment>,
    rows: Vec<TableRow>,
    current_row: Vec<Vec<Span<'static>>>,
    in_head: bool,
}

impl TableState {
    fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            rows: Vec::new(),
            current_row: Vec::new(),
            in_head: false,
        }
    }
}

struct RenderState<'t> {
    theme: &'t MarkdownTheme,
    rule_width: u16,
    table_max_width: Option<u16>,
    table_effect_phase: Option<f32>,
    #[cfg(feature = "syntax")]
    syntax_highlighter: Option<&'t SyntaxHighlighter>,
    lines: Vec<Line>,
    current_spans: Vec<Span<'static>>,
    style_stack: Vec<StyleContext>,
    list_stack: Vec<ListState>,
    /// Whether we're collecting text inside a code block.
    in_code_block: bool,
    code_block_lines: Vec<String>,
    /// Language of the current code block (for special handling like mermaid).
    code_block_lang: Option<String>,
    /// Whether we're inside a blockquote.
    blockquote_depth: u16,
    /// Current admonition type (if in an admonition blockquote).
    current_admonition: Option<AdmonitionKind>,
    /// Track if we need a blank line separator.
    needs_blank: bool,
    /// Pending task list marker (checked state).
    pending_task_marker: Option<bool>,
    /// Whether we're waiting to emit a list item prefix.
    /// Deferred so task markers can replace the bullet.
    pending_list_prefix: bool,
    /// Footnote definitions collected during parsing.
    footnotes: Vec<(String, Vec<Line>)>,
    /// Current footnote being collected.
    current_footnote: Option<String>,
    /// Lines being collected for the current footnote definition.
    current_footnote_lines: Vec<Line>,
    /// Table state (if currently parsing a table).
    table_state: Option<TableState>,
}

impl<'t> RenderState<'t> {
    fn new(
        theme: &'t MarkdownTheme,
        rule_width: u16,
        table_max_width: Option<u16>,
        table_effect_phase: Option<f32>,
    ) -> Self {
        Self {
            theme,
            rule_width,
            table_max_width,
            table_effect_phase,
            #[cfg(feature = "syntax")]
            syntax_highlighter: None,
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: Vec::new(),
            list_stack: Vec::new(),
            in_code_block: false,
            code_block_lines: Vec::new(),
            code_block_lang: None,
            blockquote_depth: 0,
            current_admonition: None,
            needs_blank: false,
            pending_task_marker: None,
            pending_list_prefix: false,
            footnotes: Vec::new(),
            current_footnote: None,
            current_footnote_lines: Vec::new(),
            table_state: None,
        }
    }

    fn process<'a>(&mut self, parser: impl Iterator<Item = Event<'a>>) {
        for event in parser {
            match event {
                Event::Start(tag) => self.start_tag(tag),
                Event::End(tag) => self.end_tag(tag),
                Event::Text(text) => self.text(&text),
                Event::Code(code) => self.inline_code(&code),
                Event::SoftBreak => self.soft_break(),
                Event::HardBreak => self.hard_break(),
                Event::Rule => self.horizontal_rule(),
                Event::TaskListMarker(checked) => self.task_list_marker(checked),
                Event::FootnoteReference(label) => self.footnote_reference(&label),
                Event::InlineMath(latex) => self.inline_math(&latex),
                Event::DisplayMath(latex) => self.display_math(&latex),
                Event::Html(html) | Event::InlineHtml(html) => self.html(&html),
            }
        }

        // Append collected footnotes at the end
        self.append_footnotes();
    }

    fn start_tag(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush_blank();
                self.style_stack.push(StyleContext::Heading(level));
            }
            Tag::Paragraph => {
                self.flush_blank();
            }
            Tag::Emphasis => {
                self.style_stack.push(StyleContext::Emphasis);
            }
            Tag::Strong => {
                self.style_stack.push(StyleContext::Strong);
            }
            Tag::Strikethrough => {
                self.style_stack.push(StyleContext::Strikethrough);
            }
            Tag::CodeBlock(kind) => {
                self.flush_blank();
                self.in_code_block = true;
                self.code_block_lines.clear();
                // Extract language from code fence
                self.code_block_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => {
                        let lang_str = lang.to_string();
                        if lang_str.is_empty() {
                            None
                        } else {
                            Some(lang_str)
                        }
                    }
                    pulldown_cmark::CodeBlockKind::Indented => None,
                };
                self.style_stack.push(StyleContext::CodeBlock);
            }
            Tag::BlockQuote(kind) => {
                self.flush_blank();
                self.blockquote_depth = self.blockquote_depth.saturating_add(1);

                // Check for GFM admonitions
                if let Some(adm) = AdmonitionKind::from_blockquote_kind(kind) {
                    self.current_admonition = Some(adm);
                    // Emit the admonition header
                    let style = self.admonition_style(adm);
                    let header = format!("{} {}", adm.icon(), adm.label());
                    self.lines.push(Line::styled(header, style));
                }

                self.style_stack.push(StyleContext::Blockquote);
            }
            Tag::Link { dest_url, .. } => {
                self.style_stack
                    .push(StyleContext::Link(dest_url.to_string()));
            }
            Tag::List(start) => match start {
                Some(n) => self.list_stack.push(ListState {
                    ordered: true,
                    next_number: n,
                }),
                None => self.list_stack.push(ListState {
                    ordered: false,
                    next_number: 0,
                }),
            },
            Tag::Item => {
                self.flush_line();
                // Defer prefix emission - TaskListMarker may come next and replace
                // the bullet with a checkbox
                self.pending_list_prefix = true;
            }
            Tag::FootnoteDefinition(label) => {
                self.flush_line();
                self.current_footnote = Some(label.to_string());
                self.style_stack.push(StyleContext::FootnoteDefinition);
            }
            Tag::Table(alignments) => {
                self.flush_blank();
                self.table_state = Some(TableState::new(alignments));
            }
            Tag::TableHead => {
                if let Some(table) = self.table_state.as_mut() {
                    table.in_head = true;
                    // In pulldown-cmark 0.13+, header cells come directly inside
                    // TableHead without a TableRow wrapper. Clear current_row to
                    // prepare for collecting header cells.
                    table.current_row.clear();
                }
            }
            Tag::TableRow => {
                if let Some(table) = self.table_state.as_mut() {
                    table.current_row.clear();
                }
            }
            Tag::TableCell => {
                // Cells are captured on end tag.
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.style_stack.pop();
                self.flush_line();
                self.needs_blank = true;
            }
            TagEnd::Paragraph => {
                self.flush_line();
                self.needs_blank = true;
            }
            TagEnd::Emphasis => {
                self.style_stack.pop();
            }
            TagEnd::Strong => {
                self.style_stack.pop();
            }
            TagEnd::Strikethrough => {
                self.style_stack.pop();
            }
            TagEnd::CodeBlock => {
                self.style_stack.pop();
                self.flush_code_block();
                self.in_code_block = false;
                self.needs_blank = true;
            }
            TagEnd::BlockQuote(_) => {
                self.style_stack.pop();
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                if self.blockquote_depth == 0 {
                    self.current_admonition = None;
                }
                self.flush_line();
                self.needs_blank = true;
            }
            TagEnd::Link => {
                self.style_stack.pop();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.flush_line();
                    self.needs_blank = true;
                }
            }
            TagEnd::Item => {
                self.flush_line();
            }
            TagEnd::FootnoteDefinition => {
                self.style_stack.pop();
                self.flush_footnote_line();
                if let Some(label) = self.current_footnote.take() {
                    // Move collected footnote lines to the footnotes list
                    let content_lines = std::mem::take(&mut self.current_footnote_lines);
                    self.footnotes.push((label, content_lines));
                }
                self.needs_blank = true;
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table_state.as_mut()
                    && !table.current_row.is_empty()
                {
                    let row = TableRow {
                        cells: std::mem::take(&mut table.current_row),
                        is_header: true,
                    };
                    table.rows.push(row);
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table_state.as_mut()
                    && !table.current_row.is_empty()
                {
                    let is_header = table.in_head && table.rows.is_empty();
                    let row = TableRow {
                        cells: std::mem::take(&mut table.current_row),
                        is_header,
                    };
                    table.rows.push(row);
                    if table.in_head && is_header {
                        table.in_head = false;
                    }
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table_state.as_mut() {
                    let spans = std::mem::take(&mut self.current_spans);
                    let cell = if spans.is_empty() {
                        vec![Span::raw("")]
                    } else {
                        spans
                    };
                    table.current_row.push(cell);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table_state.as_mut()
                    && !table.current_row.is_empty()
                {
                    let row = TableRow {
                        cells: std::mem::take(&mut table.current_row),
                        is_header: table.in_head,
                    };
                    table.rows.push(row);
                }
                self.flush_table();
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_block_lines.push(text.to_string());
            return;
        }

        self.push_blockquote_prefix_if_needed();

        // Handle deferred list item prefix
        // Task markers take precedence over bullet points
        if self.pending_list_prefix {
            self.pending_list_prefix = false;
            let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));

            if let Some(checked) = self.pending_task_marker.take() {
                // Task list item - use checkbox instead of bullet
                let (marker, style) = if checked {
                    ("✓ ", self.theme.task_done)
                } else {
                    ("☐ ", self.theme.task_todo)
                };
                self.current_spans
                    .push(Span::styled(format!("{indent}{marker}"), style));
            } else {
                // Regular list item - use bullet
                let prefix = self.list_prefix();
                self.current_spans.push(Span::styled(
                    format!("{indent}{prefix}"),
                    self.theme.list_bullet,
                ));
            }
        } else if let Some(checked) = self.pending_task_marker.take() {
            // Task marker without pending prefix (shouldn't happen normally)
            let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
            let (marker, style) = if checked {
                ("✓ ", self.theme.task_done)
            } else {
                ("☐ ", self.theme.task_todo)
            };
            self.current_spans
                .push(Span::styled(format!("{indent}{marker}"), style));
        }

        let style = self.current_style();
        let link = self.current_link();
        let content = text.to_string();

        let mut span = match style {
            Some(s) => Span::styled(content, s),
            None => Span::raw(content),
        };

        if let Some(url) = link {
            span = span.link(url);
        }

        self.current_spans.push(span);
    }

    fn push_blockquote_prefix_if_needed(&mut self) {
        if self.blockquote_depth == 0 || !self.current_spans.is_empty() {
            return;
        }
        let bar_style = self
            .current_admonition
            .map(|adm| self.admonition_style(adm))
            .unwrap_or(self.theme.blockquote);
        let prefix = if self.current_admonition.is_some() {
            "┃ ".repeat(self.blockquote_depth as usize)
        } else {
            "│ ".repeat(self.blockquote_depth as usize)
        };
        self.current_spans
            .push(Span::styled(prefix, bar_style.dim()));
    }

    fn inline_code(&mut self, code: &str) {
        let mut span = Span::styled(format!("`{code}`"), self.theme.code_inline);
        if let Some(url) = self.current_link() {
            span = span.link(url);
        }
        self.current_spans.push(span);
    }

    fn soft_break(&mut self) {
        self.current_spans.push(Span::raw(String::from(" ")));
    }

    fn hard_break(&mut self) {
        if self.in_table() {
            self.current_spans.push(Span::raw(String::from(" ")));
        } else {
            self.flush_line();
        }
    }

    fn horizontal_rule(&mut self) {
        self.flush_blank();
        let rule = "─".repeat(self.rule_width as usize);
        self.lines
            .push(Line::styled(rule, self.theme.horizontal_rule));
        self.needs_blank = true;
    }

    fn task_list_marker(&mut self, checked: bool) {
        // Defer until we get the text content
        self.pending_task_marker = Some(checked);
    }

    fn footnote_reference(&mut self, label: &str) {
        let reference = format!("[^{label}]");
        self.current_spans
            .push(Span::styled(reference, self.theme.footnote_ref));
    }

    fn inline_math(&mut self, latex: &str) {
        let unicode = latex_to_unicode(latex);
        self.current_spans
            .push(Span::styled(unicode, self.theme.math_inline));
    }

    fn display_math(&mut self, latex: &str) {
        self.flush_blank();
        let unicode = latex_to_unicode(latex);

        // Center the math block with a subtle indicator
        for line in unicode.lines() {
            let formatted = format!("  {line}");
            self.lines
                .push(Line::styled(formatted, self.theme.math_block));
        }
        if unicode.is_empty() {
            self.lines
                .push(Line::styled(String::from("  "), self.theme.math_block));
        }
        self.needs_blank = true;
    }

    fn html(&mut self, html: &str) {
        // Handle a subset of HTML tags that make sense in terminal
        let html_lower = html.to_ascii_lowercase();
        let html_trimmed = html_lower.trim();

        // Line breaks
        if html_trimmed == "<br>" || html_trimmed == "<br/>" || html_trimmed == "<br />" {
            self.hard_break();
            return;
        }

        // Horizontal rule
        if html_trimmed == "<hr>" || html_trimmed == "<hr/>" || html_trimmed == "<hr />" {
            self.horizontal_rule();
            return;
        }

        // Keyboard key styling
        if html_trimmed.starts_with("<kbd>") {
            // Extract content between <kbd> and </kbd> if on same line
            if let Some(end_pos) = html_lower.find("</kbd>") {
                let content = &html[5..end_pos];
                let styled = format!("[{content}]");
                self.current_spans
                    .push(Span::styled(styled, self.theme.code_inline));
                return;
            }
        }

        // Details/Summary - show as collapsible indicator
        if html_trimmed.starts_with("<details") {
            self.flush_blank();
            self.current_spans
                .push(Span::styled(String::from("▶ "), self.theme.strong));
            return;
        }
        if html_trimmed == "</details>" {
            self.flush_line();
            self.needs_blank = true;
            return;
        }
        if html_trimmed.starts_with("<summary>") {
            // Extract summary text if present
            if let Some(end_pos) = html_lower.find("</summary>") {
                let content = &html[9..end_pos];
                self.current_spans
                    .push(Span::styled(content.trim().to_string(), self.theme.strong));
                self.flush_line();
            }
            return;
        }
        if html_trimmed == "</summary>" {
            return;
        }

        // Image - show alt text
        if html_trimmed.starts_with("<img ") {
            // Try to extract alt text
            if let Some(alt_start) = html_lower.find("alt=\"") {
                let after_alt = &html[alt_start + 5..];
                if let Some(alt_end) = after_alt.find('"') {
                    let alt_text = &after_alt[..alt_end];
                    if !alt_text.is_empty() {
                        self.current_spans
                            .push(Span::styled(format!("[{alt_text}]"), self.theme.emphasis));
                        return;
                    }
                }
            }
            // Fallback: show [image]
            self.current_spans
                .push(Span::styled(String::from("[image]"), self.theme.emphasis));
            return;
        }

        // Subscript - convert to Unicode subscript if possible
        if html_trimmed.starts_with("<sub>")
            && let Some(end_pos) = html_lower.find("</sub>")
        {
            let content = &html[5..end_pos];
            let subscript = to_unicode_subscript(content);
            self.current_spans.push(Span::raw(subscript));
            return;
        }

        // Superscript - convert to Unicode superscript if possible
        if html_trimmed.starts_with("<sup>")
            && let Some(end_pos) = html_lower.find("</sup>")
        {
            let content = &html[5..end_pos];
            let superscript = to_unicode_superscript(content);
            self.current_spans.push(Span::raw(superscript));
        }

        // Other HTML is ignored in terminal output
    }

    // -- helpers --

    fn admonition_style(&self, kind: AdmonitionKind) -> Style {
        match kind {
            AdmonitionKind::Note => self.theme.admonition_note,
            AdmonitionKind::Tip => self.theme.admonition_tip,
            AdmonitionKind::Important => self.theme.admonition_important,
            AdmonitionKind::Warning => self.theme.admonition_warning,
            AdmonitionKind::Caution => self.theme.admonition_caution,
        }
    }

    fn current_style(&self) -> Option<Style> {
        let mut result: Option<Style> = None;
        for ctx in &self.style_stack {
            let s = match ctx {
                StyleContext::Heading(HeadingLevel::H1) => self.theme.h1,
                StyleContext::Heading(HeadingLevel::H2) => self.theme.h2,
                StyleContext::Heading(HeadingLevel::H3) => self.theme.h3,
                StyleContext::Heading(HeadingLevel::H4) => self.theme.h4,
                StyleContext::Heading(HeadingLevel::H5) => self.theme.h5,
                StyleContext::Heading(HeadingLevel::H6) => self.theme.h6,
                StyleContext::Emphasis => self.theme.emphasis,
                StyleContext::Strong => self.theme.strong,
                StyleContext::Strikethrough => self.theme.strikethrough,
                StyleContext::CodeBlock => self.theme.code_block,
                StyleContext::Blockquote => self.theme.blockquote,
                StyleContext::Link(_) => self.theme.link,
                StyleContext::FootnoteDefinition => self.theme.footnote_def,
            };
            result = Some(match result {
                Some(existing) => s.merge(&existing),
                None => s,
            });
        }
        result
    }

    fn current_link(&self) -> Option<String> {
        // Return the most recently pushed link URL
        for ctx in self.style_stack.iter().rev() {
            if let StyleContext::Link(url) = ctx {
                return Some(url.clone());
            }
        }
        None
    }

    fn list_prefix(&mut self) -> String {
        if let Some(list) = self.list_stack.last_mut() {
            if list.ordered {
                let n = list.next_number;
                list.next_number += 1;
                format!("{n}. ")
            } else {
                String::from("• ")
            }
        } else {
            String::from("• ")
        }
    }

    fn in_table(&self) -> bool {
        self.table_state.is_some()
    }

    fn flush_table(&mut self) {
        let Some(table) = self.table_state.take() else {
            return;
        };
        if table.rows.is_empty() {
            return;
        }

        let column_count = table
            .rows
            .iter()
            .map(|row| row.cells.len())
            .max()
            .unwrap_or(0)
            .max(table.alignments.len());
        if column_count == 0 {
            return;
        }

        let mut widths = vec![0usize; column_count];
        for row in &table.rows {
            for (idx, cell) in row.cells.iter().enumerate() {
                let width = cell.iter().map(|span| span.width()).sum();
                widths[idx] = widths[idx].max(width);
            }
        }
        self.fit_table_widths(&mut widths);

        let table_theme = &self.theme.table_theme;
        let border_style = table_theme.border;
        let divider_style = table_theme.divider;
        let resolver = table_theme.effect_resolver();
        let effect_phase = self.table_effect_phase;
        self.lines
            .push(self.table_border_line(&widths, '┌', '┬', '┐', border_style));

        let last_header = table.rows.iter().rposition(|row| row.is_header);
        let mut body_index = 0usize;
        let mut header_index = 0usize;

        for (idx, row) in table.rows.iter().enumerate() {
            let (section, row_index) = if row.is_header {
                let row_index = header_index;
                header_index += 1;
                (TableSection::Header, row_index)
            } else {
                let row_index = body_index;
                body_index += 1;
                (TableSection::Body, row_index)
            };

            let base_style = if row.is_header {
                table_theme.header
            } else if row_index.is_multiple_of(2) {
                table_theme.row
            } else {
                table_theme.row_alt
            };

            let base_style = if let Some(phase) = effect_phase {
                let resolved =
                    resolver.resolve(base_style, TableEffectScope::section(section), phase);
                resolver.resolve(resolved, TableEffectScope::row(section, row_index), phase)
            } else {
                base_style
            };

            let context = TableRowContext {
                widths: &widths,
                alignments: &table.alignments,
                base_style,
                border_style,
                section,
                effect_phase,
                resolver: &resolver,
            };
            let line = self.table_row_line(row, &context);
            self.lines.push(line);

            if row.is_header && Some(idx) == last_header && idx + 1 < table.rows.len() {
                self.lines
                    .push(self.table_border_line(&widths, '├', '┼', '┤', divider_style));
            }
        }

        self.lines
            .push(self.table_border_line(&widths, '└', '┴', '┘', border_style));
        self.needs_blank = true;
    }

    fn fit_table_widths(&self, widths: &mut [usize]) {
        let Some(max_width) = self.table_max_width else {
            return;
        };
        if widths.is_empty() {
            return;
        }

        let max_width = usize::from(max_width);
        let border_width = widths.len().saturating_mul(3).saturating_add(1);
        if max_width <= border_width {
            widths.fill(1);
            return;
        }

        let max_content = max_width.saturating_sub(border_width);
        let total: usize = widths.iter().sum();
        if total <= max_content {
            return;
        }

        let min_width = 1usize;
        let min_total = min_width.saturating_mul(widths.len());
        if max_content <= min_total {
            widths.fill(min_width);
            return;
        }

        let total_orig = total;
        let mut new_widths = vec![min_width; widths.len()];
        let mut remaining = max_content.saturating_sub(min_total);

        let mut allocations = Vec::with_capacity(widths.len());
        for &orig in widths.iter() {
            let share = remaining.saturating_mul(orig) / total_orig;
            let cap = orig.saturating_sub(min_width);
            allocations.push(share.min(cap));
        }
        let used: usize = allocations.iter().sum();
        remaining = remaining.saturating_sub(used);

        for (slot, add) in new_widths.iter_mut().zip(allocations.iter()) {
            *slot = slot.saturating_add(*add);
        }

        if remaining > 0 {
            let mut order: Vec<usize> = (0..widths.len()).collect();
            order.sort_by_key(|&idx| {
                let cap = widths[idx].saturating_sub(new_widths[idx]);
                (std::cmp::Reverse(cap), idx)
            });
            while remaining > 0 {
                let mut progressed = false;
                for &idx in &order {
                    let cap = widths[idx].saturating_sub(new_widths[idx]);
                    if cap > 0 {
                        new_widths[idx] += 1;
                        remaining -= 1;
                        progressed = true;
                        if remaining == 0 {
                            break;
                        }
                    }
                }
                if !progressed {
                    break;
                }
            }
        }

        widths.copy_from_slice(&new_widths);
    }

    fn table_border_line(
        &self,
        widths: &[usize],
        left: char,
        mid: char,
        right: char,
        style: Style,
    ) -> Line {
        let mut line = String::new();
        line.push(left);
        for (idx, width) in widths.iter().enumerate() {
            line.push_str(&"─".repeat(width.saturating_add(2)));
            if idx + 1 < widths.len() {
                line.push(mid);
            }
        }
        line.push(right);
        Line::styled(line, style)
    }

    fn table_row_line(&self, row: &TableRow, context: &TableRowContext<'_>) -> Line {
        let mut spans = Vec::new();
        spans.push(Span::styled("│", context.border_style));

        for (idx, width) in context.widths.iter().enumerate() {
            let cell_spans = row
                .cells
                .get(idx)
                .cloned()
                .unwrap_or_else(|| vec![Span::raw("")]);
            let cell_style = if let Some(phase) = context.effect_phase {
                context.resolver.resolve(
                    context.base_style,
                    TableEffectScope::column(context.section, idx),
                    phase,
                )
            } else {
                context.base_style
            };
            let (cell_spans, cell_width) = self.table_cell_spans(&cell_spans, *width, cell_style);
            let extra = width.saturating_sub(cell_width);
            let alignment = context
                .alignments
                .get(idx)
                .copied()
                .unwrap_or(Alignment::None);

            let (left_extra, right_extra) = match alignment {
                Alignment::Right => (extra, 0),
                Alignment::Center => (extra / 2, extra - (extra / 2)),
                _ => (0, extra),
            };

            let left_pad = 1 + left_extra;
            let right_pad = 1 + right_extra;

            spans.push(Span::styled(" ".repeat(left_pad), cell_style));
            spans.extend(cell_spans);
            spans.push(Span::styled(" ".repeat(right_pad), cell_style));
            spans.push(Span::styled("│", context.border_style));
        }

        Line::from_spans(spans)
    }

    fn table_cell_spans(
        &self,
        spans: &[Span<'static>],
        max_width: usize,
        base_style: Style,
    ) -> (Vec<Span<'static>>, usize) {
        if max_width == 0 {
            return (Vec::new(), 0);
        }

        let styled = self.apply_table_cell_style(spans, base_style);
        let total_width: usize = styled.iter().map(|span| span.width()).sum();
        if total_width <= max_width {
            return (styled, total_width);
        }

        let content_limit = max_width.saturating_sub(1);
        let mut out = Vec::new();
        let mut used = 0usize;

        if content_limit > 0 {
            for span in styled {
                let span_width = span.width();
                if used + span_width <= content_limit {
                    used += span_width;
                    out.push(span);
                } else {
                    let remaining = content_limit.saturating_sub(used);
                    if remaining > 0 {
                        let (left, _right) = span.split_at_cell(remaining);
                        out.push(left);
                    }
                    break;
                }
            }
        }

        out.push(Span::styled("…", base_style));
        (out, max_width)
    }

    fn apply_table_cell_style(
        &self,
        spans: &[Span<'static>],
        base_style: Style,
    ) -> Vec<Span<'static>> {
        spans
            .iter()
            .map(|span| {
                let content = span.content.to_string();
                let style = match span.style {
                    Some(style) => style.merge(&base_style),
                    None => base_style,
                };
                let mut styled = Span::styled(content, style);
                if let Some(link) = &span.link {
                    styled = styled.link(link.to_string());
                }
                styled
            })
            .collect()
    }

    fn flush_line(&mut self) {
        if self.in_table() {
            return;
        }
        if !self.current_spans.is_empty() {
            let spans = std::mem::take(&mut self.current_spans);
            let line = Line::from_spans(spans);
            if self.in_footnote_definition() {
                // Redirect to footnote collection with indentation
                let indented = Line::styled(
                    format!("  {}", line.to_plain_text()),
                    self.theme.footnote_def,
                );
                self.current_footnote_lines.push(indented);
            } else {
                self.lines.push(line);
            }
        }
    }

    fn flush_blank(&mut self) {
        if self.in_table() {
            return;
        }
        self.flush_line();
        if self.needs_blank && !self.lines.is_empty() {
            self.lines.push(Line::new());
            self.needs_blank = false;
        }
    }

    fn flush_code_block(&mut self) {
        let mut code = std::mem::take(&mut self.code_block_lines).join("");
        let lang = self.code_block_lang.take();
        let style = self.theme.code_block;
        let lang_lower = lang.as_ref().map(|value| value.to_ascii_lowercase());

        #[cfg(feature = "diagram")]
        {
            let is_excluded = matches!(
                lang_lower.as_deref(),
                Some("mermaid" | "math" | "latex" | "tex")
            );
            if !is_excluded && diagram::is_likely_diagram(&code) {
                code = diagram::correct_diagram(&code);
            }
        }

        // Handle special languages
        if let Some(ref lang_str) = lang {
            let lang_lower = lang_lower.as_deref().unwrap_or("");

            // Mermaid diagrams - show with diagram indicator
            if lang_lower == "mermaid" {
                self.lines.push(Line::styled(
                    String::from("┌─ 📊 Mermaid Diagram ─┐"),
                    self.theme.admonition_note,
                ));
                for line_text in code.lines() {
                    self.lines.push(Line::styled(
                        format!("│ {line_text}"),
                        self.theme.code_block,
                    ));
                }
                self.lines.push(Line::styled(
                    String::from("└─────────────────────┘"),
                    self.theme.admonition_note,
                ));
                return;
            }

            // Math code blocks (alternative to $$ syntax)
            if lang_lower == "math" || lang_lower == "latex" || lang_lower == "tex" {
                let unicode = latex_to_unicode(&code);
                for line in unicode.lines() {
                    self.lines
                        .push(Line::styled(format!("  {line}"), self.theme.math_block));
                }
                if unicode.is_empty() || code.is_empty() {
                    self.lines
                        .push(Line::styled(String::from("  "), self.theme.math_block));
                }
                return;
            }

            // Show language label for syntax-highlighted languages
            // (actual highlighting would require syntect, but we can at least show the language)
            let common_langs = [
                "rust",
                "python",
                "javascript",
                "typescript",
                "go",
                "java",
                "c",
                "cpp",
                "ruby",
                "php",
                "swift",
                "kotlin",
                "scala",
                "haskell",
                "elixir",
                "clojure",
                "bash",
                "sh",
                "zsh",
                "fish",
                "powershell",
                "sql",
                "html",
                "css",
                "scss",
                "json",
                "yaml",
                "toml",
                "xml",
                "markdown",
                "md",
            ];
            if common_langs.contains(&lang_lower) {
                self.lines.push(Line::styled(
                    format!("─── {lang_str} ───"),
                    self.theme.code_inline.dim(),
                ));
            }

            #[cfg(feature = "syntax")]
            if let Some(highlighter) = self.syntax_highlighter
                && !matches!(lang_lower, "mermaid" | "math" | "latex" | "tex")
            {
                let code_for_highlight = code.strip_suffix('\n').unwrap_or(&code);
                let highlighted = highlighter.highlight(code_for_highlight, lang_str);
                for line in highlighted.lines() {
                    let mut spans = Vec::with_capacity(line.len().saturating_add(1));
                    spans.push(Span::styled("  ", style));
                    for span in line.spans() {
                        let merged = span.style.map(|s| s.merge(&style)).unwrap_or(style);
                        let mut out_span = span.clone();
                        out_span.style = Some(merged);
                        spans.push(out_span);
                    }
                    self.lines.push(Line::from_spans(spans));
                }
                return;
            }
        }

        // Regular code block
        for line_text in code.lines() {
            self.lines
                .push(Line::styled(format!("  {line_text}"), style));
        }
        // If the code block was empty or ended with newline, still show at least nothing
        if code.is_empty() {
            self.lines.push(Line::styled(String::from("  "), style));
        }
    }

    fn in_footnote_definition(&self) -> bool {
        self.current_footnote.is_some()
    }

    fn flush_footnote_line(&mut self) {
        if !self.current_spans.is_empty() {
            let spans = std::mem::take(&mut self.current_spans);
            let line = Line::from_spans(spans);
            let indented_line = Line::styled(
                format!("  {}", line.to_plain_text()),
                self.theme.footnote_def,
            );
            self.current_footnote_lines.push(indented_line);
        }
    }

    fn append_footnotes(&mut self) {
        if self.footnotes.is_empty() {
            return;
        }

        // Add separator before footnotes
        self.flush_line();
        self.lines.push(Line::new());
        let separator = "─".repeat(20);
        self.lines
            .push(Line::styled(separator, self.theme.horizontal_rule));

        for (label, content_lines) in std::mem::take(&mut self.footnotes) {
            // Footnote header
            let header = format!("[^{label}]:");
            self.lines
                .push(Line::styled(header, self.theme.footnote_def));

            // Footnote content (indented)
            for line in content_lines {
                self.lines.push(line);
            }
        }
    }

    fn finish(mut self) -> Text {
        self.flush_line();
        if self.lines.is_empty() {
            return Text::new();
        }
        Text::from_lines(self.lines)
    }
}

// ---------------------------------------------------------------------------
// Convenience function
// ---------------------------------------------------------------------------

/// Render Markdown to styled [`Text`] using the default theme.
///
/// This is a convenience function for quick rendering without customization.
/// For custom themes or settings, use [`MarkdownRenderer`] directly.
#[must_use]
pub fn render_markdown(markdown: &str) -> Text {
    MarkdownRenderer::default().render(markdown)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &Text) -> String {
        text.lines()
            .iter()
            .map(|l| l.to_plain_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // =========================================================================
    // Basic Markdown tests (existing)
    // =========================================================================

    #[test]
    fn render_empty_string() {
        let text = render_markdown("");
        assert!(text.is_empty());
    }

    #[test]
    fn render_plain_paragraph() {
        let text = render_markdown("Hello, world!");
        let content = plain(&text);
        assert!(content.contains("Hello, world!"));
    }

    #[test]
    fn render_heading_h1() {
        let text = render_markdown("# Title");
        let content = plain(&text);
        assert!(content.contains("Title"));
        // H1 should be on its own line
        assert!(text.height() >= 1);
    }

    #[test]
    fn render_heading_levels() {
        let md = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("H1"));
        assert!(content.contains("H6"));
    }

    #[test]
    fn render_bold_text() {
        let text = render_markdown("Some **bold** text.");
        let content = plain(&text);
        assert!(content.contains("bold"));
    }

    #[test]
    fn render_italic_text() {
        let text = render_markdown("Some *italic* text.");
        let content = plain(&text);
        assert!(content.contains("italic"));
    }

    #[test]
    fn render_strikethrough() {
        let text = render_markdown("Some ~~struck~~ text.");
        let content = plain(&text);
        assert!(content.contains("struck"));
    }

    #[test]
    fn render_inline_code() {
        let text = render_markdown("Use `code` here.");
        let content = plain(&text);
        assert!(content.contains("`code`"));
    }

    #[test]
    fn render_code_block() {
        let md = "```rust\nfn main() {}\n```";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("fn main()"));
    }

    #[cfg(feature = "diagram")]
    #[test]
    fn render_code_block_diagram_correction_applied() {
        let md = "```\n+----+\n|Hi|\n+----+\n```";
        let text = render_markdown(md);
        let content = plain(&text);
        let corrected = crate::diagram::correct_diagram("+----+\n|Hi|\n+----+\n");
        let expected = corrected
            .lines()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(content.contains(&expected));
    }

    #[cfg(feature = "diagram")]
    #[test]
    fn render_code_block_non_diagram_unchanged() {
        let md = "```\nfn main() {}\n```";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("  fn main() {}"));
    }

    #[test]
    fn render_blockquote() {
        let text = render_markdown("> Quoted text");
        let content = plain(&text);
        assert!(content.contains("Quoted text"));
    }

    #[test]
    fn render_unordered_list() {
        let md = "- Item 1\n- Item 2\n- Item 3";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("• Item 1"));
        assert!(content.contains("• Item 2"));
        assert!(content.contains("• Item 3"));
    }

    #[test]
    fn render_ordered_list() {
        let md = "1. First\n2. Second\n3. Third";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("1. First"));
        assert!(content.contains("2. Second"));
        assert!(content.contains("3. Third"));
    }

    #[test]
    fn render_horizontal_rule() {
        let md = "Above\n\n---\n\nBelow";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Above"));
        assert!(content.contains("Below"));
        assert!(content.contains("─"));
    }

    #[test]
    fn render_link() {
        let text = render_markdown("[click here](https://example.com)");
        let content = plain(&text);
        assert!(content.contains("click here"));
    }

    #[test]
    fn render_nested_emphasis() {
        let text = render_markdown("***bold and italic***");
        let content = plain(&text);
        assert!(content.contains("bold and italic"));
    }

    #[test]
    fn render_nested_list() {
        let md = "- Outer\n  - Inner\n- Back";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Outer"));
        assert!(content.contains("Inner"));
        assert!(content.contains("Back"));
    }

    #[test]
    fn render_multiple_paragraphs() {
        let md = "First paragraph.\n\nSecond paragraph.";
        let text = render_markdown(md);
        // Should have a blank line between paragraphs
        assert!(text.height() >= 3);
    }

    #[test]
    fn custom_theme() {
        let theme = MarkdownTheme {
            h1: Style::new().fg(PackedRgba::rgb(255, 0, 0)),
            ..Default::default()
        };
        let renderer = MarkdownRenderer::new(theme);
        let text = renderer.render("# Red Title");
        assert!(!text.is_empty());
    }

    #[test]
    fn custom_rule_width() {
        let renderer = MarkdownRenderer::default().rule_width(20);
        let text = renderer.render("---");
        let content = plain(&text);
        // Rule should be 20 chars wide
        let rule_line = content.lines().find(|l| l.contains('─')).unwrap();
        assert_eq!(rule_line.chars().filter(|&c| c == '─').count(), 20);
    }

    #[test]
    fn render_code_block_preserves_whitespace() {
        let md = "```\n  indented\n    more\n```";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("  indented"));
        assert!(content.contains("    more"));
    }

    #[test]
    fn render_empty_code_block() {
        let md = "```\n```";
        let text = render_markdown(md);
        // Should still produce at least one line
        assert!(text.height() >= 1);
    }

    #[test]
    fn blockquote_has_bar_prefix() {
        let text = render_markdown("> quoted");
        let content = plain(&text);
        assert!(content.contains("│"));
    }

    #[test]
    fn blockquote_prefix_not_repeated_with_inline_formatting() {
        let text = render_markdown("> **Bold** text");
        let line = text.lines().first().expect("blockquote line");
        let plain = line.to_plain_text();
        assert_eq!(plain.matches('│').count(), 1);
    }

    // =========================================================================
    // GFM extension tests
    // =========================================================================

    #[test]
    fn render_table() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("A"));
        assert!(content.contains("B"));
        assert!(content.contains("1"));
        assert!(content.contains("2"));
    }

    #[test]
    fn markdown_table_alignment_variants() {
        let md = "| Left | Center | Right |\n| :--- | :----: | ----: |\n| a | b | c |";
        let text = render_markdown(md);
        let content = plain(&text);
        let row_line = content
            .lines()
            .find(|line| line.starts_with('│') && line.contains('a') && line.contains('b'))
            .expect("table row line");
        let segments: Vec<&str> = row_line.split('│').collect();
        assert_eq!(segments.len(), 5);
        assert_eq!(segments[1], " a    ");
        assert_eq!(segments[2], "   b    ");
        assert_eq!(segments[3], "     c ");
    }

    #[test]
    fn markdown_table_escaped_pipe() {
        let md = r"| Col |\n| --- |\n| A \| B |";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("A | B"));
    }

    #[test]
    fn markdown_table_br_cell_keeps_single_row() {
        let md = "| Col |\n| --- |\n| line1<br>line2 |";
        let text = render_markdown(md);
        let content = plain(&text);
        let row_line = content
            .lines()
            .find(|line| line.starts_with('│') && line.contains("line1"))
            .expect("table row line");
        assert!(row_line.contains("line1 line2"));
    }

    #[test]
    fn markdown_table_border_alignment_for_varied_widths() {
        use ftui_text::wrap::display_width;

        let md = "| H1 | H2 |\n| --- | --- |\n| short | loooooong |";
        let text = render_markdown(md);
        let content = plain(&text);
        let lines: Vec<&str> = content
            .lines()
            .filter(|line| {
                line.starts_with('┌')
                    || line.starts_with('├')
                    || line.starts_with('│')
                    || line.starts_with('└')
            })
            .collect();
        // Use display width, not byte length - box-drawing chars are 3 bytes each
        let expected = display_width(lines.first().expect("table lines"));
        for line in lines {
            assert_eq!(display_width(line), expected);
        }
    }

    #[test]
    fn render_nested_blockquotes() {
        let md = "> Level 1\n> > Level 2\n> > > Level 3";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Level 1"));
        assert!(content.contains("Level 2"));
        assert!(content.contains("Level 3"));
    }

    #[test]
    fn render_link_with_inline_code() {
        let md = "[`code link`](https://example.com)";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("`code link`"));
    }

    #[test]
    fn render_ordered_list_custom_start() {
        let md = "5. Fifth\n6. Sixth\n7. Seventh";
        let text = render_markdown(md);
        let content = plain(&text);
        // Should start at 5
        assert!(content.contains("5. Fifth"));
        assert!(content.contains("6. Sixth"));
        assert!(content.contains("7. Seventh"));
    }

    #[test]
    fn render_mixed_list_types() {
        let md = "1. Ordered\n- Unordered\n2. Ordered again";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("1. Ordered"));
        assert!(content.contains("• Unordered"));
    }

    #[test]
    fn render_code_in_heading() {
        let md = "# Heading with `code`";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Heading with"));
        assert!(content.contains("`code`"));
    }

    #[test]
    fn render_emphasis_in_list() {
        let md = "- Item with **bold** text";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("bold"));
    }

    #[test]
    fn render_soft_break() {
        let md = "Line one\nLine two";
        let text = render_markdown(md);
        let content = plain(&text);
        // Soft break becomes space
        assert!(content.contains("Line one"));
        assert!(content.contains("Line two"));
    }

    #[test]
    fn render_hard_break() {
        let md = "Line one  \nLine two"; // Two spaces before newline
        let text = render_markdown(md);
        // Hard break creates new line
        assert!(text.height() >= 2);
    }

    #[test]
    fn theme_default_creates_valid_styles() {
        use ftui_style::StyleFlags;
        let theme = MarkdownTheme::default();
        // All styles should be valid
        assert!(theme.h1.has_attr(StyleFlags::BOLD));
        assert!(theme.h2.has_attr(StyleFlags::BOLD));
        assert!(theme.emphasis.has_attr(StyleFlags::ITALIC));
        assert!(theme.strong.has_attr(StyleFlags::BOLD));
        assert!(theme.strikethrough.has_attr(StyleFlags::STRIKETHROUGH));
        assert!(theme.link.has_attr(StyleFlags::UNDERLINE));
        assert!(theme.blockquote.has_attr(StyleFlags::ITALIC));
    }

    #[test]
    fn theme_clone() {
        use ftui_style::StyleFlags;
        let theme1 = MarkdownTheme::default();
        let theme2 = theme1.clone();
        // Both should have same styles
        assert_eq!(
            theme1.h1.has_attr(StyleFlags::BOLD),
            theme2.h1.has_attr(StyleFlags::BOLD)
        );
    }

    #[test]
    fn renderer_clone() {
        let renderer1 = MarkdownRenderer::default();
        let renderer2 = renderer1.clone();
        // Both should render the same
        let text1 = renderer1.render("# Test");
        let text2 = renderer2.render("# Test");
        assert_eq!(plain(&text1), plain(&text2));
    }

    #[test]
    fn render_whitespace_only() {
        let text = render_markdown("   \n   \n   ");
        // Should handle gracefully
        let content = plain(&text);
        assert!(content.trim().is_empty() || content.contains(" "));
    }

    #[test]
    fn render_complex_nested_structure() {
        let md = r#"# Main Title

Some intro text with **bold** and *italic*.

## Section 1

> A blockquote with:
> - A list item
> - Another item

```rust
fn example() {
    println!("code");
}
```

## Section 2

1. First
2. Second
   - Nested bullet

---

The end.
"#;
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Main Title"));
        assert!(content.contains("Section 1"));
        assert!(content.contains("Section 2"));
        assert!(content.contains("blockquote"));
        assert!(content.contains("fn example"));
        assert!(content.contains("─"));
        assert!(content.contains("The end"));
    }

    #[test]
    fn render_unicode_in_markdown() {
        let md = "# 日本語タイトル\n\n**太字** and *斜体*";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("日本語タイトル"));
        assert!(content.contains("太字"));
        assert!(content.contains("斜体"));
    }

    #[test]
    fn render_emoji_in_markdown() {
        let md = "# Celebration\n\n**Launch** today!";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Celebration"));
        assert!(content.contains("Launch"));
    }

    #[test]
    fn render_consecutive_headings() {
        let md = "# H1\n## H2\n### H3";
        let text = render_markdown(md);
        // Should have blank lines between headings
        assert!(text.height() >= 5);
    }

    #[test]
    fn render_link_in_blockquote() {
        let md = "> Check [this link](https://example.com)";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("│"));
        assert!(content.contains("this link"));
    }

    #[test]
    fn render_code_block_with_language() {
        let md = "```python\nprint('hello')\n```";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("print"));
    }

    #[test]
    fn render_deeply_nested_list() {
        let md = "- Level 1\n  - Level 2\n    - Level 3\n      - Level 4";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Level 1"));
        assert!(content.contains("Level 4"));
    }

    #[test]
    fn render_multiple_code_blocks() {
        let md = "```\nblock1\n```\n\n```\nblock2\n```";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("block1"));
        assert!(content.contains("block2"));
    }

    #[test]
    fn render_emphasis_across_words() {
        let md = "*multiple words in italic*";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("multiple words in italic"));
    }

    #[test]
    fn render_bold_and_italic_together() {
        let md = "***bold and italic*** and **just bold** and *just italic*";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("bold and italic"));
        assert!(content.contains("just bold"));
        assert!(content.contains("just italic"));
    }

    #[test]
    fn render_escaped_characters() {
        let md = r#"\*not italic\* and \`not code\`"#;
        let text = render_markdown(md);
        let content = plain(&text);
        // Escaped characters should appear as-is
        assert!(content.contains("*not italic*"));
    }

    #[test]
    fn markdown_renderer_default() {
        let renderer = MarkdownRenderer::default();
        let text = renderer.render("test");
        assert!(!text.is_empty());
    }

    #[test]
    fn render_markdown_function() {
        let text = render_markdown("# Heading\nParagraph");
        assert!(!text.is_empty());
        let content = plain(&text);
        assert!(content.contains("Heading"));
        assert!(content.contains("Paragraph"));
    }

    #[test]
    fn render_table_multicolumn() {
        let md = "| Col1 | Col2 | Col3 |\n|------|------|------|\n| A | B | C |\n| D | E | F |";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Col1"));
        assert!(content.contains("Col2"));
        assert!(content.contains("Col3"));
        assert!(content.contains("A"));
        assert!(content.contains("F"));
    }

    #[test]
    fn markdown_table_respects_max_width() {
        let md = "| Col A | Column B |\n| --- | --- |\n| superlongvalue | evenlongervalue |\n";
        let text = MarkdownRenderer::new(MarkdownTheme::default())
            .table_max_width(20)
            .render(md);
        assert!(text.width() <= 20);
        let content = plain(&text);
        assert!(content.contains('…'));
    }

    #[test]
    fn markdown_table_truncation_keeps_borders_aligned() {
        use ftui_text::wrap::display_width;

        let md = "| Col A | Column B |\n| --- | --- |\n| superlongvalue | evenlongervalue |\n";
        let text = MarkdownRenderer::new(MarkdownTheme::default())
            .table_max_width(20)
            .render(md);
        let content = plain(&text);
        // Use display width, not byte length - box-drawing chars are 3 bytes each
        let top_width = display_width(
            content
                .lines()
                .find(|line| line.starts_with('┌'))
                .expect("top border"),
        );
        let row_width = display_width(
            content
                .lines()
                .find(|line| line.starts_with('│') && line.contains('…'))
                .expect("row line with ellipsis"),
        );
        assert_eq!(row_width, top_width);
    }

    #[test]
    fn render_very_long_line() {
        let long_text = "word ".repeat(100);
        let md = format!("# {}", long_text);
        let text = render_markdown(&md);
        assert!(!text.is_empty());
    }

    #[test]
    fn render_only_whitespace_in_code_block() {
        let md = "```\n   \n```";
        let text = render_markdown(md);
        // Should handle gracefully
        assert!(text.height() >= 1);
    }

    #[test]
    fn style_context_heading_levels() {
        // Each heading level should have different styling
        for level in 1..=6 {
            let md = format!("{} Heading Level {}", "#".repeat(level), level);
            let text = render_markdown(&md);
            let content = plain(&text);
            assert!(content.contains(&format!("Heading Level {}", level)));
        }
    }

    // =========================================================================
    // Task list tests
    // =========================================================================

    #[test]
    fn render_task_list_unchecked() {
        let md = "- [ ] Todo item";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("☐") || content.contains("Todo item"));
    }

    #[test]
    fn render_task_list_checked() {
        let md = "- [x] Done item";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("✓") || content.contains("Done item"));
    }

    #[test]
    fn render_task_list_mixed() {
        let md = "- [ ] Not done\n- [x] Done\n- [ ] Also not done";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Not done"));
        assert!(content.contains("Done"));
        assert!(content.contains("Also not done"));
    }

    // =========================================================================
    // Math tests
    // =========================================================================

    #[test]
    fn render_inline_math() {
        let md = "The equation $E=mc^2$ is famous.";
        let text = render_markdown(md);
        let content = plain(&text);
        // Should contain the converted math (E=mc² or similar)
        assert!(content.contains("E") && content.contains("mc"));
    }

    #[test]
    fn render_display_math() {
        let md = "$$\n\\sum_{i=1}^n i = \\frac{n(n+1)}{2}\n$$";
        let text = render_markdown(md);
        let content = plain(&text);
        // Should render something (even if not perfectly formatted)
        assert!(!content.is_empty());
    }

    #[test]
    fn render_math_with_greek() {
        let md = "The angle $\\theta$ and $\\alpha + \\beta = \\gamma$.";
        let text = render_markdown(md);
        let content = plain(&text);
        // Greek letters should be converted to Unicode
        assert!(content.contains("θ") || content.contains("alpha"));
    }

    #[test]
    fn render_math_with_fractions() {
        let md = "Half is $\\frac{1}{2}$.";
        let text = render_markdown(md);
        let content = plain(&text);
        // Should convert to ½ or 1/2
        assert!(content.contains("½") || content.contains("1/2"));
    }

    #[test]
    fn render_math_with_sqrt() {
        let md = "The square root $\\sqrt{x}$ is useful.";
        let text = render_markdown(md);
        let content = plain(&text);
        // Should contain √
        assert!(content.contains("√") || content.contains("sqrt"));
    }

    // =========================================================================
    // Footnote tests
    // =========================================================================

    #[test]
    fn render_footnote_reference() {
        let md = "This has a footnote[^1].";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("[^1]") || content.contains("footnote"));
    }

    // =========================================================================
    // LaTeX conversion tests
    // =========================================================================

    #[test]
    fn latex_greek_letters() {
        assert!(latex_to_unicode(r"\alpha").contains('α'));
        assert!(latex_to_unicode(r"\beta").contains('β'));
        assert!(latex_to_unicode(r"\gamma").contains('γ'));
        assert!(latex_to_unicode(r"\pi").contains('π'));
    }

    #[test]
    fn latex_operators() {
        assert!(latex_to_unicode(r"\times").contains('×'));
        assert!(latex_to_unicode(r"\div").contains('÷'));
        assert!(latex_to_unicode(r"\pm").contains('±'));
    }

    #[test]
    fn latex_comparison() {
        assert!(latex_to_unicode(r"\leq").contains('≤'));
        assert!(latex_to_unicode(r"\geq").contains('≥'));
        assert!(latex_to_unicode(r"\neq").contains('≠'));
    }

    #[test]
    fn latex_set_theory() {
        assert!(latex_to_unicode(r"\subset").contains('⊂'));
        assert!(latex_to_unicode(r"\cup").contains('∪'));
        assert!(latex_to_unicode(r"\cap").contains('∩'));
        assert!(latex_to_unicode(r"\emptyset").contains('∅'));
    }

    #[test]
    fn latex_logic() {
        assert!(latex_to_unicode(r"\forall").contains('∀'));
        assert!(latex_to_unicode(r"\exists").contains('∃'));
        assert!(latex_to_unicode(r"\land").contains('∧'));
        assert!(latex_to_unicode(r"\lor").contains('∨'));
    }

    #[test]
    fn latex_fractions() {
        assert!(latex_to_unicode(r"\frac{1}{2}").contains('½'));
        assert!(latex_to_unicode(r"\frac{1}{4}").contains('¼'));
        assert!(latex_to_unicode(r"\frac{3}{4}").contains('¾'));
    }

    #[test]
    fn latex_generic_fraction() {
        let result = latex_to_unicode(r"\frac{a}{b}");
        assert!(result.contains("a/b") || result.contains("a") && result.contains("b"));
    }

    #[test]
    fn latex_sqrt() {
        let result = latex_to_unicode(r"\sqrt{x}");
        assert!(result.contains("√x") || result.contains("√"));
    }

    #[test]
    fn find_matching_brace_works() {
        assert_eq!(find_matching_brace("abc}"), Some(3));
        assert_eq!(find_matching_brace("a{b}c}"), Some(5));
        assert_eq!(find_matching_brace("abc"), None);
    }

    // =========================================================================
    // Theme tests for new fields
    // =========================================================================

    #[test]
    fn theme_has_task_styles() {
        let theme = MarkdownTheme::default();
        // Task styles should exist and be different
        assert!(theme.task_done.fg.is_some());
        assert!(theme.task_todo.fg.is_some());
    }

    #[test]
    fn theme_has_math_styles() {
        use ftui_style::StyleFlags;
        let theme = MarkdownTheme::default();
        // Math styles should be styled
        assert!(theme.math_inline.fg.is_some());
        assert!(theme.math_inline.has_attr(StyleFlags::ITALIC));
        assert!(theme.math_block.fg.is_some());
        assert!(theme.math_block.has_attr(StyleFlags::BOLD));
    }

    #[test]
    fn theme_has_admonition_styles() {
        let theme = MarkdownTheme::default();
        // All admonition styles should have colors
        assert!(theme.admonition_note.fg.is_some());
        assert!(theme.admonition_tip.fg.is_some());
        assert!(theme.admonition_important.fg.is_some());
        assert!(theme.admonition_warning.fg.is_some());
        assert!(theme.admonition_caution.fg.is_some());
    }

    #[test]
    fn admonition_kind_icons_and_labels() {
        assert!(!AdmonitionKind::Note.icon().is_empty());
        assert!(!AdmonitionKind::Note.label().is_empty());
        assert!(!AdmonitionKind::Warning.icon().is_empty());
        assert!(!AdmonitionKind::Warning.label().is_empty());
    }

    // =========================================================================
    // GFM Auto-Detection tests
    // =========================================================================

    #[test]
    fn detection_plain_text_not_markdown() {
        let result = is_likely_markdown("just some plain text");
        assert!(!result.is_likely());
        assert_eq!(result.indicators, 0);
    }

    #[test]
    fn detection_heading_is_markdown() {
        // Heading with another indicator (bold)
        let result = is_likely_markdown("# Hello **World**");
        assert!(result.is_likely());
        assert!(result.indicators >= 2);
    }

    #[test]
    fn detection_heading_alone_has_indicator() {
        // Single heading alone has 1 indicator (below threshold)
        let result = is_likely_markdown("# Title");
        assert_eq!(result.indicators, 1);
        assert!(!result.is_likely()); // Needs 2+ to be "likely"
    }

    #[test]
    fn detection_bold_is_markdown() {
        // Opening and closing ** = 2 indicators
        let result = is_likely_markdown("some **bold** text");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_code_fence_is_confident() {
        let result = is_likely_markdown("```rust\ncode\n```");
        assert!(result.is_confident());
        assert!(result.indicators >= 4);
    }

    #[test]
    fn detection_inline_code() {
        // Two backticks = 2 indicators
        let result = is_likely_markdown("use `code` here");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_link() {
        // Link with another element
        let result = is_likely_markdown("click [**here**](https://example.com)");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_list_items() {
        let result = is_likely_markdown("- item 1\n- item 2");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_math() {
        // Two $ signs = 2 indicators
        let result = is_likely_markdown("equation $E = mc^2$");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_display_math() {
        // $$ = 2 indicators each, plus two pairs = 4 indicators
        let result = is_likely_markdown("$$\\sum_{i=1}^n x_i$$");
        assert!(result.is_confident());
    }

    #[test]
    fn detection_table() {
        // Multiple | = multiple indicators
        let result = is_likely_markdown("| col1 | col2 |\n|------|------|");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_task_list() {
        // Task checkboxes + list markers = multiple indicators
        let result = is_likely_markdown("- [ ] todo\n- [x] done");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_blockquote() {
        // Blockquote with another element
        let result = is_likely_markdown("> **quoted** text");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_strikethrough() {
        // Two ~~ = 2 indicators
        let result = is_likely_markdown("~~deleted~~");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_footnote() {
        // Footnote with another element
        let result = is_likely_markdown("See **note**[^1]");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_html_tags() {
        // Two kbd tags = 2 indicators
        let result = is_likely_markdown("press <kbd>Ctrl</kbd>+<kbd>C</kbd>");
        assert!(result.is_likely());
    }

    #[test]
    fn detection_confidence_score() {
        let plain = is_likely_markdown("hello");
        let rich = is_likely_markdown("# Title\n\n**bold** and *italic*\n\n```code```");
        assert!(rich.confidence() > plain.confidence());
        assert!(rich.confidence() > 0.5);
    }

    #[test]
    fn detection_empty_string() {
        let result = is_likely_markdown("");
        assert!(!result.is_likely());
        assert_eq!(result.indicators, 0);
    }

    // =========================================================================
    // Streaming / Fragment tests
    // =========================================================================

    #[test]
    fn streaming_unclosed_code_fence() {
        let text = render_streaming("```rust\nfn main()", &MarkdownTheme::default());
        let content = plain(&text);
        assert!(content.contains("fn main()"));
    }

    #[test]
    fn streaming_unclosed_bold() {
        let text = render_streaming("some **bold", &MarkdownTheme::default());
        let content = plain(&text);
        assert!(content.contains("bold"));
    }

    #[test]
    fn streaming_unclosed_inline_code() {
        let text = render_streaming("use `code", &MarkdownTheme::default());
        let content = plain(&text);
        assert!(content.contains("code"));
    }

    #[test]
    fn streaming_unclosed_math() {
        let text = render_streaming("equation $E = mc^2", &MarkdownTheme::default());
        let content = plain(&text);
        assert!(content.contains("E"));
    }

    #[test]
    fn streaming_unclosed_display_math() {
        let text = render_streaming("$$\\sum_i", &MarkdownTheme::default());
        let _content = plain(&text);
        // Should contain something - the exact rendering depends on unicodeit
        assert!(text.height() > 0);
    }

    #[test]
    fn streaming_complete_text_unchanged() {
        // Complete markdown should render the same way
        let complete = "# Hello\n\n**bold**";
        let regular = render_markdown(complete);
        let streaming = render_streaming(complete, &MarkdownTheme::default());
        assert_eq!(plain(&regular), plain(&streaming));
    }

    #[test]
    fn auto_render_detects_markdown() {
        let theme = MarkdownTheme::default();
        let md_text = auto_render("# Hello\n\n**bold**", &theme);
        let plain_text = auto_render("just plain text", &theme);

        // Markdown should be styled (more lines due to paragraph handling)
        assert!(md_text.height() > 0);
        assert_eq!(plain_text.height(), 1);
    }

    #[test]
    fn auto_render_streaming_handles_fragments() {
        let theme = MarkdownTheme::default();
        let text = auto_render_streaming("# Hello\n**bold", &theme);
        let content = plain(&text);
        assert!(content.contains("Hello"));
        assert!(content.contains("bold"));
    }

    #[test]
    fn renderer_method_streaming() {
        let renderer = MarkdownRenderer::default();
        let text = renderer.render_streaming("```\ncode");
        assert!(text.height() > 0);
    }

    #[test]
    fn renderer_method_auto_render() {
        let renderer = MarkdownRenderer::default();
        let md = renderer.auto_render("# Heading\n**bold**");
        let plain_result = renderer.auto_render("just text");

        assert!(md.height() > 1);
        assert_eq!(plain_result.height(), 1);
    }

    #[test]
    fn streaming_unclosed_link_bracket() {
        // Unclosed [text should close with ](...)
        let text = render_streaming("See [the docs", &MarkdownTheme::default());
        assert!(text.height() > 0);
    }

    #[test]
    fn streaming_unclosed_link_after_bracket() {
        // [text] without ( should close with (...)
        let text = render_streaming("See [docs]", &MarkdownTheme::default());
        assert!(text.height() > 0);
    }

    #[test]
    fn streaming_unclosed_link_paren() {
        // [text](url without closing ) should close with )
        let text = render_streaming("See [docs](https://example.com", &MarkdownTheme::default());
        assert!(text.height() > 0);
    }

    #[test]
    fn streaming_unclosed_link_nested() {
        // [text1](url1)[text2](url2 without closing ) should close with )
        let text = render_streaming(
            "See [docs1](https://example.com)[docs2]",
            &MarkdownTheme::default(),
        );
        assert!(text.height() > 0);
    }

    #[test]
    fn complete_fragment_handles_multiple_unclosed() {
        // Test the internal complete_fragment function indirectly
        let text = render_streaming(
            "```rust\nfn main() { **bold $math",
            &MarkdownTheme::default(),
        );
        // Should not panic and should produce output
        assert!(text.height() > 0);
    }

    // =========================================================================
    // Complex realistic GFM tests
    // =========================================================================

    /// Realistic LLM response with multiple GFM features.
    const REALISTIC_LLM_RESPONSE: &str = r#"# Implementing a REST API in Rust

## Overview

This guide covers building a **production-ready** REST API using [Actix Web](https://actix.rs).

### Prerequisites

- [x] Rust 1.70+ installed
- [x] Basic understanding of async/await
- [ ] PostgreSQL database (optional)

## Quick Start

```rust
use actix_web::{get, App, HttpServer, Responder};

#[get("/health")]
async fn health() -> impl Responder {
    "OK"
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| App::new().service(health))
        .bind("127.0.0.1:8080")?
        .run()
        .await
}
```

> [!NOTE]
> This requires the `actix-web` crate in your `Cargo.toml`.

## Performance Considerations

The time complexity is $O(n \log n)$ for most operations, where $n$ is the request count.

For batch processing:

$$\text{throughput} = \frac{\text{requests}}{\text{time}} \approx 10^5 \text{ req/s}$$

## Error Handling

Use the `?` operator with custom error types[^1]:

```rust
#[derive(Debug)]
struct ApiError(String);

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}
```

[^1]: See the [error handling docs](https://docs.rs/actix-web) for details.

---

*Happy coding!* 🦀
"#;

    #[test]
    fn realistic_llm_response_renders() {
        let text = render_markdown(REALISTIC_LLM_RESPONSE);
        let content = plain(&text);

        // Check all major GFM features rendered
        assert!(content.contains("Implementing a REST API"));
        assert!(content.contains("async fn health"));
        assert!(content.contains("NOTE"));
        assert!(content.contains("throughput"));
        assert!(content.contains("/health"));
    }

    #[test]
    fn realistic_llm_response_detection() {
        let detection = is_likely_markdown(REALISTIC_LLM_RESPONSE);
        assert!(detection.is_confident());
        assert!(detection.confidence() > 0.8);
    }

    #[test]
    fn realistic_streaming_fragments() {
        // Test partial fragments that might occur during LLM streaming
        let fragments = [
            "# Building a",                             // Partial heading
            "# Building a CLI\n\n```rust",              // Partial code block
            "# Building\n\n- [x] Done\n- [ ",           // Partial task list
            "The formula $E = mc",                      // Partial math
            "| Col1 | Col2 |\n|---",                    // Partial table
            "> [!WARNING]\n> This is",                  // Partial admonition
            "See the [docs](https://exam",              // Partial link
            "Use **bold** and ~~strike",                // Partial strikethrough
            "Footnote[^1]\n\n[^1]: The actual content", // Partial footnote
        ];

        let theme = MarkdownTheme::default();
        for fragment in fragments {
            let text = render_streaming(fragment, &theme);
            // All fragments should render without panic
            assert!(text.height() > 0, "Failed on fragment: {fragment}");
        }
    }

    #[test]
    fn complex_nested_structure() {
        let complex = r#"
# Main Title

## Section 1

> **Important:** This blockquote contains *nested* formatting.
>
> - List inside blockquote
> - With **bold** items
>
> And a code block:
> ```python
> def nested():
>     pass
> ```
"#;
        let text = render_markdown(complex);
        let content = plain(&text);

        assert!(content.contains("Main Title"));
        assert!(content.contains("Important:"));
        assert!(content.contains("def nested"));
    }

    #[test]
    fn detection_realistic_code_response() {
        // Typical LLM code response
        let response = r#"Here's how to implement it:

```python
def fibonacci(n: int) -> int:
    """Calculate nth Fibonacci number."""
    if n <= 1:
        return n
    return fibonacci(n - 1) + fibonacci(n - 2)
```

The time complexity is **O(2^n)** which can be improved with memoization."#;

        let detection = is_likely_markdown(response);
        assert!(detection.is_confident());
    }

    #[test]
    fn streaming_preserves_partial_code_block_content() {
        let fragment = "```rust\nfn main() {\n    println!(\"Hello";
        let text = render_streaming(fragment, &MarkdownTheme::default());
        let content = plain(&text);

        // The code content should be preserved even though block is unclosed
        assert!(content.contains("fn main"));
        assert!(content.contains("println"));
    }

    #[test]
    fn streaming_partial_table_renders() {
        let fragment = "| Name | Age |\n|------|-----|\n| Alice | 30 |\n| Bob |";
        let text = render_streaming(fragment, &MarkdownTheme::default());
        let content = plain(&text);

        assert!(content.contains("Name"));
        assert!(content.contains("Alice"));
        assert!(content.contains("Bob"));
    }

    #[test]
    fn auto_detect_and_render_realistic() {
        let theme = MarkdownTheme::default();

        // Should detect and render as markdown
        let md_response =
            "## Summary\n\nHere are the key points:\n\n- Point **one**\n- Point *two*";
        let text = auto_render(md_response, &theme);
        assert!(text.height() > 3); // Multiple lines from parsing

        // Should NOT detect as markdown (just plain text)
        let plain_response = "The API returned status 200 and the data looks correct.";
        let text = auto_render(plain_response, &theme);
        assert_eq!(text.height(), 1); // Single line, rendered as plain text
    }
}

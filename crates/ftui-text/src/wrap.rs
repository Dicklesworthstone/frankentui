#![forbid(unsafe_code)]

//! Text wrapping with Unicode correctness.
//!
//! This module provides width-correct text wrapping that respects:
//! - Grapheme cluster boundaries (never break emoji, ZWJ sequences, etc.)
//! - Cell widths (CJK characters are 2 cells wide)
//! - Word boundaries when possible
//!
//! # Example
//! ```
//! use ftui_text::wrap::{wrap_text, WrapMode};
//!
//! // Word wrap
//! let lines = wrap_text("Hello world foo bar", 10, WrapMode::Word);
//! assert_eq!(lines, vec!["Hello", "world foo", "bar"]);
//!
//! // Character wrap (for long words)
//! let lines = wrap_text("Supercalifragilistic", 10, WrapMode::Char);
//! assert_eq!(lines.len(), 2);
//! ```

use unicode_segmentation::UnicodeSegmentation;

/// Text wrapping mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    /// No wrapping - lines may exceed width.
    None,
    /// Wrap at word boundaries when possible.
    #[default]
    Word,
    /// Wrap at character (grapheme) boundaries.
    Char,
    /// Word wrap with character fallback for long words.
    WordChar,
}

/// Options for text wrapping.
#[derive(Debug, Clone)]
pub struct WrapOptions {
    /// Maximum width in cells.
    pub width: usize,
    /// Wrapping mode.
    pub mode: WrapMode,
    /// Preserve leading whitespace on continued lines.
    pub preserve_indent: bool,
    /// Trim trailing whitespace from wrapped lines.
    pub trim_trailing: bool,
}

impl WrapOptions {
    /// Create new wrap options with the given width.
    #[must_use]
    pub fn new(width: usize) -> Self {
        Self {
            width,
            mode: WrapMode::Word,
            preserve_indent: false,
            trim_trailing: true,
        }
    }

    /// Set the wrap mode.
    #[must_use]
    pub fn mode(mut self, mode: WrapMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set whether to preserve indentation.
    #[must_use]
    pub fn preserve_indent(mut self, preserve: bool) -> Self {
        self.preserve_indent = preserve;
        self
    }

    /// Set whether to trim trailing whitespace.
    #[must_use]
    pub fn trim_trailing(mut self, trim: bool) -> Self {
        self.trim_trailing = trim;
        self
    }
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self::new(80)
    }
}

/// Wrap text to the specified width.
///
/// This is a convenience function using default word-wrap mode.
#[must_use]
pub fn wrap_text(text: &str, width: usize, mode: WrapMode) -> Vec<String> {
    // Char mode should preserve leading whitespace since it's raw character-boundary wrapping
    let preserve = mode == WrapMode::Char;
    wrap_with_options(
        text,
        &WrapOptions::new(width).mode(mode).preserve_indent(preserve),
    )
}

/// Wrap text with full options.
#[must_use]
pub fn wrap_with_options(text: &str, options: &WrapOptions) -> Vec<String> {
    if options.width == 0 {
        return vec![text.to_string()];
    }

    match options.mode {
        WrapMode::None => vec![text.to_string()],
        WrapMode::Char => wrap_chars(text, options),
        WrapMode::Word => wrap_words(text, options, false),
        WrapMode::WordChar => wrap_words(text, options, true),
    }
}

/// Wrap at grapheme boundaries (character wrap).
fn wrap_chars(text: &str, options: &WrapOptions) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0;

    for grapheme in text.graphemes(true) {
        // Handle newlines
        if grapheme == "\n" || grapheme == "\r\n" {
            lines.push(finalize_line(&current_line, options));
            current_line.clear();
            current_width = 0;
            continue;
        }

        let grapheme_width = crate::wrap::grapheme_width(grapheme);

        // Check if this grapheme fits
        if current_width + grapheme_width > options.width && !current_line.is_empty() {
            lines.push(finalize_line(&current_line, options));
            current_line.clear();
            current_width = 0;
        }

        // Add grapheme to current line
        current_line.push_str(grapheme);
        current_width += grapheme_width;
    }

    // Always push the pending line at the end.
    // This handles the last segment of text, or the empty line after a trailing newline.
    lines.push(finalize_line(&current_line, options));

    lines
}

/// Wrap at word boundaries.
fn wrap_words(text: &str, options: &WrapOptions, char_fallback: bool) -> Vec<String> {
    let mut lines = Vec::new();

    // Split by existing newlines first
    for raw_paragraph in text.split('\n') {
        let paragraph = raw_paragraph.strip_suffix('\r').unwrap_or(raw_paragraph);
        let mut current_line = String::new();
        let mut current_width = 0;

        let len_before = lines.len();

        wrap_paragraph(
            paragraph,
            options,
            char_fallback,
            &mut lines,
            &mut current_line,
            &mut current_width,
        );

        // Push the last line of the paragraph if non-empty, or if wrap_paragraph
        // added no lines (empty paragraph from explicit newline).
        if !current_line.is_empty() || lines.len() == len_before {
            lines.push(finalize_line(&current_line, options));
        }
    }

    lines
}

/// Wrap a single paragraph (no embedded newlines).
fn wrap_paragraph(
    text: &str,
    options: &WrapOptions,
    char_fallback: bool,
    lines: &mut Vec<String>,
    current_line: &mut String,
    current_width: &mut usize,
) {
    for word in split_words(text) {
        let word_width = display_width(&word);

        // If word fits on current line
        if *current_width + word_width <= options.width {
            current_line.push_str(&word);
            *current_width += word_width;
            continue;
        }

        // Word doesn't fit - need to wrap
        if !current_line.is_empty() {
            lines.push(finalize_line(current_line, options));
            current_line.clear();
            *current_width = 0;

            // If the word causing the wrap is just whitespace, discard it.
            // It was effectively "trailing whitespace" of the previous line.
            // Exception: if preserve_indent is true, we might want to keep it?
            // Usually, inter-word spaces that wrap should be discarded regardless of indentation policy.
            // Indentation policy applies to *explicit* leading whitespace of the paragraph.
            if word.trim().is_empty() {
                continue;
            }
        }

        // Check if word itself exceeds width
        if word_width > options.width {
            if char_fallback {
                // Break the long word into pieces
                wrap_long_word(&word, options, lines, current_line, current_width);
            } else {
                // Just put the long word on its own line
                lines.push(finalize_line(&word, options));
            }
        } else {
            // Word fits on a fresh line
            let (fragment, fragment_width) = if options.preserve_indent {
                (word.as_str(), word_width)
            } else {
                let trimmed = word.trim_start();
                (trimmed, display_width(trimmed))
            };
            if !fragment.is_empty() {
                current_line.push_str(fragment);
            }
            *current_width = fragment_width;
        }
    }
}

/// Break a long word that exceeds the width limit.
fn wrap_long_word(
    word: &str,
    options: &WrapOptions,
    lines: &mut Vec<String>,
    current_line: &mut String,
    current_width: &mut usize,
) {
    for grapheme in word.graphemes(true) {
        let grapheme_width = crate::wrap::grapheme_width(grapheme);

        // Skip leading whitespace on new lines
        if *current_width == 0 && grapheme.trim().is_empty() && !options.preserve_indent {
            continue;
        }

        if *current_width + grapheme_width > options.width && !current_line.is_empty() {
            lines.push(finalize_line(current_line, options));
            current_line.clear();
            *current_width = 0;

            // Skip leading whitespace after wrap
            if grapheme.trim().is_empty() && !options.preserve_indent {
                continue;
            }
        }

        current_line.push_str(grapheme);
        *current_width += grapheme_width;
    }
}

/// Split text into words (preserving whitespace with words).
///
/// Splits on whitespace boundaries, keeping whitespace-only segments
/// separate from non-whitespace segments.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_whitespace = false;

    for grapheme in text.graphemes(true) {
        let is_ws = grapheme.chars().all(is_breaking_whitespace);

        if is_ws != in_whitespace && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }

        current.push_str(grapheme);
        in_whitespace = is_ws;
    }

    if !current.is_empty() {
        words.push(current);
    }

    words
}

/// Finalize a line (apply trimming, etc.).
fn finalize_line(line: &str, options: &WrapOptions) -> String {
    let mut result = if options.trim_trailing {
        line.trim_end().to_string()
    } else {
        line.to_string()
    };

    if !options.preserve_indent {
        // We only trim start if the user explicitly opted out of preserving indent.
        // However, standard wrapping usually preserves start indent of the first line
        // and only indents continuations.
        // The `preserve_indent` option in `WrapOptions` usually refers to *hanging* indent
        // or preserving leading whitespace on new lines.
        //
        // In this implementation, `wrap_paragraph` logic trims start of *continuation* lines
        // if they fit.
        //
        // But for `finalize_line`, which handles the *completed* line string,
        // we generally don't want to aggressively strip leading whitespace unless
        // it was a blank line.
        //
        // Let's stick to the requested change: trim start if not preserving indent.
        // But wait, `line.trim_start()` would kill paragraph indentation.
        //
        // Re-reading intent: "trim leading indentation if preserve_indent is false".
        // This implies that if `preserve_indent` is false, we want flush-left text.

        let trimmed = result.trim_start();
        if trimmed.len() != result.len() {
            result = trimmed.to_string();
        }
    }

    result
}

/// Truncate text to fit within a width, adding ellipsis if needed.
///
/// This function respects grapheme boundaries - it will never break
/// an emoji, ZWJ sequence, or combining character sequence.
#[must_use]
pub fn truncate_with_ellipsis(text: &str, max_width: usize, ellipsis: &str) -> String {
    let text_width = display_width(text);

    if text_width <= max_width {
        return text.to_string();
    }

    let ellipsis_width = display_width(ellipsis);

    // If ellipsis alone exceeds width, just truncate without ellipsis
    if ellipsis_width >= max_width {
        return truncate_to_width(text, max_width);
    }

    let target_width = max_width - ellipsis_width;
    let mut result = truncate_to_width(text, target_width);
    result.push_str(ellipsis);
    result
}

/// Truncate text to exactly fit within a width (no ellipsis).
///
/// Respects grapheme boundaries.
#[must_use]
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut current_width = 0;

    for grapheme in text.graphemes(true) {
        let grapheme_width = crate::wrap::grapheme_width(grapheme);

        if current_width + grapheme_width > max_width {
            break;
        }

        result.push_str(grapheme);
        current_width += grapheme_width;
    }

    result
}

/// Returns `Some(width)` if text is printable ASCII only, `None` otherwise.
///
/// This is a fast-path optimization. For printable ASCII (0x20-0x7E), display width
/// equals byte length, so we can avoid the full Unicode width calculation.
///
/// Returns `None` for:
/// - Non-ASCII characters (multi-byte UTF-8)
/// - ASCII control characters (0x00-0x1F, 0x7F) which have display width 0
///
/// # Example
/// ```
/// use ftui_text::wrap::ascii_width;
///
/// assert_eq!(ascii_width("hello"), Some(5));
/// assert_eq!(ascii_width("你好"), None);  // Contains CJK
/// assert_eq!(ascii_width(""), Some(0));
/// assert_eq!(ascii_width("hello\tworld"), None);  // Contains tab (control char)
/// ```
#[inline]
#[must_use]
pub fn ascii_width(text: &str) -> Option<usize> {
    ftui_core::text_width::ascii_width(text)
}

/// Calculate the display width of a single grapheme cluster.
///
/// Uses `unicode-display-width` so grapheme clusters (ZWJ emoji, flags, combining
/// marks) are treated as a single glyph with correct terminal width.
///
/// If `FTUI_TEXT_CJK_WIDTH=1` (or `FTUI_CJK_WIDTH=1`) or a CJK locale is detected,
/// ambiguous-width characters are treated as double-width.
#[inline]
#[must_use]
pub fn grapheme_width(grapheme: &str) -> usize {
    ftui_core::text_width::grapheme_width(grapheme)
}

/// Calculate the display width of text in cells.
///
/// Uses ASCII fast-path when possible, falling back to Unicode width calculation.
///
/// If `FTUI_TEXT_CJK_WIDTH=1` (or `FTUI_CJK_WIDTH=1`) or a CJK locale is detected,
/// ambiguous-width characters are treated as double-width.
///
/// # Performance
/// - ASCII text: O(n) byte scan, no allocations
/// - Non-ASCII: Grapheme segmentation + per-grapheme width
#[inline]
#[must_use]
pub fn display_width(text: &str) -> usize {
    ftui_core::text_width::display_width(text)
}

/// Check if a string contains any wide characters (width > 1).
#[must_use]
pub fn has_wide_chars(text: &str) -> bool {
    text.graphemes(true)
        .any(|g| crate::wrap::grapheme_width(g) > 1)
}

/// Check if a string is ASCII-only (fast path possible).
#[must_use]
pub fn is_ascii_only(text: &str) -> bool {
    text.is_ascii()
}

// =============================================================================
// Grapheme Segmentation Helpers (bd-6e9.8)
// =============================================================================

/// Count the number of grapheme clusters in a string.
///
/// A grapheme cluster is a user-perceived character, which may consist of
/// multiple Unicode code points (e.g., emoji with modifiers, combining marks).
///
/// # Example
/// ```
/// use ftui_text::wrap::grapheme_count;
///
/// assert_eq!(grapheme_count("hello"), 5);
/// assert_eq!(grapheme_count("e\u{0301}"), 1);  // e + combining acute = 1 grapheme
/// assert_eq!(grapheme_count("\u{1F468}\u{200D}\u{1F469}"), 1);  // ZWJ sequence = 1 grapheme
/// ```
#[inline]
#[must_use]
pub fn grapheme_count(text: &str) -> usize {
    text.graphemes(true).count()
}

/// Iterate over grapheme clusters in a string.
///
/// Returns an iterator yielding `&str` slices for each grapheme cluster.
/// Uses extended grapheme clusters (UAX #29).
///
/// # Example
/// ```
/// use ftui_text::wrap::graphemes;
///
/// let chars: Vec<&str> = graphemes("e\u{0301}bc").collect();
/// assert_eq!(chars, vec!["e\u{0301}", "b", "c"]);
/// ```
#[inline]
pub fn graphemes(text: &str) -> impl Iterator<Item = &str> {
    text.graphemes(true)
}

/// Truncate text to fit within a maximum display width.
///
/// Returns a tuple of (truncated_text, actual_width) where:
/// - `truncated_text` is the prefix that fits within `max_width`
/// - `actual_width` is the display width of the truncated text
///
/// Respects grapheme boundaries - will never split an emoji, ZWJ sequence,
/// or combining character sequence.
///
/// # Example
/// ```
/// use ftui_text::wrap::truncate_to_width_with_info;
///
/// let (text, width) = truncate_to_width_with_info("hello world", 5);
/// assert_eq!(text, "hello");
/// assert_eq!(width, 5);
///
/// // CJK characters are 2 cells wide
/// let (text, width) = truncate_to_width_with_info("\u{4F60}\u{597D}", 3);
/// assert_eq!(text, "\u{4F60}");  // Only first char fits
/// assert_eq!(width, 2);
/// ```
#[must_use]
pub fn truncate_to_width_with_info(text: &str, max_width: usize) -> (&str, usize) {
    let mut byte_end = 0;
    let mut current_width = 0;

    for grapheme in text.graphemes(true) {
        let grapheme_width = crate::wrap::grapheme_width(grapheme);

        if current_width + grapheme_width > max_width {
            break;
        }

        current_width += grapheme_width;
        byte_end += grapheme.len();
    }

    (&text[..byte_end], current_width)
}

/// Find word boundary positions suitable for line breaking.
///
/// Returns byte indices where word breaks can occur. This is useful for
/// implementing soft-wrap at word boundaries.
///
/// # Example
/// ```
/// use ftui_text::wrap::word_boundaries;
///
/// let breaks: Vec<usize> = word_boundaries("hello world foo").collect();
/// // Breaks occur after spaces
/// assert!(breaks.contains(&6));   // After "hello "
/// assert!(breaks.contains(&12));  // After "world "
/// ```
pub fn word_boundaries(text: &str) -> impl Iterator<Item = usize> + '_ {
    text.split_word_bound_indices().filter_map(|(idx, word)| {
        // Return index at end of whitespace sequences (good break points)
        if word.chars().all(is_breaking_whitespace) {
            Some(idx + word.len())
        } else {
            None
        }
    })
}

/// Split text into word segments preserving boundaries.
///
/// Each segment is either a word or a whitespace sequence.
/// Useful for word-based text processing.
///
/// # Example
/// ```
/// use ftui_text::wrap::word_segments;
///
/// let segments: Vec<&str> = word_segments("hello  world").collect();
/// assert_eq!(segments, vec!["hello", "  ", "world"]);
/// ```
pub fn word_segments(text: &str) -> impl Iterator<Item = &str> {
    text.split_word_bounds()
}

// =============================================================================
// Knuth-Plass Optimal Line Breaking (bd-4kq0.5.1)
// =============================================================================
//
// # Algorithm
//
// Classic Knuth-Plass DP for optimal paragraph line-breaking.
// Given text split into words with measured widths, find line breaks
// that minimize total "badness" across all lines.
//
// ## Badness Function
//
// For a line with slack `s = width - line_content_width`:
//   badness(s, width) = (s / width)^3 * BADNESS_SCALE
//
// Badness is infinite (BADNESS_INF) for lines that overflow (s < 0).
// The last line has badness 0 (TeX convention: last line is never penalized
// for being short).
//
// ## Penalties
//
// - PENALTY_HYPHEN: cost for breaking at a hyphen (not yet used, reserved)
// - PENALTY_FLAGGED: cost for consecutive flagged breaks
// - PENALTY_FORCE_BREAK: large penalty for forcing a break mid-word
//
// ## DP Recurrence
//
// cost[j] = min over all valid i < j of:
//   cost[i] + badness(line from word i to word j-1) + penalty(break at j)
//
// Backtrack via `from[j]` to recover the optimal break sequence.
//
// ## Tie-Breaking
//
// When two break sequences have equal cost, prefer:
// 1. Fewer lines (later break)
// 2. More balanced distribution (lower max badness)

/// Scale factor for badness computation. Matches TeX convention.
const BADNESS_SCALE: u64 = 10_000;

/// Badness value for infeasible lines (overflow).
const BADNESS_INF: u64 = u64::MAX / 2;

/// Penalty for forcing a mid-word character break.
const PENALTY_FORCE_BREAK: u64 = 5000;

/// Maximum lookahead (words per line) for DP pruning.
/// Limits worst-case to O(n × MAX_LOOKAHEAD) instead of O(n²).
/// Any line with more than this many words will use the greedy breakpoint.
const KP_MAX_LOOKAHEAD: usize = 64;

/// Compute the badness of a line with the given slack.
///
/// Badness grows as the cube of the ratio `slack / width`, scaled by
/// `BADNESS_SCALE`. This heavily penalizes very loose lines while being
/// lenient on small amounts of slack.
///
/// Returns `BADNESS_INF` if the line overflows (`slack < 0`).
/// Returns 0 for the last line (TeX convention).
#[inline]
fn knuth_plass_badness(slack: i64, width: usize, is_last_line: bool) -> u64 {
    if slack < 0 {
        return BADNESS_INF;
    }
    if is_last_line {
        return 0;
    }
    if width == 0 {
        return if slack == 0 { 0 } else { BADNESS_INF };
    }
    // badness = (slack/width)^3 * BADNESS_SCALE
    // Use integer arithmetic to avoid floating point:
    // (slack^3 * BADNESS_SCALE) / width^3
    let s = slack as u64;
    let w = width as u64;
    // Prevent overflow: compute in stages
    let s3 = s.saturating_mul(s).saturating_mul(s);
    let w3 = w.saturating_mul(w).saturating_mul(w);
    if w3 == 0 {
        return BADNESS_INF;
    }
    s3.saturating_mul(BADNESS_SCALE) / w3
}

/// Check if a character is a breaking whitespace (candidate for wrapping).
///
/// Returns true for standard whitespace (space, tab, newline) but false for
/// Non-Breaking Space (U+00A0) and Narrow No-Break Space (U+202F).
pub(crate) fn is_breaking_whitespace(c: char) -> bool {
    c.is_whitespace() && c != '\u{00A0}' && c != '\u{202F}'
}

/// A word token with its measured cell width.
#[derive(Debug, Clone)]
struct KpWord {
    /// The word text (including any trailing space).
    text: String,
    /// Cell width of the content (excluding trailing space for break purposes).
    content_width: usize,
    /// Cell width of the trailing space (0 if none).
    space_width: usize,
}

/// Split text into KpWord tokens for Knuth-Plass processing.
fn kp_tokenize(text: &str) -> Vec<KpWord> {
    let mut words = Vec::new();
    let raw_segments: Vec<&str> = text.split_word_bounds().collect();

    let mut current_text = String::new();
    let mut current_width = 0;

    for seg in raw_segments {
        if seg.chars().all(is_breaking_whitespace) {
            // This is a break point (space).
            // Finish current word if any.
            if !current_text.is_empty() {
                words.push(KpWord {
                    text: std::mem::take(&mut current_text),
                    content_width: current_width,
                    space_width: display_width(seg), // attach space to it
                });
                current_width = 0;
            } else if let Some(last) = words.last_mut() {
                // Attach space to previous word
                last.text.push_str(seg);
                last.space_width += display_width(seg);
            } else {
                // Handle leading whitespace as a word with 0 content width
                words.push(KpWord {
                    text: seg.to_string(),
                    content_width: 0,
                    space_width: display_width(seg),
                });
            }
        } else {
            // Content (word or NBSP).
            // Append to current accumulator.
            current_text.push_str(seg);
            current_width += display_width(seg);
        }
    }

    // Push remaining content
    if !current_text.is_empty() {
        words.push(KpWord {
            text: current_text,
            content_width: current_width,
            space_width: 0,
        });
    }

    words
}

/// Result of optimal line breaking.
#[derive(Debug, Clone)]
pub struct KpBreakResult {
    /// The wrapped lines.
    pub lines: Vec<String>,
    /// Total cost (sum of badness + penalties).
    pub total_cost: u64,
    /// Per-line badness values (for diagnostics).
    pub line_badness: Vec<u64>,
}

/// Compute optimal line breaks using Knuth-Plass DP.
///
/// Given a paragraph of text and a target width, finds the set of line
/// breaks that minimizes total badness (cubic slack penalty).
///
/// Falls back to greedy word-wrap if the DP cost is prohibitive (very
/// long paragraphs), controlled by `max_words`.
///
/// # Arguments
/// * `text` - The paragraph to wrap (no embedded newlines expected).
/// * `width` - Target line width in cells.
///
/// # Returns
/// `KpBreakResult` with optimal lines, total cost, and per-line badness.
pub fn wrap_optimal(text: &str, width: usize) -> KpBreakResult {
    if width == 0 || text.is_empty() {
        return KpBreakResult {
            lines: vec![text.to_string()],
            total_cost: 0,
            line_badness: vec![0],
        };
    }

    let words = kp_tokenize(text);
    if words.is_empty() {
        return KpBreakResult {
            lines: vec![text.to_string()],
            total_cost: 0,
            line_badness: vec![0],
        };
    }

    let n = words.len();

    // cost[j] = minimum cost to set words 0..j
    // from[j] = index i such that line starts at word i for the break ending at j
    let mut cost = vec![BADNESS_INF; n + 1];
    let mut from = vec![0usize; n + 1];
    cost[0] = 0;

    for j in 1..=n {
        let mut line_width: usize = 0;
        // Try all possible line starts i (going backwards from j).
        // Bounded by KP_MAX_LOOKAHEAD to keep runtime O(n × lookahead).
        let earliest = j.saturating_sub(KP_MAX_LOOKAHEAD);
        for i in (earliest..j).rev() {
            // Add word i's width
            line_width += words[i].content_width;
            if i < j - 1 {
                // Add space between words (from word i's trailing space)
                line_width += words[i].space_width;
            }

            // Check if line overflows
            if line_width > width && i < j - 1 {
                // Can't fit — and we've already tried adding more words
                break;
            }

            let slack = width as i64 - line_width as i64;
            let is_last = j == n;
            let badness = if line_width > width {
                // Single word too wide — must force-break
                PENALTY_FORCE_BREAK
            } else {
                knuth_plass_badness(slack, width, is_last)
            };

            let candidate = cost[i].saturating_add(badness);
            // Tie-breaking: prefer later break (fewer lines)
            if candidate < cost[j] || (candidate == cost[j] && i > from[j]) {
                cost[j] = candidate;
                from[j] = i;
            }
        }
    }

    // Backtrack to recover break positions
    let mut breaks = Vec::new();
    let mut pos = n;
    while pos > 0 {
        breaks.push(from[pos]);
        pos = from[pos];
    }
    breaks.reverse();

    // Build output lines
    let mut lines = Vec::new();
    let mut line_badness = Vec::new();
    let break_count = breaks.len();

    for (idx, &start) in breaks.iter().enumerate() {
        let end = if idx + 1 < break_count {
            breaks[idx + 1]
        } else {
            n
        };

        // Reconstruct line text
        let mut line = String::new();
        for word in words.iter().take(end).skip(start) {
            line.push_str(&word.text);
        }

        // Trim trailing whitespace from each line
        let trimmed = line.trim_end().to_string();

        // Compute this line's badness for diagnostics
        let line_w = display_width(trimmed.as_str());
        let slack = width as i64 - line_w as i64;
        let is_last = idx == break_count - 1;
        let bad = if slack < 0 {
            PENALTY_FORCE_BREAK
        } else {
            knuth_plass_badness(slack, width, is_last)
        };

        lines.push(trimmed);
        line_badness.push(bad);
    }

    KpBreakResult {
        lines,
        total_cost: cost[n],
        line_badness,
    }
}

/// Wrap text optimally, returning just the lines (convenience wrapper).
///
/// Handles multiple paragraphs separated by `\n`.
#[must_use]
pub fn wrap_text_optimal(text: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for raw_paragraph in text.split('\n') {
        let paragraph = raw_paragraph.strip_suffix('\r').unwrap_or(raw_paragraph);
        if paragraph.is_empty() {
            result.push(String::new());
            continue;
        }
        let kp = wrap_optimal(paragraph, width);
        result.extend(kp.lines);
    }
    result
}

// =============================================================================
// Formal Paragraph Objective (bd-2vr05.15.2.1)
// =============================================================================
//
// Extends the basic Knuth-Plass badness model with:
// - Configurable penalty and demerit weights
// - Adjacency penalties (consecutive tight/loose lines, consecutive hyphens)
// - Readability constraints (stretch/compress bounds, widow/orphan guards)
// - Formal demerit computation as specified in The TeXbook Chapter 14
//
// # Demerit Formula (TeX-standard)
//
//   demerit(line) = (linepenalty + badness)^2 + penalty^2
//                   + adjacency_demerit
//
// Where `adjacency_demerit` detects:
// - Consecutive flagged breaks (e.g. two hyphens in a row)
// - Fitness class transitions (tight→loose or vice-versa)
//
// # Fitness Classes (TeX §851)
//
//   0: tight     (adjustment_ratio < -0.5)
//   1: normal    (-0.5 ≤ r < 0.5)
//   2: loose     (0.5 ≤ r < 1.0)
//   3: very loose (r ≥ 1.0)
//
// Transitions between non-adjacent classes incur `fitness_demerit`.

/// Fitness class for a line based on its adjustment ratio.
///
/// The adjustment ratio `r = slack / stretch` (or `slack / shrink` for
/// negative slack) determines how much a line differs from its natural width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FitnessClass {
    /// r < -0.5 (compressed line).
    Tight = 0,
    /// -0.5 ≤ r < 0.5 (well-set line).
    Normal = 1,
    /// 0.5 ≤ r < 1.0 (somewhat loose line).
    Loose = 2,
    /// r ≥ 1.0 (very loose line).
    VeryLoose = 3,
}

impl FitnessClass {
    /// Classify a line's fitness from its adjustment ratio.
    ///
    /// The ratio is `slack / width` for positive slack (stretch)
    /// or `slack / width` for negative slack (shrink).
    #[must_use]
    pub fn from_ratio(ratio: f64) -> Self {
        if ratio < -0.5 {
            FitnessClass::Tight
        } else if ratio < 0.5 {
            FitnessClass::Normal
        } else if ratio < 1.0 {
            FitnessClass::Loose
        } else {
            FitnessClass::VeryLoose
        }
    }

    /// Whether two consecutive fitness classes are incompatible
    /// (differ by more than one level), warranting a fitness demerit.
    #[must_use]
    pub const fn incompatible(self, other: Self) -> bool {
        let a = self as i8;
        let b = other as i8;
        // abs(a - b) > 1
        (a - b > 1) || (b - a > 1)
    }
}

/// Type of break point in the paragraph item stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakKind {
    /// Break at inter-word space (penalty = 0 by default).
    Space,
    /// Break at explicit hyphenation point (flagged break).
    Hyphen,
    /// Forced break (e.g. `\n`, end of paragraph).
    Forced,
    /// Emergency break mid-word when no feasible break exists.
    Emergency,
}

/// Penalty value for a break point.
///
/// Penalties influence where breaks occur:
/// - Negative penalty attracts breaks (e.g. after punctuation).
/// - Positive penalty repels breaks (e.g. avoid breaking before "I").
/// - `PENALTY_FORBIDDEN` (`i64::MAX`) makes the break infeasible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakPenalty {
    /// The penalty value. Higher = less desirable break.
    pub value: i64,
    /// Whether this is a flagged break (e.g. hyphenation).
    /// Two consecutive flagged breaks incur `double_hyphen_demerit`.
    pub flagged: bool,
}

impl BreakPenalty {
    /// Standard inter-word break (penalty 0, not flagged).
    pub const SPACE: Self = Self {
        value: 0,
        flagged: false,
    };

    /// Hyphenation break (moderate penalty, flagged).
    pub const HYPHEN: Self = Self {
        value: 50,
        flagged: true,
    };

    /// Forced break (negative infinity — must break here).
    pub const FORCED: Self = Self {
        value: i64::MIN,
        flagged: false,
    };

    /// Emergency mid-word break (high penalty, not flagged).
    pub const EMERGENCY: Self = Self {
        value: 5000,
        flagged: false,
    };
}

/// Configuration for the paragraph objective function.
///
/// All weight values are in the same "demerit" unit space. Higher values
/// mean stronger penalties. The TeX defaults are provided by `Default`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParagraphObjective {
    /// Base penalty added to every line's badness before squaring (TeX `\linepenalty`).
    /// Higher values prefer fewer lines.
    /// Default: 10 (TeX standard).
    pub line_penalty: u64,

    /// Additional demerit when consecutive lines have incompatible fitness classes.
    /// Default: 100 (TeX `\adjdemerits`).
    pub fitness_demerit: u64,

    /// Additional demerit when two consecutive lines both end with flagged breaks
    /// (typically hyphens). Default: 100 (TeX `\doublehyphendemerits`).
    pub double_hyphen_demerit: u64,

    /// Additional demerit when the penultimate line has a flagged break and the
    /// last line is short. Default: 100 (TeX `\finalhyphendemerits`).
    pub final_hyphen_demerit: u64,

    /// Maximum allowed adjustment ratio before the line is considered infeasible.
    /// Lines looser than this threshold get `BADNESS_INF`.
    /// Default: 2.0 (generous for terminal rendering).
    pub max_adjustment_ratio: f64,

    /// Minimum allowed adjustment ratio (negative = compression).
    /// Default: -1.0 (allow moderate compression).
    pub min_adjustment_ratio: f64,

    /// Widow penalty: extra demerit if the last line of a paragraph has
    /// fewer than `widow_threshold` characters.
    /// Default: 150.
    pub widow_demerit: u64,

    /// Character count below which the last line triggers `widow_demerit`.
    /// Default: 15 (approximately one short word).
    pub widow_threshold: usize,

    /// Orphan penalty: extra demerit if the first line of a paragraph
    /// followed by a break has fewer than `orphan_threshold` characters.
    /// Default: 150.
    pub orphan_demerit: u64,

    /// Character count below which a first-line break triggers `orphan_demerit`.
    /// Default: 20.
    pub orphan_threshold: usize,

    /// Scale factor for badness computation. Matches TeX convention.
    /// Default: 10_000.
    pub badness_scale: u64,
}

impl Default for ParagraphObjective {
    fn default() -> Self {
        Self {
            line_penalty: 10,
            fitness_demerit: 100,
            double_hyphen_demerit: 100,
            final_hyphen_demerit: 100,
            max_adjustment_ratio: 2.0,
            min_adjustment_ratio: -1.0,
            widow_demerit: 150,
            widow_threshold: 15,
            orphan_demerit: 150,
            orphan_threshold: 20,
            badness_scale: BADNESS_SCALE,
        }
    }
}

impl ParagraphObjective {
    /// Preset optimized for terminal rendering where cells are monospaced
    /// and compression is not possible (no inter-character stretch).
    #[must_use]
    pub fn terminal() -> Self {
        Self {
            // Higher line penalty: terminals prefer fewer lines
            line_penalty: 20,
            // Lower fitness demerit: monospace can't adjust spacing
            fitness_demerit: 50,
            // No compression possible in monospace
            min_adjustment_ratio: 0.0,
            // Wider tolerance for loose lines
            max_adjustment_ratio: 3.0,
            // Relaxed widow/orphan since terminal is not print
            widow_demerit: 50,
            orphan_demerit: 50,
            ..Self::default()
        }
    }

    /// Preset for high-quality proportional typography (closest to TeX defaults).
    #[must_use]
    pub fn typographic() -> Self {
        Self::default()
    }

    /// Compute the badness of a line with the given slack and target width.
    ///
    /// Badness is `(|ratio|^3) * badness_scale` where `ratio = slack / width`.
    /// Returns `None` if the line is infeasible (ratio outside bounds).
    #[must_use]
    pub fn badness(&self, slack: i64, width: usize) -> Option<u64> {
        if width == 0 {
            return if slack == 0 { Some(0) } else { None };
        }

        let ratio = slack as f64 / width as f64;

        // Check feasibility against adjustment bounds
        if ratio < self.min_adjustment_ratio || ratio > self.max_adjustment_ratio {
            return None; // infeasible
        }

        let abs_ratio = ratio.abs();
        let badness = (abs_ratio * abs_ratio * abs_ratio * self.badness_scale as f64) as u64;
        Some(badness)
    }

    /// Compute the adjustment ratio for a line.
    #[must_use]
    pub fn adjustment_ratio(&self, slack: i64, width: usize) -> f64 {
        if width == 0 {
            return 0.0;
        }
        slack as f64 / width as f64
    }

    /// Compute demerits for a single break point.
    ///
    /// This is the full TeX demerit formula:
    ///   demerit = (line_penalty + badness)^2 + penalty^2
    ///
    /// For forced breaks (negative penalty), the formula becomes:
    ///   demerit = (line_penalty + badness)^2 - penalty^2
    ///
    /// Returns `None` if the line is infeasible.
    #[must_use]
    pub fn demerits(&self, slack: i64, width: usize, penalty: &BreakPenalty) -> Option<u64> {
        let badness = self.badness(slack, width)?;

        let base = self.line_penalty.saturating_add(badness);
        let base_sq = base.saturating_mul(base);

        let pen_sq = (penalty.value.unsigned_abs()).saturating_mul(penalty.value.unsigned_abs());

        if penalty.value >= 0 {
            Some(base_sq.saturating_add(pen_sq))
        } else if penalty.value > i64::MIN {
            // Forced/attractive break: subtract penalty²
            Some(base_sq.saturating_sub(pen_sq))
        } else {
            // Forced break: just base²
            Some(base_sq)
        }
    }

    /// Compute adjacency demerits between two consecutive line breaks.
    ///
    /// Returns the additional demerit to add when `prev` and `curr` are
    /// consecutive break points.
    #[must_use]
    pub fn adjacency_demerits(
        &self,
        prev_fitness: FitnessClass,
        curr_fitness: FitnessClass,
        prev_flagged: bool,
        curr_flagged: bool,
    ) -> u64 {
        let mut extra = 0u64;

        // Fitness class incompatibility
        if prev_fitness.incompatible(curr_fitness) {
            extra = extra.saturating_add(self.fitness_demerit);
        }

        // Double flagged break (consecutive hyphens)
        if prev_flagged && curr_flagged {
            extra = extra.saturating_add(self.double_hyphen_demerit);
        }

        extra
    }

    /// Check if the last line triggers widow penalty.
    ///
    /// A "widow" here means the last line of a paragraph is very short,
    /// leaving a visually orphaned fragment.
    #[must_use]
    pub fn widow_demerits(&self, last_line_chars: usize) -> u64 {
        if last_line_chars < self.widow_threshold {
            self.widow_demerit
        } else {
            0
        }
    }

    /// Check if the first line triggers orphan penalty.
    ///
    /// An "orphan" here means the first line before a break is very short.
    #[must_use]
    pub fn orphan_demerits(&self, first_line_chars: usize) -> u64 {
        if first_line_chars < self.orphan_threshold {
            self.orphan_demerit
        } else {
            0
        }
    }
}

#[cfg(test)]
trait TestWidth {
    fn width(&self) -> usize;
}

#[cfg(test)]
impl TestWidth for str {
    fn width(&self) -> usize {
        display_width(self)
    }
}

#[cfg(test)]
impl TestWidth for String {
    fn width(&self) -> usize {
        display_width(self)
    }
}

#[cfg(test)]
mod tests {
    use super::TestWidth;
    use super::*;

    // ==========================================================================
    // wrap_text tests
    // ==========================================================================

    #[test]
    fn wrap_text_no_wrap_needed() {
        let lines = wrap_text("hello", 10, WrapMode::Word);
        assert_eq!(lines, vec!["hello"]);
    }

    #[test]
    fn wrap_text_single_word_wrap() {
        let lines = wrap_text("hello world", 5, WrapMode::Word);
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn wrap_text_multiple_words() {
        let lines = wrap_text("hello world foo bar", 11, WrapMode::Word);
        assert_eq!(lines, vec!["hello world", "foo bar"]);
    }

    #[test]
    fn wrap_text_preserves_newlines() {
        let lines = wrap_text("line1\nline2", 20, WrapMode::Word);
        assert_eq!(lines, vec!["line1", "line2"]);
    }

    #[test]
    fn wrap_text_preserves_crlf_newlines() {
        let lines = wrap_text("line1\r\nline2\r\n", 20, WrapMode::Word);
        assert_eq!(lines, vec!["line1", "line2", ""]);
    }

    #[test]
    fn wrap_text_trailing_newlines() {
        // "line1\n" -> ["line1", ""]
        let lines = wrap_text("line1\n", 20, WrapMode::Word);
        assert_eq!(lines, vec!["line1", ""]);

        // "\n" -> ["", ""]
        let lines = wrap_text("\n", 20, WrapMode::Word);
        assert_eq!(lines, vec!["", ""]);

        // Same for Char mode
        let lines = wrap_text("line1\n", 20, WrapMode::Char);
        assert_eq!(lines, vec!["line1", ""]);
    }

    #[test]
    fn wrap_text_empty_string() {
        let lines = wrap_text("", 10, WrapMode::Word);
        assert_eq!(lines, vec![""]);
    }

    #[test]
    fn wrap_text_long_word_no_fallback() {
        let lines = wrap_text("supercalifragilistic", 10, WrapMode::Word);
        // Without fallback, long word stays on its own line
        assert_eq!(lines, vec!["supercalifragilistic"]);
    }

    #[test]
    fn wrap_text_long_word_with_fallback() {
        let lines = wrap_text("supercalifragilistic", 10, WrapMode::WordChar);
        // With fallback, long word is broken
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.width() <= 10);
        }
    }

    #[test]
    fn wrap_char_mode() {
        let lines = wrap_text("hello world", 5, WrapMode::Char);
        assert_eq!(lines, vec!["hello", " worl", "d"]);
    }

    #[test]
    fn wrap_none_mode() {
        let lines = wrap_text("hello world", 5, WrapMode::None);
        assert_eq!(lines, vec!["hello world"]);
    }

    // ==========================================================================
    // CJK wrapping tests
    // ==========================================================================

    #[test]
    fn wrap_cjk_respects_width() {
        // Each CJK char is 2 cells
        let lines = wrap_text("你好世界", 4, WrapMode::Char);
        assert_eq!(lines, vec!["你好", "世界"]);
    }

    #[test]
    fn wrap_cjk_odd_width() {
        // Width 5 can fit 2 CJK chars (4 cells)
        let lines = wrap_text("你好世", 5, WrapMode::Char);
        assert_eq!(lines, vec!["你好", "世"]);
    }

    #[test]
    fn wrap_mixed_ascii_cjk() {
        let lines = wrap_text("hi你好", 4, WrapMode::Char);
        assert_eq!(lines, vec!["hi你", "好"]);
    }

    // ==========================================================================
    // Emoji/ZWJ tests
    // ==========================================================================

    #[test]
    fn wrap_emoji_as_unit() {
        // Emoji should not be broken
        let lines = wrap_text("😀😀😀", 4, WrapMode::Char);
        // Each emoji is typically 2 cells, so 2 per line
        assert_eq!(lines.len(), 2);
        for line in &lines {
            // No partial emoji
            assert!(!line.contains("\\u"));
        }
    }

    #[test]
    fn wrap_zwj_sequence_as_unit() {
        // Family emoji (ZWJ sequence) - should stay together
        let text = "👨‍👩‍👧";
        let lines = wrap_text(text, 2, WrapMode::Char);
        // The ZWJ sequence should not be broken
        // It will exceed width but stay as one unit
        assert!(lines.iter().any(|l| l.contains("👨‍👩‍👧")));
    }

    #[test]
    fn wrap_mixed_ascii_and_emoji_respects_width() {
        let lines = wrap_text("a😀b", 3, WrapMode::Char);
        assert_eq!(lines, vec!["a😀", "b"]);
    }

    // ==========================================================================
    // Truncation tests
    // ==========================================================================

    #[test]
    fn truncate_no_change_if_fits() {
        let result = truncate_with_ellipsis("hello", 10, "...");
        assert_eq!(result, "hello");
    }

    #[test]
    fn truncate_with_ellipsis_ascii() {
        let result = truncate_with_ellipsis("hello world", 8, "...");
        assert_eq!(result, "hello...");
    }

    #[test]
    fn truncate_cjk() {
        let result = truncate_with_ellipsis("你好世界", 6, "...");
        // 6 - 3 (ellipsis) = 3 cells for content
        // 你 = 2 cells fits, 好 = 2 cells doesn't fit
        assert_eq!(result, "你...");
    }

    #[test]
    fn truncate_to_width_basic() {
        let result = truncate_to_width("hello world", 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn truncate_to_width_cjk() {
        let result = truncate_to_width("你好世界", 4);
        assert_eq!(result, "你好");
    }

    #[test]
    fn truncate_to_width_odd_boundary() {
        // Can't fit half a CJK char
        let result = truncate_to_width("你好", 3);
        assert_eq!(result, "你");
    }

    #[test]
    fn truncate_combining_chars() {
        // e + combining acute accent
        let text = "e\u{0301}test";
        let result = truncate_to_width(text, 2);
        // Should keep é together and add 't'
        assert_eq!(result.chars().count(), 3); // e + combining + t
    }

    // ==========================================================================
    // Helper function tests
    // ==========================================================================

    #[test]
    fn display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
    }

    #[test]
    fn display_width_cjk() {
        assert_eq!(display_width("你好"), 4);
    }

    #[test]
    fn display_width_emoji_sequences() {
        assert_eq!(display_width("👩‍🔬"), 2);
        assert_eq!(display_width("👨‍👩‍👧‍👦"), 2);
        assert_eq!(display_width("👩‍🚀x"), 3);
    }

    #[test]
    fn display_width_misc_symbol_emoji() {
        assert_eq!(display_width("⏳"), 2);
        assert_eq!(display_width("⌛"), 2);
    }

    #[test]
    fn display_width_emoji_presentation_selector() {
        // Text-default emoji + VS16: terminals render at width 1.
        assert_eq!(display_width("❤️"), 1);
        assert_eq!(display_width("⌨️"), 1);
        assert_eq!(display_width("⚠️"), 1);
    }

    #[test]
    fn display_width_misc_symbol_ranges() {
        // Wide characters (east_asian_width=W) are always width 2
        assert_eq!(display_width("⌚"), 2); // U+231A WATCH, Wide
        assert_eq!(display_width("⭐"), 2); // U+2B50 WHITE MEDIUM STAR, Wide

        // Neutral characters (east_asian_width=N): width depends on CJK mode
        let airplane_width = display_width("✈"); // U+2708 AIRPLANE, Neutral
        let arrow_width = display_width("⬆"); // U+2B06 UPWARDS BLACK ARROW, Neutral
        assert!(
            [1, 2].contains(&airplane_width),
            "airplane should be 1 (non-CJK) or 2 (CJK), got {airplane_width}"
        );
        assert_eq!(
            airplane_width, arrow_width,
            "both Neutral-width chars should have same width in any mode"
        );
    }

    #[test]
    fn display_width_flags() {
        assert_eq!(display_width("🇺🇸"), 2);
        assert_eq!(display_width("🇯🇵"), 2);
        assert_eq!(display_width("🇺🇸🇯🇵"), 4);
    }

    #[test]
    fn display_width_skin_tone_modifiers() {
        assert_eq!(display_width("👍🏻"), 2);
        assert_eq!(display_width("👍🏽"), 2);
    }

    #[test]
    fn display_width_zwj_sequences() {
        assert_eq!(display_width("👩‍💻"), 2);
        assert_eq!(display_width("👨‍👩‍👧‍👦"), 2);
    }

    #[test]
    fn display_width_mixed_ascii_and_emoji() {
        assert_eq!(display_width("A😀B"), 4);
        assert_eq!(display_width("A👩‍💻B"), 4);
        assert_eq!(display_width("ok ✅"), 5);
    }

    #[test]
    fn display_width_file_icons() {
        // Inherently-wide emoji (Emoji_Presentation=Yes or EAW=W): width 2
        // ⚡️ (U+26A1+FE0F) has EAW=W, so remains wide after VS16 stripping.
        let wide_icons = ["📁", "🔗", "🦀", "🐍", "📜", "📝", "🎵", "🎬", "⚡️", "📄"];
        for icon in wide_icons {
            assert_eq!(display_width(icon), 2, "icon width mismatch: {icon}");
        }
        // Text-default (EAW=N) + VS16: terminals render at width 1
        let narrow_icons = ["⚙️", "🖼️"];
        for icon in narrow_icons {
            assert_eq!(display_width(icon), 1, "VS16 icon width mismatch: {icon}");
        }
    }

    #[test]
    fn grapheme_width_emoji_sequence() {
        assert_eq!(grapheme_width("👩‍🔬"), 2);
    }

    #[test]
    fn grapheme_width_flags_and_modifiers() {
        assert_eq!(grapheme_width("🇺🇸"), 2);
        assert_eq!(grapheme_width("👍🏽"), 2);
    }

    #[test]
    fn display_width_empty() {
        assert_eq!(display_width(""), 0);
    }

    // ==========================================================================
    // ASCII width fast-path tests
    // ==========================================================================

    #[test]
    fn ascii_width_pure_ascii() {
        assert_eq!(ascii_width("hello"), Some(5));
        assert_eq!(ascii_width("hello world 123"), Some(15));
    }

    #[test]
    fn ascii_width_empty() {
        assert_eq!(ascii_width(""), Some(0));
    }

    #[test]
    fn ascii_width_non_ascii_returns_none() {
        assert_eq!(ascii_width("你好"), None);
        assert_eq!(ascii_width("héllo"), None);
        assert_eq!(ascii_width("hello😀"), None);
    }

    #[test]
    fn ascii_width_mixed_returns_none() {
        assert_eq!(ascii_width("hi你好"), None);
        assert_eq!(ascii_width("caf\u{00e9}"), None); // café
    }

    #[test]
    fn ascii_width_control_chars_returns_none() {
        // Control characters are ASCII but have display width 0, not byte length
        assert_eq!(ascii_width("\t"), None); // tab
        assert_eq!(ascii_width("\n"), None); // newline
        assert_eq!(ascii_width("\r"), None); // carriage return
        assert_eq!(ascii_width("\0"), None); // NUL
        assert_eq!(ascii_width("\x7F"), None); // DEL
        assert_eq!(ascii_width("hello\tworld"), None); // mixed with tab
        assert_eq!(ascii_width("line1\nline2"), None); // mixed with newline
    }

    #[test]
    fn display_width_uses_ascii_fast_path() {
        // ASCII should work (implicitly tests fast path)
        assert_eq!(display_width("test"), 4);
        // Non-ASCII should also work (tests fallback)
        assert_eq!(display_width("你"), 2);
    }

    #[test]
    fn has_wide_chars_true() {
        assert!(has_wide_chars("hi你好"));
    }

    #[test]
    fn has_wide_chars_false() {
        assert!(!has_wide_chars("hello"));
    }

    #[test]
    fn is_ascii_only_true() {
        assert!(is_ascii_only("hello world 123"));
    }

    #[test]
    fn is_ascii_only_false() {
        assert!(!is_ascii_only("héllo"));
    }

    // ==========================================================================
    // Grapheme helper tests (bd-6e9.8)
    // ==========================================================================

    #[test]
    fn grapheme_count_ascii() {
        assert_eq!(grapheme_count("hello"), 5);
        assert_eq!(grapheme_count(""), 0);
    }

    #[test]
    fn grapheme_count_combining() {
        // e + combining acute = 1 grapheme
        assert_eq!(grapheme_count("e\u{0301}"), 1);
        // Multiple combining marks
        assert_eq!(grapheme_count("e\u{0301}\u{0308}"), 1);
    }

    #[test]
    fn grapheme_count_cjk() {
        assert_eq!(grapheme_count("你好"), 2);
    }

    #[test]
    fn grapheme_count_emoji() {
        assert_eq!(grapheme_count("😀"), 1);
        // Emoji with skin tone modifier = 1 grapheme
        assert_eq!(grapheme_count("👍🏻"), 1);
    }

    #[test]
    fn grapheme_count_zwj() {
        // Family emoji (ZWJ sequence) = 1 grapheme
        assert_eq!(grapheme_count("👨‍👩‍👧"), 1);
    }

    #[test]
    fn graphemes_iteration() {
        let gs: Vec<&str> = graphemes("e\u{0301}bc").collect();
        assert_eq!(gs, vec!["e\u{0301}", "b", "c"]);
    }

    #[test]
    fn graphemes_empty() {
        let gs: Vec<&str> = graphemes("").collect();
        assert!(gs.is_empty());
    }

    #[test]
    fn graphemes_cjk() {
        let gs: Vec<&str> = graphemes("你好").collect();
        assert_eq!(gs, vec!["你", "好"]);
    }

    #[test]
    fn truncate_to_width_with_info_basic() {
        let (text, width) = truncate_to_width_with_info("hello world", 5);
        assert_eq!(text, "hello");
        assert_eq!(width, 5);
    }

    #[test]
    fn truncate_to_width_with_info_cjk() {
        let (text, width) = truncate_to_width_with_info("你好世界", 3);
        assert_eq!(text, "你");
        assert_eq!(width, 2);
    }

    #[test]
    fn truncate_to_width_with_info_combining() {
        let (text, width) = truncate_to_width_with_info("e\u{0301}bc", 2);
        assert_eq!(text, "e\u{0301}b");
        assert_eq!(width, 2);
    }

    #[test]
    fn truncate_to_width_with_info_fits() {
        let (text, width) = truncate_to_width_with_info("hi", 10);
        assert_eq!(text, "hi");
        assert_eq!(width, 2);
    }

    #[test]
    fn word_boundaries_basic() {
        let breaks: Vec<usize> = word_boundaries("hello world").collect();
        assert!(breaks.contains(&6)); // After "hello "
    }

    #[test]
    fn word_boundaries_multiple_spaces() {
        let breaks: Vec<usize> = word_boundaries("a  b").collect();
        assert!(breaks.contains(&3)); // After "a  "
    }

    #[test]
    fn word_segments_basic() {
        let segs: Vec<&str> = word_segments("hello  world").collect();
        // split_word_bounds gives individual segments
        assert!(segs.contains(&"hello"));
        assert!(segs.contains(&"world"));
    }

    // ==========================================================================
    // WrapOptions tests
    // ==========================================================================

    #[test]
    fn wrap_options_builder() {
        let opts = WrapOptions::new(40)
            .mode(WrapMode::Char)
            .preserve_indent(true)
            .trim_trailing(false);

        assert_eq!(opts.width, 40);
        assert_eq!(opts.mode, WrapMode::Char);
        assert!(opts.preserve_indent);
        assert!(!opts.trim_trailing);
    }

    #[test]
    fn wrap_options_trim_trailing() {
        let opts = WrapOptions::new(10).trim_trailing(true);
        let lines = wrap_with_options("hello   world", &opts);
        // Trailing spaces should be trimmed
        assert!(!lines.iter().any(|l| l.ends_with(' ')));
    }

    #[test]
    fn wrap_preserve_indent_keeps_leading_ws_on_new_line() {
        let opts = WrapOptions::new(7)
            .mode(WrapMode::Word)
            .preserve_indent(true);
        let lines = wrap_with_options("word12  abcde", &opts);
        assert_eq!(lines, vec!["word12", "  abcde"]);
    }

    #[test]
    fn wrap_no_preserve_indent_trims_leading_ws_on_new_line() {
        let opts = WrapOptions::new(7)
            .mode(WrapMode::Word)
            .preserve_indent(false);
        let lines = wrap_with_options("word12  abcde", &opts);
        assert_eq!(lines, vec!["word12", "abcde"]);
    }

    #[test]
    fn wrap_zero_width() {
        let lines = wrap_text("hello", 0, WrapMode::Word);
        // Zero width returns original text
        assert_eq!(lines, vec!["hello"]);
    }

    // ==========================================================================
    // Additional coverage tests for width measurement
    // ==========================================================================

    #[test]
    fn wrap_mode_default() {
        let mode = WrapMode::default();
        assert_eq!(mode, WrapMode::Word);
    }

    #[test]
    fn wrap_options_default() {
        let opts = WrapOptions::default();
        assert_eq!(opts.width, 80);
        assert_eq!(opts.mode, WrapMode::Word);
        assert!(!opts.preserve_indent);
        assert!(opts.trim_trailing);
    }

    #[test]
    fn display_width_emoji_skin_tone() {
        let width = display_width("👍🏻");
        assert_eq!(width, 2);
    }

    #[test]
    fn display_width_flag_emoji() {
        let width = display_width("🇺🇸");
        assert_eq!(width, 2);
    }

    #[test]
    fn display_width_zwj_family() {
        let width = display_width("👨‍👩‍👧");
        assert_eq!(width, 2);
    }

    #[test]
    fn display_width_multiple_combining() {
        // e + combining acute + combining diaeresis = still 1 cell
        let width = display_width("e\u{0301}\u{0308}");
        assert_eq!(width, 1);
    }

    #[test]
    fn ascii_width_printable_range() {
        // Test entire printable ASCII range (0x20-0x7E)
        let printable: String = (0x20u8..=0x7Eu8).map(|b| b as char).collect();
        assert_eq!(ascii_width(&printable), Some(printable.len()));
    }

    #[test]
    fn ascii_width_newline_returns_none() {
        // Newline is a control character
        assert!(ascii_width("hello\nworld").is_none());
    }

    #[test]
    fn ascii_width_tab_returns_none() {
        // Tab is a control character
        assert!(ascii_width("hello\tworld").is_none());
    }

    #[test]
    fn ascii_width_del_returns_none() {
        // DEL (0x7F) is a control character
        assert!(ascii_width("hello\x7Fworld").is_none());
    }

    #[test]
    fn has_wide_chars_cjk_mixed() {
        assert!(has_wide_chars("abc你def"));
        assert!(has_wide_chars("你"));
        assert!(!has_wide_chars("abc"));
    }

    #[test]
    fn has_wide_chars_emoji() {
        assert!(has_wide_chars("😀"));
        assert!(has_wide_chars("hello😀"));
    }

    #[test]
    fn grapheme_count_empty() {
        assert_eq!(grapheme_count(""), 0);
    }

    #[test]
    fn grapheme_count_regional_indicators() {
        // US flag = 2 regional indicators = 1 grapheme
        assert_eq!(grapheme_count("🇺🇸"), 1);
    }

    #[test]
    fn word_boundaries_no_spaces() {
        let breaks: Vec<usize> = word_boundaries("helloworld").collect();
        assert!(breaks.is_empty());
    }

    #[test]
    fn word_boundaries_only_spaces() {
        let breaks: Vec<usize> = word_boundaries("   ").collect();
        assert!(!breaks.is_empty());
    }

    #[test]
    fn word_segments_empty() {
        let segs: Vec<&str> = word_segments("").collect();
        assert!(segs.is_empty());
    }

    #[test]
    fn word_segments_single_word() {
        let segs: Vec<&str> = word_segments("hello").collect();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0], "hello");
    }

    #[test]
    fn truncate_to_width_empty() {
        let result = truncate_to_width("", 10);
        assert_eq!(result, "");
    }

    #[test]
    fn truncate_to_width_zero_width() {
        let result = truncate_to_width("hello", 0);
        assert_eq!(result, "");
    }

    #[test]
    fn truncate_with_ellipsis_exact_fit() {
        // String exactly fits without needing truncation
        let result = truncate_with_ellipsis("hello", 5, "...");
        assert_eq!(result, "hello");
    }

    #[test]
    fn truncate_with_ellipsis_empty_ellipsis() {
        let result = truncate_with_ellipsis("hello world", 5, "");
        assert_eq!(result, "hello");
    }

    #[test]
    fn truncate_to_width_with_info_empty() {
        let (text, width) = truncate_to_width_with_info("", 10);
        assert_eq!(text, "");
        assert_eq!(width, 0);
    }

    #[test]
    fn truncate_to_width_with_info_zero_width() {
        let (text, width) = truncate_to_width_with_info("hello", 0);
        assert_eq!(text, "");
        assert_eq!(width, 0);
    }

    #[test]
    fn truncate_to_width_wide_char_boundary() {
        // Try to truncate at width 3 where a CJK char (width 2) would split
        let (text, width) = truncate_to_width_with_info("a你好", 2);
        // "a" is 1 cell, "你" is 2 cells, so only "a" fits in width 2
        assert_eq!(text, "a");
        assert_eq!(width, 1);
    }

    #[test]
    fn wrap_mode_none() {
        let lines = wrap_text("hello world", 5, WrapMode::None);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn wrap_long_word_no_char_fallback() {
        // WordChar mode handles long words by falling back to char wrap
        let lines = wrap_text("supercalifragilistic", 10, WrapMode::WordChar);
        // Should wrap even the long word
        for line in &lines {
            assert!(line.width() <= 10);
        }
    }

    // =========================================================================
    // Knuth-Plass Optimal Line Breaking Tests (bd-4kq0.5.1)
    // =========================================================================

    #[test]
    fn unit_badness_monotone() {
        // Larger slack => higher badness (for non-last lines)
        let width = 80;
        let mut prev = knuth_plass_badness(0, width, false);
        for slack in 1..=80i64 {
            let bad = knuth_plass_badness(slack, width, false);
            assert!(
                bad >= prev,
                "badness must be monotonically non-decreasing: \
                 badness({slack}) = {bad} < badness({}) = {prev}",
                slack - 1
            );
            prev = bad;
        }
    }

    #[test]
    fn unit_badness_zero_slack() {
        // Perfect fit: badness should be 0
        assert_eq!(knuth_plass_badness(0, 80, false), 0);
        assert_eq!(knuth_plass_badness(0, 80, true), 0);
    }

    #[test]
    fn unit_badness_overflow_is_inf() {
        // Negative slack (overflow) => BADNESS_INF
        assert_eq!(knuth_plass_badness(-1, 80, false), BADNESS_INF);
        assert_eq!(knuth_plass_badness(-10, 80, false), BADNESS_INF);
    }

    #[test]
    fn unit_badness_last_line_always_zero() {
        // Last line: badness is always 0 regardless of slack
        assert_eq!(knuth_plass_badness(0, 80, true), 0);
        assert_eq!(knuth_plass_badness(40, 80, true), 0);
        assert_eq!(knuth_plass_badness(79, 80, true), 0);
    }

    #[test]
    fn unit_badness_cubic_growth() {
        let width = 100;
        let b10 = knuth_plass_badness(10, width, false);
        let b20 = knuth_plass_badness(20, width, false);
        let b40 = knuth_plass_badness(40, width, false);

        // Doubling slack should ~8× badness (cubic)
        // Allow some tolerance for integer arithmetic
        assert!(
            b20 >= b10 * 6,
            "doubling slack 10→20: expected ~8× but got {}× (b10={b10}, b20={b20})",
            b20.checked_div(b10).unwrap_or(0)
        );
        assert!(
            b40 >= b20 * 6,
            "doubling slack 20→40: expected ~8× but got {}× (b20={b20}, b40={b40})",
            b40.checked_div(b20).unwrap_or(0)
        );
    }

    #[test]
    fn unit_penalty_applied() {
        // A single word that's too wide incurs PENALTY_FORCE_BREAK
        let result = wrap_optimal("superlongwordthatcannotfit", 10);
        // The word can't fit in width=10, so it must force-break
        assert!(
            result.total_cost >= PENALTY_FORCE_BREAK,
            "force-break penalty should be applied: cost={}",
            result.total_cost
        );
    }

    #[test]
    fn kp_simple_wrap() {
        let result = wrap_optimal("Hello world foo bar", 10);
        // All lines should fit within width
        for line in &result.lines {
            assert!(
                line.width() <= 10,
                "line '{line}' exceeds width 10 (width={})",
                line.width()
            );
        }
        // Should produce at least 2 lines
        assert!(result.lines.len() >= 2);
    }

    #[test]
    fn kp_perfect_fit() {
        // Words that perfectly fill each line should have zero badness
        let result = wrap_optimal("aaaa bbbb", 9);
        // "aaaa bbbb" is 9 chars, fits in one line
        assert_eq!(result.lines.len(), 1);
        assert_eq!(result.total_cost, 0);
    }

    #[test]
    fn kp_optimal_vs_greedy() {
        // Classic example where greedy is suboptimal:
        // "aaa bb cc ddddd" with width 6
        // Greedy: "aaa bb" / "cc" / "ddddd" → unbalanced (cc line has 4 slack)
        // Optimal: "aaa" / "bb cc" / "ddddd" → more balanced
        let result = wrap_optimal("aaa bb cc ddddd", 6);

        // Verify all lines fit
        for line in &result.lines {
            assert!(line.width() <= 6, "line '{line}' exceeds width 6");
        }

        // The greedy solution would put "aaa bb" on line 1.
        // The optimal solution should find a lower-cost arrangement.
        // Just verify it produces reasonable output.
        assert!(result.lines.len() >= 2);
    }

    #[test]
    fn kp_empty_text() {
        let result = wrap_optimal("", 80);
        assert_eq!(result.lines, vec![""]);
        assert_eq!(result.total_cost, 0);
    }

    #[test]
    fn kp_single_word() {
        let result = wrap_optimal("hello", 80);
        assert_eq!(result.lines, vec!["hello"]);
        assert_eq!(result.total_cost, 0); // last line, zero badness
    }

    #[test]
    fn kp_multiline_preserves_newlines() {
        let lines = wrap_text_optimal("hello world\nfoo bar baz", 10);
        // Each paragraph wrapped independently
        assert!(lines.len() >= 2);
        // First paragraph lines
        assert!(lines[0].width() <= 10);
    }

    #[test]
    fn kp_tokenize_basic() {
        let words = kp_tokenize("hello world foo");
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].content_width, 5);
        assert_eq!(words[0].space_width, 1);
        assert_eq!(words[1].content_width, 5);
        assert_eq!(words[1].space_width, 1);
        assert_eq!(words[2].content_width, 3);
        assert_eq!(words[2].space_width, 0);
    }

    #[test]
    fn kp_diagnostics_line_badness() {
        let result = wrap_optimal("short text here for testing the dp", 15);
        // Each line should have a badness value
        assert_eq!(result.line_badness.len(), result.lines.len());
        // Last line should have badness 0
        assert_eq!(
            *result.line_badness.last().unwrap(),
            0,
            "last line should have zero badness"
        );
    }

    #[test]
    fn kp_deterministic() {
        let text = "The quick brown fox jumps over the lazy dog near a riverbank";
        let r1 = wrap_optimal(text, 20);
        let r2 = wrap_optimal(text, 20);
        assert_eq!(r1.lines, r2.lines);
        assert_eq!(r1.total_cost, r2.total_cost);
    }

    // =========================================================================
    // Knuth-Plass Implementation + Pruning Tests (bd-4kq0.5.2)
    // =========================================================================

    #[test]
    fn unit_dp_matches_known() {
        // Known optimal break for "aaa bb cc ddddd" at width 6:
        // Greedy: "aaa bb" / "cc" / "ddddd" — line "cc" has 4 slack → badness = (4/6)^3*10000 = 2962
        // Optimal: "aaa" / "bb cc" / "ddddd" — line "aaa" has 3 slack → 1250, "bb cc" has 1 slack → 4
        // So optimal total < greedy total.
        let result = wrap_optimal("aaa bb cc ddddd", 6);

        // Verify all lines fit
        for line in &result.lines {
            assert!(line.width() <= 6, "line '{line}' exceeds width 6");
        }

        // The optimal should produce: "aaa" / "bb cc" / "ddddd"
        assert_eq!(
            result.lines.len(),
            3,
            "expected 3 lines, got {:?}",
            result.lines
        );
        assert_eq!(result.lines[0], "aaa");
        assert_eq!(result.lines[1], "bb cc");
        assert_eq!(result.lines[2], "ddddd");

        // Verify last line has zero badness
        assert_eq!(*result.line_badness.last().unwrap(), 0);
    }

    #[test]
    fn unit_dp_known_two_line() {
        // "hello world" at width 11 → fits in one line
        let r1 = wrap_optimal("hello world", 11);
        assert_eq!(r1.lines, vec!["hello world"]);
        assert_eq!(r1.total_cost, 0);

        // "hello world" at width 7 → must split
        let r2 = wrap_optimal("hello world", 7);
        assert_eq!(r2.lines.len(), 2);
        assert_eq!(r2.lines[0], "hello");
        assert_eq!(r2.lines[1], "world");
        // "hello" has 2 slack on width 7, badness = (2^3 * 10000) / 7^3 = 80000/343 = 233
        // "world" is last line, badness = 0
        assert!(
            r2.total_cost > 0 && r2.total_cost < 300,
            "expected cost ~233, got {}",
            r2.total_cost
        );
    }

    #[test]
    fn unit_dp_optimal_beats_greedy() {
        // Construct a case where greedy produces worse results
        // "aa bb cc dd ee" at width 6
        // Greedy: "aa bb" / "cc dd" / "ee" → slacks: 1, 1, 4 → badness ~0 + 0 + 0(last)
        // vs: "aa bb" / "cc dd" / "ee" — actually greedy might be optimal here
        //
        // Better example: "xx yy zzz aa bbb" at width 7
        // Greedy: "xx yy" / "zzz aa" / "bbb" → slacks: 2, 1, 4(last=0)
        // Optimal might produce: "xx yy" / "zzz aa" / "bbb" (same)
        //
        // Use a real suboptimal greedy case:
        // "a bb ccc dddd" width 6
        // Greedy: "a bb" (slack 2) / "ccc" (slack 3) / "dddd" (slack 2, last=0)
        //   → badness: (2/6)^3*10000=370 + (3/6)^3*10000=1250 = 1620
        // Optimal: "a" (slack 5) / "bb ccc" (slack 0) / "dddd" (last=0)
        //   → badness: (5/6)^3*10000=5787 + 0 = 5787
        // Or: "a bb" (slack 2) / "ccc" (slack 3) / "dddd" (last=0)
        //   → 370 + 1250 + 0 = 1620 — actually greedy is better here!
        //
        // The classic example is when greedy makes a very short line mid-paragraph.
        // "the quick brown fox" width 10
        let greedy = wrap_text("the quick brown fox", 10, WrapMode::Word);
        let optimal = wrap_optimal("the quick brown fox", 10);

        // Both should produce valid output
        for line in &greedy {
            assert!(line.width() <= 10);
        }
        for line in &optimal.lines {
            assert!(line.width() <= 10);
        }

        // Optimal cost should be <= greedy cost (by definition)
        // Compute greedy cost for comparison
        let mut greedy_cost: u64 = 0;
        for (i, line) in greedy.iter().enumerate() {
            let slack = 10i64 - line.width() as i64;
            let is_last = i == greedy.len() - 1;
            greedy_cost += knuth_plass_badness(slack, 10, is_last);
        }
        assert!(
            optimal.total_cost <= greedy_cost,
            "optimal ({}) should be <= greedy ({}) for 'the quick brown fox' at width 10",
            optimal.total_cost,
            greedy_cost
        );
    }

    #[test]
    fn perf_wrap_large() {
        use std::time::Instant;

        // Generate a large paragraph (~1000 words)
        let words: Vec<&str> = [
            "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "and", "then", "runs",
            "back", "to", "its", "den", "in",
        ]
        .to_vec();

        let mut paragraph = String::new();
        for i in 0..1000 {
            if i > 0 {
                paragraph.push(' ');
            }
            paragraph.push_str(words[i % words.len()]);
        }

        let iterations = 20;
        let start = Instant::now();
        for _ in 0..iterations {
            let result = wrap_optimal(&paragraph, 80);
            assert!(!result.lines.is_empty());
        }
        let elapsed = start.elapsed();

        eprintln!(
            "{{\"test\":\"perf_wrap_large\",\"words\":1000,\"width\":80,\"iterations\":{},\"total_ms\":{},\"per_iter_us\":{}}}",
            iterations,
            elapsed.as_millis(),
            elapsed.as_micros() / iterations as u128
        );

        // Budget: 1000 words × 20 iterations should complete in < 2s
        assert!(
            elapsed.as_secs() < 2,
            "Knuth-Plass DP too slow: {elapsed:?} for {iterations} iterations of 1000 words"
        );
    }

    #[test]
    fn kp_pruning_lookahead_bound() {
        // Verify MAX_LOOKAHEAD doesn't break correctness for normal text
        let text = "a b c d e f g h i j k l m n o p q r s t u v w x y z";
        let result = wrap_optimal(text, 10);
        for line in &result.lines {
            assert!(line.width() <= 10, "line '{line}' exceeds width");
        }
        // All 26 letters should appear in output
        let joined: String = result.lines.join(" ");
        for ch in 'a'..='z' {
            assert!(joined.contains(ch), "missing letter '{ch}' in output");
        }
    }

    #[test]
    fn kp_very_narrow_width() {
        // Width 1: every word must be on its own line (or force-broken)
        let result = wrap_optimal("ab cd ef", 2);
        assert_eq!(result.lines, vec!["ab", "cd", "ef"]);
    }

    #[test]
    fn kp_wide_width_single_line() {
        // Width much larger than text: single line, zero cost
        let result = wrap_optimal("hello world", 1000);
        assert_eq!(result.lines, vec!["hello world"]);
        assert_eq!(result.total_cost, 0);
    }

    // =========================================================================
    // Snapshot Wrap Quality (bd-4kq0.5.3)
    // =========================================================================

    /// FNV-1a hash for deterministic checksums of line break positions.
    fn fnv1a_lines(lines: &[String]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for (i, line) in lines.iter().enumerate() {
            for byte in (i as u32)
                .to_le_bytes()
                .iter()
                .chain(line.as_bytes().iter())
            {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        hash
    }

    #[test]
    fn snapshot_wrap_quality() {
        // Known paragraphs at multiple widths — verify deterministic and sensible output.
        let paragraphs = [
            "The quick brown fox jumps over the lazy dog near a riverbank while the sun sets behind the mountains in the distance",
            "To be or not to be that is the question whether tis nobler in the mind to suffer the slings and arrows of outrageous fortune",
            "aaa bb cc ddddd ee fff gg hhhh ii jjj kk llll mm nnn oo pppp qq rrr ss tttt",
        ];

        let widths = [20, 40, 60, 80];

        for paragraph in &paragraphs {
            for &width in &widths {
                let result = wrap_optimal(paragraph, width);

                // Determinism: same input → same output
                let result2 = wrap_optimal(paragraph, width);
                assert_eq!(
                    fnv1a_lines(&result.lines),
                    fnv1a_lines(&result2.lines),
                    "non-deterministic wrap at width {width}"
                );

                // All lines fit within width
                for line in &result.lines {
                    assert!(line.width() <= width, "line '{line}' exceeds width {width}");
                }

                // No empty lines (except if paragraph is empty)
                if !paragraph.is_empty() {
                    for line in &result.lines {
                        assert!(!line.is_empty(), "empty line in output at width {width}");
                    }
                }

                // All content preserved
                let original_words: Vec<&str> = paragraph.split_whitespace().collect();
                let result_words: Vec<&str> = result
                    .lines
                    .iter()
                    .flat_map(|l| l.split_whitespace())
                    .collect();
                assert_eq!(
                    original_words, result_words,
                    "content lost at width {width}"
                );

                // Last line has zero badness
                assert_eq!(
                    *result.line_badness.last().unwrap(),
                    0,
                    "last line should have zero badness at width {width}"
                );
            }
        }
    }

    // =========================================================================
    // Perf Wrap Bench with JSONL (bd-4kq0.5.3)
    // =========================================================================

    #[test]
    fn perf_wrap_bench() {
        use std::time::Instant;

        let sample_words = [
            "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "and", "then", "runs",
            "back", "to", "its", "den", "in", "forest", "while", "birds", "sing", "above", "trees",
            "near",
        ];

        let scenarios: &[(usize, usize, &str)] = &[
            (50, 40, "short_40"),
            (50, 80, "short_80"),
            (200, 40, "medium_40"),
            (200, 80, "medium_80"),
            (500, 40, "long_40"),
            (500, 80, "long_80"),
        ];

        for &(word_count, width, label) in scenarios {
            // Build paragraph
            let mut paragraph = String::new();
            for i in 0..word_count {
                if i > 0 {
                    paragraph.push(' ');
                }
                paragraph.push_str(sample_words[i % sample_words.len()]);
            }

            let iterations = 30u32;
            let mut times_us = Vec::with_capacity(iterations as usize);
            let mut last_lines = 0usize;
            let mut last_cost = 0u64;
            let mut last_checksum = 0u64;

            for _ in 0..iterations {
                let start = Instant::now();
                let result = wrap_optimal(&paragraph, width);
                let elapsed = start.elapsed();

                last_lines = result.lines.len();
                last_cost = result.total_cost;
                last_checksum = fnv1a_lines(&result.lines);
                times_us.push(elapsed.as_micros() as u64);
            }

            times_us.sort();
            let len = times_us.len();
            let p50 = times_us[len / 2];
            let p95 = times_us[((len as f64 * 0.95) as usize).min(len.saturating_sub(1))];

            // JSONL log
            eprintln!(
                "{{\"ts\":\"2026-02-03T00:00:00Z\",\"test\":\"perf_wrap_bench\",\"scenario\":\"{label}\",\"words\":{word_count},\"width\":{width},\"lines\":{last_lines},\"badness_total\":{last_cost},\"algorithm\":\"dp\",\"p50_us\":{p50},\"p95_us\":{p95},\"breaks_checksum\":\"0x{last_checksum:016x}\"}}"
            );

            // Determinism across iterations
            let verify = wrap_optimal(&paragraph, width);
            assert_eq!(
                fnv1a_lines(&verify.lines),
                last_checksum,
                "non-deterministic: {label}"
            );

            // Budget: 500 words at p95 should be < 5ms
            if word_count >= 500 && p95 > 5000 {
                eprintln!("WARN: {label} p95={p95}µs exceeds 5ms budget");
            }
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::TestWidth;
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn wrapped_lines_never_exceed_width(s in "[a-zA-Z ]{1,100}", width in 5usize..50) {
            let lines = wrap_text(&s, width, WrapMode::Char);
            for line in &lines {
                prop_assert!(line.width() <= width, "Line '{}' exceeds width {}", line, width);
            }
        }

        #[test]
        fn wrapped_content_preserved(s in "[a-zA-Z]{1,50}", width in 5usize..20) {
            let lines = wrap_text(&s, width, WrapMode::Char);
            let rejoined: String = lines.join("");
            // Content should be preserved (though whitespace may change)
            prop_assert_eq!(s.replace(" ", ""), rejoined.replace(" ", ""));
        }

        #[test]
        fn truncate_never_exceeds_width(s in "[a-zA-Z0-9]{1,50}", width in 5usize..30) {
            let result = truncate_with_ellipsis(&s, width, "...");
            prop_assert!(result.width() <= width, "Result '{}' exceeds width {}", result, width);
        }

        #[test]
        fn truncate_to_width_exact(s in "[a-zA-Z]{1,50}", width in 1usize..30) {
            let result = truncate_to_width(&s, width);
            prop_assert!(result.width() <= width);
            // If original was longer, result should be at max width or close
            if s.width() > width {
                // Should be close to width (may be less due to wide char at boundary)
                prop_assert!(result.width() >= width.saturating_sub(1) || s.width() <= width);
            }
        }

        #[test]
        fn wordchar_mode_respects_width(s in "[a-zA-Z ]{1,100}", width in 5usize..30) {
            let lines = wrap_text(&s, width, WrapMode::WordChar);
            for line in &lines {
                prop_assert!(line.width() <= width, "Line '{}' exceeds width {}", line, width);
            }
        }

        // =====================================================================
        // Knuth-Plass Property Tests (bd-4kq0.5.3)
        // =====================================================================

        /// Property: DP optimal cost is never worse than greedy cost.
        #[test]
        fn property_dp_vs_greedy(
            text in "[a-zA-Z]{1,6}( [a-zA-Z]{1,6}){2,20}",
            width in 8usize..40,
        ) {
            let greedy = wrap_text(&text, width, WrapMode::Word);
            let optimal = wrap_optimal(&text, width);

            // Compute greedy cost using same badness function
            let mut greedy_cost: u64 = 0;
            for (i, line) in greedy.iter().enumerate() {
                let lw = line.width();
                let slack = width as i64 - lw as i64;
                let is_last = i == greedy.len() - 1;
                if slack >= 0 {
                    greedy_cost = greedy_cost.saturating_add(
                        knuth_plass_badness(slack, width, is_last)
                    );
                } else {
                    greedy_cost = greedy_cost.saturating_add(PENALTY_FORCE_BREAK);
                }
            }

            prop_assert!(
                optimal.total_cost <= greedy_cost,
                "DP ({}) should be <= greedy ({}) for width={}: {:?} vs {:?}",
                optimal.total_cost, greedy_cost, width, optimal.lines, greedy
            );
        }

        /// Property: DP output lines never exceed width.
        #[test]
        fn property_dp_respects_width(
            text in "[a-zA-Z]{1,5}( [a-zA-Z]{1,5}){1,15}",
            width in 6usize..30,
        ) {
            let result = wrap_optimal(&text, width);
            for line in &result.lines {
                prop_assert!(
                    line.width() <= width,
                    "DP line '{}' (width {}) exceeds target {}",
                    line, line.width(), width
                );
            }
        }

        /// Property: DP preserves all non-whitespace content.
        #[test]
        fn property_dp_preserves_content(
            text in "[a-zA-Z]{1,5}( [a-zA-Z]{1,5}){1,10}",
            width in 8usize..30,
        ) {
            let result = wrap_optimal(&text, width);
            let original_words: Vec<&str> = text.split_whitespace().collect();
            let result_words: Vec<&str> = result.lines.iter()
                .flat_map(|l| l.split_whitespace())
                .collect();
            prop_assert_eq!(
                original_words, result_words,
                "DP should preserve all words"
            );
        }
    }

    // ======================================================================
    // ParagraphObjective tests (bd-2vr05.15.2.1)
    // ======================================================================

    #[test]
    fn fitness_class_from_ratio() {
        assert_eq!(FitnessClass::from_ratio(-0.8), FitnessClass::Tight);
        assert_eq!(FitnessClass::from_ratio(-0.5), FitnessClass::Normal);
        assert_eq!(FitnessClass::from_ratio(0.0), FitnessClass::Normal);
        assert_eq!(FitnessClass::from_ratio(0.49), FitnessClass::Normal);
        assert_eq!(FitnessClass::from_ratio(0.5), FitnessClass::Loose);
        assert_eq!(FitnessClass::from_ratio(0.99), FitnessClass::Loose);
        assert_eq!(FitnessClass::from_ratio(1.0), FitnessClass::VeryLoose);
        assert_eq!(FitnessClass::from_ratio(2.0), FitnessClass::VeryLoose);
    }

    #[test]
    fn fitness_class_incompatible() {
        assert!(!FitnessClass::Tight.incompatible(FitnessClass::Tight));
        assert!(!FitnessClass::Tight.incompatible(FitnessClass::Normal));
        assert!(FitnessClass::Tight.incompatible(FitnessClass::Loose));
        assert!(FitnessClass::Tight.incompatible(FitnessClass::VeryLoose));
        assert!(!FitnessClass::Normal.incompatible(FitnessClass::Loose));
        assert!(FitnessClass::Normal.incompatible(FitnessClass::VeryLoose));
    }

    #[test]
    fn objective_default_is_tex_standard() {
        let obj = ParagraphObjective::default();
        assert_eq!(obj.line_penalty, 10);
        assert_eq!(obj.fitness_demerit, 100);
        assert_eq!(obj.double_hyphen_demerit, 100);
        assert_eq!(obj.badness_scale, BADNESS_SCALE);
    }

    #[test]
    fn objective_terminal_preset() {
        let obj = ParagraphObjective::terminal();
        assert_eq!(obj.line_penalty, 20);
        assert_eq!(obj.min_adjustment_ratio, 0.0);
        assert!(obj.max_adjustment_ratio > 2.0);
    }

    #[test]
    fn badness_zero_slack_is_zero() {
        let obj = ParagraphObjective::default();
        assert_eq!(obj.badness(0, 80), Some(0));
    }

    #[test]
    fn badness_moderate_slack() {
        let obj = ParagraphObjective::default();
        // 10 cells slack on 80-wide line: ratio = 0.125
        // badness = (0.125)^3 * 10000 ≈ 19
        let b = obj.badness(10, 80).unwrap();
        assert!(b > 0 && b < 100, "badness = {b}");
    }

    #[test]
    fn badness_excessive_slack_infeasible() {
        let obj = ParagraphObjective::default();
        // ratio = 3.0, exceeds max_adjustment_ratio of 2.0
        assert!(obj.badness(240, 80).is_none());
    }

    #[test]
    fn badness_negative_slack_within_bounds() {
        let obj = ParagraphObjective::default();
        // -40 slack on 80-wide: ratio = -0.5, within min_adjustment_ratio of -1.0
        let b = obj.badness(-40, 80);
        assert!(b.is_some());
    }

    #[test]
    fn badness_negative_slack_beyond_bounds() {
        let obj = ParagraphObjective::default();
        // -100 slack on 80-wide: ratio = -1.25, exceeds min_adjustment_ratio of -1.0
        assert!(obj.badness(-100, 80).is_none());
    }

    #[test]
    fn badness_terminal_no_compression() {
        let obj = ParagraphObjective::terminal();
        // Terminal preset: min_adjustment_ratio = 0.0, no compression
        assert!(obj.badness(-1, 80).is_none());
    }

    #[test]
    fn demerits_space_break() {
        let obj = ParagraphObjective::default();
        let d = obj.demerits(10, 80, &BreakPenalty::SPACE).unwrap();
        // (line_penalty + badness)^2 + 0^2
        let badness = obj.badness(10, 80).unwrap();
        let expected = (obj.line_penalty + badness).pow(2);
        assert_eq!(d, expected);
    }

    #[test]
    fn demerits_hyphen_break() {
        let obj = ParagraphObjective::default();
        let d_space = obj.demerits(10, 80, &BreakPenalty::SPACE).unwrap();
        let d_hyphen = obj.demerits(10, 80, &BreakPenalty::HYPHEN).unwrap();
        // Hyphen break should cost more than space break
        assert!(d_hyphen > d_space);
    }

    #[test]
    fn demerits_forced_break() {
        let obj = ParagraphObjective::default();
        let d = obj.demerits(0, 80, &BreakPenalty::FORCED).unwrap();
        // Forced break: just (line_penalty + 0)^2
        assert_eq!(d, obj.line_penalty.pow(2));
    }

    #[test]
    fn demerits_infeasible_returns_none() {
        let obj = ParagraphObjective::default();
        // Slack beyond bounds
        assert!(obj.demerits(300, 80, &BreakPenalty::SPACE).is_none());
    }

    #[test]
    fn adjacency_fitness_incompatible() {
        let obj = ParagraphObjective::default();
        let d = obj.adjacency_demerits(FitnessClass::Tight, FitnessClass::Loose, false, false);
        assert_eq!(d, obj.fitness_demerit);
    }

    #[test]
    fn adjacency_fitness_compatible() {
        let obj = ParagraphObjective::default();
        let d = obj.adjacency_demerits(FitnessClass::Normal, FitnessClass::Loose, false, false);
        assert_eq!(d, 0);
    }

    #[test]
    fn adjacency_double_hyphen() {
        let obj = ParagraphObjective::default();
        let d = obj.adjacency_demerits(FitnessClass::Normal, FitnessClass::Normal, true, true);
        assert_eq!(d, obj.double_hyphen_demerit);
    }

    #[test]
    fn adjacency_double_hyphen_plus_fitness() {
        let obj = ParagraphObjective::default();
        let d = obj.adjacency_demerits(FitnessClass::Tight, FitnessClass::VeryLoose, true, true);
        assert_eq!(d, obj.fitness_demerit + obj.double_hyphen_demerit);
    }

    #[test]
    fn widow_penalty_short_last_line() {
        let obj = ParagraphObjective::default();
        assert_eq!(obj.widow_demerits(5), obj.widow_demerit);
        assert_eq!(obj.widow_demerits(14), obj.widow_demerit);
        assert_eq!(obj.widow_demerits(15), 0);
        assert_eq!(obj.widow_demerits(80), 0);
    }

    #[test]
    fn orphan_penalty_short_first_line() {
        let obj = ParagraphObjective::default();
        assert_eq!(obj.orphan_demerits(10), obj.orphan_demerit);
        assert_eq!(obj.orphan_demerits(19), obj.orphan_demerit);
        assert_eq!(obj.orphan_demerits(20), 0);
        assert_eq!(obj.orphan_demerits(80), 0);
    }

    #[test]
    fn adjustment_ratio_computation() {
        let obj = ParagraphObjective::default();
        let r = obj.adjustment_ratio(10, 80);
        assert!((r - 0.125).abs() < 1e-10);
    }

    #[test]
    fn adjustment_ratio_zero_width() {
        let obj = ParagraphObjective::default();
        assert_eq!(obj.adjustment_ratio(5, 0), 0.0);
    }

    #[test]
    fn badness_zero_width_zero_slack() {
        let obj = ParagraphObjective::default();
        assert_eq!(obj.badness(0, 0), Some(0));
    }

    #[test]
    fn badness_zero_width_nonzero_slack() {
        let obj = ParagraphObjective::default();
        assert!(obj.badness(5, 0).is_none());
    }
}

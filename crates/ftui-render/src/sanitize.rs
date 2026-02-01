#![forbid(unsafe_code)]

//! Sanitization for untrusted terminal output.
//!
//! This module implements the sanitize-by-default policy (ADR-006) to protect
//! against terminal escape injection attacks. Any untrusted bytes displayed
//! as logs, tool output, or LLM streams must be treated as **data**, not
//! executed as terminal control sequences.
//!
//! # Threat Model
//!
//! Malicious content in logs could:
//! 1. Manipulate cursor position (break inline mode)
//! 2. Change terminal colors/modes persistently
//! 3. Hide text or show fake prompts (social engineering)
//! 4. Trigger terminal queries that exfiltrate data
//! 5. Set window title to misleading values
//!
//! # Performance
//!
//! - **Fast path (95%+ of cases)**: Scan for ESC byte using memchr.
//!   If no ESC found, content is safe - return borrowed slice.
//!   Zero allocation in common case, < 100ns for typical log line.
//!
//! - **Slow path**: Allocate output buffer, strip control sequences,
//!   return owned String. Linear in input size.
//!
//! # Usage
//!
//! ```
//! use ftui_render::sanitize::sanitize;
//! use std::borrow::Cow;
//!
//! // Fast path - no escapes, returns borrowed
//! let safe = sanitize("Normal log message");
//! assert!(matches!(safe, Cow::Borrowed(_)));
//!
//! // Slow path - escapes stripped, returns owned
//! let malicious = sanitize("Evil \x1b[31mred\x1b[0m text");
//! assert!(matches!(malicious, Cow::Owned(_)));
//! assert_eq!(malicious.as_ref(), "Evil red text");
//! ```

use std::borrow::Cow;

use memchr::memchr;

/// Sanitize untrusted text for safe terminal display.
///
/// # Fast Path
/// If no ESC (0x1B) found and no forbidden C0 controls, returns borrowed input
/// with zero allocation.
///
/// # Slow Path
/// Strips all escape sequences and forbidden C0 controls, returns owned String.
///
/// # What Gets Stripped
/// - ESC (0x1B) and all following CSI/OSC/DCS/APC sequences
/// - C0 controls except: TAB (0x09), LF (0x0A), CR (0x0D)
/// - C1 controls (U+0080..U+009F) — these are the 8-bit equivalents of
///   ESC-prefixed sequences and some terminals honor them
/// - DEL (0x7F)
///
/// # What Gets Preserved
/// - TAB, LF, CR (allowed control characters)
/// - All printable ASCII (0x20-0x7E)
/// - All valid UTF-8 sequences above U+009F
#[inline]
pub fn sanitize(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();

    // Fast path: check for any ESC byte, forbidden C0 controls, DEL, or C1 controls.
    // C1 controls (U+0080..U+009F) are encoded in UTF-8 as \xC2\x80..\xC2\x9F.
    if memchr(0x1B, bytes).is_none()
        && memchr(0x7F, bytes).is_none()
        && !has_forbidden_c0(bytes)
        && !has_c1_controls(bytes)
    {
        return Cow::Borrowed(input);
    }

    // Slow path: strip escape sequences
    Cow::Owned(sanitize_slow(input))
}

/// Check if any forbidden C0 control characters are present.
///
/// Forbidden: 0x00-0x08, 0x0B-0x0C, 0x0E-0x1A, 0x1C-0x1F
/// Allowed: TAB (0x09), LF (0x0A), CR (0x0D)
#[inline]
fn has_forbidden_c0(bytes: &[u8]) -> bool {
    bytes.iter().any(|&b| is_forbidden_c0(b))
}

/// Check if a single byte is a forbidden C0 control.
#[inline]
const fn is_forbidden_c0(b: u8) -> bool {
    matches!(
        b,
        0x00..=0x08 | 0x0B..=0x0C | 0x0E..=0x1A | 0x1C..=0x1F
    )
}

/// Check if any C1 control characters (U+0080..U+009F) are present.
///
/// In UTF-8, these are encoded as the two-byte sequence \xC2\x80..\xC2\x9F.
/// C1 controls include CSI (U+009B), OSC (U+009D), DCS (U+0090), APC (U+009F),
/// etc. — some terminals honor these as equivalent to their ESC-prefixed forms.
#[inline]
fn has_c1_controls(bytes: &[u8]) -> bool {
    bytes
        .windows(2)
        .any(|w| w[0] == 0xC2 && (0x80..=0x9F).contains(&w[1]))
}

/// Slow path: strip escape sequences and forbidden controls.
fn sanitize_slow(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        match b {
            // ESC - start of escape sequence
            0x1B => {
                i = skip_escape_sequence(bytes, i);
            }
            // Allowed C0 controls: TAB, LF, CR
            0x09 | 0x0A | 0x0D => {
                output.push(b as char);
                i += 1;
            }
            // Forbidden C0 controls - skip
            0x00..=0x08 | 0x0B..=0x0C | 0x0E..=0x1A | 0x1C..=0x1F => {
                i += 1;
            }
            // DEL - skip
            0x7F => {
                i += 1;
            }
            // Printable ASCII
            0x20..=0x7E => {
                output.push(b as char);
                i += 1;
            }
            // Start of UTF-8 sequence (high bit set)
            0x80..=0xFF => {
                if let Some((c, len)) = decode_utf8_char(&bytes[i..]) {
                    // Skip C1 controls (U+0080..U+009F) — these are the 8-bit
                    // equivalents of ESC-prefixed sequences (CSI, OSC, DCS, etc.)
                    if !('\u{0080}'..='\u{009F}').contains(&c) {
                        output.push(c);
                    }
                    i += len;
                } else {
                    // Invalid UTF-8, skip byte
                    i += 1;
                }
            }
        }
    }

    output
}

/// Skip over escape sequence, returning index after it.
///
/// Handles:
/// - CSI: ESC [ ... final_byte (0x40-0x7E)
/// - OSC: ESC ] ... (BEL or ST)
/// - DCS: ESC P ... ST
/// - PM: ESC ^ ... ST
/// - APC: ESC _ ... ST
/// - Single-char escapes: ESC char
fn skip_escape_sequence(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1; // Skip ESC
    if i >= bytes.len() {
        return i;
    }

    match bytes[i] {
        // CSI sequence: ESC [ params... final_byte
        b'[' => {
            i += 1;
            // Consume parameter bytes (0x30-0x3F) and intermediate bytes (0x20-0x2F)
            // Stop at final byte (0x40-0x7E)
            while i < bytes.len() {
                let b = bytes[i];
                if (0x40..=0x7E).contains(&b) {
                    return i + 1;
                }
                // Valid parameter/intermediate bytes are 0x20-0x3F
                if !(0x20..=0x3F).contains(&b) {
                    // Invalid char in CSI (e.g. newline, control char, or high byte)
                    // Abort sequence processing to prevent eating valid text
                    return i;
                }
                i += 1;
            }
        }
        // OSC sequence: ESC ] ... (BEL or ST)
        b']' => {
            i += 1;
            while i < bytes.len() {
                let b = bytes[i];
                // BEL terminates OSC
                if b == 0x07 {
                    return i + 1;
                }
                // ST (ESC \) terminates OSC
                if b == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    return i + 2;
                }
                // Abort on other C0 controls (e.g. newline) to prevent swallowing logs
                // 0x1B is allowed as part of ST check above, but lone ESC is tricky.
                // We'll allow 0x1B here to let the loop check for ST next iteration,
                // but strictly block other C0 (0x00-0x1F except 0x1B).
                if b < 0x20 && b != 0x1B {
                    return i;
                }
                i += 1;
            }
        }
        // DCS/PM/APC: ESC P/^/_ ... ST
        b'P' | b'^' | b'_' => {
            i += 1;
            while i < bytes.len() {
                let b = bytes[i];
                // ST (ESC \) terminates
                if b == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    return i + 2;
                }
                // Abort on C0 controls
                if b < 0x20 && b != 0x1B {
                    return i;
                }
                i += 1;
            }
        }
        // Single-char escape sequences (ESC followed by 0x20-0x7E)
        0x20..=0x7E => {
            return i + 1;
        }
        // Unknown or invalid - just skip the ESC
        _ => {}
    }

    i
}

/// Decode a single UTF-8 character from byte slice.
///
/// Returns the character and number of bytes consumed, or None if invalid.
fn decode_utf8_char(bytes: &[u8]) -> Option<(char, usize)> {
    if bytes.is_empty() {
        return None;
    }

    let first = bytes[0];
    let (expected_len, mut codepoint) = match first {
        0x00..=0x7F => return Some((first as char, 1)),
        0xC0..=0xDF => (2, (first & 0x1F) as u32),
        0xE0..=0xEF => (3, (first & 0x0F) as u32),
        0xF0..=0xF7 => (4, (first & 0x07) as u32),
        _ => return None, // Invalid lead byte
    };

    if bytes.len() < expected_len {
        return None;
    }

    // Process continuation bytes
    for &b in bytes.iter().take(expected_len).skip(1) {
        if (b & 0xC0) != 0x80 {
            return None; // Invalid continuation byte
        }
        codepoint = (codepoint << 6) | (b & 0x3F) as u32;
    }

    // Validate codepoint
    char::from_u32(codepoint).map(|c| (c, expected_len))
}

/// Text with trust level annotation.
///
/// Use this to explicitly mark whether text has been sanitized or comes
/// from a trusted source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text<'a> {
    /// Sanitized text (escape sequences stripped).
    Sanitized(Cow<'a, str>),

    /// Trusted text (may contain ANSI sequences).
    /// Only use with content from trusted sources.
    Trusted(Cow<'a, str>),
}

impl<'a> Text<'a> {
    /// Create sanitized text from an untrusted source.
    #[inline]
    pub fn sanitized(s: &'a str) -> Self {
        Text::Sanitized(sanitize(s))
    }

    /// Create from a trusted source (ANSI sequences allowed).
    ///
    /// # Safety
    /// Only use with content from trusted sources. Untrusted content
    /// can corrupt terminal state or deceive users.
    #[inline]
    pub fn trusted(s: &'a str) -> Self {
        Text::Trusted(Cow::Borrowed(s))
    }

    /// Create owned sanitized text.
    #[inline]
    pub fn sanitized_owned(s: String) -> Self {
        Text::Sanitized(Cow::Owned(sanitize_slow(&s)))
    }

    /// Create owned trusted text.
    #[inline]
    pub fn trusted_owned(s: String) -> Self {
        Text::Trusted(Cow::Owned(s))
    }

    /// Get the inner string slice.
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            Text::Sanitized(cow) => cow.as_ref(),
            Text::Trusted(cow) => cow.as_ref(),
        }
    }

    /// Check if this text is sanitized.
    #[inline]
    pub fn is_sanitized(&self) -> bool {
        matches!(self, Text::Sanitized(_))
    }

    /// Check if this text is trusted.
    #[inline]
    pub fn is_trusted(&self) -> bool {
        matches!(self, Text::Trusted(_))
    }

    /// Convert to owned version.
    pub fn into_owned(self) -> Text<'static> {
        match self {
            Text::Sanitized(cow) => Text::Sanitized(Cow::Owned(cow.into_owned())),
            Text::Trusted(cow) => Text::Trusted(Cow::Owned(cow.into_owned())),
        }
    }
}

impl AsRef<str> for Text<'_> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============== Fast Path Tests ==============

    #[test]
    fn fast_path_no_escape() {
        let input = "Normal log message without escapes";
        let result = sanitize(input);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result.as_ref(), input);
    }

    #[test]
    fn fast_path_with_allowed_controls() {
        let input = "Line1\nLine2\tTabbed\rCarriage";
        let result = sanitize(input);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result.as_ref(), input);
    }

    #[test]
    fn fast_path_unicode() {
        let input = "Hello \u{4e16}\u{754c} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let result = sanitize(input);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result.as_ref(), input);
    }

    #[test]
    fn fast_path_empty() {
        let input = "";
        let result = sanitize(input);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn fast_path_printable_ascii() {
        let input = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()";
        let result = sanitize(input);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result.as_ref(), input);
    }

    // ============== Slow Path: CSI Sequences ==============

    #[test]
    fn slow_path_strips_sgr_color() {
        let input = "Hello \x1b[31mred\x1b[0m world";
        let result = sanitize(input);
        assert!(matches!(result, Cow::Owned(_)));
        assert_eq!(result.as_ref(), "Hello red world");
    }

    #[test]
    fn slow_path_strips_cursor_movement() {
        let input = "Before\x1b[2;5HAfter";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "BeforeAfter");
    }

    #[test]
    fn slow_path_strips_erase() {
        let input = "Text\x1b[2JCleared";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "TextCleared");
    }

    #[test]
    fn slow_path_strips_multiple_sequences() {
        let input = "\x1b[1mBold\x1b[0m \x1b[4mUnderline\x1b[24m \x1b[38;5;196mColor\x1b[0m";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "Bold Underline Color");
    }

    // ============== Slow Path: OSC Sequences ==============

    #[test]
    fn slow_path_strips_osc_title_bel() {
        // OSC 0: set title, terminated by BEL
        let input = "Text\x1b]0;Evil Title\x07More";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "TextMore");
    }

    #[test]
    fn slow_path_strips_osc_title_st() {
        // OSC 0: set title, terminated by ST
        let input = "Text\x1b]0;Evil Title\x1b\\More";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "TextMore");
    }

    #[test]
    fn slow_path_strips_osc8_hyperlink() {
        // OSC 8: hyperlink
        let input = "Click \x1b]8;;https://evil.com\x07here\x1b]8;;\x07 please";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "Click here please");
    }

    // ============== Slow Path: DCS/PM/APC ==============

    #[test]
    fn slow_path_strips_dcs() {
        let input = "Before\x1bPdevice control string\x1b\\After";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "BeforeAfter");
    }

    #[test]
    fn slow_path_strips_apc() {
        let input = "Before\x1b_application program command\x1b\\After";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "BeforeAfter");
    }

    #[test]
    fn slow_path_strips_pm() {
        let input = "Before\x1b^privacy message\x1b\\After";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "BeforeAfter");
    }

    #[test]
    fn slow_path_strips_osc52_clipboard() {
        let input = "Before\x1b]52;c;SGVsbG8=\x07After";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "BeforeAfter");
    }

    #[test]
    fn slow_path_strips_osc52_clipboard_st() {
        let input = "Before\x1b]52;c;SGVsbG8=\x1b\\After";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "BeforeAfter");
    }

    #[test]
    fn slow_path_strips_private_modes() {
        let input = "A\x1b[?1049hB\x1b[?1000hC\x1b[?2004hD";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "ABCD");
    }

    // ============== Slow Path: C0 Controls ==============

    #[test]
    fn slow_path_strips_nul() {
        let input = "Hello\x00World";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "HelloWorld");
    }

    #[test]
    fn slow_path_strips_bel() {
        // BEL (0x07) outside of OSC should be stripped
        let input = "Hello\x07World";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "HelloWorld");
    }

    #[test]
    fn slow_path_strips_backspace() {
        let input = "Hello\x08World";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "HelloWorld");
    }

    #[test]
    fn slow_path_strips_form_feed() {
        let input = "Hello\x0CWorld";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "HelloWorld");
    }

    #[test]
    fn slow_path_strips_vertical_tab() {
        let input = "Hello\x0BWorld";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "HelloWorld");
    }

    #[test]
    fn slow_path_strips_del() {
        let input = "Hello\x7FWorld";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "HelloWorld");
    }

    #[test]
    fn slow_path_preserves_tab_lf_cr() {
        let input = "Line1\nLine2\tTabbed\rReturn";
        // This should trigger slow path due to needing to scan
        // but preserve tab/lf/cr
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "Line1\nLine2\tTabbed\rReturn");
    }

    // ============== Edge Cases ==============

    #[test]
    fn handles_truncated_csi() {
        let input = "Hello\x1b[";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        assert_eq!(result.as_ref(), "Hello");
    }

    #[test]
    fn handles_truncated_dcs() {
        let input = "Hello\x1bP1;2;3";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        assert_eq!(result.as_ref(), "Hello");
    }

    #[test]
    fn handles_truncated_apc() {
        let input = "Hello\x1b_test";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        assert_eq!(result.as_ref(), "Hello");
    }

    #[test]
    fn handles_truncated_pm() {
        let input = "Hello\x1b^secret";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        assert_eq!(result.as_ref(), "Hello");
    }

    #[test]
    fn handles_truncated_osc() {
        let input = "Hello\x1b]0;Title";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        assert_eq!(result.as_ref(), "Hello");
    }

    #[test]
    fn handles_esc_at_end() {
        let input = "Hello\x1b";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "Hello");
    }

    #[test]
    fn handles_lone_esc() {
        let input = "\x1b";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn handles_single_char_escape() {
        // ESC 7 (save cursor) and ESC 8 (restore cursor)
        let input = "Before\x1b7Middle\x1b8After";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "BeforeMiddleAfter");
    }

    #[test]
    fn handles_unknown_escape() {
        // ESC followed by a byte that's not a valid escape introducer
        // Using a valid printable byte that's not a known escape char
        let input = "Before\x1b!After";
        let result = sanitize(input);
        // Single-char escape: ESC ! gets stripped
        assert_eq!(result.as_ref(), "BeforeAfter");
    }

    // ============== Unicode Tests ==============

    #[test]
    fn preserves_unicode_characters() {
        let input = "\u{4e16}\u{754c}"; // Chinese characters
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "\u{4e16}\u{754c}");
    }

    #[test]
    fn preserves_emoji() {
        let input = "\u{1f600}\u{1f389}\u{1f680}"; // Emoji
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "\u{1f600}\u{1f389}\u{1f680}");
    }

    #[test]
    fn preserves_combining_characters() {
        // e with combining acute accent
        let input = "e\u{0301}";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "e\u{0301}");
    }

    #[test]
    fn mixed_unicode_and_escapes() {
        let input = "\u{4e16}\x1b[31m\u{754c}\x1b[0m";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "\u{4e16}\u{754c}");
    }

    // ============== Text Type Tests ==============

    #[test]
    fn text_sanitized() {
        let text = Text::sanitized("Hello \x1b[31mWorld\x1b[0m");
        assert!(text.is_sanitized());
        assert!(!text.is_trusted());
        assert_eq!(text.as_str(), "Hello World");
    }

    #[test]
    fn text_trusted() {
        let text = Text::trusted("Hello \x1b[31mWorld\x1b[0m");
        assert!(!text.is_sanitized());
        assert!(text.is_trusted());
        assert_eq!(text.as_str(), "Hello \x1b[31mWorld\x1b[0m");
    }

    #[test]
    fn text_into_owned() {
        let text = Text::sanitized("Hello");
        let owned = text.into_owned();
        assert!(owned.is_sanitized());
        assert_eq!(owned.as_str(), "Hello");
    }

    #[test]
    fn text_display() {
        let text = Text::sanitized("Hello");
        assert_eq!(format!("{text}"), "Hello");
    }

    // ============== Property Tests (basic) ==============

    #[test]
    fn output_never_contains_esc() {
        let inputs = [
            "Normal text",
            "\x1b[31mRed\x1b[0m",
            "\x1b]0;Title\x07",
            "\x1bPDCS\x1b\\",
            "Mixed\x1b[1m\x1b]8;;url\x07text\x1b]8;;\x07\x1b[0m",
            "",
            "\x1b",
            "\x1b[",
            "\x1b]",
        ];

        for input in inputs {
            let result = sanitize(input);
            assert!(
                !result.contains('\x1b'),
                "Output contains ESC for input: {input:?}"
            );
        }
    }

    #[test]
    fn output_never_contains_forbidden_c0() {
        let inputs = [
            "\x00\x01\x02\x03\x04\x05\x06\x07",
            "\x08\x0B\x0C\x0E\x0F",
            "\x10\x11\x12\x13\x14\x15\x16\x17",
            "\x18\x19\x1A\x1C\x1D\x1E\x1F",
            "Mixed\x00text\x07with\x0Ccontrols",
        ];

        for input in inputs {
            let result = sanitize(input);
            for b in result.as_bytes() {
                if is_forbidden_c0(*b) {
                    panic!("Output contains forbidden C0 0x{b:02X} for input: {input:?}");
                }
            }
        }
    }

    #[test]
    fn allowed_controls_preserved_in_output() {
        let input = "Tab\there\nNewline\rCarriage";
        let result = sanitize(input);
        assert!(result.contains('\t'));
        assert!(result.contains('\n'));
        assert!(result.contains('\r'));
    }

    // ============== Decode UTF-8 Tests ==============

    #[test]
    fn decode_ascii() {
        let bytes = b"A";
        let result = decode_utf8_char(bytes);
        assert_eq!(result, Some(('A', 1)));
    }

    #[test]
    fn decode_two_byte() {
        let bytes = "\u{00E9}".as_bytes(); // é
        let result = decode_utf8_char(bytes);
        assert_eq!(result, Some(('\u{00E9}', 2)));
    }

    #[test]
    fn decode_three_byte() {
        let bytes = "\u{4e16}".as_bytes(); // Chinese
        let result = decode_utf8_char(bytes);
        assert_eq!(result, Some(('\u{4e16}', 3)));
    }

    #[test]
    fn decode_four_byte() {
        let bytes = "\u{1f600}".as_bytes(); // Emoji
        let result = decode_utf8_char(bytes);
        assert_eq!(result, Some(('\u{1f600}', 4)));
    }

    #[test]
    fn decode_invalid_lead() {
        let bytes = &[0xFF];
        let result = decode_utf8_char(bytes);
        assert_eq!(result, None);
    }

    #[test]
    fn decode_truncated() {
        let bytes = &[0xC2]; // Incomplete 2-byte sequence
        let result = decode_utf8_char(bytes);
        assert_eq!(result, None);
    }

    #[test]
    fn decode_invalid_continuation() {
        let bytes = &[0xC2, 0x00]; // Invalid continuation byte
        let result = decode_utf8_char(bytes);
        assert_eq!(result, None);
    }

    // ================================================================
    // Adversarial Security Tests (bd-397)
    //
    // Tests below exercise the specific threat model from ADR-006:
    //   1. Log injection / cursor corruption
    //   2. Title injection (OSC 0)
    //   3. Clipboard hijacking (OSC 52)
    //   4. Terminal mode hijacking
    //   5. Data exfiltration via terminal queries
    //   6. Social engineering via fake prompts
    //   7. C1 control code injection
    //   8. Sequence terminator confusion
    //   9. DoS via large / deeply nested payloads
    //  10. Combined / chained attacks
    // ================================================================

    // ---- 1. Log injection / cursor corruption ----

    #[test]
    fn adversarial_clear_screen() {
        let input = "\x1b[2J";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_home_cursor() {
        let input = "visible\x1b[Hhidden";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "visiblehidden");
    }

    #[test]
    fn adversarial_cursor_absolute_position() {
        let input = "ok\x1b[999;999Hmalicious";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "okmalicious");
    }

    #[test]
    fn adversarial_scroll_up() {
        let input = "text\x1b[5Smore";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "textmore");
    }

    #[test]
    fn adversarial_scroll_down() {
        let input = "text\x1b[5Tmore";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "textmore");
    }

    #[test]
    fn adversarial_erase_line() {
        let input = "secret\x1b[2Koverwrite";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "secretoverwrite");
    }

    #[test]
    fn adversarial_insert_delete_lines() {
        let input = "text\x1b[10Linserted\x1b[5Mdeleted";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "textinserteddeleted");
    }

    // ---- 2. Title injection (OSC 0, 1, 2) ----

    #[test]
    fn adversarial_osc0_title_injection() {
        let input = "\x1b]0;PWNED - Enter Password\x07";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
        assert!(!result.contains('\x1b'));
        assert!(!result.contains('\x07'));
    }

    #[test]
    fn adversarial_osc1_icon_title() {
        let input = "\x1b]1;evil-icon\x07";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_osc2_window_title() {
        let input = "\x1b]2;sudo password required\x1b\\";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    // ---- 3. Clipboard hijacking (OSC 52) ----

    #[test]
    fn adversarial_osc52_clipboard_set_bel() {
        // Set clipboard to "rm -rf /" encoded in base64
        let input = "safe\x1b]52;c;cm0gLXJmIC8=\x07text";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "safetext");
    }

    #[test]
    fn adversarial_osc52_clipboard_set_st() {
        let input = "safe\x1b]52;c;cm0gLXJmIC8=\x1b\\text";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "safetext");
    }

    #[test]
    fn adversarial_osc52_clipboard_query() {
        // Query clipboard (could exfiltrate data)
        let input = "\x1b]52;c;?\x07";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    // ---- 4. Terminal mode hijacking ----

    #[test]
    fn adversarial_alt_screen_enable() {
        let input = "\x1b[?1049h";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_alt_screen_disable() {
        let input = "\x1b[?1049l";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_mouse_enable() {
        let input = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_bracketed_paste_enable() {
        let input = "\x1b[?2004h";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_focus_events_enable() {
        let input = "\x1b[?1004h";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_raw_mode_sequence() {
        // Attempt to set raw mode
        let input = "\x1b[?7727h";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_cursor_hide_show() {
        let input = "\x1b[?25l\x1b[?25h";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    // ---- 5. Data exfiltration via terminal queries ----

    #[test]
    fn adversarial_device_attributes_query_da1() {
        let input = "\x1b[c";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_device_attributes_query_da2() {
        let input = "\x1b[>c";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_device_status_report() {
        let input = "\x1b[6n";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_osc_color_query() {
        // Query background color (OSC 11)
        let input = "\x1b]11;?\x07";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_decrpm_query() {
        let input = "\x1b[?2026$p";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "");
    }

    // ---- 6. Social engineering via fake prompts ----

    #[test]
    fn adversarial_fake_shell_prompt() {
        // Try to move cursor to create a fake prompt
        let input = "\x1b[999;1H\x1b[2K$ sudo rm -rf /\x1b[A";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        // Only text content should survive
        assert_eq!(result.as_ref(), "$ sudo rm -rf /");
    }

    #[test]
    fn adversarial_fake_password_prompt() {
        // Combine title set + cursor move + fake prompt
        let input = "\x1b]0;Terminal\x07\x1b[2J\x1b[HPassword: ";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "Password: ");
    }

    #[test]
    fn adversarial_overwrite_existing_content() {
        // Try to use backspaces + CR to overwrite existing output
        let input = "safe output\r\x1b[2Kmalicious replacement";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "safe output\rmalicious replacement");
    }

    // ---- 7. C1 control codes (single-byte, 0x80-0x9F) ----
    //
    // In ISO-8859-1, 0x80-0x9F are C1 control characters.
    // In UTF-8, these byte values are continuation bytes and should
    // be handled by the UTF-8 decoder (invalid as leading bytes).
    // The sanitizer should not let them through as control codes.

    #[test]
    fn adversarial_c1_single_byte_csi() {
        // U+009B is the C1 equivalent of ESC [ (CSI)
        // Some terminals treat this as a CSI introducer, so it MUST be stripped.
        let input = "text\u{009B}31mmalicious";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        assert!(
            !result.contains('\u{009B}'),
            "C1 CSI (U+009B) must be stripped"
        );
    }

    #[test]
    fn adversarial_c1_osc_byte() {
        // U+009D is the C1 equivalent of ESC ] (OSC)
        let input = "text\u{009D}0;Evil Title\x07malicious";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        assert!(
            !result.contains('\u{009D}'),
            "C1 OSC (U+009D) must be stripped"
        );
    }

    #[test]
    fn adversarial_c1_dcs_byte() {
        // U+0090 (DCS)
        let input = "A\u{0090}device control\x1b\\B";
        let result = sanitize(input);
        assert!(!result.contains('\u{0090}'));
    }

    #[test]
    fn adversarial_c1_apc_byte() {
        // U+009F (APC)
        let input = "A\u{009F}app command\x1b\\B";
        let result = sanitize(input);
        assert!(!result.contains('\u{009F}'));
    }

    #[test]
    fn adversarial_c1_pm_byte() {
        // U+009E (PM)
        let input = "A\u{009E}private msg\x1b\\B";
        let result = sanitize(input);
        assert!(!result.contains('\u{009E}'));
    }

    #[test]
    fn adversarial_c1_st_byte() {
        // U+009C (ST = String Terminator)
        let input = "A\u{009C}B";
        let result = sanitize(input);
        assert!(!result.contains('\u{009C}'));
    }

    #[test]
    fn adversarial_all_c1_controls_stripped() {
        // Every C1 control (U+0080..U+009F) must be stripped
        for cp in 0x0080..=0x009F_u32 {
            let c = char::from_u32(cp).unwrap();
            let input = format!("A{c}B");
            let result = sanitize(&input);
            assert!(
                !result
                    .chars()
                    .any(|ch| ('\u{0080}'..='\u{009F}').contains(&ch)),
                "C1 control U+{cp:04X} passed through sanitizer"
            );
            // The surrounding text must survive
            assert!(result.contains('A'), "Text before C1 U+{cp:04X} lost");
            assert!(result.contains('B'), "Text after C1 U+{cp:04X} lost");
        }
    }

    #[test]
    fn adversarial_c1_fast_path_triggers_slow_path() {
        // C1 controls must trigger the slow path even without ESC/DEL/C0
        let input = "clean\u{0085}text"; // U+0085 = NEL (Next Line)
        let result = sanitize(input);
        assert!(
            matches!(result, Cow::Owned(_)),
            "C1 should trigger slow path"
        );
        assert!(!result.contains('\u{0085}'));
        assert_eq!(result.as_ref(), "cleantext");
    }

    // ---- 8. Sequence terminator confusion ----

    #[test]
    fn adversarial_nested_osc_in_osc() {
        // OSC within OSC - inner should not terminate outer
        let input = "safe\x1b]8;;\x1b]0;evil\x07https://ok.com\x07text";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        assert!(!result.contains('\x07'));
    }

    #[test]
    fn adversarial_st_inside_dcs() {
        // Ensure ST properly terminates DCS
        let input = "A\x1bPsome\x1bdata\x1b\\B";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "AB");
    }

    #[test]
    fn adversarial_bel_vs_st_terminator() {
        // OSC terminated by BEL, then more text, then ST
        let input = "A\x1b]0;title\x07B\x1b\\C";
        let result = sanitize(input);
        // BEL terminates the OSC; "B" is text; ESC \ is a single-char escape
        assert!(!result.contains('\x1b'));
        assert!(!result.contains('\x07'));
    }

    #[test]
    fn adversarial_csi_without_final_byte() {
        // CSI with only parameter bytes, never reaching a final byte
        let input = "A\x1b[0;0;0;0;0;0;0;0;0;0B";
        let result = sanitize(input);
        // The 'B' (0x42) IS a valid CSI final byte, so entire CSI is consumed
        assert_eq!(result.as_ref(), "A");
    }

    #[test]
    fn adversarial_csi_many_params_then_final() {
        // CSI with many parameters followed by a valid final byte
        let input = "X\x1b[1;2;3;4;5;6;7;8;9;10mY";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "XY");
    }

    // ---- 9. DoS-style payloads ----

    #[test]
    fn adversarial_very_long_csi_params() {
        // Very long CSI parameter string
        let params: String = std::iter::repeat_n("0;", 10_000).collect();
        let input = format!("start\x1b[{params}mend");
        let result = sanitize(&input);
        assert_eq!(result.as_ref(), "startend");
    }

    #[test]
    fn adversarial_many_short_sequences() {
        // Many small CSI sequences back to back
        let input: String = (0..10_000).map(|_| "\x1b[0m").collect();
        let input = format!("start{input}end");
        let result = sanitize(&input);
        assert_eq!(result.as_ref(), "startend");
    }

    #[test]
    fn adversarial_very_long_osc_content() {
        // Very long OSC payload (could be used to cause memory issues)
        let payload: String = std::iter::repeat_n('A', 100_000).collect();
        let input = format!("text\x1b]0;{payload}\x07more");
        let result = sanitize(&input);
        assert_eq!(result.as_ref(), "textmore");
    }

    #[test]
    fn adversarial_very_long_dcs_content() {
        let payload: String = std::iter::repeat_n('X', 100_000).collect();
        let input = format!("text\x1bP{payload}\x1b\\more");
        let result = sanitize(&input);
        assert_eq!(result.as_ref(), "textmore");
    }

    #[test]
    fn adversarial_only_escape_bytes() {
        // Input composed entirely of ESC bytes
        let input: String = std::iter::repeat_n('\x1b', 1000).collect();
        let result = sanitize(&input);
        assert_eq!(result.as_ref(), "");
    }

    #[test]
    fn adversarial_alternating_esc_and_text() {
        // ESC-char-ESC-char pattern
        let input: String = (0..1000)
            .map(|i| if i % 2 == 0 { "\x1b[m" } else { "a" })
            .collect();
        let result = sanitize(&input);
        // Only the "a" chars survive
        let expected: String = std::iter::repeat_n('a', 500).collect();
        assert_eq!(result.as_ref(), expected);
    }

    #[test]
    fn adversarial_all_forbidden_c0_in_sequence() {
        // Every forbidden C0 byte
        let mut input = String::from("start");
        for b in 0x00u8..=0x1F {
            if b != 0x09 && b != 0x0A && b != 0x0D && b != 0x1B {
                input.push(b as char);
            }
        }
        input.push_str("end");
        let result = sanitize(&input);
        assert_eq!(result.as_ref(), "startend");
    }

    // ---- 10. Combined / chained attacks ----

    #[test]
    fn adversarial_combined_title_clear_clipboard() {
        // Chain: set title + clear screen + set clipboard + fake prompt
        let input = concat!(
            "\x1b]0;Terminal\x07",    // set title
            "\x1b[2J",                // clear screen
            "\x1b[H",                 // home cursor
            "\x1b]52;c;cm0gLXJm\x07", // set clipboard
            "Password: ",             // fake prompt
        );
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "Password: ");
        assert!(!result.contains('\x1b'));
        assert!(!result.contains('\x07'));
    }

    #[test]
    fn adversarial_sgr_color_soup() {
        // Many SGR sequences interspersed with text to try to leak colors
        let input = "\x1b[31m\x1b[1m\x1b[4m\x1b[7m\x1b[38;2;255;0;0mred\x1b[0m";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "red");
    }

    #[test]
    fn adversarial_hyperlink_wrapping_attack() {
        // Try to create a clickable region that covers existing content
        let input = concat!(
            "\x1b]8;;https://evil.com\x07",
            "Click here for info",
            "\x1b]8;;\x07",
        );
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "Click here for info");
    }

    #[test]
    fn adversarial_kitty_graphics_protocol() {
        // Kitty graphics protocol uses APC
        let input = "img\x1b_Gf=100,s=1,v=1;AAAA\x1b\\text";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "imgtext");
    }

    #[test]
    fn adversarial_sixel_data() {
        // Sixel graphics data via DCS
        let input = "pre\x1bPq#0;2;0;0;0#1;2;100;100;100~-\x1b\\post";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "prepost");
    }

    #[test]
    fn adversarial_mixed_valid_utf8_and_escapes() {
        // Unicode text interspersed with escape sequences
        let input = "\u{1f512}\x1b[31m\u{26a0}\x1b[0m secure\x1b]0;evil\x07\u{2705}";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "\u{1f512}\u{26a0} secure\u{2705}");
    }

    #[test]
    fn adversarial_control_char_near_escape() {
        // Control chars adjacent to escape sequences
        let input = "\x01\x1b[31m\x02text\x03\x1b[0m\x04";
        let result = sanitize(input);
        assert!(!result.contains('\x1b'));
        assert_eq!(result.as_ref(), "text");
    }

    #[test]
    fn adversarial_save_restore_cursor_attack() {
        // Save cursor, write fake content, restore cursor to hide it
        let input = "\x1b7fake prompt\x1b8real content";
        let result = sanitize(input);
        assert_eq!(result.as_ref(), "fake promptreal content");
    }

    #[test]
    fn adversarial_dec_set_reset_barrage() {
        // Barrage of DEC private mode set/reset sequences
        let input = (1..100)
            .map(|i| format!("\x1b[?{i}h\x1b[?{i}l"))
            .collect::<String>();
        let input = format!("A{input}B");
        let result = sanitize(&input);
        assert_eq!(result.as_ref(), "AB");
    }

    // ---- Property-based tests via proptest ----

    mod proptest_adversarial {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn sanitize_never_panics(input in ".*") {
                let _ = sanitize(&input);
            }

            #[test]
            fn sanitize_output_never_contains_esc(input in ".*") {
                let result = sanitize(&input);
                prop_assert!(
                    !result.contains('\x1b'),
                    "Output contained ESC for input {:?}", input
                );
            }

            #[test]
            fn sanitize_output_never_contains_del(input in ".*") {
                let result = sanitize(&input);
                prop_assert!(
                    !result.contains('\x7f'),
                    "Output contained DEL for input {:?}", input
                );
            }

            #[test]
            fn sanitize_output_no_forbidden_c0(input in ".*") {
                let result = sanitize(&input);
                for &b in result.as_bytes() {
                    prop_assert!(
                        !is_forbidden_c0(b),
                        "Output contains forbidden C0 0x{:02X}", b
                    );
                }
            }

            #[test]
            fn sanitize_preserves_clean_input(input in "[a-zA-Z0-9 .,!?\\n\\t]+") {
                let result = sanitize(&input);
                prop_assert_eq!(result.as_ref(), input.as_str());
            }

            #[test]
            fn sanitize_idempotent(input in ".*") {
                let first = sanitize(&input);
                let second = sanitize(first.as_ref());
                prop_assert_eq!(
                    first.as_ref(),
                    second.as_ref(),
                    "Sanitize is not idempotent"
                );
            }

            #[test]
            fn sanitize_output_len_lte_input(input in ".*") {
                let result = sanitize(&input);
                prop_assert!(
                    result.len() <= input.len(),
                    "Output ({}) longer than input ({})", result.len(), input.len()
                );
            }

            #[test]
            fn sanitize_output_is_valid_utf8(input in ".*") {
                let result = sanitize(&input);
                // The return type is Cow<str> so it's guaranteed valid UTF-8,
                // but verify the invariant explicitly.
                prop_assert!(std::str::from_utf8(result.as_bytes()).is_ok());
            }

            #[test]
            fn sanitize_output_no_c1_controls(input in ".*") {
                let result = sanitize(&input);
                for c in result.as_ref().chars() {
                    prop_assert!(
                        !('\u{0080}'..='\u{009F}').contains(&c),
                        "Output contains C1 control U+{:04X}", c as u32
                    );
                }
            }
        }

        // Targeted generators for adversarial byte patterns

        fn escape_sequence() -> impl Strategy<Value = String> {
            prop_oneof![
                // CSI sequences with random params and final bytes
                (
                    proptest::collection::vec(0x30u8..=0x3F, 0..20),
                    0x40u8..=0x7E,
                )
                    .prop_map(|(params, final_byte)| {
                        let mut s = String::from("\x1b[");
                        for b in params {
                            s.push(b as char);
                        }
                        s.push(final_byte as char);
                        s
                    }),
                // OSC with BEL terminator
                proptest::string::string_regex("[^\x07\x1b]{0,50}")
                    .unwrap()
                    .prop_map(|content| format!("\x1b]{content}\x07")),
                // OSC with ST terminator
                proptest::string::string_regex("[^\x1b]{0,50}")
                    .unwrap()
                    .prop_map(|content| format!("\x1b]{content}\x1b\\")),
                // DCS
                proptest::string::string_regex("[^\x1b]{0,50}")
                    .unwrap()
                    .prop_map(|content| format!("\x1bP{content}\x1b\\")),
                // APC
                proptest::string::string_regex("[^\x1b]{0,50}")
                    .unwrap()
                    .prop_map(|content| format!("\x1b_{content}\x1b\\")),
                // PM
                proptest::string::string_regex("[^\x1b]{0,50}")
                    .unwrap()
                    .prop_map(|content| format!("\x1b^{content}\x1b\\")),
                // Single-char escapes
                (0x20u8..=0x7E).prop_map(|b| format!("\x1b{}", b as char)),
            ]
        }

        fn mixed_adversarial_input() -> impl Strategy<Value = String> {
            proptest::collection::vec(
                prop_oneof![
                    // Clean text
                    proptest::string::string_regex("[a-zA-Z0-9 ]{1,10}").unwrap(),
                    // Escape sequences
                    escape_sequence(),
                    // Forbidden C0 controls
                    (0x00u8..=0x1F)
                        .prop_filter("not allowed control", |b| {
                            *b != 0x09 && *b != 0x0A && *b != 0x0D
                        })
                        .prop_map(|b| String::from(b as char)),
                ],
                1..20,
            )
            .prop_map(|parts| parts.join(""))
        }

        proptest! {
            #[test]
            fn adversarial_mixed_input_safe(input in mixed_adversarial_input()) {
                let result = sanitize(&input);
                prop_assert!(!result.contains('\x1b'));
                prop_assert!(!result.contains('\x7f'));
                for &b in result.as_bytes() {
                    prop_assert!(!is_forbidden_c0(b));
                }
            }

            #[test]
            fn escape_sequences_fully_stripped(seq in escape_sequence()) {
                let input = format!("before{seq}after");
                let result = sanitize(&input);
                prop_assert!(
                    !result.contains('\x1b'),
                    "Output contains ESC for sequence {:?}", seq
                );
                prop_assert!(
                    result.starts_with("before"),
                    "Output doesn't start with 'before' for {:?}: got {:?}", seq, result
                );
                // Note: unterminated DCS/APC/PM/OSC sequences consume to
                // end of input, so "after" may be absorbed. This is correct
                // security behavior — consuming unterminated sequences is
                // safer than letting potential payload through.
            }
        }
    }
}

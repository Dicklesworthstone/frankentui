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
///
/// # What Gets Preserved
/// - TAB, LF, CR (allowed control characters)
/// - All printable ASCII (0x20-0x7E)
/// - All valid UTF-8 sequences
#[inline]
pub fn sanitize(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();

    // Fast path: check for any ESC byte or forbidden C0 controls
    if memchr(0x1B, bytes).is_none() && !has_forbidden_c0(bytes) {
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
                    output.push(c);
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
            // Consume parameter bytes and intermediate bytes until final byte
            while i < bytes.len() {
                match bytes[i] {
                    // Final byte: 0x40-0x7E
                    0x40..=0x7E => {
                        return i + 1;
                    }
                    // Continue parsing
                    _ => {
                        i += 1;
                    }
                }
            }
        }
        // OSC sequence: ESC ] ... (BEL or ST)
        b']' => {
            i += 1;
            while i < bytes.len() {
                // BEL terminates OSC
                if bytes[i] == 0x07 {
                    return i + 1;
                }
                // ST (ESC \) terminates OSC
                if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    return i + 2;
                }
                i += 1;
            }
        }
        // DCS/PM/APC: ESC P/^/_ ... ST
        b'P' | b'^' | b'_' => {
            i += 1;
            while i < bytes.len() {
                // ST (ESC \) terminates
                if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    return i + 2;
                }
                i += 1;
            }
        }
        // Single-char escape sequences (ESC followed by 0x20-0x7E)
        0x20..=0x7E => {
            return i + 1;
        }
        // Unknown - just skip the ESC
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
    for i in 1..expected_len {
        let b = bytes[i];
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
}

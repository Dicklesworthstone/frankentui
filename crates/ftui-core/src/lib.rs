// Forbid unsafe in production; deny (with targeted allows) in tests for env var helpers.
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]

//! Core: terminal lifecycle, capability detection, events, and input parsing.
//!
//! # Role in FrankenTUI
//! `ftui-core` is the input layer. It owns terminal session setup/teardown,
//! capability probing, and normalized event types that the runtime consumes.
//!
//! # Primary responsibilities
//! - **TerminalSession**: RAII lifecycle for raw mode, alt-screen, and cleanup.
//! - **Event**: canonical input events (keys, mouse, paste, resize, focus).
//! - **Capability detection**: terminal features and overrides.
//! - **Input parsing**: robust decoding of terminal input streams.
//!
//! # How it fits in the system
//! The runtime (`ftui-runtime`) consumes `ftui-core::Event` values and drives
//! application models. The render kernel (`ftui-render`) is independent of
//! input, so `ftui-core` is the clean bridge between terminal I/O and the
//! deterministic render pipeline.

pub mod animation;
pub mod capability_override;
pub mod cursor;
pub mod cx;
pub mod event;
pub mod event_coalescer;
pub mod geometry;
pub mod gesture;
pub mod glyph_policy;
pub mod hover_stabilizer;
pub mod inline_mode;
pub mod input_parser;
pub mod key_sequence;
pub mod keybinding;
pub mod logging;
pub mod mux_passthrough;
pub mod s3_fifo;
pub mod semantic_event;
pub mod terminal_capabilities;
#[cfg(all(not(target_arch = "wasm32"), feature = "crossterm"))]
pub mod terminal_session;

#[cfg(feature = "caps-probe")]
pub mod caps_probe;

// Re-export tracing macros at crate root for ergonomic use.
#[cfg(feature = "tracing")]
pub use logging::{
    debug, debug_span, error, error_span, info, info_span, trace, trace_span, warn, warn_span,
};

pub mod text_width {
    //! Shared display width helpers for layout and rendering.
    //!
    //! This module centralizes glyph width calculation so layout (ftui-text)
    //! and rendering (ftui-render) stay in lockstep. It intentionally avoids
    //! ad-hoc emoji heuristics and relies on Unicode data tables.

    use std::sync::OnceLock;

    use unicode_display_width::width as unicode_display_width;
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    #[inline]
    fn env_flag(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    }

    #[inline]
    fn is_cjk_locale(locale: &str) -> bool {
        let lower = locale.trim().to_ascii_lowercase();
        lower.starts_with("ja") || lower.starts_with("zh") || lower.starts_with("ko")
    }

    #[inline]
    fn cjk_width_from_env_impl<F>(get_env: F) -> bool
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(value) = get_env("FTUI_GLYPH_DOUBLE_WIDTH") {
            return env_flag(&value);
        }
        if let Some(value) = get_env("FTUI_TEXT_CJK_WIDTH").or_else(|| get_env("FTUI_CJK_WIDTH")) {
            return env_flag(&value);
        }
        if let Some(locale) = get_env("LC_CTYPE").or_else(|| get_env("LANG")) {
            return is_cjk_locale(&locale);
        }
        false
    }

    #[inline]
    fn use_cjk_width() -> bool {
        static CJK_WIDTH: OnceLock<bool> = OnceLock::new();
        *CJK_WIDTH.get_or_init(|| cjk_width_from_env_impl(|key| std::env::var(key).ok()))
    }

    /// Whether the terminal is trusted to render text-default emoji + VS16 at
    /// width 2 (matching the Unicode spec).  Most terminals do NOT — they
    /// render these at width 1 — so the default is `false`.
    ///
    /// Set `FTUI_EMOJI_VS16_WIDTH=unicode` (or `=2`) to opt in for terminals
    /// that handle this correctly (WezTerm, Kitty, Ghostty).
    #[inline]
    fn trust_vs16_width() -> bool {
        static TRUST: OnceLock<bool> = OnceLock::new();
        *TRUST.get_or_init(|| {
            std::env::var("FTUI_EMOJI_VS16_WIDTH")
                .map(|v| v.eq_ignore_ascii_case("unicode") || v == "2")
                .unwrap_or(false)
        })
    }

    /// Compute VS16 trust policy using a custom environment lookup (testable).
    #[inline]
    pub fn vs16_trust_from_env<F>(get_env: F) -> bool
    where
        F: Fn(&str) -> Option<String>,
    {
        get_env("FTUI_EMOJI_VS16_WIDTH")
            .map(|v| v.eq_ignore_ascii_case("unicode") || v == "2")
            .unwrap_or(false)
    }

    /// Cached VS16 width trust policy (fast path).
    #[inline]
    pub fn vs16_width_trusted() -> bool {
        trust_vs16_width()
    }

    /// Strip U+FE0F (VS16) from a grapheme cluster.  Returns `None` if the
    /// grapheme does not contain VS16 (no allocation needed).
    #[inline]
    fn strip_vs16(grapheme: &str) -> Option<String> {
        if grapheme.contains('\u{FE0F}') {
            Some(grapheme.chars().filter(|&c| c != '\u{FE0F}').collect())
        } else {
            None
        }
    }

    /// Compute CJK width policy using a custom environment lookup.
    #[inline]
    pub fn cjk_width_from_env<F>(get_env: F) -> bool
    where
        F: Fn(&str) -> Option<String>,
    {
        cjk_width_from_env_impl(get_env)
    }

    /// Cached CJK width policy (fast path).
    #[inline]
    pub fn cjk_width_enabled() -> bool {
        use_cjk_width()
    }

    #[inline]
    fn ascii_display_width(text: &str) -> usize {
        let mut width = 0;
        for b in text.bytes() {
            match b {
                b'\t' | b'\n' | b'\r' => width += 1,
                0x20..=0x7E => width += 1,
                _ => {}
            }
        }
        width
    }

    /// Fast-path width for pure printable ASCII.
    #[inline]
    #[must_use]
    pub fn ascii_width(text: &str) -> Option<usize> {
        if text.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
            Some(text.len())
        } else {
            None
        }
    }

    #[inline]
    fn is_zero_width_codepoint(c: char) -> bool {
        let u = c as u32;
        matches!(u, 0x0000..=0x001F | 0x007F..=0x009F)
            || matches!(u, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF)
            || matches!(u, 0xFE20..=0xFE2F)
            || matches!(u, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
            || matches!(
                u,
                0x00AD
                    | 0x034F
                    | 0x180E
                    | 0x200B
                    | 0x200C
                    | 0x200D
                    | 0x200E
                    | 0x200F
                    | 0x2060
                    | 0xFEFF
            )
            || matches!(u, 0x202A..=0x202E | 0x2066..=0x2069 | 0x206A..=0x206F)
    }

    /// Width of a single grapheme cluster.
    #[inline]
    #[must_use]
    pub fn grapheme_width(grapheme: &str) -> usize {
        if grapheme.is_ascii() {
            return ascii_display_width(grapheme);
        }
        if grapheme.chars().all(is_zero_width_codepoint) {
            return 0;
        }
        if use_cjk_width() {
            return grapheme.width_cjk();
        }
        // Terminal-realistic VS16 handling: most terminals render text-default
        // emoji (Emoji_Presentation=No) at 1 cell even with VS16 appended.
        // Strip VS16 so unicode_display_width returns the text-presentation width.
        if !trust_vs16_width()
            && let Some(stripped) = strip_vs16(grapheme)
        {
            if stripped.is_empty() {
                return 0;
            }
            return unicode_display_width(&stripped) as usize;
        }
        unicode_display_width(grapheme) as usize
    }

    /// Width of a single Unicode scalar.
    #[inline]
    #[must_use]
    pub fn char_width(ch: char) -> usize {
        if ch.is_ascii() {
            return match ch {
                '\t' | '\n' | '\r' => 1,
                ' '..='~' => 1,
                _ => 0,
            };
        }
        if is_zero_width_codepoint(ch) {
            return 0;
        }
        if use_cjk_width() {
            ch.width_cjk().unwrap_or(0)
        } else {
            ch.width().unwrap_or(0)
        }
    }

    /// Width of a string in terminal cells.
    #[inline]
    #[must_use]
    pub fn display_width(text: &str) -> usize {
        if let Some(width) = ascii_width(text) {
            return width;
        }
        if text.is_ascii() {
            return ascii_display_width(text);
        }
        let cjk_width = use_cjk_width();
        if !text.chars().any(is_zero_width_codepoint) {
            if cjk_width {
                return text.width_cjk();
            }
            return unicode_display_width(text) as usize;
        }
        text.graphemes(true).map(grapheme_width).sum()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // ── env helpers (testable without OnceLock) ─────────────────

        #[test]
        fn cjk_width_env_explicit_true() {
            let get = |key: &str| match key {
                "FTUI_GLYPH_DOUBLE_WIDTH" => Some("1".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_explicit_false() {
            let get = |key: &str| match key {
                "FTUI_GLYPH_DOUBLE_WIDTH" => Some("0".into()),
                _ => None,
            };
            assert!(!cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_text_cjk_key() {
            let get = |key: &str| match key {
                "FTUI_TEXT_CJK_WIDTH" => Some("true".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_fallback_key() {
            let get = |key: &str| match key {
                "FTUI_CJK_WIDTH" => Some("yes".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_japanese_locale() {
            let get = |key: &str| match key {
                "LC_CTYPE" => Some("ja_JP.UTF-8".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_chinese_locale() {
            let get = |key: &str| match key {
                "LANG" => Some("zh_CN.UTF-8".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_korean_locale() {
            let get = |key: &str| match key {
                "LC_CTYPE" => Some("ko_KR.UTF-8".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_english_locale_returns_false() {
            let get = |key: &str| match key {
                "LANG" => Some("en_US.UTF-8".into()),
                _ => None,
            };
            assert!(!cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_no_vars_returns_false() {
            let get = |_: &str| -> Option<String> { None };
            assert!(!cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_glyph_overrides_locale() {
            // FTUI_GLYPH_DOUBLE_WIDTH=0 should override a CJK locale
            let get = |key: &str| match key {
                "FTUI_GLYPH_DOUBLE_WIDTH" => Some("0".into()),
                "LANG" => Some("ja_JP.UTF-8".into()),
                _ => None,
            };
            assert!(!cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_on_is_true() {
            let get = |key: &str| match key {
                "FTUI_GLYPH_DOUBLE_WIDTH" => Some("on".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_case_insensitive() {
            let get = |key: &str| match key {
                "FTUI_CJK_WIDTH" => Some("TRUE".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        // ── VS16 trust from env ─────────────────────────────────────

        #[test]
        fn vs16_trust_unicode_string() {
            let get = |key: &str| match key {
                "FTUI_EMOJI_VS16_WIDTH" => Some("unicode".into()),
                _ => None,
            };
            assert!(vs16_trust_from_env(get));
        }

        #[test]
        fn vs16_trust_value_2() {
            let get = |key: &str| match key {
                "FTUI_EMOJI_VS16_WIDTH" => Some("2".into()),
                _ => None,
            };
            assert!(vs16_trust_from_env(get));
        }

        #[test]
        fn vs16_trust_not_set() {
            let get = |_: &str| -> Option<String> { None };
            assert!(!vs16_trust_from_env(get));
        }

        #[test]
        fn vs16_trust_other_value() {
            let get = |key: &str| match key {
                "FTUI_EMOJI_VS16_WIDTH" => Some("1".into()),
                _ => None,
            };
            assert!(!vs16_trust_from_env(get));
        }

        #[test]
        fn vs16_trust_case_insensitive() {
            let get = |key: &str| match key {
                "FTUI_EMOJI_VS16_WIDTH" => Some("UNICODE".into()),
                _ => None,
            };
            assert!(vs16_trust_from_env(get));
        }

        // ── ascii_width fast path ───────────────────────────────────

        #[test]
        fn ascii_width_pure_ascii() {
            assert_eq!(ascii_width("hello"), Some(5));
        }

        #[test]
        fn ascii_width_empty() {
            assert_eq!(ascii_width(""), Some(0));
        }

        #[test]
        fn ascii_width_with_space() {
            assert_eq!(ascii_width("hello world"), Some(11));
        }

        #[test]
        fn ascii_width_non_ascii_returns_none() {
            assert_eq!(ascii_width("héllo"), None);
        }

        #[test]
        fn ascii_width_with_tab_returns_none() {
            // Tab (0x09) is outside 0x20..=0x7E
            assert_eq!(ascii_width("hello\tworld"), None);
        }

        #[test]
        fn ascii_width_with_newline_returns_none() {
            assert_eq!(ascii_width("hello\n"), None);
        }

        #[test]
        fn ascii_width_control_char_returns_none() {
            assert_eq!(ascii_width("\x01"), None);
        }

        // ── char_width ──────────────────────────────────────────────

        #[test]
        fn char_width_ascii_letter() {
            assert_eq!(char_width('A'), 1);
        }

        #[test]
        fn char_width_space() {
            assert_eq!(char_width(' '), 1);
        }

        #[test]
        fn char_width_tab() {
            assert_eq!(char_width('\t'), 1);
        }

        #[test]
        fn char_width_newline() {
            assert_eq!(char_width('\n'), 1);
        }

        #[test]
        fn char_width_nul() {
            // NUL (0x00) is an ASCII control char, zero width
            assert_eq!(char_width('\0'), 0);
        }

        #[test]
        fn char_width_bell() {
            // BEL (0x07) is an ASCII control char, zero width
            assert_eq!(char_width('\x07'), 0);
        }

        #[test]
        fn char_width_combining_accent() {
            // U+0301 COMBINING ACUTE ACCENT is zero-width
            assert_eq!(char_width('\u{0301}'), 0);
        }

        #[test]
        fn char_width_zwj() {
            // U+200D ZERO WIDTH JOINER
            assert_eq!(char_width('\u{200D}'), 0);
        }

        #[test]
        fn char_width_zwnbsp() {
            // U+FEFF ZERO WIDTH NO-BREAK SPACE
            assert_eq!(char_width('\u{FEFF}'), 0);
        }

        #[test]
        fn char_width_soft_hyphen() {
            // U+00AD SOFT HYPHEN
            assert_eq!(char_width('\u{00AD}'), 0);
        }

        #[test]
        fn char_width_wide_east_asian() {
            // '⚡' (U+26A1) has east_asian_width=W, always width 2
            assert_eq!(char_width('⚡'), 2);
        }

        #[test]
        fn char_width_cjk_ideograph() {
            // CJK ideographs are always width 2
            assert_eq!(char_width('中'), 2);
        }

        #[test]
        fn char_width_variation_selector() {
            // U+FE0F VARIATION SELECTOR-16 is zero-width
            assert_eq!(char_width('\u{FE0F}'), 0);
        }

        // ── display_width ───────────────────────────────────────────

        #[test]
        fn display_width_ascii() {
            assert_eq!(display_width("hello"), 5);
        }

        #[test]
        fn display_width_empty() {
            assert_eq!(display_width(""), 0);
        }

        #[test]
        fn display_width_cjk_chars() {
            // Each CJK character is width 2
            assert_eq!(display_width("中文"), 4);
        }

        #[test]
        fn display_width_mixed_ascii_cjk() {
            // 'a' = 1, '中' = 2, 'b' = 1
            assert_eq!(display_width("a中b"), 4);
        }

        #[test]
        fn display_width_combining_chars() {
            // 'e' + combining acute = 1 grapheme, width 1
            assert_eq!(display_width("e\u{0301}"), 1);
        }

        #[test]
        fn display_width_ascii_with_control_codes() {
            // Non-printable ASCII control chars in non-pure-ASCII path
            // Tab/newline/CR get width 1 via ascii_display_width
            assert_eq!(display_width("a\tb"), 3);
        }

        // ── grapheme_width ──────────────────────────────────────────

        #[test]
        fn grapheme_width_ascii_char() {
            assert_eq!(grapheme_width("A"), 1);
        }

        #[test]
        fn grapheme_width_cjk_ideograph() {
            assert_eq!(grapheme_width("中"), 2);
        }

        #[test]
        fn grapheme_width_combining_sequence() {
            // 'e' + combining accent is one grapheme, width 1
            assert_eq!(grapheme_width("e\u{0301}"), 1);
        }

        #[test]
        fn grapheme_width_zwj_cluster() {
            // ZWJ alone is zero-width
            assert_eq!(grapheme_width("\u{200D}"), 0);
        }
    }
}

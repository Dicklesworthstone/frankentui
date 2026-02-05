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
pub mod semantic_event;
pub mod terminal_capabilities;
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
}

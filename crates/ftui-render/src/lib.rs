#![forbid(unsafe_code)]

//! Render kernel: cells, buffers, diffs, and ANSI presentation.
//!
//! # Role in FrankenTUI
//! `ftui-render` is the deterministic rendering engine. It turns a logical
//! `Frame` into a `Buffer`, computes diffs, and emits minimal ANSI output via
//! the `Presenter`.
//!
//! # Primary responsibilities
//! - **Cell/Buffer**: 2D grid with fixed-size cells and scissor/opacity stacks.
//! - **BufferDiff**: efficient change detection between frames.
//! - **Presenter**: stateful ANSI emitter with cursor/mode tracking.
//! - **Frame**: rendering surface used by widgets and application views.
//!
//! # How it fits in the system
//! `ftui-runtime` calls your model's `view()` to render into a `Frame`. That
//! frame becomes a `Buffer`, which is diffed and presented to the terminal via
//! `TerminalWriter`. This crate is the kernel of FrankenTUI's flicker-free,
//! deterministic output guarantees.

pub mod alloc_budget;
pub mod ansi;
pub mod budget;
pub mod buffer;
pub mod cell;
pub mod counting_writer;
pub mod diff;
pub mod diff_strategy;
pub mod drawing;
pub mod frame;
pub mod grapheme_pool;
pub mod headless;
pub mod link_registry;
pub mod presenter;
pub mod sanitize;
pub mod spatial_hit_index;
pub mod terminal_model;

mod text_width {
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
    fn use_cjk_width() -> bool {
        static CJK_WIDTH: OnceLock<bool> = OnceLock::new();
        *CJK_WIDTH.get_or_init(|| {
            if let Ok(value) =
                std::env::var("FTUI_TEXT_CJK_WIDTH").or_else(|_| std::env::var("FTUI_CJK_WIDTH"))
            {
                return env_flag(&value);
            }
            if let Ok(locale) = std::env::var("LC_CTYPE").or_else(|_| std::env::var("LANG")) {
                return is_cjk_locale(&locale);
            }
            false
        })
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

    #[inline]
    fn ascii_width(text: &str) -> Option<usize> {
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

    #[inline]
    pub(crate) fn grapheme_width(grapheme: &str) -> usize {
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

    #[inline]
    pub(crate) fn char_width(ch: char) -> usize {
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

    #[inline]
    pub(crate) fn display_width(text: &str) -> usize {
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

pub(crate) use text_width::{char_width, display_width, grapheme_width};

#[cfg(test)]
mod tests {
    use super::{display_width, grapheme_width};

    #[test]
    fn display_width_matches_expected_samples() {
        // Avoid CJK samples to keep results independent of locale/CJK width flags.
        let samples = [
            ("hello", 5usize),
            ("😀", 2usize),
            ("👩‍💻", 2usize),
            ("🇺🇸", 2usize),
            ("❤️", 2usize),
            ("⌨️", 2usize),
            ("⚠️", 2usize),
            ("⭐", 2usize),
            ("A😀B", 4usize),
            ("ok ✅", 5usize),
        ];
        for (sample, expected) in samples {
            assert_eq!(
                display_width(sample),
                expected,
                "display width mismatch for {sample:?}"
            );
        }
    }

    #[test]
    fn grapheme_width_matches_expected_samples() {
        let samples = [
            ("a", 1usize),
            ("😀", 2usize),
            ("👩‍💻", 2usize),
            ("🇺🇸", 2usize),
            ("👍🏽", 2usize),
            ("❤️", 2usize),
            ("⌨️", 2usize),
            ("⚠️", 2usize),
            ("⭐", 2usize),
        ];
        for (grapheme, expected) in samples {
            assert_eq!(
                grapheme_width(grapheme),
                expected,
                "grapheme width mismatch for {grapheme:?}"
            );
        }
    }
}

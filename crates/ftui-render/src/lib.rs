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
pub mod arena;
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

// Re-export text width helpers from ftui-core (single source of truth).
pub(crate) use ftui_core::text_width::{char_width, display_width, grapheme_width};

#[cfg(test)]
mod tests {
    use super::{char_width, display_width, grapheme_width};

    // ── display_width ────────────────────────────────────────────────

    #[test]
    fn display_width_matches_expected_samples() {
        // Avoid CJK samples to keep results independent of locale/CJK width flags.
        // Note: ftui-core strips VS16 (U+FE0F) by default for terminal-realistic
        // widths, so text-default emoji like ❤️/⌨️/⚠️ measure as their base
        // text-presentation width rather than emoji-presentation width 2.
        let samples = [
            ("hello", 5usize),
            ("😀", 2usize),
            ("👩‍💻", 2usize),
            ("🇺🇸", 2usize),
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
    fn display_width_empty_string() {
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn display_width_single_ascii_char() {
        assert_eq!(display_width("x"), 1);
        assert_eq!(display_width(" "), 1);
    }

    #[test]
    fn display_width_pure_ascii_fast_path() {
        assert_eq!(display_width("Hello, World!"), 13);
        assert_eq!(display_width("fn main() {}"), 12);
    }

    #[test]
    fn display_width_ascii_with_tabs() {
        assert_eq!(display_width("a\tb"), 3);
        assert_eq!(display_width("\n"), 1);
    }

    #[test]
    fn display_width_mixed_ascii_emoji() {
        assert_eq!(display_width("hi 🎉"), 5);
        assert_eq!(display_width("🚀start"), 7);
    }

    #[test]
    fn display_width_zero_width_chars_in_string() {
        let s = "a\u{00AD}b";
        assert_eq!(display_width(s), 2);
    }

    #[test]
    fn display_width_combining_characters() {
        let s = "e\u{0301}";
        assert_eq!(display_width(s), 1);
    }

    #[test]
    fn display_width_multiple_emoji() {
        assert_eq!(display_width("😀😀😀"), 6);
    }

    // ── grapheme_width ───────────────────────────────────────────────

    #[test]
    fn grapheme_width_matches_expected_samples() {
        // VS16 emoji (❤️/⌨️/⚠️) removed — ftui-core strips VS16 by default
        // (terminal-realistic) so their widths depend on base char EAW, not emoji
        // presentation.  Dedicated VS16 tests live in ftui-core.
        let samples = [
            ("a", 1usize),
            ("😀", 2usize),
            ("👩‍💻", 2usize),
            ("🇺🇸", 2usize),
            ("👍🏽", 2usize),
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

    #[test]
    fn grapheme_width_ascii_space() {
        assert_eq!(grapheme_width(" "), 1);
    }

    #[test]
    fn grapheme_width_ascii_tilde() {
        assert_eq!(grapheme_width("~"), 1);
    }

    #[test]
    fn grapheme_width_tab() {
        assert_eq!(grapheme_width("\t"), 1);
    }

    #[test]
    fn grapheme_width_newline() {
        assert_eq!(grapheme_width("\n"), 1);
    }

    #[test]
    fn grapheme_width_combining_accent() {
        assert_eq!(grapheme_width("e\u{0301}"), 1);
    }

    #[test]
    fn grapheme_width_zero_width_space() {
        assert_eq!(grapheme_width("\u{200B}"), 0);
    }

    #[test]
    fn grapheme_width_zero_width_joiner() {
        assert_eq!(grapheme_width("\u{200D}"), 0);
    }

    #[test]
    fn grapheme_width_skin_tone_modifier() {
        assert_eq!(grapheme_width("👍🏿"), 2);
    }

    // ── char_width ───────────────────────────────────────────────────

    #[test]
    fn char_width_ascii_printable() {
        assert_eq!(char_width('A'), 1);
        assert_eq!(char_width('z'), 1);
        assert_eq!(char_width(' '), 1);
        assert_eq!(char_width('~'), 1);
        assert_eq!(char_width('!'), 1);
    }

    #[test]
    fn char_width_ascii_whitespace() {
        assert_eq!(char_width('\t'), 1);
        assert_eq!(char_width('\n'), 1);
        assert_eq!(char_width('\r'), 1);
    }

    #[test]
    fn char_width_ascii_control() {
        assert_eq!(char_width('\x00'), 0);
        assert_eq!(char_width('\x01'), 0);
        assert_eq!(char_width('\x1F'), 0);
        assert_eq!(char_width('\x7F'), 0);
    }

    #[test]
    fn char_width_zero_width_combining() {
        assert_eq!(char_width('\u{0300}'), 0);
        assert_eq!(char_width('\u{0301}'), 0);
    }

    #[test]
    fn char_width_zero_width_special() {
        assert_eq!(char_width('\u{200B}'), 0);
        assert_eq!(char_width('\u{200D}'), 0);
        assert_eq!(char_width('\u{FEFF}'), 0);
        assert_eq!(char_width('\u{00AD}'), 0);
    }

    #[test]
    fn char_width_variation_selectors() {
        assert_eq!(char_width('\u{FE00}'), 0);
        assert_eq!(char_width('\u{FE0F}'), 0);
    }

    #[test]
    fn char_width_bidi_controls() {
        assert_eq!(char_width('\u{200E}'), 0);
        assert_eq!(char_width('\u{200F}'), 0);
    }

    #[test]
    fn char_width_normal_non_ascii() {
        assert_eq!(char_width('é'), 1);
        assert_eq!(char_width('ñ'), 1);
    }

    #[test]
    fn char_width_euro_sign() {
        assert_eq!(char_width('€'), 1);
    }
}

#![forbid(unsafe_code)]

//! Render kernel: cells, buffers, diffs, and ANSI presentation.

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
    use unicode_display_width::width as unicode_display_width;
    use unicode_segmentation::UnicodeSegmentation;

    #[inline]
    fn ascii_width(text: &str) -> Option<usize> {
        if text.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
            Some(text.len())
        } else {
            None
        }
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
    fn is_probable_emoji(c: char) -> bool {
        let u = c as u32;
        matches!(
            u,
            0x1F000..=0x1FAFF | 0x2300..=0x23FF | 0x2600..=0x27BF | 0x2B00..=0x2BFF
        ) && u != 0x2764
    }

    #[inline]
    fn has_emoji_presentation_selector(grapheme: &str) -> bool {
        grapheme.chars().any(|c| c as u32 == 0xFE0F)
    }

    #[inline]
    pub(crate) fn grapheme_width(grapheme: &str) -> usize {
        if grapheme.is_ascii() {
            return ascii_display_width(grapheme);
        }
        if grapheme.chars().all(is_zero_width_codepoint) {
            return 0;
        }
        if has_emoji_presentation_selector(grapheme) || grapheme.chars().any(is_probable_emoji) {
            return 2;
        }
        unicode_display_width(grapheme) as usize
    }

    #[inline]
    pub(crate) fn display_width(text: &str) -> usize {
        if let Some(width) = ascii_width(text) {
            return width;
        }
        if text.is_ascii() {
            return ascii_display_width(text);
        }
        if !text.chars().any(is_zero_width_codepoint) {
            if text.chars().any(is_probable_emoji) {
                return text.graphemes(true).map(grapheme_width).sum();
            }
            return unicode_display_width(text) as usize;
        }
        text.graphemes(true).map(grapheme_width).sum()
    }
}

pub(crate) use text_width::{display_width, grapheme_width};

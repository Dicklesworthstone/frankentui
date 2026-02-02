#![forbid(unsafe_code)]

//! Text view utilities for scrollable, wrapped display.
//!
//! The view precomputes "virtual lines" produced by wrapping so callers can
//! perform deterministic viewport math (scroll by line/page, map source lines
//! to wrapped lines, and compute visible ranges) without duplicating logic.

use crate::rope::Rope;
use crate::wrap::{WrapMode, WrapOptions, display_width, wrap_with_options};
use std::ops::Range;

/// Viewport size in terminal cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Viewport {
    /// Width in terminal columns.
    pub width: usize,
    /// Height in terminal rows.
    pub height: usize,
}

impl Viewport {
    /// Create a new viewport size.
    #[must_use]
    pub const fn new(width: usize, height: usize) -> Self {
        Self { width, height }
    }
}

/// A single wrapped (virtual) line in the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewLine {
    /// The rendered text for this virtual line.
    pub text: String,
    /// The source (logical) line index from the original text.
    pub source_line: usize,
    /// True if this is a wrapped continuation of the source line.
    pub is_wrap: bool,
    /// Display width in terminal cells.
    pub width: usize,
}

/// A scrollable, wrapped view over a text buffer.
#[derive(Debug, Clone)]
pub struct TextView {
    text: Rope,
    wrap: WrapMode,
    width: usize,
    lines: Vec<ViewLine>,
    max_width: usize,
    source_line_count: usize,
}

impl TextView {
    /// Build a view from raw text, wrap mode, and viewport width.
    #[must_use]
    pub fn new(text: impl Into<Rope>, width: usize, wrap: WrapMode) -> Self {
        let mut view = Self {
            text: text.into(),
            wrap,
            width,
            lines: Vec::new(),
            max_width: 0,
            source_line_count: 0,
        };
        view.rebuild();
        view
    }

    /// Replace the text and recompute layout.
    pub fn set_text(&mut self, text: impl Into<Rope>) {
        self.text = text.into();
        self.rebuild();
    }

    /// Update wrap mode and recompute layout.
    pub fn set_wrap(&mut self, wrap: WrapMode) {
        if self.wrap != wrap {
            self.wrap = wrap;
            self.rebuild();
        }
    }

    /// Update viewport width and recompute layout.
    pub fn set_width(&mut self, width: usize) {
        if self.width != width {
            self.width = width;
            self.rebuild();
        }
    }

    /// Current wrap mode.
    #[must_use]
    pub const fn wrap_mode(&self) -> WrapMode {
        self.wrap
    }

    /// Current viewport width used for wrapping.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Number of logical (source) lines in the text.
    #[must_use]
    pub const fn source_line_count(&self) -> usize {
        self.source_line_count
    }

    /// Number of virtual (wrapped) lines.
    #[must_use]
    pub fn virtual_line_count(&self) -> usize {
        self.lines.len()
    }

    /// Maximum display width across all virtual lines.
    #[must_use]
    pub const fn max_width(&self) -> usize {
        self.max_width
    }

    /// Access all virtual lines.
    #[must_use]
    pub fn lines(&self) -> &[ViewLine] {
        &self.lines
    }

    /// Map a source line index to its first virtual line index.
    #[must_use]
    pub fn source_to_virtual(&self, source_line: usize) -> Option<usize> {
        self.lines
            .iter()
            .position(|line| line.source_line == source_line)
    }

    /// Map a virtual line index to its source line index.
    #[must_use]
    pub fn virtual_to_source(&self, virtual_line: usize) -> Option<usize> {
        self.lines.get(virtual_line).map(|line| line.source_line)
    }

    /// Clamp scroll position to a valid range for the given viewport height.
    #[must_use]
    pub fn clamp_scroll(&self, scroll_y: usize, viewport_height: usize) -> usize {
        let total = self.lines.len();
        if total == 0 {
            return 0;
        }
        if viewport_height == 0 {
            return scroll_y.min(total);
        }
        let max_scroll = total.saturating_sub(viewport_height);
        scroll_y.min(max_scroll)
    }

    /// Maximum scroll offset for the given viewport height.
    #[must_use]
    pub fn max_scroll(&self, viewport_height: usize) -> usize {
        let total = self.lines.len();
        if total == 0 {
            return 0;
        }
        if viewport_height == 0 {
            return total;
        }
        total.saturating_sub(viewport_height)
    }

    /// Compute the visible virtual line range for a scroll offset + viewport height.
    #[must_use]
    pub fn visible_range(&self, scroll_y: usize, viewport_height: usize) -> Range<usize> {
        let total = self.lines.len();
        if total == 0 || viewport_height == 0 {
            return 0..0;
        }
        let scroll = self.clamp_scroll(scroll_y, viewport_height);
        let end = (scroll + viewport_height).min(total);
        scroll..end
    }

    /// Get the visible virtual lines for a scroll offset + viewport height.
    #[must_use]
    pub fn visible_lines(&self, scroll_y: usize, viewport_height: usize) -> &[ViewLine] {
        let range = self.visible_range(scroll_y, viewport_height);
        &self.lines[range]
    }

    /// Scroll so the given source line is at the top of the viewport.
    /// Returns `None` if the source line doesn't exist.
    #[must_use]
    pub fn scroll_to_line(&self, source_line: usize, viewport_height: usize) -> Option<usize> {
        let virtual_line = self.source_to_virtual(source_line)?;
        Some(self.clamp_scroll(virtual_line, viewport_height))
    }

    /// Scroll to the top of the view.
    #[must_use]
    pub fn scroll_to_top(&self) -> usize {
        0
    }

    /// Scroll to the bottom of the view.
    #[must_use]
    pub fn scroll_to_bottom(&self, viewport_height: usize) -> usize {
        self.max_scroll(viewport_height)
    }

    /// Scroll by a line delta (positive or negative).
    #[must_use]
    pub fn scroll_by_lines(&self, scroll_y: usize, delta: isize, viewport_height: usize) -> usize {
        let next = (scroll_y as i64) + (delta as i64);
        let next = if next < 0 { 0 } else { next as usize };
        self.clamp_scroll(next, viewport_height)
    }

    /// Scroll by a page delta (positive or negative).
    #[must_use]
    pub fn scroll_by_pages(&self, scroll_y: usize, pages: isize, viewport_height: usize) -> usize {
        if viewport_height == 0 {
            return self.clamp_scroll(scroll_y, viewport_height);
        }
        let delta = (viewport_height as i64) * (pages as i64);
        let next = (scroll_y as i64) + delta;
        let next = if next < 0 { 0 } else { next as usize };
        self.clamp_scroll(next, viewport_height)
    }

    fn rebuild(&mut self) {
        self.lines.clear();
        self.max_width = 0;

        let preserve_indent = self.wrap == WrapMode::Char;
        let options = WrapOptions::new(self.width)
            .mode(self.wrap)
            .preserve_indent(preserve_indent);

        let mut source_lines = 0;

        for (source_line, line) in self.text.lines().enumerate() {
            source_lines += 1;
            let mut line_text = line.to_string();
            if line_text.ends_with('\n') {
                line_text.pop();
            }

            let wrapped = wrap_with_options(&line_text, &options);
            if wrapped.is_empty() {
                let width = 0;
                self.lines.push(ViewLine {
                    text: String::new(),
                    source_line,
                    is_wrap: false,
                    width,
                });
                self.max_width = self.max_width.max(width);
                continue;
            }

            for (idx, part) in wrapped.into_iter().enumerate() {
                let width = display_width(&part);
                self.max_width = self.max_width.max(width);
                self.lines.push(ViewLine {
                    text: part,
                    source_line,
                    is_wrap: idx > 0,
                    width,
                });
            }
        }

        self.source_line_count = source_lines;
    }
}

#[cfg(test)]
mod tests {
    use super::{TextView, Viewport};
    use crate::wrap::WrapMode;

    #[test]
    fn view_basic_counts() {
        let view = TextView::new("a\nbb", 10, WrapMode::None);
        assert_eq!(view.source_line_count(), 2);
        assert_eq!(view.virtual_line_count(), 2);
        assert_eq!(view.max_width(), 2);
    }

    #[test]
    fn view_wraps_word() {
        let view = TextView::new("hello world", 5, WrapMode::Word);
        let lines: Vec<&str> = view.lines().iter().map(|l| l.text.as_str()).collect();
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn view_wraps_cjk_by_cells() {
        let view = TextView::new("你好世界", 4, WrapMode::Char);
        let lines: Vec<&str> = view.lines().iter().map(|l| l.text.as_str()).collect();
        assert_eq!(lines, vec!["你好", "世界"]);
    }

    #[test]
    fn visible_range_clamps_scroll() {
        let view = TextView::new("a\nb\nc", 10, WrapMode::None);
        let range = view.visible_range(5, 2);
        assert_eq!(range, 1..3);
    }

    #[test]
    fn scroll_to_line_clamps() {
        let view = TextView::new("a\nb\nc\nd", 10, WrapMode::None);
        let scroll = view.scroll_to_line(3, 2).expect("line 3 exists");
        assert_eq!(scroll, 2);
    }

    #[test]
    fn scroll_by_pages_moves_in_viewport_steps() {
        let view = TextView::new("1\n2\n3\n4\n5", 10, WrapMode::None);
        let scroll = view.scroll_by_pages(0, 1, 2);
        assert_eq!(scroll, 2);
        let back = view.scroll_by_pages(scroll, -1, 2);
        assert_eq!(back, 0);
    }

    #[test]
    fn scroll_to_bottom_respects_viewport() {
        let view = TextView::new("a\nb\nc\nd", 10, WrapMode::None);
        let bottom = view.scroll_to_bottom(2);
        assert_eq!(bottom, 2);
        let top = view.scroll_to_top();
        assert_eq!(top, 0);
    }

    #[test]
    fn visible_lines_returns_slice() {
        let view = TextView::new("a\nb\nc\nd", 10, WrapMode::None);
        let visible = view.visible_lines(1, 2);
        let texts: Vec<&str> = visible.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["b", "c"]);
    }

    #[test]
    fn viewport_struct_is_copyable() {
        let viewport = Viewport::new(80, 24);
        let copy = viewport;
        assert_eq!(copy.width, 80);
        assert_eq!(copy.height, 24);
    }

    // ====== Empty text ======

    #[test]
    fn empty_text_view() {
        let view = TextView::new("", 10, WrapMode::None);
        assert_eq!(view.source_line_count(), 1); // empty text is still 1 source line
        assert_eq!(view.virtual_line_count(), 1);
        assert_eq!(view.max_width(), 0);
    }

    #[test]
    fn empty_text_scroll() {
        let view = TextView::new("", 10, WrapMode::None);
        assert_eq!(view.max_scroll(5), 0);
        assert_eq!(view.clamp_scroll(100, 5), 0);
        assert_eq!(view.visible_range(0, 5), 0..1);
    }

    // ====== Source/virtual line mapping ======

    #[test]
    fn source_to_virtual_no_wrap() {
        let view = TextView::new("a\nb\nc", 10, WrapMode::None);
        assert_eq!(view.source_to_virtual(0), Some(0));
        assert_eq!(view.source_to_virtual(1), Some(1));
        assert_eq!(view.source_to_virtual(2), Some(2));
        assert_eq!(view.source_to_virtual(3), None);
    }

    #[test]
    fn virtual_to_source_no_wrap() {
        let view = TextView::new("a\nb\nc", 10, WrapMode::None);
        assert_eq!(view.virtual_to_source(0), Some(0));
        assert_eq!(view.virtual_to_source(1), Some(1));
        assert_eq!(view.virtual_to_source(2), Some(2));
        assert_eq!(view.virtual_to_source(3), None);
    }

    #[test]
    fn source_to_virtual_with_wrap() {
        // "abcde" with width 3 wraps into 2 virtual lines
        let view = TextView::new("abcde\nxy", 3, WrapMode::Char);
        assert_eq!(view.source_to_virtual(0), Some(0)); // first virtual line of "abcde"
        assert_eq!(view.source_to_virtual(1), Some(2)); // "xy" starts at virtual line 2
    }

    #[test]
    fn virtual_to_source_with_wrap() {
        let view = TextView::new("abcde\nxy", 3, WrapMode::Char);
        assert_eq!(view.virtual_to_source(0), Some(0)); // first wrap of "abcde"
        assert_eq!(view.virtual_to_source(1), Some(0)); // second wrap of "abcde"
        assert_eq!(view.virtual_to_source(2), Some(1)); // "xy"
    }

    // ====== Wrap flag ======

    #[test]
    fn is_wrap_flag_set_correctly() {
        let view = TextView::new("abcde", 3, WrapMode::Char);
        let lines = view.lines();
        assert!(!lines[0].is_wrap); // first segment is NOT a wrap
        assert!(lines[1].is_wrap);  // second segment IS a wrap continuation
    }

    // ====== set_text / set_wrap / set_width ======

    #[test]
    fn set_text_recomputes() {
        let mut view = TextView::new("abc", 10, WrapMode::None);
        assert_eq!(view.source_line_count(), 1);
        view.set_text("a\nb\nc");
        assert_eq!(view.source_line_count(), 3);
        assert_eq!(view.virtual_line_count(), 3);
    }

    #[test]
    fn set_wrap_recomputes() {
        let mut view = TextView::new("hello world", 5, WrapMode::None);
        let before = view.virtual_line_count();
        view.set_wrap(WrapMode::Word);
        let after = view.virtual_line_count();
        // word wrap at width 5 should produce more lines than no wrap
        assert!(after >= before);
    }

    #[test]
    fn set_wrap_same_mode_is_noop() {
        let mut view = TextView::new("hello", 10, WrapMode::None);
        let count1 = view.virtual_line_count();
        view.set_wrap(WrapMode::None); // same mode
        assert_eq!(view.virtual_line_count(), count1);
    }

    #[test]
    fn set_width_recomputes() {
        let mut view = TextView::new("abcdef", 3, WrapMode::Char);
        let count_narrow = view.virtual_line_count();
        view.set_width(100);
        let count_wide = view.virtual_line_count();
        assert!(count_narrow > count_wide);
    }

    #[test]
    fn set_width_same_is_noop() {
        let mut view = TextView::new("abc", 10, WrapMode::None);
        let count = view.virtual_line_count();
        view.set_width(10);
        assert_eq!(view.virtual_line_count(), count);
    }

    // ====== Accessors ======

    #[test]
    fn wrap_mode_accessor() {
        let view = TextView::new("abc", 10, WrapMode::Word);
        assert_eq!(view.wrap_mode(), WrapMode::Word);
    }

    #[test]
    fn width_accessor() {
        let view = TextView::new("abc", 42, WrapMode::None);
        assert_eq!(view.width(), 42);
    }

    // ====== max_width ======

    #[test]
    fn max_width_across_lines() {
        let view = TextView::new("ab\nabcde\nxy", 100, WrapMode::None);
        assert_eq!(view.max_width(), 5); // "abcde" is the widest
    }

    #[test]
    fn max_width_with_wide_chars() {
        let view = TextView::new("\u{4E16}\u{754C}", 100, WrapMode::None); // "世界" = 4 cells
        assert_eq!(view.max_width(), 4);
    }

    // ====== Scroll clamping ======

    #[test]
    fn clamp_scroll_within_bounds() {
        let view = TextView::new("a\nb\nc\nd", 10, WrapMode::None); // 4 lines
        assert_eq!(view.clamp_scroll(0, 2), 0);
        assert_eq!(view.clamp_scroll(1, 2), 1);
        assert_eq!(view.clamp_scroll(2, 2), 2); // max scroll for 4 lines, viewport 2
        assert_eq!(view.clamp_scroll(3, 2), 2); // clamped
        assert_eq!(view.clamp_scroll(100, 2), 2); // clamped
    }

    #[test]
    fn clamp_scroll_viewport_larger_than_content() {
        let view = TextView::new("a\nb", 10, WrapMode::None); // 2 lines
        // viewport 10 > 2 lines, max_scroll = 0
        assert_eq!(view.clamp_scroll(0, 10), 0);
        assert_eq!(view.clamp_scroll(5, 10), 0);
    }

    #[test]
    fn clamp_scroll_zero_viewport() {
        let view = TextView::new("a\nb\nc", 10, WrapMode::None);
        // viewport 0: clamp to total (min of scroll_y and total)
        let result = view.clamp_scroll(1, 0);
        assert_eq!(result, 1);
    }

    // ====== max_scroll ======

    #[test]
    fn max_scroll_basic() {
        let view = TextView::new("a\nb\nc\nd\ne", 10, WrapMode::None); // 5 lines
        assert_eq!(view.max_scroll(3), 2); // 5 - 3 = 2
        assert_eq!(view.max_scroll(5), 0); // exact fit
        assert_eq!(view.max_scroll(10), 0); // viewport bigger
    }

    #[test]
    fn max_scroll_zero_viewport() {
        let view = TextView::new("a\nb\nc", 10, WrapMode::None);
        assert_eq!(view.max_scroll(0), 3); // total lines
    }

    // ====== visible_range / visible_lines ======

    #[test]
    fn visible_range_basic() {
        let view = TextView::new("a\nb\nc\nd\ne", 10, WrapMode::None);
        assert_eq!(view.visible_range(0, 3), 0..3);
        assert_eq!(view.visible_range(1, 3), 1..4);
        assert_eq!(view.visible_range(2, 3), 2..5);
    }

    #[test]
    fn visible_range_zero_viewport() {
        let view = TextView::new("a\nb\nc", 10, WrapMode::None);
        assert_eq!(view.visible_range(0, 0), 0..0);
    }

    #[test]
    fn visible_lines_content() {
        let view = TextView::new("alpha\nbeta\ngamma\ndelta", 10, WrapMode::None);
        let visible = view.visible_lines(1, 2);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].text, "beta");
        assert_eq!(visible[1].text, "gamma");
    }

    // ====== scroll_to_line ======

    #[test]
    fn scroll_to_line_basic() {
        let view = TextView::new("a\nb\nc\nd\ne", 10, WrapMode::None);
        assert_eq!(view.scroll_to_line(0, 3), Some(0));
        assert_eq!(view.scroll_to_line(2, 3), Some(2));
        assert_eq!(view.scroll_to_line(4, 3), Some(2)); // clamped to max_scroll
    }

    #[test]
    fn scroll_to_line_nonexistent() {
        let view = TextView::new("a\nb", 10, WrapMode::None);
        assert_eq!(view.scroll_to_line(5, 2), None);
    }

    // ====== scroll_by_lines ======

    #[test]
    fn scroll_by_lines_positive() {
        let view = TextView::new("a\nb\nc\nd\ne", 10, WrapMode::None);
        assert_eq!(view.scroll_by_lines(0, 2, 3), 2);
        assert_eq!(view.scroll_by_lines(0, 100, 3), 2); // clamped
    }

    #[test]
    fn scroll_by_lines_negative() {
        let view = TextView::new("a\nb\nc\nd\ne", 10, WrapMode::None);
        assert_eq!(view.scroll_by_lines(2, -1, 3), 1);
        assert_eq!(view.scroll_by_lines(2, -100, 3), 0); // clamped to 0
    }

    // ====== scroll_by_pages ======

    #[test]
    fn scroll_by_pages_forward() {
        // 10 lines, viewport 3: pages move by 3
        let text = (0..10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let view = TextView::new(text.as_str(), 100, WrapMode::None);
        assert_eq!(view.scroll_by_pages(0, 1, 3), 3);
        assert_eq!(view.scroll_by_pages(0, 2, 3), 6);
    }

    #[test]
    fn scroll_by_pages_backward() {
        let text = (0..10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let view = TextView::new(text.as_str(), 100, WrapMode::None);
        assert_eq!(view.scroll_by_pages(6, -1, 3), 3);
        assert_eq!(view.scroll_by_pages(6, -3, 3), 0); // clamps to 0
    }

    #[test]
    fn scroll_by_pages_zero_viewport() {
        let view = TextView::new("a\nb\nc", 10, WrapMode::None);
        // zero viewport: should not panic, return clamped
        let result = view.scroll_by_pages(0, 1, 0);
        assert_eq!(result, 0);
    }

    // ====== scroll_to_top / scroll_to_bottom ======

    #[test]
    fn scroll_to_top_and_bottom() {
        let view = TextView::new("a\nb\nc\nd\ne", 10, WrapMode::None);
        assert_eq!(view.scroll_to_top(), 0);
        assert_eq!(view.scroll_to_bottom(3), 2);
        assert_eq!(view.scroll_to_bottom(5), 0);
        assert_eq!(view.scroll_to_bottom(1), 4);
    }

    // ====== Trailing newline ======

    #[test]
    fn trailing_newline_text() {
        let view = TextView::new("a\nb\n", 10, WrapMode::None);
        assert_eq!(view.source_line_count(), 3);
        // Last source line is empty
        let lines = view.lines();
        assert_eq!(lines.last().unwrap().text, "");
    }

    // ====== Only newlines ======

    #[test]
    fn only_newlines() {
        let view = TextView::new("\n\n\n", 10, WrapMode::None);
        assert_eq!(view.source_line_count(), 4); // 3 newlines = 4 lines
        assert_eq!(view.virtual_line_count(), 4);
        for line in view.lines() {
            assert_eq!(line.text, "");
        }
    }

    // ====== ViewLine fields ======

    #[test]
    fn view_line_source_line_tracking() {
        let view = TextView::new("ab\ncd\nef", 10, WrapMode::None);
        for (i, line) in view.lines().iter().enumerate() {
            assert_eq!(line.source_line, i);
            assert!(!line.is_wrap);
        }
    }

    #[test]
    fn view_line_width_tracking() {
        let view = TextView::new("ab\nabcde\n\u{4E16}", 10, WrapMode::None);
        assert_eq!(view.lines()[0].width, 2);
        assert_eq!(view.lines()[1].width, 5);
        assert_eq!(view.lines()[2].width, 2); // CJK = 2 cells
    }

    // ====== Viewport struct ======

    #[test]
    fn viewport_default() {
        let v = Viewport::default();
        assert_eq!(v.width, 0);
        assert_eq!(v.height, 0);
    }

    #[test]
    fn viewport_equality() {
        assert_eq!(Viewport::new(80, 24), Viewport::new(80, 24));
        assert_ne!(Viewport::new(80, 24), Viewport::new(120, 24));
    }
}

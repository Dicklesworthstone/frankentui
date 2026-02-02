#![forbid(unsafe_code)]

//! Text view utilities for scrollable, wrapped display.
//!
//! The view precomputes "virtual lines" produced by wrapping so callers can
//! perform deterministic viewport math (scroll by line/page, map source lines
//! to wrapped lines, and compute visible ranges) without duplicating logic.

use crate::wrap::{WrapMode, WrapOptions, display_width, wrap_with_options};
use std::ops::Range;

/// Viewport size in terminal cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Viewport {
    pub width: usize,
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
    text: String,
    wrap: WrapMode,
    width: usize,
    lines: Vec<ViewLine>,
    max_width: usize,
    source_line_count: usize,
}

impl TextView {
    /// Build a view from raw text, wrap mode, and viewport width.
    #[must_use]
    pub fn new(text: impl Into<String>, width: usize, wrap: WrapMode) -> Self {
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
    pub fn set_text(&mut self, text: impl Into<String>) {
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

        for (source_line, line) in self.text.split('\n').enumerate() {
            source_lines += 1;

            let wrapped = wrap_with_options(line, &options);
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
}

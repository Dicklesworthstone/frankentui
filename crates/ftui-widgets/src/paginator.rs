#![forbid(unsafe_code)]

//! Paginator widget.

use crate::{Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use unicode_width::UnicodeWidthStr;

/// Display mode for the paginator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaginatorMode {
    /// Render as "Page X/Y".
    Page,
    /// Render as "X/Y".
    Compact,
    /// Render as dot indicators (e.g. "**.*").
    Dots,
}

/// A simple paginator widget for page indicators.
#[derive(Debug, Clone)]
pub struct Paginator<'a> {
    current_page: u64,
    total_pages: u64,
    mode: PaginatorMode,
    style: Style,
    active_symbol: &'a str,
    inactive_symbol: &'a str,
}

impl<'a> Default for Paginator<'a> {
    fn default() -> Self {
        Self {
            current_page: 0,
            total_pages: 0,
            mode: PaginatorMode::Compact,
            style: Style::default(),
            active_symbol: "*",
            inactive_symbol: ".",
        }
    }
}

impl<'a> Paginator<'a> {
    /// Create a new paginator with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a paginator with the provided page counts.
    pub fn with_pages(current_page: u64, total_pages: u64) -> Self {
        Self::default()
            .current_page(current_page)
            .total_pages(total_pages)
    }

    /// Set the current page (1-based).
    pub fn current_page(mut self, current_page: u64) -> Self {
        self.current_page = current_page;
        self
    }

    /// Set the total pages.
    pub fn total_pages(mut self, total_pages: u64) -> Self {
        self.total_pages = total_pages;
        self
    }

    /// Set the display mode.
    pub fn mode(mut self, mode: PaginatorMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set the overall style for the paginator text.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the symbols used for dot mode.
    pub fn dots_symbols(mut self, active: &'a str, inactive: &'a str) -> Self {
        self.active_symbol = active;
        self.inactive_symbol = inactive;
        self
    }

    fn normalized_pages(&self) -> (u64, u64) {
        let total = self.total_pages;
        if total == 0 {
            return (0, 0);
        }
        let current = self.current_page.clamp(1, total);
        (current, total)
    }

    fn format_compact(&self) -> String {
        let (current, total) = self.normalized_pages();
        format!("{current}/{total}")
    }

    fn format_page(&self) -> String {
        let (current, total) = self.normalized_pages();
        format!("Page {current}/{total}")
    }

    fn format_dots(&self, max_width: usize) -> Option<String> {
        let (current, total) = self.normalized_pages();
        if total == 0 || max_width == 0 {
            return None;
        }

        let active_width = UnicodeWidthStr::width(self.active_symbol);
        let inactive_width = UnicodeWidthStr::width(self.inactive_symbol);
        let symbol_width = active_width.max(inactive_width);
        if symbol_width == 0 {
            return None;
        }

        let max_dots = max_width / symbol_width;
        if max_dots == 0 {
            return None;
        }

        let total_usize = total as usize;
        if total_usize > max_dots {
            return None;
        }

        let mut out = String::new();
        for idx in 1..=total_usize {
            if idx as u64 == current {
                out.push_str(self.active_symbol);
            } else {
                out.push_str(self.inactive_symbol);
            }
        }

        if UnicodeWidthStr::width(out.as_str()) > max_width {
            return None;
        }
        Some(out)
    }

    fn format_for_width(&self, max_width: usize) -> String {
        if max_width == 0 {
            return String::new();
        }

        match self.mode {
            PaginatorMode::Page => self.format_page(),
            PaginatorMode::Compact => self.format_compact(),
            PaginatorMode::Dots => self
                .format_dots(max_width)
                .unwrap_or_else(|| self.format_compact()),
        }
    }
}

impl Widget for Paginator<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Paginator",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        if !deg.render_content() {
            return;
        }

        let style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };

        let text = self.format_for_width(area.width as usize);
        if text.is_empty() {
            return;
        }

        draw_text_span(frame, area.x, area.y, &text, style, area.right());
    }

    fn is_essential(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn compact_zero_total() {
        let pager = Paginator::new().mode(PaginatorMode::Compact);
        assert_eq!(pager.format_for_width(10), "0/0");
    }

    #[test]
    fn page_clamps_current() {
        let pager = Paginator::with_pages(10, 3).mode(PaginatorMode::Page);
        assert_eq!(pager.format_for_width(20), "Page 3/3");
    }

    #[test]
    fn compact_clamps_zero_current() {
        let pager = Paginator::with_pages(0, 5).mode(PaginatorMode::Compact);
        assert_eq!(pager.format_for_width(10), "1/5");
    }

    #[test]
    fn dots_basic() {
        let pager = Paginator::with_pages(3, 5).mode(PaginatorMode::Dots);
        assert_eq!(pager.format_for_width(10), "..*..");
    }

    #[test]
    fn dots_fallbacks_when_too_narrow() {
        let pager = Paginator::with_pages(5, 10).mode(PaginatorMode::Dots);
        assert_eq!(pager.format_for_width(5), "5/10");
    }

    #[test]
    fn compact_one_page() {
        let pager = Paginator::with_pages(1, 1).mode(PaginatorMode::Compact);
        assert_eq!(pager.format_for_width(10), "1/1");
    }

    #[test]
    fn page_one_page() {
        let pager = Paginator::with_pages(1, 1).mode(PaginatorMode::Page);
        assert_eq!(pager.format_for_width(20), "Page 1/1");
    }

    #[test]
    fn dots_one_page() {
        let pager = Paginator::with_pages(1, 1).mode(PaginatorMode::Dots);
        assert_eq!(pager.format_for_width(10), "*");
    }

    #[test]
    fn compact_large_counts() {
        let pager = Paginator::with_pages(999, 1000).mode(PaginatorMode::Compact);
        assert_eq!(pager.format_for_width(20), "999/1000");
    }

    #[test]
    fn page_large_counts() {
        let pager = Paginator::with_pages(42, 9999).mode(PaginatorMode::Page);
        assert_eq!(pager.format_for_width(30), "Page 42/9999");
    }

    #[test]
    fn zero_width_returns_empty() {
        let pager = Paginator::with_pages(1, 5).mode(PaginatorMode::Compact);
        assert_eq!(pager.format_for_width(0), "");
    }

    #[test]
    fn dots_zero_total() {
        let pager = Paginator::new().mode(PaginatorMode::Dots);
        // Falls back to compact: "0/0"
        assert_eq!(pager.format_for_width(10), "0/0");
    }

    #[test]
    fn page_zero_total() {
        let pager = Paginator::new().mode(PaginatorMode::Page);
        assert_eq!(pager.format_for_width(20), "Page 0/0");
    }

    #[test]
    fn dots_first_page() {
        let pager = Paginator::with_pages(1, 5).mode(PaginatorMode::Dots);
        assert_eq!(pager.format_for_width(10), "*...." );
    }

    #[test]
    fn dots_last_page() {
        let pager = Paginator::with_pages(5, 5).mode(PaginatorMode::Dots);
        assert_eq!(pager.format_for_width(10), "....*");
    }

    #[test]
    fn dots_custom_symbols() {
        let pager = Paginator::with_pages(2, 4)
            .mode(PaginatorMode::Dots)
            .dots_symbols("●", "○");
        assert_eq!(pager.format_for_width(20), "○●○○");
    }

    #[test]
    fn builder_chain() {
        let pager = Paginator::new()
            .current_page(3)
            .total_pages(7)
            .mode(PaginatorMode::Compact)
            .style(Style::default());
        assert_eq!(pager.format_for_width(10), "3/7");
    }

    #[test]
    fn normalized_pages_clamps_high() {
        let pager = Paginator::with_pages(100, 5);
        let (cur, total) = pager.normalized_pages();
        assert_eq!(cur, 5);
        assert_eq!(total, 5);
    }

    #[test]
    fn normalized_pages_clamps_zero() {
        let pager = Paginator::with_pages(0, 5);
        let (cur, total) = pager.normalized_pages();
        assert_eq!(cur, 1);
        assert_eq!(total, 5);
    }

    #[test]
    fn normalized_pages_zero_total() {
        let pager = Paginator::new();
        let (cur, total) = pager.normalized_pages();
        assert_eq!(cur, 0);
        assert_eq!(total, 0);
    }

    #[test]
    fn render_on_empty_area() {
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        let pager = Paginator::with_pages(1, 5);
        pager.render(area, &mut frame);
        // No panic, nothing drawn
    }

    #[test]
    fn render_compact() {
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let pager = Paginator::with_pages(2, 5).mode(PaginatorMode::Compact);
        pager.render(area, &mut frame);
        let mut text = String::new();
        for x in 0..10u16 {
            if let Some(cell) = frame.buffer.get(x, 0) {
                if let Some(ch) = cell.content.as_char() {
                    text.push(ch);
                }
            }
        }
        assert!(text.starts_with("2/5"), "got: {text}");
    }

    #[test]
    fn is_essential() {
        let pager = Paginator::new();
        assert!(pager.is_essential());
    }

    #[test]
    fn default_mode_is_compact() {
        let pager = Paginator::new();
        assert_eq!(pager.mode, PaginatorMode::Compact);
    }

    #[test]
    fn with_pages_constructor() {
        let pager = Paginator::with_pages(3, 10);
        assert_eq!(pager.current_page, 3);
        assert_eq!(pager.total_pages, 10);
    }
}

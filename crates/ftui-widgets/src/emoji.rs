//! Emoji widget for rendering emoji characters with width-aware layout.
//!
//! Renders emoji text into a [`Frame`] respecting terminal width rules.
//! Provides fallback behavior for unsupported or ambiguous-width emoji.
//!
//! # Example
//!
//! ```
//! use ftui_widgets::emoji::Emoji;
//!
//! let emoji = Emoji::new("🎉");
//! assert_eq!(emoji.text(), "🎉");
//!
//! let with_fallback = Emoji::new("🦀").with_fallback("[crab]");
//! assert_eq!(with_fallback.fallback(), Some("[crab]"));
//! ```

use crate::{Widget, draw_text_span};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::wrap::display_width;

/// Widget for rendering emoji with width awareness and fallback.
#[derive(Debug, Clone)]
pub struct Emoji {
    /// The emoji text to display.
    text: String,
    /// Fallback text for terminals that cannot render the emoji.
    fallback: Option<String>,
    /// Style applied to the emoji.
    style: Style,
    /// Style applied to fallback text.
    fallback_style: Style,
}

impl Emoji {
    /// Create a new emoji widget.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            fallback: None,
            style: Style::default(),
            fallback_style: Style::default(),
        }
    }

    /// Set fallback text shown when emoji can't be rendered.
    #[must_use]
    pub fn with_fallback(mut self, fallback: impl Into<String>) -> Self {
        self.fallback = Some(fallback.into());
        self
    }

    /// Set the style for the emoji.
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the style for fallback text.
    #[must_use]
    pub fn with_fallback_style(mut self, style: Style) -> Self {
        self.fallback_style = style;
        self
    }

    /// Get the emoji text.
    #[inline]
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Get the fallback text, if set.
    #[inline]
    #[must_use]
    pub fn fallback(&self) -> Option<&str> {
        self.fallback.as_deref()
    }

    /// Compute the display width of the emoji.
    #[inline]
    #[must_use]
    pub fn width(&self) -> usize {
        display_width(&self.text)
    }

    /// Compute the display width of the fallback (or emoji if no fallback).
    #[must_use]
    pub fn effective_width(&self) -> usize {
        match &self.fallback {
            Some(fb) => display_width(fb),
            None => self.width(),
        }
    }

    /// Whether to use fallback based on emoji support.
    #[must_use]
    pub fn should_use_fallback(&self, use_emoji: bool) -> bool {
        !use_emoji && self.fallback.is_some()
    }
}

impl Widget for Emoji {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 || self.text.is_empty() {
            return;
        }

        let deg = frame.buffer.degradation;
        let max_x = area.right();

        // Use emoji directly if styling is available, otherwise try fallback
        if deg.apply_styling() {
            draw_text_span(frame, area.x, area.y, &self.text, self.style, max_x);
        } else if let Some(fb) = &self.fallback {
            draw_text_span(frame, area.x, area.y, fb, self.fallback_style, max_x);
        } else {
            draw_text_span(frame, area.x, area.y, &self.text, Style::default(), max_x);
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn new_emoji() {
        let e = Emoji::new("🎉");
        assert_eq!(e.text(), "🎉");
        assert!(e.fallback().is_none());
    }

    #[test]
    fn with_fallback() {
        let e = Emoji::new("🦀").with_fallback("[crab]");
        assert_eq!(e.fallback(), Some("[crab]"));
    }

    #[test]
    fn width_measurement() {
        let e = Emoji::new("🎉");
        // Emoji typically 2 cells wide
        assert!(e.width() > 0);
    }

    #[test]
    fn effective_width_with_fallback() {
        let e = Emoji::new("🦀").with_fallback("[crab]");
        assert_eq!(e.effective_width(), 6); // "[crab]" = 6 chars
    }

    #[test]
    fn effective_width_without_fallback() {
        let e = Emoji::new("🎉");
        assert_eq!(e.effective_width(), e.width());
    }

    #[test]
    fn render_basic() {
        let e = Emoji::new("A");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let area = Rect::new(0, 0, 10, 1);
        e.render(area, &mut frame);

        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('A'));
    }

    #[test]
    fn render_zero_area() {
        let e = Emoji::new("🎉");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        e.render(Rect::new(0, 0, 0, 0), &mut frame); // No panic
    }

    #[test]
    fn render_empty_text() {
        let e = Emoji::new("");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        e.render(Rect::new(0, 0, 10, 1), &mut frame); // No panic
    }

    #[test]
    fn is_not_essential() {
        let e = Emoji::new("🎉");
        assert!(!e.is_essential());
    }

    #[test]
    fn multi_char_emoji() {
        let e = Emoji::new("👩‍💻");
        assert!(e.width() > 0);
    }

    #[test]
    fn text_as_emoji() {
        // Simple text should work too
        let e = Emoji::new("OK");
        assert_eq!(e.width(), 2);
    }

    #[test]
    fn should_use_fallback_logic() {
        let e = Emoji::new("🎉").with_fallback("(party)");
        assert!(e.should_use_fallback(false));
        assert!(!e.should_use_fallback(true));
    }

    #[test]
    fn should_not_use_fallback_without_setting() {
        let e = Emoji::new("🎉");
        assert!(!e.should_use_fallback(false));
    }
}

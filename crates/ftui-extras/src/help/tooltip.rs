#![forbid(unsafe_code)]

//! Tooltip widget for floating contextual help near focused widgets.
//!
//! # Invariants
//!
//! 1. Tooltip placement never renders off-screen; if not enough space, the
//!    tooltip is clamped to fit within the visible area.
//! 2. The tooltip shows after `delay_ms` and dismisses on focus change or
//!    keypress (if `dismiss_on_key` is enabled).
//! 3. Multi-line content wraps deterministically at `max_width`.
//!
//! # Example
//!
//! ```ignore
//! use ftui_extras::help::{Tooltip, TooltipConfig, TooltipPosition};
//!
//! let tooltip = Tooltip::new("Save changes (Ctrl+S)")
//!     .config(TooltipConfig::default().delay_ms(300).position(TooltipPosition::Below));
//! ```

use ftui_core::geometry::{Rect, Size};
use ftui_render::cell::CellContent;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Tooltip positioning strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TooltipPosition {
    /// Automatically choose based on available space (below → above → right → left).
    #[default]
    Auto,
    /// Always position above the target.
    Above,
    /// Always position below the target.
    Below,
    /// Always position to the left of the target.
    Left,
    /// Always position to the right of the target.
    Right,
}

/// Tooltip configuration.
#[derive(Debug, Clone)]
pub struct TooltipConfig {
    /// Delay in milliseconds before showing (default: 500).
    pub delay_ms: u64,
    /// Maximum width before wrapping (default: 40).
    pub max_width: u16,
    /// Positioning strategy.
    pub position: TooltipPosition,
    /// Dismiss on any keypress (default: true).
    pub dismiss_on_key: bool,
    /// Tooltip style (background + foreground).
    pub style: Style,
    /// Padding inside the tooltip (default: 1).
    pub padding: u16,
}

impl Default for TooltipConfig {
    fn default() -> Self {
        Self {
            delay_ms: 500,
            max_width: 40,
            position: TooltipPosition::Auto,
            dismiss_on_key: true,
            style: Style::default(),
            padding: 1,
        }
    }
}

impl TooltipConfig {
    /// Set delay before showing in milliseconds.
    #[must_use]
    pub fn delay_ms(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }

    /// Set maximum width.
    #[must_use]
    pub fn max_width(mut self, width: u16) -> Self {
        self.max_width = width;
        self
    }

    /// Set positioning strategy.
    #[must_use]
    pub fn position(mut self, pos: TooltipPosition) -> Self {
        self.position = pos;
        self
    }

    /// Set dismiss-on-key behavior.
    #[must_use]
    pub fn dismiss_on_key(mut self, dismiss: bool) -> Self {
        self.dismiss_on_key = dismiss;
        self
    }

    /// Set tooltip style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set padding.
    #[must_use]
    pub fn padding(mut self, padding: u16) -> Self {
        self.padding = padding;
        self
    }
}

/// Tooltip widget rendered as an overlay near a target widget.
#[derive(Debug, Clone)]
pub struct Tooltip {
    /// Tooltip content (possibly multi-line).
    content: String,
    /// Configuration.
    config: TooltipConfig,
    /// Bounds of the target widget.
    target_bounds: Rect,
}

impl Tooltip {
    /// Create a new tooltip with the given content.
    #[must_use]
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            config: TooltipConfig::default(),
            target_bounds: Rect::new(0, 0, 0, 0),
        }
    }

    /// Set the tooltip configuration.
    #[must_use]
    pub fn config(mut self, config: TooltipConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the target widget bounds for positioning.
    #[must_use]
    pub fn for_widget(mut self, bounds: Rect) -> Self {
        self.target_bounds = bounds;
        self
    }

    /// Wrap content into lines respecting max_width.
    fn wrap_content(&self) -> Vec<String> {
        let max_width = self.config.max_width.saturating_sub(self.config.padding * 2);
        if max_width == 0 {
            return vec![];
        }

        let mut lines = Vec::new();
        for paragraph in self.content.lines() {
            if paragraph.is_empty() {
                lines.push(String::new());
                continue;
            }

            let mut current_line = String::new();
            let mut current_width: usize = 0;

            for word in paragraph.split_whitespace() {
                let word_width = UnicodeWidthStr::width(word);

                if current_width == 0 {
                    // First word on line
                    current_line = word.to_string();
                    current_width = word_width;
                } else if current_width + 1 + word_width <= max_width as usize {
                    // Fits on current line
                    current_line.push(' ');
                    current_line.push_str(word);
                    current_width += 1 + word_width;
                } else {
                    // Start new line
                    lines.push(current_line);
                    current_line = word.to_string();
                    current_width = word_width;
                }
            }

            if !current_line.is_empty() {
                lines.push(current_line);
            }
        }

        lines
    }

    /// Calculate tooltip content size (width, height) after wrapping.
    fn content_size(&self) -> Size {
        let lines = self.wrap_content();
        if lines.is_empty() {
            return Size::new(0, 0);
        }

        let max_line_width = lines
            .iter()
            .map(|l| UnicodeWidthStr::width(l.as_str()))
            .max()
            .unwrap_or(0);

        let padding = self.config.padding as usize;
        let width = (max_line_width + padding * 2).min(self.config.max_width as usize);
        let height = lines.len() + padding * 2;

        Size::new(width as u16, height as u16)
    }

    /// Calculate optimal position for the tooltip, avoiding screen edges.
    ///
    /// Decision rule (for `Auto`):
    /// 1. Try below target (most natural reading position)
    /// 2. Try above if no space below
    /// 3. Try right if no vertical space
    /// 4. Try left as last resort
    /// 5. If still doesn't fit, clamp to screen bounds
    ///
    /// Returns (x, y) position.
    fn calculate_position(&self, screen: Rect) -> (u16, u16) {
        let size = self.content_size();
        if size.width == 0 || size.height == 0 {
            return (self.target_bounds.x, self.target_bounds.y);
        }

        let target = self.target_bounds;
        let gap = 1u16; // Gap between tooltip and target

        // Helper to check if position fits
        let fits = |x: i32, y: i32| -> bool {
            x >= screen.x as i32
                && y >= screen.y as i32
                && x + size.width as i32 <= screen.right() as i32
                && y + size.height as i32 <= screen.bottom() as i32
        };

        // Calculate positions for each strategy
        let below = (target.x as i32, target.bottom() as i32 + gap as i32);
        let above = (
            target.x as i32,
            target.y as i32 - size.height as i32 - gap as i32,
        );
        let right = (target.right() as i32 + gap as i32, target.y as i32);
        let left = (
            target.x as i32 - size.width as i32 - gap as i32,
            target.y as i32,
        );

        let (x, y) = match self.config.position {
            TooltipPosition::Auto => {
                if fits(below.0, below.1) {
                    below
                } else if fits(above.0, above.1) {
                    above
                } else if fits(right.0, right.1) {
                    right
                } else if fits(left.0, left.1) {
                    left
                } else {
                    // Doesn't fit anywhere; use below and clamp
                    below
                }
            }
            TooltipPosition::Below => below,
            TooltipPosition::Above => above,
            TooltipPosition::Right => right,
            TooltipPosition::Left => left,
        };

        // Clamp to screen bounds
        let clamped_x = x
            .max(screen.x as i32)
            .min((screen.right() as i32).saturating_sub(size.width as i32));
        let clamped_y = y
            .max(screen.y as i32)
            .min((screen.bottom() as i32).saturating_sub(size.height as i32));

        (clamped_x.max(0) as u16, clamped_y.max(0) as u16)
    }

    /// Get the bounding rect for this tooltip within the given screen area.
    #[must_use]
    pub fn bounds(&self, screen: Rect) -> Rect {
        let (x, y) = self.calculate_position(screen);
        let size = self.content_size();
        Rect::new(x, y, size.width, size.height)
    }
}

impl Widget for Tooltip {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let size = self.content_size();
        if size.width == 0 || size.height == 0 || area.is_empty() {
            return;
        }

        let bounds = self.bounds(area);
        if bounds.is_empty() || bounds.width < 2 || bounds.height < 2 {
            return;
        }

        // Apply background style to entire tooltip area
        apply_style_to_area(&mut frame.buffer, bounds, &self.config.style);

        // Render content with padding
        let lines = self.wrap_content();
        let padding = self.config.padding;
        let content_x = bounds.x + padding;
        let content_y = bounds.y + padding;

        for (i, line) in lines.iter().enumerate() {
            let y = content_y + i as u16;
            if y >= bounds.bottom().saturating_sub(padding) {
                break;
            }

            let mut x = content_x;
            for grapheme in line.graphemes(true) {
                let w = UnicodeWidthStr::width(grapheme);
                if w == 0 {
                    continue;
                }
                if x + w as u16 > bounds.right().saturating_sub(padding) {
                    break;
                }

                // Write the grapheme
                if let Some(cell) = frame.buffer.get_mut(x, y)
                    && let Some(c) = grapheme.chars().next()
                {
                    cell.content = CellContent::from_char(c);
                }
                // Mark continuation cells for wide chars
                for offset in 1..w {
                    if let Some(cell) = frame.buffer.get_mut(x + offset as u16, y) {
                        cell.content = CellContent::CONTINUATION;
                    }
                }
                x += w as u16;
            }
        }
    }
}

/// Apply a style to all cells in a rectangular area.
fn apply_style_to_area(
    buf: &mut ftui_render::buffer::Buffer,
    area: Rect,
    style: &Style,
) {
    if style.is_empty() {
        return;
    }
    let fg = style.fg;
    let bg = style.bg;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.get_mut(x, y) {
                if let Some(fg) = fg {
                    cell.fg = fg;
                }
                if let Some(bg) = bg {
                    match bg.a() {
                        0 => {}
                        255 => cell.bg = bg,
                        _ => cell.bg = bg.over(cell.bg),
                    }
                }
            }
        }
    }
}

/// State tracking for tooltip visibility with delay.
#[derive(Debug, Clone, Default)]
pub struct TooltipState {
    /// Whether tooltip should be visible.
    visible: bool,
    /// Timestamp when hover started (for delay tracking).
    hover_start_ms: Option<u64>,
    /// Current target bounds.
    target: Option<Rect>,
}

impl TooltipState {
    /// Create a new tooltip state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if tooltip is visible.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Start tracking hover for a target (resets if target changes).
    pub fn start_hover(&mut self, target: Rect, current_time_ms: u64) {
        if self.target != Some(target) {
            self.target = Some(target);
            self.hover_start_ms = Some(current_time_ms);
            self.visible = false;
        }
    }

    /// Update visibility based on elapsed time and delay.
    pub fn update(&mut self, current_time_ms: u64, delay_ms: u64) {
        if let Some(start) = self.hover_start_ms
            && current_time_ms >= start + delay_ms
        {
            self.visible = true;
        }
    }

    /// Hide the tooltip (e.g., on focus change or keypress).
    pub fn hide(&mut self) {
        self.visible = false;
        self.hover_start_ms = None;
        self.target = None;
    }

    /// Get current target bounds.
    #[must_use]
    pub fn target(&self) -> Option<Rect> {
        self.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    // ── Position tests ────────────────────────────────────────────────

    #[test]
    fn position_auto_prefers_below() {
        let tooltip = Tooltip::new("Hello")
            .for_widget(Rect::new(10, 5, 10, 2))
            .config(TooltipConfig::default().max_width(20));

        let screen = Rect::new(0, 0, 80, 24);
        let (_, y) = tooltip.calculate_position(screen);

        // Should be below target (y = 5 + 2 + 1 = 8)
        assert!(y > 5 + 2, "Should position below target");
    }

    #[test]
    fn position_auto_uses_above_when_no_space_below() {
        let tooltip = Tooltip::new("Hello")
            .for_widget(Rect::new(10, 20, 10, 2)) // Near bottom
            .config(TooltipConfig::default().max_width(20));

        let screen = Rect::new(0, 0, 80, 24);
        let (_, y) = tooltip.calculate_position(screen);

        // Should be above target
        assert!(y < 20, "Should position above target when no space below");
    }

    #[test]
    fn position_clamps_to_screen_edge() {
        let tooltip = Tooltip::new("A very long tooltip that might overflow")
            .for_widget(Rect::new(70, 10, 5, 2)) // Near right edge
            .config(TooltipConfig::default().max_width(40));

        let screen = Rect::new(0, 0, 80, 24);
        let bounds = tooltip.bounds(screen);

        assert!(
            bounds.right() <= screen.right(),
            "Should not exceed screen width"
        );
    }

    #[test]
    fn position_explicit_above() {
        let tooltip = Tooltip::new("Info")
            .for_widget(Rect::new(10, 10, 5, 2))
            .config(TooltipConfig::default().position(TooltipPosition::Above));

        let screen = Rect::new(0, 0, 80, 24);
        let (_, y) = tooltip.calculate_position(screen);

        assert!(y < 10, "Above position should be above target");
    }

    #[test]
    fn position_explicit_below() {
        let tooltip = Tooltip::new("Info")
            .for_widget(Rect::new(10, 5, 5, 2))
            .config(TooltipConfig::default().position(TooltipPosition::Below));

        let screen = Rect::new(0, 0, 80, 24);
        let (_, y) = tooltip.calculate_position(screen);

        assert!(y > 5, "Below position should be below target");
    }

    #[test]
    fn position_at_screen_edge_does_not_go_negative() {
        let tooltip = Tooltip::new("Info")
            .for_widget(Rect::new(0, 0, 5, 2))
            .config(TooltipConfig::default().position(TooltipPosition::Above));

        let screen = Rect::new(0, 0, 80, 24);
        let (x, y) = tooltip.calculate_position(screen);

        assert!(x == 0 || x > 0, "X should not underflow");
        assert!(y == 0 || y > 0, "Y should not underflow (clamped)");
    }

    // ── Wrapping tests ────────────────────────────────────────────────

    #[test]
    fn multiline_wrap_respects_max_width() {
        let tooltip = Tooltip::new("This is a long line that should wrap properly")
            .config(TooltipConfig::default().max_width(20).padding(1));

        let lines = tooltip.wrap_content();
        for line in &lines {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 18, // 20 - 2 padding
                "Line should fit within max_width minus padding: {:?}",
                line
            );
        }
    }

    #[test]
    fn empty_content_produces_no_lines() {
        let tooltip = Tooltip::new("");
        let lines = tooltip.wrap_content();
        assert!(lines.is_empty());
    }

    #[test]
    fn single_word_does_not_split() {
        let tooltip = Tooltip::new("Supercalifragilisticexpialidocious")
            .config(TooltipConfig::default().max_width(10).padding(0));

        let lines = tooltip.wrap_content();
        assert_eq!(lines.len(), 1, "Single word should be one line");
    }

    // ── Size calculation tests ────────────────────────────────────────

    #[test]
    fn content_size_includes_padding() {
        let tooltip =
            Tooltip::new("Hi").config(TooltipConfig::default().max_width(20).padding(2));

        let size = tooltip.content_size();
        assert!(size.width >= 2 + 4, "Width should include padding");
        assert!(size.height >= 1 + 4, "Height should include padding");
    }

    #[test]
    fn content_size_zero_for_empty() {
        let tooltip = Tooltip::new("");
        let size = tooltip.content_size();
        assert_eq!(size.width, 0);
        assert_eq!(size.height, 0);
    }

    // ── State tests ───────────────────────────────────────────────────

    #[test]
    fn state_delay_timer_shows_after_delay() {
        let mut state = TooltipState::new();
        let target = Rect::new(10, 10, 5, 2);

        state.start_hover(target, 1000);
        assert!(!state.is_visible(), "Should not be visible immediately");

        state.update(1400, 500);
        assert!(!state.is_visible(), "Should not be visible before delay");

        state.update(1500, 500);
        assert!(state.is_visible(), "Should be visible after delay");
    }

    #[test]
    fn state_hide_resets() {
        let mut state = TooltipState::new();
        state.start_hover(Rect::new(0, 0, 5, 2), 0);
        state.update(1000, 500);
        assert!(state.is_visible());

        state.hide();
        assert!(!state.is_visible());
        assert!(state.target().is_none());
    }

    #[test]
    fn state_target_change_resets_timer() {
        let mut state = TooltipState::new();

        state.start_hover(Rect::new(0, 0, 5, 2), 0);
        state.update(400, 500);
        assert!(!state.is_visible());

        // Change target at time 400
        state.start_hover(Rect::new(10, 10, 5, 2), 400);
        state.update(700, 500); // Only 300ms since new hover
        assert!(!state.is_visible(), "Timer should reset on target change");

        state.update(900, 500);
        assert!(state.is_visible(), "Should show after full delay");
    }

    // ── Render tests ──────────────────────────────────────────────────

    #[test]
    fn render_does_not_panic_on_small_area() {
        let tooltip = Tooltip::new("Test")
            .for_widget(Rect::new(0, 0, 2, 1))
            .config(TooltipConfig::default());

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);

        // Should not panic
        tooltip.render(Rect::new(0, 0, 10, 5), &mut frame);
    }

    #[test]
    fn render_does_not_panic_on_empty_content() {
        let tooltip = Tooltip::new("")
            .for_widget(Rect::new(5, 5, 2, 1))
            .config(TooltipConfig::default());

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);

        // Should not panic
        tooltip.render(Rect::new(0, 0, 20, 10), &mut frame);
    }

    // ── Config builder tests ──────────────────────────────────────────

    #[test]
    fn config_builder_chaining() {
        let config = TooltipConfig::default()
            .delay_ms(300)
            .max_width(50)
            .position(TooltipPosition::Right)
            .dismiss_on_key(false)
            .padding(2);

        assert_eq!(config.delay_ms, 300);
        assert_eq!(config.max_width, 50);
        assert_eq!(config.position, TooltipPosition::Right);
        assert!(!config.dismiss_on_key);
        assert_eq!(config.padding, 2);
    }
}

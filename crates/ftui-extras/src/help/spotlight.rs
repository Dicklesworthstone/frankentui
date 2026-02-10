#![forbid(unsafe_code)]

//! Spotlight overlay widget for highlighting target widgets during tours.
//!
//! # Invariants
//!
//! 1. The spotlight creates a dimmed overlay with a "cutout" for the target.
//! 2. The info panel is positioned to avoid obscuring the target.
//! 3. Animation state is deterministic given elapsed time.
//!
//! # Example
//!
//! ```ignore
//! use ftui_extras::help::{Spotlight, SpotlightConfig};
//!
//! let spotlight = Spotlight::new()
//!     .target(Rect::new(10, 5, 20, 3))
//!     .title("Search Bar")
//!     .content("Use this to find items quickly.");
//! ```

use ftui_core::geometry::Rect;
use ftui_render::cell::{CellContent, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_widgets::Widget;
use unicode_display_width::width as unicode_display_width;
use unicode_segmentation::UnicodeSegmentation;

#[inline]
fn width_u64_to_usize(width: u64) -> usize {
    width.min(usize::MAX as u64) as usize
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

fn grapheme_width(grapheme: &str) -> usize {
    if grapheme.is_ascii() {
        return ascii_display_width(grapheme);
    }
    if grapheme.chars().all(is_zero_width_codepoint) {
        return 0;
    }
    width_u64_to_usize(unicode_display_width(grapheme))
}

fn display_width(text: &str) -> usize {
    if text.is_ascii() && text.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return text.len();
    }
    if text.is_ascii() {
        return ascii_display_width(text);
    }
    if !text.chars().any(is_zero_width_codepoint) {
        return width_u64_to_usize(unicode_display_width(text));
    }
    text.graphemes(true).map(grapheme_width).sum()
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
            0x00AD | 0x034F | 0x180E | 0x200B | 0x200C | 0x200D | 0x200E | 0x200F | 0x2060 | 0xFEFF
        )
        || matches!(u, 0x202A..=0x202E | 0x2066..=0x2069 | 0x206A..=0x206F)
}

/// Spotlight configuration.
#[derive(Debug, Clone)]
pub struct SpotlightConfig {
    /// Overlay dim color (semi-transparent).
    pub overlay_color: PackedRgba,
    /// Info panel background color.
    pub panel_bg: PackedRgba,
    /// Info panel foreground color.
    pub panel_fg: PackedRgba,
    /// Title style.
    pub title_style: Style,
    /// Content style.
    pub content_style: Style,
    /// Navigation hint style.
    pub hint_style: Style,
    /// Padding around the target (spotlight "breathing room").
    pub target_padding: u16,
    /// Panel max width.
    pub panel_max_width: u16,
    /// Panel padding.
    pub panel_padding: u16,
    /// Show navigation hints.
    pub show_hints: bool,
}

impl Default for SpotlightConfig {
    fn default() -> Self {
        Self {
            overlay_color: PackedRgba::rgba(0, 0, 0, 180),
            panel_bg: PackedRgba::rgb(40, 44, 52),
            panel_fg: PackedRgba::rgb(220, 220, 220),
            title_style: Style::new().fg(PackedRgba::rgb(97, 175, 239)),
            content_style: Style::new().fg(PackedRgba::rgb(200, 200, 200)),
            hint_style: Style::new().fg(PackedRgba::rgb(140, 140, 140)),
            target_padding: 1,
            panel_max_width: 50,
            panel_padding: 1,
            show_hints: true,
        }
    }
}

impl SpotlightConfig {
    /// Set overlay color.
    #[must_use]
    pub fn overlay_color(mut self, color: PackedRgba) -> Self {
        self.overlay_color = color;
        self
    }

    /// Set panel background.
    #[must_use]
    pub fn panel_bg(mut self, color: PackedRgba) -> Self {
        self.panel_bg = color;
        self
    }

    /// Set panel foreground.
    #[must_use]
    pub fn panel_fg(mut self, color: PackedRgba) -> Self {
        self.panel_fg = color;
        self
    }

    /// Set title style.
    #[must_use]
    pub fn title_style(mut self, style: Style) -> Self {
        self.title_style = style;
        self
    }

    /// Set target padding.
    #[must_use]
    pub fn target_padding(mut self, padding: u16) -> Self {
        self.target_padding = padding;
        self
    }

    /// Set panel max width.
    #[must_use]
    pub fn panel_max_width(mut self, width: u16) -> Self {
        self.panel_max_width = width;
        self
    }

    /// Set whether to show navigation hints.
    #[must_use]
    pub fn show_hints(mut self, show: bool) -> Self {
        self.show_hints = show;
        self
    }
}

/// Position for the info panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPosition {
    /// Above the target.
    Above,
    /// Below the target.
    Below,
    /// Left of the target.
    Left,
    /// Right of the target.
    Right,
}

/// Spotlight overlay widget.
#[derive(Debug, Clone)]
pub struct Spotlight {
    /// Target bounds to highlight.
    target: Option<Rect>,
    /// Step title.
    title: String,
    /// Step content.
    content: String,
    /// Progress indicator (e.g., "2 of 5").
    progress: Option<String>,
    /// Navigation hints (e.g., "Enter: Next | Esc: Skip").
    hints: Option<String>,
    /// Configuration.
    config: SpotlightConfig,
    /// Force panel position.
    forced_position: Option<PanelPosition>,
}

impl Default for Spotlight {
    fn default() -> Self {
        Self::new()
    }
}

impl Spotlight {
    /// Create a new spotlight.
    #[must_use]
    pub fn new() -> Self {
        Self {
            target: None,
            title: String::new(),
            content: String::new(),
            progress: None,
            hints: None,
            config: SpotlightConfig::default(),
            forced_position: None,
        }
    }

    /// Set the target bounds.
    #[must_use]
    pub fn target(mut self, bounds: Rect) -> Self {
        self.target = Some(bounds);
        self
    }

    /// Set the title.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the content.
    #[must_use]
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    /// Set progress text.
    #[must_use]
    pub fn progress(mut self, progress: impl Into<String>) -> Self {
        self.progress = Some(progress.into());
        self
    }

    /// Set navigation hints.
    #[must_use]
    pub fn hints(mut self, hints: impl Into<String>) -> Self {
        self.hints = Some(hints.into());
        self
    }

    /// Set configuration.
    #[must_use]
    pub fn config(mut self, config: SpotlightConfig) -> Self {
        self.config = config;
        self
    }

    /// Force panel position.
    #[must_use]
    pub fn force_position(mut self, position: PanelPosition) -> Self {
        self.forced_position = Some(position);
        self
    }

    /// Get padded target bounds.
    fn padded_target(&self) -> Option<Rect> {
        self.target.map(|t| {
            let pad = self.config.target_padding;
            Rect::new(
                t.x.saturating_sub(pad),
                t.y.saturating_sub(pad),
                t.width + pad * 2,
                t.height + pad * 2,
            )
        })
    }

    /// Wrap text into lines respecting max width.
    fn wrap_text(&self, text: &str, max_width: usize) -> Vec<String> {
        if max_width == 0 {
            return vec![];
        }

        let mut lines = Vec::new();
        for paragraph in text.lines() {
            if paragraph.is_empty() {
                lines.push(String::new());
                continue;
            }

            let mut current_line = String::new();
            let mut current_width: usize = 0;

            for word in paragraph.split_whitespace() {
                let word_width = display_width(word);

                if current_width == 0 {
                    current_line = word.to_string();
                    current_width = word_width;
                } else if current_width + 1 + word_width <= max_width {
                    current_line.push(' ');
                    current_line.push_str(word);
                    current_width += 1 + word_width;
                } else {
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

    /// Calculate panel dimensions.
    fn panel_size(&self, screen: Rect) -> (u16, u16) {
        let padding = self.config.panel_padding as usize;
        let inner_width = (self.config.panel_max_width as usize).saturating_sub(padding * 2);

        let title_lines = self.wrap_text(&self.title, inner_width);
        let content_lines = self.wrap_text(&self.content, inner_width);

        let mut height = padding * 2;
        height += title_lines.len();
        if !content_lines.is_empty() {
            height += 1; // Spacing
            height += content_lines.len();
        }
        if self.progress.is_some() {
            height += 1;
        }
        if self.config.show_hints && self.hints.is_some() {
            height += 1;
        }

        let max_line_width = title_lines
            .iter()
            .chain(content_lines.iter())
            .map(|l| display_width(l.as_str()))
            .max()
            .unwrap_or(0);

        let width = (max_line_width + padding * 2)
            .min(self.config.panel_max_width as usize)
            .min(screen.width as usize);

        (width as u16, height as u16)
    }

    /// Calculate panel position.
    fn panel_position(&self, screen: Rect) -> (u16, u16, PanelPosition) {
        let (width, height) = self.panel_size(screen);
        let target =
            self.padded_target()
                .unwrap_or(Rect::new(screen.width / 2, screen.height / 2, 0, 0));

        let gap = 1u16;

        // Helper to check if position fits
        let fits = |x: i32, y: i32| -> bool {
            x >= screen.x as i32
                && y >= screen.y as i32
                && x + width as i32 <= screen.right() as i32
                && y + height as i32 <= screen.bottom() as i32
        };

        // Try positions in order: below, above, right, left
        let below = (target.x as i32, target.bottom() as i32 + gap as i32);
        let above = (
            target.x as i32,
            target.y as i32 - height as i32 - gap as i32,
        );
        let right = (target.right() as i32 + gap as i32, target.y as i32);
        let left = (target.x as i32 - width as i32 - gap as i32, target.y as i32);

        let (x, y, pos) = match self.forced_position {
            Some(PanelPosition::Below) => (below.0, below.1, PanelPosition::Below),
            Some(PanelPosition::Above) => (above.0, above.1, PanelPosition::Above),
            Some(PanelPosition::Right) => (right.0, right.1, PanelPosition::Right),
            Some(PanelPosition::Left) => (left.0, left.1, PanelPosition::Left),
            None => {
                if fits(below.0, below.1) {
                    (below.0, below.1, PanelPosition::Below)
                } else if fits(above.0, above.1) {
                    (above.0, above.1, PanelPosition::Above)
                } else if fits(right.0, right.1) {
                    (right.0, right.1, PanelPosition::Right)
                } else if fits(left.0, left.1) {
                    (left.0, left.1, PanelPosition::Left)
                } else {
                    // Default to below, clamped
                    (below.0, below.1, PanelPosition::Below)
                }
            }
        };

        // Clamp to screen bounds
        let clamped_x = x
            .max(screen.x as i32)
            .min((screen.right() as i32).saturating_sub(width as i32));
        let clamped_y = y
            .max(screen.y as i32)
            .min((screen.bottom() as i32).saturating_sub(height as i32));

        (clamped_x.max(0) as u16, clamped_y.max(0) as u16, pos)
    }

    /// Render the dimmed overlay (excluding target area).
    fn render_overlay(&self, frame: &mut Frame, area: Rect) {
        let target = self.padded_target();
        let overlay_color = self.config.overlay_color;

        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                // Skip the target cutout area
                if let Some(t) = target
                    && x >= t.x
                    && x < t.right()
                    && y >= t.y
                    && y < t.bottom()
                {
                    continue;
                }

                if let Some(cell) = frame.buffer.get_mut(x, y) {
                    // Blend overlay color onto existing background
                    match overlay_color.a() {
                        0 => {}
                        255 => cell.bg = overlay_color,
                        _ => cell.bg = overlay_color.over(cell.bg),
                    }
                }
            }
        }
    }

    /// Render the info panel.
    fn render_panel(&self, frame: &mut Frame, area: Rect) {
        let (px, py, _pos) = self.panel_position(area);
        let (width, height) = self.panel_size(area);
        let panel_rect = Rect::new(px, py, width, height);

        if panel_rect.is_empty() || width < 2 || height < 2 {
            return;
        }

        // Fill panel background
        for y in panel_rect.y..panel_rect.bottom() {
            for x in panel_rect.x..panel_rect.right() {
                if let Some(cell) = frame.buffer.get_mut(x, y) {
                    cell.bg = self.config.panel_bg;
                    cell.fg = self.config.panel_fg;
                    cell.content = CellContent::from_char(' ');
                }
            }
        }

        let padding = self.config.panel_padding;
        let inner_width = (width as usize).saturating_sub(padding as usize * 2);
        let mut row = panel_rect.y + padding;

        // Render title
        let title_lines = self.wrap_text(&self.title, inner_width);
        for line in &title_lines {
            if row >= panel_rect.bottom().saturating_sub(padding) {
                break;
            }
            self.render_line(
                frame,
                panel_rect.x + padding,
                row,
                line,
                &self.config.title_style,
                inner_width,
            );
            row += 1;
        }

        // Render content
        let content_lines = self.wrap_text(&self.content, inner_width);
        if !content_lines.is_empty() {
            row += 1; // Spacing
            for line in &content_lines {
                if row >= panel_rect.bottom().saturating_sub(padding) {
                    break;
                }
                self.render_line(
                    frame,
                    panel_rect.x + padding,
                    row,
                    line,
                    &self.config.content_style,
                    inner_width,
                );
                row += 1;
            }
        }

        // Render progress
        if let Some(ref progress) = self.progress
            && row < panel_rect.bottom().saturating_sub(padding)
        {
            self.render_line(
                frame,
                panel_rect.x + padding,
                row,
                progress,
                &self.config.hint_style,
                inner_width,
            );
            row += 1;
        }

        // Render hints
        if self.config.show_hints
            && let Some(ref hints) = self.hints
            && row < panel_rect.bottom().saturating_sub(padding)
        {
            self.render_line(
                frame,
                panel_rect.x + padding,
                row,
                hints,
                &self.config.hint_style,
                inner_width,
            );
        }
    }

    /// Render a single line of text.
    fn render_line(
        &self,
        frame: &mut Frame,
        start_x: u16,
        y: u16,
        text: &str,
        style: &Style,
        max_width: usize,
    ) {
        let mut x = start_x;
        let mut width_used = 0usize;

        for grapheme in text.graphemes(true) {
            let w = grapheme_width(grapheme);
            if w == 0 {
                continue;
            }
            if width_used + w > max_width {
                break;
            }

            if let Some(cell) = frame.buffer.get_mut(x, y)
                && let Some(c) = grapheme.chars().next()
            {
                cell.content = CellContent::from_char(c);
                if let Some(fg) = style.fg {
                    cell.fg = fg;
                }
                if let Some(bg) = style.bg {
                    cell.bg = bg;
                }
            }

            // Mark continuation cells for wide chars
            for offset in 1..w {
                if let Some(cell) = frame.buffer.get_mut(x + offset as u16, y) {
                    cell.content = CellContent::CONTINUATION;
                }
            }

            x += w as u16;
            width_used += w;
        }
    }

    /// Get the panel bounds for hit testing.
    #[must_use]
    pub fn panel_bounds(&self, screen: Rect) -> Rect {
        let (px, py, _) = self.panel_position(screen);
        let (width, height) = self.panel_size(screen);
        Rect::new(px, py, width, height)
    }
}

impl Widget for Spotlight {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        // First render the overlay
        self.render_overlay(frame, area);

        // Then render the info panel
        self.render_panel(frame, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    // ── Configuration tests ──────────────────────────────────────────────

    #[test]
    fn config_builder() {
        let config = SpotlightConfig::default()
            .target_padding(2)
            .panel_max_width(60)
            .show_hints(false);

        assert_eq!(config.target_padding, 2);
        assert_eq!(config.panel_max_width, 60);
        assert!(!config.show_hints);
    }

    // ── Spotlight construction ───────────────────────────────────────────

    #[test]
    fn spotlight_construction() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 5, 20, 3))
            .title("Test Title")
            .content("Test content here")
            .progress("Step 1 of 3")
            .hints("Enter: Next | Esc: Skip");

        assert_eq!(spotlight.title, "Test Title");
        assert_eq!(spotlight.content, "Test content here");
        assert_eq!(spotlight.progress, Some("Step 1 of 3".into()));
        assert_eq!(spotlight.hints, Some("Enter: Next | Esc: Skip".into()));
    }

    // ── Panel positioning ────────────────────────────────────────────────

    #[test]
    fn panel_prefers_below() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 5, 20, 3))
            .title("Test")
            .content("Content");

        let screen = Rect::new(0, 0, 80, 24);
        let (_, py, pos) = spotlight.panel_position(screen);

        assert_eq!(pos, PanelPosition::Below);
        assert!(py > 5 + 3, "Panel should be below target");
    }

    #[test]
    fn panel_uses_above_when_no_space_below() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 18, 20, 3)) // Near bottom
            .title("Test")
            .content("Content");

        let screen = Rect::new(0, 0, 80, 24);
        let (_, py, pos) = spotlight.panel_position(screen);

        assert_eq!(pos, PanelPosition::Above);
        assert!(py < 18, "Panel should be above target");
    }

    #[test]
    fn panel_forced_position() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 5, 20, 3))
            .title("Test")
            .force_position(PanelPosition::Right);

        let screen = Rect::new(0, 0, 80, 24);
        let (_, _, pos) = spotlight.panel_position(screen);

        assert_eq!(pos, PanelPosition::Right);
    }

    // ── Text wrapping ────────────────────────────────────────────────────

    #[test]
    fn text_wrap_respects_width() {
        let spotlight = Spotlight::new();
        let lines = spotlight.wrap_text("This is a long line that should wrap", 15);

        for line in &lines {
            assert!(
                display_width(line.as_str()) <= 15,
                "Line too wide: {:?}",
                line
            );
        }
    }

    #[test]
    fn text_wrap_empty() {
        let spotlight = Spotlight::new();
        let lines = spotlight.wrap_text("", 20);
        assert!(lines.is_empty());
    }

    // ── Render tests ─────────────────────────────────────────────────────

    #[test]
    fn render_does_not_panic() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 5, 20, 3))
            .title("Welcome")
            .content("This is a test spotlight.");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        spotlight.render(Rect::new(0, 0, 80, 24), &mut frame);
    }

    #[test]
    fn render_empty_area() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 5, 20, 3))
            .title("Test");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        // Should not panic
        spotlight.render(Rect::new(0, 0, 0, 0), &mut frame);
    }

    #[test]
    fn render_no_target() {
        let spotlight = Spotlight::new().title("Centered").content("No target");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        // Should render centered panel
        spotlight.render(Rect::new(0, 0, 80, 24), &mut frame);
    }

    // ── Panel bounds ─────────────────────────────────────────────────────

    #[test]
    fn panel_bounds_for_hit_testing() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 5, 20, 3))
            .title("Test")
            .content("Content");

        let screen = Rect::new(0, 0, 80, 24);
        let bounds = spotlight.panel_bounds(screen);

        assert!(bounds.width > 0);
        assert!(bounds.height > 0);
    }

    // ── Width utility tests ─────────────────────────────────────────────

    #[test]
    fn width_u64_to_usize_normal_and_overflow() {
        assert_eq!(width_u64_to_usize(42), 42);
        assert_eq!(width_u64_to_usize(0), 0);
        assert_eq!(width_u64_to_usize(u64::MAX), usize::MAX);
    }

    #[test]
    fn ascii_display_width_counts_printable_and_control() {
        assert_eq!(ascii_display_width("hello"), 5);
        assert_eq!(ascii_display_width(""), 0);
        assert_eq!(ascii_display_width("a\tb\nc"), 5); // tab/newline each count as 1
        assert_eq!(ascii_display_width("\r"), 1);
    }

    #[test]
    fn display_width_pure_ascii() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
        assert_eq!(display_width(" "), 1);
    }

    #[test]
    fn display_width_ascii_with_control() {
        assert_eq!(display_width("a\tb"), 3);
    }

    #[test]
    fn is_zero_width_combining_marks() {
        assert!(is_zero_width_codepoint('\u{0300}')); // Combining grave
        assert!(is_zero_width_codepoint('\u{0301}')); // Combining acute
        assert!(is_zero_width_codepoint('\u{200B}')); // Zero-width space
        assert!(is_zero_width_codepoint('\u{200D}')); // Zero-width joiner
        assert!(is_zero_width_codepoint('\u{FE0F}')); // Variation selector
        assert!(is_zero_width_codepoint('\u{FEFF}')); // BOM
        assert!(is_zero_width_codepoint('\u{00AD}')); // Soft hyphen
        assert!(!is_zero_width_codepoint('a'));
        assert!(!is_zero_width_codepoint(' '));
    }

    #[test]
    fn is_zero_width_control_chars() {
        assert!(is_zero_width_codepoint('\u{0000}')); // null
        assert!(is_zero_width_codepoint('\u{001F}')); // unit separator
        assert!(is_zero_width_codepoint('\u{007F}')); // DEL
        assert!(is_zero_width_codepoint('\u{009F}')); // APC
        assert!(!is_zero_width_codepoint('\u{0020}')); // space is not zero-width
    }

    #[test]
    fn is_zero_width_bidi_marks() {
        assert!(is_zero_width_codepoint('\u{202A}')); // LRE
        assert!(is_zero_width_codepoint('\u{202E}')); // RLO
        assert!(is_zero_width_codepoint('\u{2066}')); // LRI
        assert!(is_zero_width_codepoint('\u{2069}')); // PDI
        assert!(is_zero_width_codepoint('\u{200E}')); // LRM
        assert!(is_zero_width_codepoint('\u{200F}')); // RLM
    }

    // ── Text wrapping edge cases ────────────────────────────────────────

    #[test]
    fn text_wrap_zero_width_returns_empty() {
        let spotlight = Spotlight::new();
        let lines = spotlight.wrap_text("some text", 0);
        assert!(lines.is_empty());
    }

    #[test]
    fn text_wrap_preserves_paragraphs() {
        let spotlight = Spotlight::new();
        let lines = spotlight.wrap_text("first\n\nsecond", 40);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "first");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "second");
    }

    #[test]
    fn text_wrap_single_word_per_line() {
        let spotlight = Spotlight::new();
        let lines = spotlight.wrap_text("a b c d", 1);
        // Each word gets its own line
        assert_eq!(lines.len(), 4);
    }

    // ── Panel size/position edge cases ──────────────────────────────────

    #[test]
    fn panel_size_with_progress() {
        let without = Spotlight::new().title("T").content("C");
        let with_progress = Spotlight::new().title("T").content("C").progress("1/3");
        let screen = Rect::new(0, 0, 80, 24);
        let (_, h1) = without.panel_size(screen);
        let (_, h2) = with_progress.panel_size(screen);
        assert_eq!(h2, h1 + 1, "progress adds one line of height");
    }

    #[test]
    fn panel_size_with_hints() {
        let without = Spotlight::new().title("T").content("C");
        let with_hints = Spotlight::new().title("T").content("C").hints("Esc: Close");
        let screen = Rect::new(0, 0, 80, 24);
        let (_, h1) = without.panel_size(screen);
        let (_, h2) = with_hints.panel_size(screen);
        assert_eq!(h2, h1 + 1, "hints add one line of height");
    }

    #[test]
    fn panel_position_falls_back_to_right() {
        // Target in the middle, fill top and bottom
        // Screen too short for below or above, but wide enough for right
        let spotlight = Spotlight::new()
            .target(Rect::new(5, 3, 10, 14)) // Tall target in short screen
            .title("T");
        let screen = Rect::new(0, 0, 80, 20);
        let (_, _, pos) = spotlight.panel_position(screen);
        // Either right or left depending on space
        assert!(
            pos == PanelPosition::Right || pos == PanelPosition::Left,
            "should fall back to side position, got {pos:?}"
        );
    }

    #[test]
    fn panel_position_no_target_centers() {
        let spotlight = Spotlight::new().title("Centered");
        let screen = Rect::new(0, 0, 80, 24);
        let (px, py, _) = spotlight.panel_position(screen);
        // Without target, default target is center of screen
        // Panel should be near center
        assert!(px < 60, "panel x should be reasonable, got {px}");
        assert!(py < 20, "panel y should be reasonable, got {py}");
    }

    #[test]
    fn panel_forced_left_position() {
        let spotlight = Spotlight::new()
            .target(Rect::new(40, 10, 20, 3))
            .title("Test")
            .force_position(PanelPosition::Left);
        let screen = Rect::new(0, 0, 80, 24);
        let (_, _, pos) = spotlight.panel_position(screen);
        assert_eq!(pos, PanelPosition::Left);
    }

    // --- Additional edge case tests (bd-pl0rv) ---

    #[test]
    fn config_overlay_color_builder() {
        let color = PackedRgba::rgba(255, 0, 0, 128);
        let config = SpotlightConfig::default().overlay_color(color);
        assert_eq!(config.overlay_color, color);
    }

    #[test]
    fn config_panel_bg_fg_builders() {
        let bg = PackedRgba::rgb(10, 20, 30);
        let fg = PackedRgba::rgb(200, 210, 220);
        let config = SpotlightConfig::default().panel_bg(bg).panel_fg(fg);
        assert_eq!(config.panel_bg, bg);
        assert_eq!(config.panel_fg, fg);
    }

    #[test]
    fn config_title_style_builder() {
        let style = Style::new().fg(PackedRgba::rgb(255, 0, 0));
        let config = SpotlightConfig::default().title_style(style);
        assert_eq!(config.title_style.fg, Some(PackedRgba::rgb(255, 0, 0)));
    }

    #[test]
    fn config_debug_clone() {
        let config = SpotlightConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.target_padding, config.target_padding);
        assert!(!format!("{:?}", config).is_empty());
    }

    #[test]
    fn spotlight_default_equals_new() {
        let a = Spotlight::default();
        let b = Spotlight::new();
        assert_eq!(a.title, b.title);
        assert_eq!(a.content, b.content);
        assert!(a.target.is_none());
        assert!(a.progress.is_none());
        assert!(a.hints.is_none());
        assert!(a.forced_position.is_none());
    }

    #[test]
    fn spotlight_debug_clone() {
        let spotlight = Spotlight::new().title("Test").content("Body");
        let cloned = spotlight.clone();
        assert_eq!(cloned.title, "Test");
        assert_eq!(cloned.content, "Body");
        assert!(!format!("{:?}", spotlight).is_empty());
    }

    #[test]
    fn spotlight_config_method() {
        let config = SpotlightConfig::default().panel_max_width(30);
        let spotlight = Spotlight::new().config(config);
        assert_eq!(spotlight.config.panel_max_width, 30);
    }

    #[test]
    fn panel_position_debug_clone_copy_eq() {
        let pos = PanelPosition::Above;
        let copied = pos;
        assert_eq!(pos, copied);
        assert_ne!(PanelPosition::Above, PanelPosition::Below);
        assert_ne!(PanelPosition::Left, PanelPosition::Right);
        assert!(!format!("{:?}", pos).is_empty());
    }

    #[test]
    fn padded_target_applies_padding() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 10, 20, 5))
            .config(SpotlightConfig::default().target_padding(3));
        let padded = spotlight.padded_target().unwrap();
        assert_eq!(padded.x, 7);
        assert_eq!(padded.y, 7);
        assert_eq!(padded.width, 26);
        assert_eq!(padded.height, 11);
    }

    #[test]
    fn padded_target_saturates_at_zero() {
        let spotlight = Spotlight::new()
            .target(Rect::new(0, 0, 10, 5))
            .config(SpotlightConfig::default().target_padding(5));
        let padded = spotlight.padded_target().unwrap();
        assert_eq!(padded.x, 0); // saturating_sub
        assert_eq!(padded.y, 0);
    }

    #[test]
    fn padded_target_none_when_no_target() {
        let spotlight = Spotlight::new();
        assert!(spotlight.padded_target().is_none());
    }

    #[test]
    fn panel_size_title_only_no_content() {
        let spotlight = Spotlight::new().title("Hello");
        let screen = Rect::new(0, 0, 80, 24);
        let (w, h) = spotlight.panel_size(screen);
        assert!(w > 0, "width should be positive");
        assert!(h > 0, "height should be positive");
    }

    #[test]
    fn panel_size_narrow_screen_clamps_width() {
        let spotlight = Spotlight::new()
            .title("A very long title that exceeds narrow screen")
            .content("And some content too");
        let narrow = Rect::new(0, 0, 10, 30);
        let (w, _) = spotlight.panel_size(narrow);
        assert!(w <= 10, "panel width should not exceed screen, got {}", w);
    }

    #[test]
    fn panel_size_hints_disabled_no_extra_height() {
        let config = SpotlightConfig::default().show_hints(false);
        let s1 = Spotlight::new()
            .title("T")
            .hints("Esc: Close")
            .config(config);
        let s2 = Spotlight::new().title("T"); // No hints at all
        let screen = Rect::new(0, 0, 80, 24);
        let (_, h1) = s1.panel_size(screen);
        let (_, h2) = s2.panel_size(screen);
        assert_eq!(h1, h2, "disabled hints should not add height");
    }

    #[test]
    fn grapheme_width_ascii() {
        assert_eq!(grapheme_width("a"), 1);
        assert_eq!(grapheme_width(" "), 1);
    }

    #[test]
    fn render_overlay_dims_cells_outside_target() {
        let spotlight = Spotlight::new()
            .target(Rect::new(5, 5, 3, 3))
            .config(SpotlightConfig::default().overlay_color(PackedRgba::rgba(0, 0, 0, 255)));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 20, &mut pool);
        let area = Rect::new(0, 0, 20, 20);

        spotlight.render_overlay(&mut frame, area);

        // Cell outside target should have been dimmed
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.bg, PackedRgba::rgba(0, 0, 0, 255));

        // Cell inside target should NOT have been dimmed (still default)
        let cell_inside = frame.buffer.get(6, 6).unwrap();
        // Should differ from the overlay color since it was skipped
        // (original bg is likely default black/transparent)
        assert_ne!(
            cell_inside.bg,
            PackedRgba::rgba(0, 0, 0, 255),
            "cell inside target should not be overlaid with opaque overlay"
        );
    }

    #[test]
    fn render_with_all_components() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 5, 20, 3))
            .title("Title")
            .content("Content text here")
            .progress("2 of 5")
            .hints("Enter: Next | Esc: Skip");

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        spotlight.render(Rect::new(0, 0, 80, 24), &mut frame);
        // Should not panic; just verify it completes
    }

    #[test]
    fn render_forced_above() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 10, 20, 3))
            .title("Above")
            .force_position(PanelPosition::Above);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        spotlight.render(Rect::new(0, 0, 80, 24), &mut frame);
    }

    #[test]
    fn panel_bounds_matches_size() {
        let spotlight = Spotlight::new()
            .target(Rect::new(10, 5, 20, 3))
            .title("Test")
            .content("Body");
        let screen = Rect::new(0, 0, 80, 24);
        let bounds = spotlight.panel_bounds(screen);
        let (w, h) = spotlight.panel_size(screen);
        assert_eq!(bounds.width, w);
        assert_eq!(bounds.height, h);
    }

    #[test]
    fn panel_position_clamped_to_screen() {
        // Force panel to a position that would go off-screen
        let spotlight = Spotlight::new()
            .target(Rect::new(0, 0, 5, 3))
            .title("Test Title Here")
            .force_position(PanelPosition::Above); // Would go negative

        let screen = Rect::new(0, 0, 80, 24);
        let (px, py, _) = spotlight.panel_position(screen);
        // panel_position returns u16 so non-negative is guaranteed;
        // verify panel stays within the screen bounds
        let (w, h) = spotlight.panel_size(screen);
        assert!(
            px + w <= screen.right(),
            "panel should not exceed screen right"
        );
        assert!(
            py + h <= screen.bottom(),
            "panel should not exceed screen bottom"
        );
    }
}

#![forbid(unsafe_code)]

//! Core widgets for FrankenTUI.

pub mod block;
pub mod borders;
pub mod cached;
pub mod constraint_overlay;
#[cfg(feature = "debug-overlay")]
pub mod debug_overlay;
pub mod error_boundary;
pub mod input;
pub mod layout_debugger;
pub mod list;
pub mod padding;
pub mod panel;
pub mod paragraph;
pub mod progress;
pub mod rule;
pub mod scrollbar;
pub mod spinner;
pub mod status_line;
pub mod table;

pub use cached::{CacheKey, CachedWidget, CachedWidgetState, FnKey, HashKey, NoCacheKey};
pub use constraint_overlay::{ConstraintOverlay, ConstraintOverlayStyle};
#[cfg(feature = "debug-overlay")]
pub use debug_overlay::{
    DebugOverlay, DebugOverlayOptions, DebugOverlayState, DebugOverlayStateful,
    DebugOverlayStatefulState,
};
pub use layout_debugger::{LayoutConstraints, LayoutDebugger, LayoutRecord};
pub use panel::Panel;
pub use status_line::{StatusItem, StatusLine};

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;

/// A `Widget` is a renderable component.
///
/// Widgets render themselves into a `Frame` within a given `Rect`.
pub trait Widget {
    /// Render the widget into the frame at the given area.
    fn render(&self, area: Rect, frame: &mut Frame);

    /// Whether this widget is essential and should always render,
    /// even at `EssentialOnly` degradation.
    ///
    /// Essential widgets include text inputs and primary content areas.
    /// Decorative widgets (borders, scrollbars, spinners, rules) are not essential.
    fn is_essential(&self) -> bool {
        false
    }
}

/// A `StatefulWidget` is a widget that renders based on mutable state.
pub trait StatefulWidget {
    type State;

    /// Render the widget into the frame with mutable state.
    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State);
}

/// Helper to apply style to a cell.
pub(crate) fn apply_style(cell: &mut Cell, style: Style) {
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg {
        cell.bg = bg;
    }
    if let Some(attrs) = style.attrs {
        // Convert ftui_style::StyleFlags to ftui_render::cell::StyleFlags
        // Assuming they are compatible or the same type re-exported.
        // If not, we might need conversion logic.
        // ftui_style::StyleFlags is u16 (likely), ftui_render is u8.
        // Let's assume the From implementation exists as per previous code.
        let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
        cell.attrs = cell.attrs.with_flags(cell_flags);
    }
}

/// Apply a style to all cells in a rectangular area.
///
/// This modifies existing cells, preserving their content.
pub(crate) fn set_style_area(buf: &mut Buffer, area: Rect, style: Style) {
    if style.is_empty() {
        return;
    }
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.get_mut(x, y) {
                apply_style(cell, style);
            }
        }
    }
}

/// Draw a text span into a frame at the given position.
///
/// Returns the x position after the last drawn character.
/// Stops at `max_x` (exclusive).
pub(crate) fn draw_text_span(
    frame: &mut Frame,
    mut x: u16,
    y: u16,
    content: &str,
    style: Style,
    max_x: u16,
) -> u16 {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    for grapheme in content.graphemes(true) {
        if x >= max_x {
            break;
        }
        let w = UnicodeWidthStr::width(grapheme);
        if w == 0 {
            continue;
        }
        if x + w as u16 > max_x {
            break;
        }

        // Intern grapheme if needed
        let cell_content = if w > 1 || grapheme.chars().count() > 1 {
            let id = frame.intern_with_width(grapheme, w as u8);
            ftui_render::cell::CellContent::from_grapheme(id)
        } else if let Some(c) = grapheme.chars().next() {
            ftui_render::cell::CellContent::from_char(c)
        } else {
            continue;
        };

        let mut cell = Cell::new(cell_content);
        apply_style(&mut cell, style);
        
        // Use set() which handles multi-width characters (atomic writes)
        frame.buffer.set(x, y, cell);
        
        x = x.saturating_add(w as u16);
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::PackedRgba;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn apply_style_sets_fg() {
        let mut cell = Cell::default();
        let style = Style::new().fg(PackedRgba::rgb(255, 0, 0));
        apply_style(&mut cell, style);
        assert_eq!(cell.fg, PackedRgba::rgb(255, 0, 0));
    }

    #[test]
    fn apply_style_sets_bg() {
        let mut cell = Cell::default();
        let style = Style::new().bg(PackedRgba::rgb(0, 255, 0));
        apply_style(&mut cell, style);
        assert_eq!(cell.bg, PackedRgba::rgb(0, 255, 0));
    }

    #[test]
    fn apply_style_preserves_content() {
        let mut cell = Cell::from_char('Z');
        let style = Style::new().fg(PackedRgba::rgb(1, 2, 3));
        apply_style(&mut cell, style);
        assert_eq!(cell.content.as_char(), Some('Z'));
    }

    #[test]
    fn apply_style_empty_is_noop() {
        let original = Cell::default();
        let mut cell = Cell::default();
        apply_style(&mut cell, Style::default());
        assert_eq!(cell.fg, original.fg);
        assert_eq!(cell.bg, original.bg);
    }

    #[test]
    fn set_style_area_applies_to_all_cells() {
        let mut buf = Buffer::new(3, 2);
        let area = Rect::new(0, 0, 3, 2);
        let style = Style::new().bg(PackedRgba::rgb(10, 20, 30));
        set_style_area(&mut buf, area, style);

        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(
                    buf.get(x, y).unwrap().bg,
                    PackedRgba::rgb(10, 20, 30),
                    "cell ({x},{y}) should have style applied"
                );
            }
        }
    }

    #[test]
    fn set_style_area_partial_rect() {
        let mut buf = Buffer::new(5, 5);
        let area = Rect::new(1, 1, 2, 2);
        let style = Style::new().fg(PackedRgba::rgb(99, 99, 99));
        set_style_area(&mut buf, area, style);

        // Inside area should be styled
        assert_eq!(buf.get(1, 1).unwrap().fg, PackedRgba::rgb(99, 99, 99));
        assert_eq!(buf.get(2, 2).unwrap().fg, PackedRgba::rgb(99, 99, 99));

        // Outside area should be default
        assert_ne!(buf.get(0, 0).unwrap().fg, PackedRgba::rgb(99, 99, 99));
    }

    #[test]
    fn set_style_area_empty_style_is_noop() {
        let mut buf = Buffer::new(3, 3);
        buf.set(0, 0, Cell::from_char('A'));
        let original_fg = buf.get(0, 0).unwrap().fg;

        set_style_area(&mut buf, Rect::new(0, 0, 3, 3), Style::default());

        // Should not have changed
        assert_eq!(buf.get(0, 0).unwrap().fg, original_fg);
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('A'));
    }

    #[test]
    fn draw_text_span_basic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let end_x = draw_text_span(&mut frame, 0, 0, "ABC", Style::default(), 10);

        assert_eq!(end_x, 3);
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert_eq!(frame.buffer.get(2, 0).unwrap().content.as_char(), Some('C'));
    }

    #[test]
    fn draw_text_span_clipped_at_max_x() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let end_x = draw_text_span(&mut frame, 0, 0, "ABCDEF", Style::default(), 3);

        assert_eq!(end_x, 3);
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(2, 0).unwrap().content.as_char(), Some('C'));
        // 'D' should not be drawn
        assert!(frame.buffer.get(3, 0).unwrap().is_empty());
    }

    #[test]
    fn draw_text_span_starts_at_offset() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let end_x = draw_text_span(&mut frame, 5, 0, "XY", Style::default(), 10);

        assert_eq!(end_x, 7);
        assert_eq!(frame.buffer.get(5, 0).unwrap().content.as_char(), Some('X'));
        assert_eq!(frame.buffer.get(6, 0).unwrap().content.as_char(), Some('Y'));
        assert!(frame.buffer.get(4, 0).unwrap().is_empty());
    }

    #[test]
    fn draw_text_span_empty_string() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        let end_x = draw_text_span(&mut frame, 0, 0, "", Style::default(), 5);
        assert_eq!(end_x, 0);
    }

    #[test]
    fn draw_text_span_applies_style() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        let style = Style::new().fg(PackedRgba::rgb(255, 128, 0));
        draw_text_span(&mut frame, 0, 0, "A", style, 5);

        assert_eq!(frame.buffer.get(0, 0).unwrap().fg, PackedRgba::rgb(255, 128, 0));
    }

    #[test]
    fn draw_text_span_max_x_at_start_draws_nothing() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        let end_x = draw_text_span(&mut frame, 3, 0, "ABC", Style::default(), 3);
        assert_eq!(end_x, 3);
        assert!(frame.buffer.get(3, 0).unwrap().is_empty());
    }
}

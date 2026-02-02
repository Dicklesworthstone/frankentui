#![forbid(unsafe_code)]

//! Core widgets for FrankenTUI.
//!
//! This crate provides the [`Widget`] and [`StatefulWidget`] traits, along with
//! a collection of ready-to-use widgets for building terminal UIs.
//!
//! # Widget Trait Design
//!
//! Widgets render into a [`Frame`] rather than directly into a [`Buffer`]. The Frame
//! provides access to several subsystems beyond the cell grid:
//!
//! - **`frame.buffer`** - The cell grid for drawing characters and styles
//! - **`frame.hit_grid`** - Optional mouse hit testing (for interactive widgets)
//! - **`frame.cursor_position`** - Cursor placement (for input widgets)
//! - **`frame.cursor_visible`** - Cursor visibility control
//! - **`frame.degradation`** - Performance budget hints (for adaptive rendering)
//!
//! # Widget Categories
//!
//! Widgets fall into four categories based on which Frame features they use:
//!
//! ## Category A: Simple Buffer-Only Widgets
//!
//! Most widgets only need buffer access. These are the simplest to implement:
//!
//! ```ignore
//! impl Widget for MyWidget {
//!     fn render(&self, area: Rect, frame: &mut Frame) {
//!         // Just write to the buffer
//!         frame.buffer.set(area.x, area.y, Cell::from_char('X'));
//!     }
//! }
//! ```
//!
//! Examples: [`block::Block`], [`paragraph::Paragraph`], [`rule::Rule`], [`StatusLine`]
//!
//! ## Category B: Interactive Widgets with Hit Testing
//!
//! Widgets that handle mouse clicks register hit regions:
//!
//! ```ignore
//! impl Widget for ClickableList {
//!     fn render(&self, area: Rect, frame: &mut Frame) {
//!         // Draw items...
//!         for (i, item) in self.items.iter().enumerate() {
//!             let row_area = Rect::new(area.x, area.y + i as u16, area.width, 1);
//!             // Draw item to buffer...
//!
//!             // Register hit region for mouse interaction
//!             if let Some(id) = self.hit_id {
//!                 frame.register_hit(row_area, id, HitRegion::Content, i as u64);
//!             }
//!         }
//!     }
//! }
//! ```
//!
//! Examples: [`list::List`], [`table::Table`], [`scrollbar::Scrollbar`]
//!
//! ## Category C: Input Widgets with Cursor Control
//!
//! Text input widgets need to position the cursor:
//!
//! ```ignore
//! impl Widget for TextInput {
//!     fn render(&self, area: Rect, frame: &mut Frame) {
//!         // Draw the input content...
//!
//!         // Position cursor when focused
//!         if self.focused {
//!             let cursor_x = area.x + self.cursor_offset as u16;
//!             frame.cursor_position = Some((cursor_x, area.y));
//!             frame.cursor_visible = true;
//!         }
//!     }
//! }
//! ```
//!
//! Examples: [`TextInput`](input::TextInput)
//!
//! ## Category D: Adaptive Widgets with Degradation Support
//!
//! Complex widgets can adapt their rendering based on performance budget:
//!
//! ```ignore
//! impl Widget for FancyProgressBar {
//!     fn render(&self, area: Rect, frame: &mut Frame) {
//!         let deg = frame.buffer.degradation;
//!
//!         if !deg.render_decorative() {
//!             // Skip decorative elements at reduced budgets
//!             return;
//!         }
//!
//!         if deg.apply_styling() {
//!             // Use full styling and effects
//!         } else {
//!             // Use simplified ASCII rendering
//!         }
//!     }
//! }
//! ```
//!
//! Examples: [`ProgressBar`](progress::ProgressBar), [`Spinner`](spinner::Spinner)
//!
//! # Essential vs Decorative Widgets
//!
//! The [`Widget::is_essential`] method indicates whether a widget should always render,
//! even at `EssentialOnly` degradation level:
//!
//! - **Essential**: Text inputs, primary content, status information
//! - **Decorative**: Borders, scrollbars, spinners, visual separators
//!
//! [`Frame`]: ftui_render::frame::Frame
//! [`Buffer`]: ftui_render::buffer::Buffer

pub mod align;
pub mod block;
pub mod borders;
pub mod cached;
pub mod columns;
pub mod constraint_overlay;
#[cfg(feature = "debug-overlay")]
pub mod debug_overlay;
pub mod emoji;
pub mod error_boundary;
pub mod group;
pub mod help;
pub mod input;
pub mod json_view;
pub mod layout_debugger;
pub mod list;
pub mod log_ring;
pub mod log_viewer;
pub mod padding;
pub mod paginator;
pub mod panel;
pub mod paragraph;
pub mod pretty;
pub mod progress;
pub mod rule;
pub mod scrollbar;
pub mod spinner;
pub mod status_line;
pub mod stopwatch;
pub mod table;
pub mod textarea;
pub mod timer;
pub mod tree;
pub mod virtualized;

pub use align::{Align, VerticalAlignment};
pub use cached::{CacheKey, CachedWidget, CachedWidgetState, FnKey, HashKey, NoCacheKey};
pub use group::Group;
pub use columns::{Column, Columns};
pub use constraint_overlay::{ConstraintOverlay, ConstraintOverlayStyle};
#[cfg(feature = "debug-overlay")]
pub use debug_overlay::{
    DebugOverlay, DebugOverlayOptions, DebugOverlayState, DebugOverlayStateful,
    DebugOverlayStatefulState,
};
pub use layout_debugger::{LayoutConstraints, LayoutDebugger, LayoutRecord};
pub use log_ring::LogRing;
pub use log_viewer::{LogViewer, LogViewerState, LogWrapMode};
pub use paginator::{Paginator, PaginatorMode};
pub use panel::Panel;
pub use status_line::{StatusItem, StatusLine};
pub use virtualized::{
    HeightCache, ItemHeight, RenderItem, Virtualized, VirtualizedList, VirtualizedListState,
    VirtualizedStorage,
};

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;

/// A widget that can render itself into a [`Frame`].
///
/// # Frame vs Buffer
///
/// Widgets render into a `Frame` rather than directly into a `Buffer`. This provides:
///
/// - **Buffer access**: `frame.buffer` for drawing cells
/// - **Hit testing**: `frame.register_hit()` for mouse interaction
/// - **Cursor control**: `frame.cursor_position` for input widgets
/// - **Performance hints**: `frame.buffer.degradation` for adaptive rendering
///
/// # Implementation Guide
///
/// Most widgets only need buffer access:
///
/// ```ignore
/// fn render(&self, area: Rect, frame: &mut Frame) {
///     for y in area.y..area.bottom() {
///         for x in area.x..area.right() {
///             frame.buffer.set(x, y, Cell::from_char('.'));
///         }
///     }
/// }
/// ```
///
/// Interactive widgets should register hit regions when a `hit_id` is set.
/// Input widgets should set `frame.cursor_position` when focused.
///
/// # Degradation Levels
///
/// Check `frame.buffer.degradation` to adapt rendering:
///
/// - `Full`: All features enabled
/// - `SimpleBorders`: Skip fancy borders, use ASCII
/// - `NoStyling`: Skip colors and attributes
/// - `EssentialOnly`: Only render essential widgets
/// - `Skeleton`: Minimal placeholder rendering
///
/// [`Frame`]: ftui_render::frame::Frame
pub trait Widget {
    /// Render the widget into the frame at the given area.
    ///
    /// The `area` defines the bounding rectangle within which the widget
    /// should render. Widgets should respect the area bounds and not
    /// draw outside them (the buffer's scissor stack enforces this).
    fn render(&self, area: Rect, frame: &mut Frame);

    /// Whether this widget is essential and should always render.
    ///
    /// Essential widgets render even at `EssentialOnly` degradation level.
    /// Override this to return `true` for:
    ///
    /// - Text inputs (user needs to see what they're typing)
    /// - Primary content areas (main information display)
    /// - Critical status indicators
    ///
    /// Returns `false` by default, appropriate for decorative widgets.
    fn is_essential(&self) -> bool {
        false
    }
}

/// A widget that renders based on mutable state.
///
/// Use `StatefulWidget` when the widget needs to:
///
/// - Update scroll position during render
/// - Track selection state
/// - Cache computed layout information
/// - Synchronize view with external model
///
/// # Example
///
/// ```ignore
/// pub struct ListState {
///     pub selected: Option<usize>,
///     pub offset: usize,
/// }
///
/// impl StatefulWidget for List<'_> {
///     type State = ListState;
///
///     fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
///         // Adjust offset to keep selection visible
///         if let Some(sel) = state.selected {
///             if sel < state.offset {
///                 state.offset = sel;
///             }
///         }
///         // Render items starting from offset...
///     }
/// }
/// ```
///
/// # Stateful vs Stateless
///
/// Prefer stateless [`Widget`] when possible. Use `StatefulWidget` only when
/// the render pass genuinely needs to modify state (e.g., scroll adjustment).
pub trait StatefulWidget {
    /// The state type associated with this widget.
    type State;

    /// Render the widget into the frame, potentially modifying state.
    ///
    /// State modifications should be limited to:
    /// - Scroll offset adjustments
    /// - Selection clamping
    /// - Layout caching
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

/// Draw a text span, optionally attaching a hyperlink.
#[allow(dead_code)]
pub(crate) fn draw_text_span_with_link(
    frame: &mut Frame,
    x: u16,
    y: u16,
    content: &str,
    style: Style,
    max_x: u16,
    link_url: Option<&str>,
) -> u16 {
    draw_text_span_scrolled(frame, x, y, content, style, max_x, 0, link_url)
}

/// Draw a text span with horizontal scrolling (skip first `scroll_x` visual cells).
#[allow(dead_code, clippy::too_many_arguments)]
pub(crate) fn draw_text_span_scrolled(
    frame: &mut Frame,
    mut x: u16,
    y: u16,
    content: &str,
    style: Style,
    max_x: u16,
    scroll_x: u16,
    link_url: Option<&str>,
) -> u16 {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    // Register link if present
    let link_id = if let Some(url) = link_url {
        frame.register_link(url)
    } else {
        0
    };

    let mut visual_pos = 0;

    for grapheme in content.graphemes(true) {
        if x >= max_x {
            break;
        }
        let w = UnicodeWidthStr::width(grapheme);
        if w == 0 {
            continue;
        }

        let next_visual_pos = visual_pos + w as u16;

        // Check if this grapheme is visible
        if next_visual_pos <= scroll_x {
            // Fully scrolled out
            visual_pos = next_visual_pos;
            continue;
        }

        if visual_pos < scroll_x {
            // Partially scrolled out (e.g. wide char starting at scroll_x - 1)
            // We skip the whole character because we can't render half a cell.
            visual_pos = next_visual_pos;
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

        // Apply link ID if present
        if link_id != 0 {
            cell.attrs = cell.attrs.with_link(link_id);
        }

        frame.buffer.set(x, y, cell);

        x = x.saturating_add(w as u16);
        visual_pos = next_visual_pos;
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

        assert_eq!(
            frame.buffer.get(0, 0).unwrap().fg,
            PackedRgba::rgb(255, 128, 0)
        );
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

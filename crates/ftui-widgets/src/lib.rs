#![forbid(unsafe_code)]

//! Core widgets for FrankenTUI.

pub mod block;
pub mod borders;
pub mod paragraph;

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, CellAttrs};
use ftui_style::style::Style;

/// A `Widget` is a renderable component.
///
/// Widgets render themselves into a `Buffer` within a given `Rect`.
pub trait Widget {
    /// Render the widget into the buffer at the given area.
    fn render(&self, area: Rect, buf: &mut Buffer);
}

/// A `StatefulWidget` is a widget that renders based on mutable state.
pub trait StatefulWidget {
    type State;
    /// Render the widget into the buffer with mutable state.
    fn render(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State);
}

/// Helper to apply a style to a cell.
pub(crate) fn apply_style(cell: &mut Cell, style: Style) {
    if let Some(fg) = style.fg {
        *cell = cell.with_fg(fg);
    }
    if let Some(bg) = style.bg {
        *cell = cell.with_bg(bg);
    }
    if let Some(attrs) = style.attrs {
        // Map StyleFlags (16-bit) to CellFlags (8-bit)
        let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
        let existing_link_id = cell.attrs.link_id();
        *cell = cell.with_attrs(CellAttrs::new(cell_flags, existing_link_id));
    }
}
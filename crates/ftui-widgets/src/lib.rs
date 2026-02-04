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
/// Badge widget (status/priority pills).
pub mod badge;
/// Block widget with borders, titles, and padding.
pub mod block;
pub mod borders;
pub mod cached;
pub mod columns;
pub mod command_palette;
pub mod constraint_overlay;
#[cfg(feature = "debug-overlay")]
pub mod debug_overlay;
/// Drag-and-drop protocol: [`Draggable`](drag::Draggable) sources, [`DropTarget`](drag::DropTarget) targets, and [`DragPayload`](drag::DragPayload).
pub mod drag;
pub mod emoji;
pub mod error_boundary;
/// Fenwick tree (Binary Indexed Tree) for O(log n) prefix sum queries.
pub mod fenwick;
pub mod file_picker;
/// Focus management: navigation graph for keyboard-driven focus traversal.
pub mod focus;
pub mod group;
/// Bayesian height prediction with conformal bounds for virtualized lists.
pub mod height_predictor;
pub mod help;
pub mod help_registry;
/// Utility-based keybinding hint ranking with Bayesian posteriors.
pub mod hint_ranker;
/// Undo/redo history panel widget for displaying command history.
pub mod history_panel;
pub mod input;
/// UI Inspector overlay for debugging widget trees and hit-test regions.
pub mod inspector;
pub mod json_view;
pub mod keyboard_drag;
pub mod layout;
pub mod layout_debugger;
pub mod list;
pub mod log_ring;
pub mod log_viewer;
/// Intrinsic sizing support for content-aware layout.
pub mod measurable;
/// Measure cache for memoizing widget measure results.
pub mod measure_cache;
pub mod modal;
/// Notification queue for managing multiple toast notifications.
pub mod notification_queue;
pub mod padding;
pub mod paginator;
pub mod panel;
/// Multi-line styled text paragraph widget.
pub mod paragraph;
pub mod pretty;
pub mod progress;
pub mod rule;
pub mod scrollbar;
pub mod sparkline;
pub mod spinner;
/// Opt-in persistable state trait for widgets.
pub mod stateful;
pub mod status_line;
pub mod stopwatch;
/// Table widget with rows, columns, and selection.
pub mod table;
pub mod textarea;
pub mod timer;
/// Toast widget for transient notifications.
pub mod toast;
pub mod tree;
/// Undo support for widgets.
pub mod undo_support;
/// Inline validation error display widget.
pub mod validation_error;
pub mod virtualized;
pub mod voi_debug_overlay;

pub use align::{Align, VerticalAlignment};
pub use badge::Badge;
pub use cached::{CacheKey, CachedWidget, CachedWidgetState, FnKey, HashKey, NoCacheKey};
pub use columns::{Column, Columns};
pub use constraint_overlay::{ConstraintOverlay, ConstraintOverlayStyle};
#[cfg(feature = "debug-overlay")]
pub use debug_overlay::{
    DebugOverlay, DebugOverlayOptions, DebugOverlayState, DebugOverlayStateful,
    DebugOverlayStatefulState,
};
pub use group::Group;
pub use help_registry::{HelpContent, HelpId, HelpRegistry, Keybinding};
pub use history_panel::{HistoryEntry, HistoryPanel, HistoryPanelMode};
pub use layout_debugger::{LayoutConstraints, LayoutDebugger, LayoutRecord};
pub use log_ring::LogRing;
pub use log_viewer::{LogViewer, LogViewerState, LogWrapMode, SearchConfig, SearchMode};
pub use paginator::{Paginator, PaginatorMode};
pub use panel::Panel;
pub use sparkline::Sparkline;
pub use status_line::{StatusItem, StatusLine};
pub use virtualized::{
    HeightCache, ItemHeight, RenderItem, Virtualized, VirtualizedList, VirtualizedListState,
    VirtualizedStorage,
};
pub use voi_debug_overlay::{
    VoiDebugOverlay, VoiDecisionSummary, VoiLedgerEntry, VoiObservationSummary, VoiOverlayData,
    VoiOverlayStyle, VoiPosteriorSummary,
};

// Toast notification widget
pub use toast::{
    KeyEvent as ToastKeyEvent, Toast, ToastAction, ToastAnimationConfig, ToastAnimationPhase,
    ToastAnimationState, ToastConfig, ToastContent, ToastEasing, ToastEntranceAnimation,
    ToastEvent, ToastExitAnimation, ToastIcon, ToastId, ToastPosition, ToastState, ToastStyle,
};

// Notification queue manager
pub use notification_queue::{
    NotificationPriority, NotificationQueue, QueueAction, QueueConfig, QueueStats,
};

// Measurable widget support for intrinsic sizing
pub use measurable::{MeasurableWidget, SizeConstraints};

// Measure cache for memoizing measure() results
pub use measure_cache::{CacheStats, MeasureCache, WidgetId};
pub use modal::{
    BackdropConfig, MODAL_HIT_BACKDROP, MODAL_HIT_CONTENT, Modal, ModalAction, ModalConfig,
    ModalPosition, ModalSizeConstraints, ModalState,
};

// UI Inspector for debugging
pub use inspector::{
    DiagnosticEntry, DiagnosticEventKind, DiagnosticLog, HitInfo, InspectorMode, InspectorOverlay,
    InspectorState, InspectorStyle, TelemetryCallback, TelemetryHooks, WidgetInfo,
    diagnostics_enabled, init_diagnostics, is_deterministic_mode, reset_event_counter,
    set_diagnostics_enabled,
};

// Focus management
pub use focus::{
    FocusEvent, FocusGraph, FocusGroup, FocusId, FocusManager, FocusNode, FocusTrap, NavDirection,
};

// Drag-and-drop protocol (source + target)
pub use drag::{
    DragConfig, DragPayload, DragState, Draggable, DropPosition, DropResult, DropTarget,
};

// Stateful persistence trait
pub use stateful::{StateKey, Stateful, VersionedState};

// Widget persist state types for state-persistence
pub use list::ListPersistState;
pub use table::TablePersistState;
pub use tree::TreePersistState;
pub use virtualized::VirtualizedListPersistState;

// Undo support for widgets
pub use undo_support::{
    ListOperation, ListUndoExt, SelectionOperation, TableOperation, TableUndoExt,
    TextEditOperation, TextInputUndoExt, TreeOperation, TreeUndoExt, UndoSupport, UndoWidgetId,
    WidgetTextEditCmd,
};

// Inline validation error display
pub use validation_error::{
    ANIMATION_DURATION_MS, ERROR_BG_DEFAULT, ERROR_FG_DEFAULT, ERROR_ICON_DEFAULT,
    ValidationErrorDisplay, ValidationErrorState,
};

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::grapheme_width;

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
    // Apply styles in-place so we don't disturb wide-character continuation cells.
    //
    // Important: `Style` backgrounds can carry alpha for internal compositing, but terminals
    // are opaque. When applying a semi-transparent background we must composite it over the
    // existing cell background (src-over) so the buffer ends up with the final opaque color.
    let fg = style.fg;
    let bg = style.bg;
    let attrs = style.attrs;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.get_mut(x, y) {
                if let Some(fg) = fg {
                    cell.fg = fg;
                }
                if let Some(bg) = bg {
                    match bg.a() {
                        0 => {} // Fully transparent: no-op
                        255 => cell.bg = bg,
                        _ => cell.bg = bg.over(cell.bg),
                    }
                }
                if let Some(attrs) = attrs {
                    let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
                    cell.attrs = cell.attrs.with_flags(cell_flags);
                }
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

    for grapheme in content.graphemes(true) {
        if x >= max_x {
            break;
        }
        let w = grapheme_width(grapheme);
        if w == 0 {
            continue;
        }
        if x.saturating_add(w as u16) > max_x {
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

    // Register link if present
    let link_id = if let Some(url) = link_url {
        frame.register_link(url)
    } else {
        0
    };

    let mut visual_pos: u16 = 0;

    for grapheme in content.graphemes(true) {
        if x >= max_x {
            break;
        }
        let w = grapheme_width(grapheme);
        if w == 0 {
            continue;
        }

        let next_visual_pos = visual_pos.saturating_add(w as u16);

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

        if x.saturating_add(w as u16) > max_x {
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
    fn set_style_area_composites_alpha_bg_over_existing_bg() {
        let mut buf = Buffer::new(1, 1);
        let base = PackedRgba::rgb(200, 0, 0);
        buf.set(0, 0, Cell::default().with_bg(base));

        let overlay = PackedRgba::rgba(0, 0, 200, 128);
        set_style_area(&mut buf, Rect::new(0, 0, 1, 1), Style::new().bg(overlay));

        let expected = overlay.over(base);
        assert_eq!(buf.get(0, 0).unwrap().bg, expected);
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

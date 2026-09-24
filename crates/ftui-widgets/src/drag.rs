#![forbid(unsafe_code)]

//! Drag-and-drop protocol (bd-1csc.1 + bd-1csc.2).
//!
//! Defines the [`Draggable`] trait for drag sources and [`DropTarget`] trait
//! for drop targets, along with [`DragPayload`] for transferring typed data,
//! [`DragState`] for tracking active drags, [`DropPosition`] for specifying
//! where within a target the drop occurs, and [`DropResult`] for communicating
//! drop outcomes.
//!
//! # Design
//!
//! ## Integration with Semantic Events
//!
//! Drag detection is handled by the gesture recognizer in `ftui-core`, which
//! emits `SemanticEvent::DragStart`, `DragMove`, `DragEnd`, and `DragCancel`;
//! its `GestureConfig::drag_threshold` decides when a drag starts. Nothing in
//! this crate consumes those events for you: the application listens for them,
//! identifies the source widget by hit-test, and calls the [`Draggable`] and
//! [`DropTarget`] methods itself. These types are the protocol it uses.
//!
//! ## Invariants
//!
//! 1. A drag operation is well-formed: exactly one `DragStart` followed by
//!    zero or more `DragMove` events, ending in either `DragEnd` or
//!    `DragCancel`.
//! 2. `on_drag_start` is called exactly once per drag, before any `DragMove`.
//! 3. `on_drag_end` is called exactly once per drag, with `success = true`
//!    if dropped on a valid target, `false` otherwise.
//! 4. `drag_type` must return a stable string for the lifetime of the drag.
//!
//! ## Failure Modes
//!
//! | Failure | Cause | Fallback |
//! |---------|-------|----------|
//! | No hit-test match at drag start | Click outside any draggable | Drag not initiated |
//! | Payload decode failure | Type mismatch at drop target | Drop rejected |
//! | Focus loss mid-drag | Window deactivation | `DragCancel` emitted |
//! | Escape pressed mid-drag | User cancellation | `DragCancel` emitted by the gesture recognizer |

use crate::Widget;
use crate::measure_cache::WidgetId;
use ftui_core::geometry::Rect;
use ftui_core::semantic_event::Position;
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_style::Style;

// ---------------------------------------------------------------------------
// DragPayload
// ---------------------------------------------------------------------------

/// Data carried during a drag operation.
///
/// The payload uses a MIME-like type string for matching against drop targets
/// and raw bytes for the actual data. This decouples the drag source from
/// the drop target — they only need to agree on the type string and byte
/// format.
///
/// # Examples
///
/// ```
/// # use ftui_widgets::drag::DragPayload;
/// let payload = DragPayload::text("hello world");
/// assert_eq!(payload.drag_type, "text/plain");
/// assert_eq!(payload.display_text.as_deref(), Some("hello world"));
/// ```
#[derive(Clone, Debug)]
pub struct DragPayload {
    /// MIME-like type identifier (e.g., `"text/plain"`, `"widget/list-item"`).
    pub drag_type: String,
    /// Raw serialized data.
    pub data: Vec<u8>,
    /// Human-readable preview text shown during drag (optional).
    pub display_text: Option<String>,
}

impl DragPayload {
    /// Create a payload with raw bytes.
    #[must_use]
    pub fn new(drag_type: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            drag_type: drag_type.into(),
            data,
            display_text: None,
        }
    }

    /// Create a plain-text payload.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        let s: String = text.into();
        let data = s.as_bytes().to_vec();
        Self {
            drag_type: "text/plain".to_string(),
            data,
            display_text: Some(s),
        }
    }

    /// Create a payload with custom display text.
    #[must_use]
    pub fn with_display_text(mut self, text: impl Into<String>) -> Self {
        self.display_text = Some(text.into());
        self
    }

    /// Attempt to decode the data as a UTF-8 string.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.data).ok()
    }

    /// Returns the byte length of the payload data.
    #[must_use]
    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    /// Returns true if the payload type matches the given pattern.
    ///
    /// Supports exact match and wildcard prefix (e.g., `"text/*"`).
    #[must_use]
    pub fn matches_type(&self, pattern: &str) -> bool {
        if pattern == "*" || pattern == "*/*" {
            return true;
        }
        if let Some(prefix) = pattern.strip_suffix("/*") {
            self.drag_type.starts_with(prefix)
                && self.drag_type.as_bytes().get(prefix.len()) == Some(&b'/')
        } else {
            self.drag_type == pattern
        }
    }
}

// ---------------------------------------------------------------------------
// DragState
// ---------------------------------------------------------------------------

/// Active drag operation state.
///
/// Created when a drag starts and destroyed when it ends or is cancelled.
/// The application's drag handling owns this state.
pub struct DragState {
    /// Widget that initiated the drag.
    pub source_id: WidgetId,
    /// Data being dragged.
    pub payload: DragPayload,
    /// Position where the drag started.
    pub start_pos: Position,
    /// Current drag position.
    pub current_pos: Position,
    /// Optional custom preview widget.
    pub preview: Option<Box<dyn Widget>>,
}

impl std::fmt::Debug for DragState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DragState")
            .field("source_id", &self.source_id)
            .field("payload", &self.payload)
            .field("start_pos", &self.start_pos)
            .field("current_pos", &self.current_pos)
            .field("preview", &self.preview.as_ref().map(|_| ".."))
            .finish()
    }
}

impl DragState {
    /// Create a new drag state.
    #[must_use]
    pub fn new(source_id: WidgetId, payload: DragPayload, start_pos: Position) -> Self {
        Self {
            source_id,
            payload,
            start_pos,
            current_pos: start_pos,
            preview: None,
        }
    }

    /// Set a custom preview widget.
    #[must_use]
    pub fn with_preview(mut self, preview: Box<dyn Widget>) -> Self {
        self.preview = Some(preview);
        self
    }

    /// Update the current position during a drag move.
    pub fn update_position(&mut self, pos: Position) {
        self.current_pos = pos;
    }

    /// Manhattan distance from start to current position.
    #[must_use]
    pub fn distance(&self) -> u32 {
        self.start_pos.manhattan_distance(self.current_pos)
    }

    /// Delta from start to current position as `(dx, dy)`.
    #[must_use]
    pub fn delta(&self) -> (i32, i32) {
        (
            self.current_pos.x as i32 - self.start_pos.x as i32,
            self.current_pos.y as i32 - self.start_pos.y as i32,
        )
    }
}

// ---------------------------------------------------------------------------
// Draggable trait
// ---------------------------------------------------------------------------

/// Trait for widgets that can be drag sources.
///
/// Implement this trait to allow a widget to participate in drag-and-drop
/// operations. The application's drag handling calls these methods during
/// the drag lifecycle.
///
/// # Example
///
/// ```ignore
/// use ftui_widgets::drag::{Draggable, DragPayload};
///
/// struct FileItem { path: String }
///
/// impl Draggable for FileItem {
///     fn drag_type(&self) -> &str { "application/file-path" }
///
///     fn drag_data(&self) -> DragPayload {
///         DragPayload::new("application/file-path", self.path.as_bytes().to_vec())
///             .with_display_text(&self.path)
///     }
/// }
/// ```
pub trait Draggable {
    /// MIME-like type identifier for the dragged data.
    ///
    /// Must return a stable string for the lifetime of the drag.
    /// Examples: `"text/plain"`, `"widget/list-item"`, `"application/file-path"`.
    fn drag_type(&self) -> &str;

    /// Produce the drag payload.
    ///
    /// Called once when the drag starts to capture the data being transferred.
    fn drag_data(&self) -> DragPayload;

    /// Optional custom preview widget shown during the drag.
    ///
    /// Return `None` to use the default text-based preview from
    /// `DragPayload::display_text`.
    fn drag_preview(&self) -> Option<Box<dyn Widget>> {
        None
    }

    /// Called when a drag operation starts from this widget.
    ///
    /// Use this to apply visual feedback (e.g., dim the source item).
    fn on_drag_start(&mut self) {}

    /// Called when the drag operation ends.
    ///
    /// `success` is `true` if the payload was accepted by a drop target,
    /// `false` if the drag was cancelled or dropped on an invalid target.
    fn on_drag_end(&mut self, _success: bool) {}
}

// ---------------------------------------------------------------------------
// DropPosition
// ---------------------------------------------------------------------------

/// Where within a drop target the drop will occur.
///
/// Used by [`DropTarget::drop_position`] to communicate precise placement
/// to the drop handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropPosition {
    /// Before the item at the given index.
    Before(usize),
    /// After the item at the given index.
    After(usize),
    /// Inside the item at the given index (for tree-like targets).
    Inside(usize),
    /// Replace the item at the given index.
    Replace(usize),
    /// Append to the end of the target's items.
    Append,
}

impl DropPosition {
    /// Returns the index associated with this position, if any.
    #[must_use]
    pub fn index(&self) -> Option<usize> {
        match self {
            Self::Before(i) | Self::After(i) | Self::Inside(i) | Self::Replace(i) => Some(*i),
            Self::Append => None,
        }
    }

    /// Returns true if this is an insertion position (`Before` or `After`).
    #[must_use]
    pub fn is_insertion(&self) -> bool {
        matches!(self, Self::Before(_) | Self::After(_) | Self::Append)
    }

    /// Calculate a list drop position from a y coordinate within a list.
    ///
    /// Divides each item's height in half: the upper half maps to `Before`,
    /// the lower half maps to `After`.
    ///
    /// # Panics
    ///
    /// Panics if `item_height` is zero.
    #[must_use]
    pub fn from_list(y: u16, item_height: u16, item_count: usize) -> Self {
        assert!(item_height > 0, "item_height must be non-zero");
        if item_count == 0 {
            return Self::Append;
        }
        let item_index = (y / item_height) as usize;
        if item_index >= item_count {
            return Self::Append;
        }
        let within_item = y % item_height;
        if within_item < item_height / 2 {
            Self::Before(item_index)
        } else {
            Self::After(item_index)
        }
    }
}

// ---------------------------------------------------------------------------
// DropResult
// ---------------------------------------------------------------------------

/// Outcome of a drop operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropResult {
    /// Drop was accepted and applied.
    Accepted,
    /// Drop was rejected with a reason.
    Rejected {
        /// Human-readable explanation for why the drop was rejected.
        reason: String,
    },
}

impl DropResult {
    /// Create a rejection with the given reason.
    #[must_use]
    pub fn rejected(reason: impl Into<String>) -> Self {
        Self::Rejected {
            reason: reason.into(),
        }
    }

    /// Returns true if the drop was accepted.
    #[must_use]
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted)
    }
}

// ---------------------------------------------------------------------------
// DropTarget trait
// ---------------------------------------------------------------------------

/// Trait for widgets that can accept drops.
///
/// Implement this trait to allow a widget to be a drop target. The drag
/// manager queries these methods during hover and drop to determine
/// acceptance and placement.
///
/// # Example
///
/// ```ignore
/// use ftui_widgets::drag::{DropTarget, DragPayload, DropPosition, DropResult};
///
/// struct FileList { files: Vec<String> }
///
/// impl DropTarget for FileList {
///     fn can_accept(&self, drag_type: &str) -> bool {
///         drag_type == "application/file-path" || drag_type == "text/plain"
///     }
///
///     fn drop_position(&self, pos: Position, _payload: &DragPayload) -> DropPosition {
///         DropPosition::from_list(pos.y, 1, self.files.len())
///     }
///
///     fn on_drop(&mut self, payload: DragPayload, position: DropPosition) -> DropResult {
///         if let Some(text) = payload.as_text() {
///             let idx = match position {
///                 DropPosition::Before(i) => i,
///                 DropPosition::After(i) => i + 1,
///                 DropPosition::Append => self.files.len(),
///                 _ => return DropResult::rejected("unsupported position"),
///             };
///             self.files.insert(idx, text.to_string());
///             DropResult::Accepted
///         } else {
///             DropResult::rejected("expected text payload")
///         }
///     }
/// }
/// ```
pub trait DropTarget {
    /// Check if this target accepts the given drag type.
    ///
    /// Called during hover to provide visual feedback (valid vs. invalid
    /// target). Must be a cheap check — called on every mouse move during
    /// a drag.
    fn can_accept(&self, drag_type: &str) -> bool;

    /// Calculate the drop position within this widget.
    ///
    /// `pos` is the cursor position relative to the widget's area origin.
    /// `payload` provides access to the drag data for type-aware positioning.
    fn drop_position(&self, pos: Position, payload: &DragPayload) -> DropPosition;

    /// Handle the actual drop.
    ///
    /// Called when the user releases the mouse button over a valid target.
    /// Returns [`DropResult::Accepted`] if the drop was handled, or
    /// [`DropResult::Rejected`] with a reason if it cannot be applied.
    fn on_drop(&mut self, payload: DragPayload, position: DropPosition) -> DropResult;

    /// Called when a drag enters this target's area.
    ///
    /// Use for hover-enter visual feedback.
    fn on_drag_enter(&mut self) {}

    /// Called when a drag leaves this target's area.
    ///
    /// Use to clear hover visual feedback.
    fn on_drag_leave(&mut self) {}

    /// Accepted drag types as a list of MIME-like patterns.
    ///
    /// Override to provide a static list for documentation or introspection.
    /// Defaults to an empty slice (use `can_accept` for runtime checks).
    fn accepted_types(&self) -> &[&str] {
        &[]
    }
}

// ---------------------------------------------------------------------------
// DragPreviewConfig
// ---------------------------------------------------------------------------

/// Configuration for the drag preview overlay.
///
/// Controls visual properties of the widget shown at the cursor during a drag.
#[derive(Clone, Debug)]
pub struct DragPreviewConfig {
    /// Opacity of the preview widget (0.0 = invisible, 1.0 = fully opaque).
    /// Default: 0.7.
    pub opacity: f32,
    /// Horizontal offset from cursor position in cells. Default: 1.
    pub offset_x: i16,
    /// Vertical offset from cursor position in cells. Default: 1.
    pub offset_y: i16,
    /// Width of the preview area in cells. Default: 20.
    pub width: u16,
    /// Height of the preview area in cells. Default: 1.
    pub height: u16,
    /// Background color for the preview area.
    pub background: Option<PackedRgba>,
    /// Whether to render a border around the preview. Default: false.
    pub show_border: bool,
}

impl Default for DragPreviewConfig {
    fn default() -> Self {
        Self {
            opacity: 0.7,
            offset_x: 1,
            offset_y: 1,
            width: 20,
            height: 1,
            background: None,
            show_border: false,
        }
    }
}

impl DragPreviewConfig {
    /// Set opacity (clamped to 0.0..=1.0).
    #[must_use]
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Set cursor offset.
    #[must_use]
    pub fn with_offset(mut self, x: i16, y: i16) -> Self {
        self.offset_x = x;
        self.offset_y = y;
        self
    }

    /// Set preview dimensions.
    #[must_use]
    pub fn with_size(mut self, width: u16, height: u16) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set background color.
    #[must_use]
    pub fn with_background(mut self, color: PackedRgba) -> Self {
        self.background = Some(color);
        self
    }

    /// Enable border rendering.
    #[must_use]
    pub fn with_border(mut self) -> Self {
        self.show_border = true;
        self
    }

    /// Calculate the preview area given cursor position and viewport bounds.
    ///
    /// Clamps the preview rectangle to stay within the viewport. Returns
    /// `None` if the preview would be fully outside the viewport.
    #[must_use]
    pub fn preview_rect(&self, cursor: Position, viewport: Rect) -> Option<Rect> {
        let raw_x = cursor.x as i32 + self.offset_x as i32;
        let raw_y = cursor.y as i32 + self.offset_y as i32;

        // Clamp to viewport
        let x = raw_x
            .max(viewport.x as i32)
            .min((viewport.x + viewport.width).saturating_sub(self.width) as i32);
        let y = raw_y
            .max(viewport.y as i32)
            .min((viewport.y + viewport.height).saturating_sub(self.height) as i32);

        if x < 0 || y < 0 {
            return None;
        }

        let x = x as u16;
        let y = y as u16;

        // Ensure the rect is within viewport
        if x >= viewport.x + viewport.width || y >= viewport.y + viewport.height {
            return None;
        }

        let w = self.width.min(viewport.x + viewport.width - x);
        let h = self.height.min(viewport.y + viewport.height - y);

        if w == 0 || h == 0 {
            return None;
        }

        Some(Rect::new(x, y, w, h))
    }
}

// ---------------------------------------------------------------------------
// DragPreview
// ---------------------------------------------------------------------------

/// Overlay widget that renders a drag preview at the cursor position.
///
/// The preview renders either a custom widget (from [`DragState::preview`])
/// or a text-based fallback from [`DragPayload::display_text`].
///
/// # Rendering
///
/// 1. Pushes the configured opacity onto the buffer's opacity stack.
/// 2. Optionally fills the background.
/// 3. Renders the custom preview widget or fallback text.
/// 4. Optionally draws a border.
/// 5. Pops the opacity.
///
/// # Invariants
///
/// - The preview is always clamped to the viewport bounds.
/// - Opacity is always restored (pop matches push) even if the area is empty.
/// - At `EssentialOnly` degradation or below, the preview is not rendered
///   (it is decorative).
pub struct DragPreview<'a> {
    /// Current drag state (position, payload, optional custom preview).
    pub drag_state: &'a DragState,
    /// Visual configuration.
    pub config: DragPreviewConfig,
}

impl<'a> DragPreview<'a> {
    /// Create a new drag preview for the given state.
    #[must_use]
    pub fn new(drag_state: &'a DragState) -> Self {
        Self {
            drag_state,
            config: DragPreviewConfig::default(),
        }
    }

    /// Create a drag preview with custom configuration.
    #[must_use]
    pub fn with_config(drag_state: &'a DragState, config: DragPreviewConfig) -> Self {
        Self { drag_state, config }
    }

    /// Render the fallback text preview.
    fn render_text_fallback(&self, area: Rect, frame: &mut Frame) {
        let text = self
            .drag_state
            .payload
            .display_text
            .as_deref()
            .or_else(|| self.drag_state.payload.as_text())
            .unwrap_or("…");

        crate::draw_text_span(
            frame,
            area.x,
            area.y,
            text,
            Style::default(),
            area.x + area.width,
        );
    }

    /// Render a simple border around the preview area.
    fn render_border(&self, area: Rect, frame: &mut Frame) {
        if area.width < 2 || area.height < 2 {
            return;
        }

        let right = area.x + area.width - 1;
        let bottom = area.y + area.height - 1;

        // Corners
        frame
            .buffer
            .set(area.x, area.y, ftui_render::cell::Cell::from_char('┌'));
        frame
            .buffer
            .set(right, area.y, ftui_render::cell::Cell::from_char('┐'));
        frame
            .buffer
            .set(area.x, bottom, ftui_render::cell::Cell::from_char('└'));
        frame
            .buffer
            .set(right, bottom, ftui_render::cell::Cell::from_char('┘'));

        // Horizontal edges
        for x in (area.x + 1)..right {
            frame
                .buffer
                .set_fast(x, area.y, ftui_render::cell::Cell::from_char('─'));
            frame
                .buffer
                .set_fast(x, bottom, ftui_render::cell::Cell::from_char('─'));
        }

        // Vertical edges
        for y in (area.y + 1)..bottom {
            frame
                .buffer
                .set_fast(area.x, y, ftui_render::cell::Cell::from_char('│'));
            frame
                .buffer
                .set_fast(right, y, ftui_render::cell::Cell::from_char('│'));
        }
    }
}

impl Widget for DragPreview<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        // Skip decorative preview at degraded rendering levels
        if !frame.buffer.degradation.render_decorative() {
            return;
        }

        let Some(preview_rect) = self.config.preview_rect(self.drag_state.current_pos, area) else {
            return;
        };

        // Push opacity for ghost effect
        frame.buffer.push_opacity(self.config.opacity);

        // Fill background if configured
        if let Some(bg) = self.config.background {
            crate::set_style_area(&mut frame.buffer, preview_rect, Style::new().bg(bg));
        }

        // Render border if enabled (needs to happen before content so content
        // can render inside the border)
        if self.config.show_border {
            self.render_border(preview_rect, frame);
        }

        // Content area (inset by border if present)
        let content_rect =
            if self.config.show_border && preview_rect.width > 2 && preview_rect.height > 2 {
                Rect::new(
                    preview_rect.x + 1,
                    preview_rect.y + 1,
                    preview_rect.width - 2,
                    preview_rect.height - 2,
                )
            } else {
                preview_rect
            };

        // Render custom preview or text fallback
        if let Some(ref preview_widget) = self.drag_state.preview {
            preview_widget.render(content_rect, frame);
        } else {
            self.render_text_fallback(content_rect, frame);
        }

        // Restore opacity
        frame.buffer.pop_opacity();
    }

    fn is_essential(&self) -> bool {
        false // Drag preview is decorative
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // === DragPayload tests ===

    #[test]
    fn payload_text_constructor() {
        let p = DragPayload::text("hello");
        assert_eq!(p.drag_type, "text/plain");
        assert_eq!(p.as_text(), Some("hello"));
        assert_eq!(p.display_text.as_deref(), Some("hello"));
    }

    #[test]
    fn payload_raw_bytes() {
        // 0xFF is never valid in UTF-8
        let p = DragPayload::new("application/octet-stream", vec![0xFF, 0xFE]);
        assert_eq!(p.data_len(), 2);
        assert_eq!(p.data, vec![0xFF, 0xFE]);
        assert!(p.as_text().is_none()); // not valid UTF-8
    }

    #[test]
    fn payload_with_display_text() {
        let p = DragPayload::new("widget/item", vec![1, 2, 3]).with_display_text("Item #42");
        assert_eq!(p.display_text.as_deref(), Some("Item #42"));
    }

    #[test]
    fn payload_matches_exact_type() {
        let p = DragPayload::text("test");
        assert!(p.matches_type("text/plain"));
        assert!(!p.matches_type("text/html"));
    }

    #[test]
    fn payload_matches_wildcard() {
        let p = DragPayload::text("test");
        assert!(p.matches_type("text/*"));
        assert!(p.matches_type("*/*"));
        assert!(p.matches_type("*"));
        assert!(!p.matches_type("application/*"));
    }

    #[test]
    fn payload_wildcard_requires_slash() {
        let p = DragPayload::new("textual/data", vec![]);
        // "text/*" should NOT match "textual/data" — prefix must end at slash
        assert!(!p.matches_type("text/*"));
    }

    #[test]
    fn payload_empty_data() {
        let p = DragPayload::new("empty/type", vec![]);
        assert_eq!(p.data_len(), 0);
        assert_eq!(p.as_text(), Some(""));
    }

    #[test]
    fn payload_clone() {
        let p1 = DragPayload::text("hello").with_display_text("Hello!");
        let p2 = p1.clone();
        assert_eq!(p1.drag_type, p2.drag_type);
        assert_eq!(p1.data, p2.data);
        assert_eq!(p1.display_text, p2.display_text);
    }

    // === DragState tests ===

    #[test]
    fn drag_state_creation() {
        let state = DragState::new(
            WidgetId(42),
            DragPayload::text("dragging"),
            Position::new(10, 5),
        );
        assert_eq!(state.source_id, WidgetId(42));
        assert_eq!(state.start_pos, Position::new(10, 5));
        assert_eq!(state.current_pos, Position::new(10, 5));
        assert!(state.preview.is_none());
    }

    #[test]
    fn drag_state_update_position() {
        let mut state = DragState::new(WidgetId(1), DragPayload::text("test"), Position::new(0, 0));
        state.update_position(Position::new(5, 3));
        assert_eq!(state.current_pos, Position::new(5, 3));
    }

    #[test]
    fn drag_state_distance() {
        let mut state = DragState::new(WidgetId(1), DragPayload::text("test"), Position::new(0, 0));
        state.update_position(Position::new(3, 4));
        assert_eq!(state.distance(), 7); // manhattan: |3| + |4|
    }

    #[test]
    fn drag_state_delta() {
        let mut state = DragState::new(
            WidgetId(1),
            DragPayload::text("test"),
            Position::new(10, 20),
        );
        state.update_position(Position::new(15, 18));
        assert_eq!(state.delta(), (5, -2));
    }

    #[test]
    fn drag_state_zero_distance_at_start() {
        let state = DragState::new(
            WidgetId(1),
            DragPayload::text("test"),
            Position::new(50, 50),
        );
        assert_eq!(state.distance(), 0);
        assert_eq!(state.delta(), (0, 0));
    }

    // === Draggable trait tests (via fixtures) ===

    struct DragSourceFixture {
        label: String,
        started: bool,
        ended_with: Option<bool>,
        log: Vec<String>,
    }

    impl DragSourceFixture {
        fn new(label: &str) -> Self {
            Self {
                label: label.to_string(),
                started: false,
                ended_with: None,
                log: Vec::new(),
            }
        }

        fn drain_log(&mut self) -> Vec<String> {
            std::mem::take(&mut self.log)
        }
    }

    impl Draggable for DragSourceFixture {
        fn drag_type(&self) -> &str {
            "text/plain"
        }

        fn drag_data(&self) -> DragPayload {
            DragPayload::text(&self.label).with_display_text(&self.label)
        }

        fn on_drag_start(&mut self) {
            self.started = true;
            self.log.push(format!("source:start label={}", self.label));
        }

        fn on_drag_end(&mut self, success: bool) {
            self.ended_with = Some(success);
            self.log.push(format!(
                "source:end label={} success={}",
                self.label, success
            ));
        }
    }

    #[test]
    fn draggable_type_and_data() {
        let d = DragSourceFixture::new("item-1");
        assert_eq!(d.drag_type(), "text/plain");
        let payload = d.drag_data();
        assert_eq!(
            payload.as_text(),
            Some("item-1"),
            "payload text mismatch for fixture"
        );
        assert_eq!(
            payload.display_text.as_deref(),
            Some("item-1"),
            "payload display_text mismatch for fixture"
        );
    }

    #[test]
    fn draggable_default_preview_is_none() {
        let d = DragSourceFixture::new("item");
        assert!(d.drag_preview().is_none());
    }

    #[test]
    fn draggable_callbacks() {
        let mut d = DragSourceFixture::new("item");
        assert!(!d.started);
        assert!(d.ended_with.is_none());

        d.on_drag_start();
        assert!(d.started);

        d.on_drag_end(true);
        assert_eq!(d.ended_with, Some(true));
        assert_eq!(
            d.drain_log(),
            vec![
                "source:start label=item".to_string(),
                "source:end label=item success=true".to_string(),
            ],
            "unexpected drag log for callbacks"
        );
    }

    #[test]
    fn draggable_callbacks_on_cancel() {
        let mut d = DragSourceFixture::new("item");
        d.on_drag_start();
        d.on_drag_end(false);
        assert_eq!(d.ended_with, Some(false));
    }

    // === DropPosition tests ===

    #[test]
    fn drop_position_index() {
        assert_eq!(DropPosition::Before(3).index(), Some(3));
        assert_eq!(DropPosition::After(5).index(), Some(5));
        assert_eq!(DropPosition::Inside(0).index(), Some(0));
        assert_eq!(DropPosition::Replace(7).index(), Some(7));
        assert_eq!(DropPosition::Append.index(), None);
    }

    #[test]
    fn drop_position_is_insertion() {
        assert!(DropPosition::Before(0).is_insertion());
        assert!(DropPosition::After(0).is_insertion());
        assert!(DropPosition::Append.is_insertion());
        assert!(!DropPosition::Inside(0).is_insertion());
        assert!(!DropPosition::Replace(0).is_insertion());
    }

    #[test]
    fn drop_position_from_list_empty() {
        assert_eq!(DropPosition::from_list(0, 2, 0), DropPosition::Append);
    }

    #[test]
    fn drop_position_from_list_upper_half() {
        // y=0, item_height=4, item_count=3 → within_item=0 < 2 → Before(0)
        assert_eq!(DropPosition::from_list(0, 4, 3), DropPosition::Before(0));
        assert_eq!(DropPosition::from_list(1, 4, 3), DropPosition::Before(0));
    }

    #[test]
    fn drop_position_from_list_lower_half() {
        // y=2, item_height=4 → within_item=2 >= 2 → After(0)
        assert_eq!(DropPosition::from_list(2, 4, 3), DropPosition::After(0));
        assert_eq!(DropPosition::from_list(3, 4, 3), DropPosition::After(0));
    }

    #[test]
    fn drop_position_from_list_second_item() {
        // y=5, item_height=4 → item_index=1, within_item=1 < 2 → Before(1)
        assert_eq!(DropPosition::from_list(4, 4, 3), DropPosition::Before(1));
        // y=6, item_height=4 → item_index=1, within_item=2 >= 2 → After(1)
        assert_eq!(DropPosition::from_list(6, 4, 3), DropPosition::After(1));
    }

    #[test]
    fn drop_position_from_list_beyond_items() {
        // y=20, item_height=4, item_count=3 → item_index=5 >= 3 → Append
        assert_eq!(DropPosition::from_list(20, 4, 3), DropPosition::Append);
    }

    #[test]
    #[should_panic(expected = "item_height must be non-zero")]
    fn drop_position_from_list_zero_height_panics() {
        let _ = DropPosition::from_list(0, 0, 5);
    }

    // === DropResult tests ===

    #[test]
    fn drop_result_accepted() {
        let r = DropResult::Accepted;
        assert!(r.is_accepted());
    }

    #[test]
    fn drop_result_rejected() {
        let r = DropResult::rejected("type mismatch");
        assert!(!r.is_accepted());
        match r {
            DropResult::Rejected { reason } => assert_eq!(reason, "type mismatch"),
            _ => unreachable!("expected Rejected"),
        }
    }

    #[test]
    fn drop_result_eq() {
        assert_eq!(DropResult::Accepted, DropResult::Accepted);
        assert_eq!(
            DropResult::rejected("x"),
            DropResult::Rejected {
                reason: "x".to_string()
            }
        );
        assert_ne!(DropResult::Accepted, DropResult::rejected("y"));
    }

    // === DropTarget trait tests (via fixtures) ===

    struct DropListFixture {
        items: Vec<String>,
        accepted: Vec<String>,
        entered: bool,
        log: Vec<String>,
    }

    impl DropListFixture {
        fn new(accepted: &[&str]) -> Self {
            Self {
                items: Vec::new(),
                accepted: accepted.iter().map(|s| s.to_string()).collect(),
                entered: false,
                log: Vec::new(),
            }
        }

        fn drain_log(&mut self) -> Vec<String> {
            std::mem::take(&mut self.log)
        }
    }

    impl DropTarget for DropListFixture {
        fn can_accept(&self, drag_type: &str) -> bool {
            self.accepted.iter().any(|t| t == drag_type)
        }

        fn drop_position(&self, pos: Position, _payload: &DragPayload) -> DropPosition {
            if self.items.is_empty() {
                DropPosition::Append
            } else {
                DropPosition::from_list(pos.y, 1, self.items.len())
            }
        }

        fn on_drop(&mut self, payload: DragPayload, position: DropPosition) -> DropResult {
            if let Some(text) = payload.as_text() {
                let idx = match position {
                    DropPosition::Before(i) => i,
                    DropPosition::After(i) => i + 1,
                    DropPosition::Append => self.items.len(),
                    _ => return DropResult::rejected("unsupported position"),
                };
                self.items.insert(idx, text.to_string());
                self.log
                    .push(format!("target:drop text={text} position={position:?}"));
                DropResult::Accepted
            } else {
                DropResult::rejected("expected text")
            }
        }

        fn on_drag_enter(&mut self) {
            self.entered = true;
            self.log.push("target:enter".to_string());
        }

        fn on_drag_leave(&mut self) {
            self.entered = false;
            self.log.push("target:leave".to_string());
        }

        fn accepted_types(&self) -> &[&str] {
            &[]
        }
    }

    #[test]
    fn drop_target_can_accept() {
        let target = DropListFixture::new(&["text/plain", "widget/item"]);
        assert!(target.can_accept("text/plain"));
        assert!(target.can_accept("widget/item"));
        assert!(!target.can_accept("image/png"));
    }

    #[test]
    fn drop_target_drop_position_empty() {
        let target = DropListFixture::new(&["text/plain"]);
        let pos = target.drop_position(Position::new(0, 0), &DragPayload::text("x"));
        assert_eq!(pos, DropPosition::Append);
    }

    #[test]
    fn drop_target_on_drop_accepted() {
        let mut target = DropListFixture::new(&["text/plain"]);
        let result = target.on_drop(DragPayload::text("hello"), DropPosition::Append);
        assert!(result.is_accepted());
        assert_eq!(target.items, vec!["hello"]);
    }

    #[test]
    fn drop_target_on_drop_insert_before() {
        let mut target = DropListFixture::new(&["text/plain"]);
        target.items = vec!["a".into(), "b".into()];
        let result = target.on_drop(DragPayload::text("x"), DropPosition::Before(1));
        assert!(result.is_accepted());
        assert_eq!(target.items, vec!["a", "x", "b"]);
    }

    #[test]
    fn drop_target_on_drop_insert_after() {
        let mut target = DropListFixture::new(&["text/plain"]);
        target.items = vec!["a".into(), "b".into()];
        let result = target.on_drop(DragPayload::text("x"), DropPosition::After(0));
        assert!(result.is_accepted());
        assert_eq!(target.items, vec!["a", "x", "b"]);
    }

    #[test]
    fn drop_target_on_drop_rejected_non_text() {
        let mut target = DropListFixture::new(&["application/octet-stream"]);
        let payload = DragPayload::new("application/octet-stream", vec![0xFF, 0xFE]);
        let result = target.on_drop(payload, DropPosition::Append);
        assert!(!result.is_accepted());
    }

    #[test]
    fn drop_target_enter_leave() {
        let mut target = DropListFixture::new(&[]);
        assert!(!target.entered);
        target.on_drag_enter();
        assert!(target.entered);
        target.on_drag_leave();
        assert!(!target.entered);
    }

    // === DragPreviewConfig tests ===

    #[test]
    fn preview_config_defaults() {
        let cfg = DragPreviewConfig::default();
        assert!((cfg.opacity - 0.7).abs() < f32::EPSILON);
        assert_eq!(cfg.offset_x, 1);
        assert_eq!(cfg.offset_y, 1);
        assert_eq!(cfg.width, 20);
        assert_eq!(cfg.height, 1);
        assert!(cfg.background.is_none());
        assert!(!cfg.show_border);
    }

    #[test]
    fn preview_config_builder() {
        let cfg = DragPreviewConfig::default()
            .with_opacity(0.5)
            .with_offset(2, 3)
            .with_size(30, 5)
            .with_background(PackedRgba::rgb(40, 40, 40))
            .with_border();
        assert!((cfg.opacity - 0.5).abs() < f32::EPSILON);
        assert_eq!(cfg.offset_x, 2);
        assert_eq!(cfg.offset_y, 3);
        assert_eq!(cfg.width, 30);
        assert_eq!(cfg.height, 5);
        assert!(cfg.background.is_some());
        assert!(cfg.show_border);
    }

    #[test]
    fn preview_config_opacity_clamped() {
        let cfg = DragPreviewConfig::default().with_opacity(2.0);
        assert!((cfg.opacity - 1.0).abs() < f32::EPSILON);
        let cfg = DragPreviewConfig::default().with_opacity(-0.5);
        assert!((cfg.opacity - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn preview_rect_basic() {
        let cfg = DragPreviewConfig::default().with_size(10, 3);
        let viewport = Rect::new(0, 0, 80, 24);
        let cursor = Position::new(10, 5);
        let rect = cfg.preview_rect(cursor, viewport).unwrap();
        assert_eq!(rect.x, 11); // cursor.x + offset_x
        assert_eq!(rect.y, 6); // cursor.y + offset_y
        assert_eq!(rect.width, 10);
        assert_eq!(rect.height, 3);
    }

    #[test]
    fn preview_rect_clamped_to_right_edge() {
        let cfg = DragPreviewConfig::default().with_size(10, 1);
        let viewport = Rect::new(0, 0, 80, 24);
        let cursor = Position::new(75, 5);
        let rect = cfg.preview_rect(cursor, viewport).unwrap();
        // Should be clamped so it doesn't extend past viewport
        assert!(rect.x + rect.width <= 80);
    }

    #[test]
    fn preview_rect_clamped_to_bottom_edge() {
        let cfg = DragPreviewConfig::default().with_size(10, 3);
        let viewport = Rect::new(0, 0, 80, 24);
        let cursor = Position::new(5, 22);
        let rect = cfg.preview_rect(cursor, viewport).unwrap();
        assert!(rect.y + rect.height <= 24);
    }

    #[test]
    fn preview_rect_at_origin() {
        let cfg = DragPreviewConfig::default()
            .with_offset(0, 0)
            .with_size(5, 2);
        let viewport = Rect::new(0, 0, 80, 24);
        let cursor = Position::new(0, 0);
        let rect = cfg.preview_rect(cursor, viewport).unwrap();
        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, 0);
    }

    #[test]
    fn preview_rect_viewport_offset() {
        let cfg = DragPreviewConfig::default()
            .with_offset(-5, -5)
            .with_size(10, 3);
        let viewport = Rect::new(10, 10, 60, 14);
        let cursor = Position::new(12, 12);
        let rect = cfg.preview_rect(cursor, viewport).unwrap();
        // Should clamp to viewport origin
        assert!(rect.x >= viewport.x);
        assert!(rect.y >= viewport.y);
    }

    // === DragPreview widget tests ===

    #[test]
    fn drag_preview_new() {
        let state = DragState::new(WidgetId(1), DragPayload::text("hello"), Position::new(5, 5));
        let preview = DragPreview::new(&state);
        assert!((preview.config.opacity - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn drag_preview_with_config() {
        let state = DragState::new(WidgetId(1), DragPayload::text("hello"), Position::new(5, 5));
        let cfg = DragPreviewConfig::default().with_opacity(0.5);
        let preview = DragPreview::with_config(&state, cfg);
        assert!((preview.config.opacity - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn drag_preview_is_not_essential() {
        let state = DragState::new(WidgetId(1), DragPayload::text("hello"), Position::new(5, 5));
        let preview = DragPreview::new(&state);
        assert!(!preview.is_essential());
    }

    #[test]
    fn drag_preview_render_text_fallback() {
        use ftui_render::grapheme_pool::GraphemePool;

        let state = DragState::new(
            WidgetId(1),
            DragPayload::text("dragged item"),
            Position::new(5, 5),
        );
        let preview =
            DragPreview::with_config(&state, DragPreviewConfig::default().with_size(20, 1));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let viewport = Rect::new(0, 0, 80, 24);
        preview.render(viewport, &mut frame);

        // Text should appear at cursor + offset = (6, 6)
        let cell = frame.buffer.get(6, 6).unwrap();
        assert_eq!(cell.content.as_char(), Some('d')); // first char of "dragged item"
    }

    #[test]
    fn drag_preview_render_with_border() {
        use ftui_render::grapheme_pool::GraphemePool;

        let state = DragState::new(WidgetId(1), DragPayload::text("hi"), Position::new(5, 5));
        let preview = DragPreview::with_config(
            &state,
            DragPreviewConfig::default().with_size(10, 3).with_border(),
        );

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let viewport = Rect::new(0, 0, 80, 24);
        preview.render(viewport, &mut frame);

        // Top-left corner should be '┌' at (6, 6)
        let corner = frame.buffer.get(6, 6).unwrap();
        assert_eq!(corner.content.as_char(), Some('┌'));
    }

    #[test]
    fn drag_preview_empty_area_noop() {
        use ftui_render::grapheme_pool::GraphemePool;

        let state = DragState::new(WidgetId(1), DragPayload::text("hi"), Position::new(0, 0));
        let preview = DragPreview::new(&state);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        // Empty area should not panic
        preview.render(Rect::new(0, 0, 0, 0), &mut frame);
    }

    // === Integration: DragState with Draggable ===

    fn run_drag_sequence(
        source: &mut DragSourceFixture,
        target: Option<&mut DropListFixture>,
        start: Position,
        moves: &[Position],
    ) -> (DragState, Option<DropResult>, Vec<String>) {
        let mut log = Vec::new();
        log.push(format!("event:start pos=({},{})", start.x, start.y));

        source.on_drag_start();
        log.extend(source.drain_log());

        let payload = source.drag_data();
        let mut state = DragState::new(WidgetId(99), payload, start);

        for (idx, pos) in moves.iter().enumerate() {
            state.update_position(*pos);
            log.push(format!(
                "event:move#{idx} pos=({},{}) delta={:?}",
                pos.x,
                pos.y,
                state.delta()
            ));
        }

        let drop_result = if let Some(target) = target {
            if target.can_accept(&state.payload.drag_type) {
                target.on_drag_enter();
                log.extend(target.drain_log());
                let pos = target.drop_position(state.current_pos, &state.payload);
                log.push(format!("event:drop_position={pos:?}"));
                let result = target.on_drop(state.payload.clone(), pos);
                log.extend(target.drain_log());
                target.on_drag_leave();
                log.extend(target.drain_log());
                source.on_drag_end(result.is_accepted());
                log.extend(source.drain_log());
                Some(result)
            } else {
                source.on_drag_end(false);
                log.extend(source.drain_log());
                None
            }
        } else {
            source.on_drag_end(false);
            log.extend(source.drain_log());
            None
        };

        (state, drop_result, log)
    }

    #[test]
    fn full_drag_lifecycle() {
        let mut source = DragSourceFixture::new("file.txt");
        let moves = [Position::new(10, 8), Position::new(20, 15)];
        let (state, result, log) =
            run_drag_sequence(&mut source, None, Position::new(5, 5), &moves);

        assert!(result.is_none(), "unexpected drop result for no target");
        assert_eq!(state.distance(), 25, "distance mismatch after moves");
        assert_eq!(source.ended_with, Some(false));
        assert_eq!(
            state.payload.as_text(),
            Some("file.txt"),
            "payload text mismatch after drag"
        );
        assert_eq!(
            log,
            vec![
                "event:start pos=(5,5)".to_string(),
                "source:start label=file.txt".to_string(),
                "event:move#0 pos=(10,8) delta=(5, 3)".to_string(),
                "event:move#1 pos=(20,15) delta=(15, 10)".to_string(),
                "source:end label=file.txt success=false".to_string(),
            ],
            "drag log mismatch"
        );
    }

    #[test]
    fn full_drag_and_drop_lifecycle() {
        let mut source = DragSourceFixture::new("item-A");
        let mut target = DropListFixture::new(&["text/plain"]);
        target.items = vec!["existing".into()];

        let moves = [Position::new(10, 5)];
        let (_state, result, log) =
            run_drag_sequence(&mut source, Some(&mut target), Position::new(0, 0), &moves);

        let result = match result {
            Some(result) => result,
            None => unreachable!("expected drop result from target"),
        };

        assert!(result.is_accepted(), "drop result should be accepted");
        assert_eq!(source.ended_with, Some(true));
        assert!(!target.entered, "target should be left after drop");
        assert_eq!(target.items.len(), 2, "target item count mismatch");
        assert_eq!(
            log,
            vec![
                "event:start pos=(0,0)".to_string(),
                "source:start label=item-A".to_string(),
                "event:move#0 pos=(10,5) delta=(10, 5)".to_string(),
                "target:enter".to_string(),
                "event:drop_position=Append".to_string(),
                "target:drop text=item-A position=Append".to_string(),
                "target:leave".to_string(),
                "source:end label=item-A success=true".to_string(),
            ],
            "drag/drop log mismatch"
        );
    }
}

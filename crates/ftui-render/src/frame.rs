#![forbid(unsafe_code)]

//! Frame = Buffer + metadata for a render pass.
//!
//! The `Frame` is the render target that `Model::view()` methods write to.
//! It bundles the cell grid ([`Buffer`]) with metadata for cursor and
//! mouse hit testing.
//!
//! # Design Rationale
//!
//! Frame does NOT own pools (GraphemePool, LinkRegistry) - those are passed
//! separately or accessed via RenderContext to allow sharing across frames.
//!
//! # Usage
//!
//! ```
//! use ftui_render::frame::Frame;
//! use ftui_render::cell::Cell;
//! use ftui_render::grapheme_pool::GraphemePool;
//!
//! let mut pool = GraphemePool::new();
//! let mut frame = Frame::new(80, 24, &mut pool);
//!
//! // Draw content
//! frame.buffer.set_raw(0, 0, Cell::from_char('H'));
//! frame.buffer.set_raw(1, 0, Cell::from_char('i'));
//!
//! // Set cursor
//! frame.set_cursor(Some((2, 0)));
//! ```

use crate::arena::FrameArena;
use crate::budget::DegradationLevel;
use crate::buffer::Buffer;
use crate::cell::{Cell, CellContent, GraphemeId};
use crate::drawing::{BorderChars, Draw};
use crate::grapheme_pool::GraphemePool;
use crate::{display_width, grapheme_width};
use ftui_core::geometry::Rect;
use unicode_segmentation::UnicodeSegmentation;

/// Identifier for a clickable region in the hit grid.
///
/// Widgets register hit regions with unique IDs to enable mouse interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HitId(pub u32);

impl HitId {
    /// Create a new hit ID from a raw value.
    #[inline]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Get the raw ID value.
    #[inline]
    pub const fn id(self) -> u32 {
        self.0
    }
}

/// Opaque user data for hit callbacks.
pub type HitData = u64;

/// Regions within a widget for mouse interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HitRegion {
    /// No interactive region.
    #[default]
    None,
    /// Main content area.
    Content,
    /// Widget border area.
    Border,
    /// Scrollbar track or thumb.
    Scrollbar,
    /// Resize handle or drag target.
    Handle,
    /// Clickable button.
    Button,
    /// Hyperlink.
    Link,
    /// Custom region tag.
    Custom(u8),
}

/// A single hit cell in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HitCell {
    /// Widget that registered this cell, if any.
    pub widget_id: Option<HitId>,
    /// Region tag for the hit area.
    pub region: HitRegion,
    /// Extra data attached to this hit cell.
    pub data: HitData,
}

impl HitCell {
    /// Create a populated hit cell.
    #[inline]
    pub const fn new(widget_id: HitId, region: HitRegion, data: HitData) -> Self {
        Self {
            widget_id: Some(widget_id),
            region,
            data,
        }
    }

    /// Check if the cell is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.widget_id.is_none()
    }
}

/// Hit testing grid for mouse interaction.
///
/// Maps screen positions to widget IDs, enabling widgets to receive
/// mouse events for their regions.
#[derive(Debug, Clone)]
pub struct HitGrid {
    width: u16,
    height: u16,
    cells: Vec<HitCell>,
}

impl HitGrid {
    /// Create a new hit grid with the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        let size = width as usize * height as usize;
        Self {
            width,
            height,
            cells: vec![HitCell::default(); size],
        }
    }

    /// Grid width.
    #[inline]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Grid height.
    #[inline]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Convert (x, y) to linear index.
    #[inline]
    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if x < self.width && y < self.height {
            Some(y as usize * self.width as usize + x as usize)
        } else {
            None
        }
    }

    /// Get the hit cell at (x, y).
    #[inline]
    #[must_use]
    pub fn get(&self, x: u16, y: u16) -> Option<&HitCell> {
        self.index(x, y).map(|i| &self.cells[i])
    }

    /// Get mutable reference to hit cell at (x, y).
    #[inline]
    #[must_use]
    pub fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut HitCell> {
        self.index(x, y).map(|i| &mut self.cells[i])
    }

    /// Register a clickable region with the given hit metadata.
    ///
    /// All cells within the rectangle will map to this hit cell.
    pub fn register(&mut self, rect: Rect, widget_id: HitId, region: HitRegion, data: HitData) {
        // Use usize to avoid overflow for large coordinates
        let x_end = (rect.x as usize + rect.width as usize).min(self.width as usize);
        let y_end = (rect.y as usize + rect.height as usize).min(self.height as usize);

        // Check if there's anything to do
        if rect.x as usize >= x_end || rect.y as usize >= y_end {
            return;
        }

        let hit_cell = HitCell::new(widget_id, region, data);

        for y in rect.y as usize..y_end {
            let row_start = y * self.width as usize;
            let start = row_start + rect.x as usize;
            let end = row_start + x_end;

            // Optimize: use slice fill for contiguous memory access
            self.cells[start..end].fill(hit_cell);
        }
    }

    /// Hit test at the given position.
    ///
    /// Returns the hit tuple if a region is registered at (x, y).
    #[must_use]
    pub fn hit_test(&self, x: u16, y: u16) -> Option<(HitId, HitRegion, HitData)> {
        self.get(x, y)
            .and_then(|cell| cell.widget_id.map(|id| (id, cell.region, cell.data)))
    }

    /// Return all hits within the given rectangle.
    pub fn hits_in(&self, rect: Rect) -> Vec<(HitId, HitRegion, HitData)> {
        let x_end = (rect.x as usize + rect.width as usize).min(self.width as usize) as u16;
        let y_end = (rect.y as usize + rect.height as usize).min(self.height as usize) as u16;
        let mut hits = Vec::new();

        for y in rect.y..y_end {
            for x in rect.x..x_end {
                if let Some((id, region, data)) = self.hit_test(x, y) {
                    hits.push((id, region, data));
                }
            }
        }

        hits
    }

    /// Clear all hit regions.
    pub fn clear(&mut self) {
        self.cells.fill(HitCell::default());
    }
}

use crate::link_registry::LinkRegistry;

/// Source of the cost estimate for widget scheduling.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CostEstimateSource {
    /// Measured from recent render timings.
    Measured,
    /// Derived from area-based fallback (cells * cost_per_cell).
    AreaFallback,
    /// Fixed default when no signals exist.
    #[default]
    FixedDefault,
}

/// Per-widget scheduling signals captured during rendering.
///
/// These signals are used by runtime policies (budgeted refresh, greedy
/// selection) to prioritize which widgets to render when budget is tight.
#[derive(Debug, Clone)]
pub struct WidgetSignal {
    /// Stable widget identifier.
    pub widget_id: u64,
    /// Whether this widget is essential.
    pub essential: bool,
    /// Base priority in [0, 1].
    pub priority: f32,
    /// Milliseconds since last render.
    pub staleness_ms: u64,
    /// Focus boost in [0, 1].
    pub focus_boost: f32,
    /// Interaction boost in [0, 1].
    pub interaction_boost: f32,
    /// Widget area in cells (width * height).
    pub area_cells: u32,
    /// Estimated render cost in microseconds.
    pub cost_estimate_us: f32,
    /// Recent measured cost (EMA), if available.
    pub recent_cost_us: f32,
    /// Cost estimate provenance.
    pub estimate_source: CostEstimateSource,
}

impl Default for WidgetSignal {
    fn default() -> Self {
        Self {
            widget_id: 0,
            essential: false,
            priority: 0.5,
            staleness_ms: 0,
            focus_boost: 0.0,
            interaction_boost: 0.0,
            area_cells: 1,
            cost_estimate_us: 5.0,
            recent_cost_us: 5.0,
            estimate_source: CostEstimateSource::FixedDefault,
        }
    }
}

impl WidgetSignal {
    /// Create a widget signal with neutral defaults.
    #[must_use]
    pub fn new(widget_id: u64) -> Self {
        Self {
            widget_id,
            ..Self::default()
        }
    }
}

/// Widget render budget policy for a single frame.
#[derive(Debug, Clone)]
pub struct WidgetBudget {
    allow_list: Option<Vec<u64>>,
}

impl Default for WidgetBudget {
    fn default() -> Self {
        Self::allow_all()
    }
}

impl WidgetBudget {
    /// Allow all widgets to render.
    #[must_use]
    pub fn allow_all() -> Self {
        Self { allow_list: None }
    }

    /// Allow only a specific set of widget IDs.
    #[must_use]
    pub fn allow_only(mut ids: Vec<u64>) -> Self {
        ids.sort_unstable();
        ids.dedup();
        Self {
            allow_list: Some(ids),
        }
    }

    /// Check whether a widget should be rendered.
    #[inline]
    pub fn allows(&self, widget_id: u64, essential: bool) -> bool {
        if essential {
            return true;
        }
        match &self.allow_list {
            None => true,
            Some(ids) => ids.binary_search(&widget_id).is_ok(),
        }
    }
}

/// Frame = Buffer + metadata for a render pass.
///
/// The Frame is passed to `Model::view()` and contains everything needed
/// to render a single frame. The Buffer holds cells; metadata controls
/// cursor and enables mouse hit testing.
///
/// # Lifetime
///
/// The frame borrows the `GraphemePool` from the runtime, so it cannot outlive
/// the render pass. This is correct because frames are ephemeral render targets.
#[derive(Debug)]
pub struct Frame<'a> {
    /// The cell grid for this render pass.
    pub buffer: Buffer,

    /// Reference to the grapheme pool for interning strings.
    pub pool: &'a mut GraphemePool,

    /// Optional reference to link registry for hyperlinks.
    pub links: Option<&'a mut LinkRegistry>,

    /// Optional hit grid for mouse hit testing.
    ///
    /// When `Some`, widgets can register clickable regions.
    pub hit_grid: Option<HitGrid>,

    /// Widget render budget policy for this frame.
    pub widget_budget: WidgetBudget,

    /// Collected per-widget scheduling signals for this frame.
    pub widget_signals: Vec<WidgetSignal>,

    /// Cursor position (if app wants to show cursor).
    ///
    /// Coordinates are relative to buffer (0-indexed).
    pub cursor_position: Option<(u16, u16)>,

    /// Whether cursor should be visible.
    pub cursor_visible: bool,

    /// Current degradation level from the render budget.
    ///
    /// Widgets can read this to skip expensive operations when the
    /// budget is constrained (e.g., use ASCII borders instead of
    /// Unicode, skip decorative rendering, etc.).
    pub degradation: DegradationLevel,

    /// Optional per-frame bump arena for temporary allocations.
    ///
    /// When set, widgets can use this arena for scratch allocations that
    /// only live for the current frame (e.g., formatted strings, temporary
    /// slices). The arena is reset at frame boundaries, eliminating
    /// allocator churn on the hot render path.
    pub arena: Option<&'a FrameArena>,
}

impl<'a> Frame<'a> {
    /// Create a new frame with given dimensions and grapheme pool.
    ///
    /// The frame starts with no hit grid and visible cursor at no position.
    pub fn new(width: u16, height: u16, pool: &'a mut GraphemePool) -> Self {
        Self {
            buffer: Buffer::new(width, height),
            pool,
            links: None,
            hit_grid: None,
            widget_budget: WidgetBudget::default(),
            widget_signals: Vec::new(),
            cursor_position: None,
            cursor_visible: true,
            degradation: DegradationLevel::Full,
            arena: None,
        }
    }

    /// Create a frame from an existing buffer.
    ///
    /// This avoids per-frame buffer allocation when callers reuse buffers.
    pub fn from_buffer(buffer: Buffer, pool: &'a mut GraphemePool) -> Self {
        Self {
            buffer,
            pool,
            links: None,
            hit_grid: None,
            widget_budget: WidgetBudget::default(),
            widget_signals: Vec::new(),
            cursor_position: None,
            cursor_visible: true,
            degradation: DegradationLevel::Full,
            arena: None,
        }
    }

    /// Create a new frame with grapheme pool and link registry.
    ///
    /// This avoids double-borrowing issues when both pool and links
    /// come from the same parent struct.
    pub fn with_links(
        width: u16,
        height: u16,
        pool: &'a mut GraphemePool,
        links: &'a mut LinkRegistry,
    ) -> Self {
        Self {
            buffer: Buffer::new(width, height),
            pool,
            links: Some(links),
            hit_grid: None,
            widget_budget: WidgetBudget::default(),
            widget_signals: Vec::new(),
            cursor_position: None,
            cursor_visible: true,
            degradation: DegradationLevel::Full,
            arena: None,
        }
    }

    /// Create a frame with hit testing enabled.
    ///
    /// The hit grid allows widgets to register clickable regions.
    pub fn with_hit_grid(width: u16, height: u16, pool: &'a mut GraphemePool) -> Self {
        Self {
            buffer: Buffer::new(width, height),
            pool,
            links: None,
            hit_grid: Some(HitGrid::new(width, height)),
            widget_budget: WidgetBudget::default(),
            widget_signals: Vec::new(),
            cursor_position: None,
            cursor_visible: true,
            degradation: DegradationLevel::Full,
            arena: None,
        }
    }

    /// Set the link registry for this frame.
    pub fn set_links(&mut self, links: &'a mut LinkRegistry) {
        self.links = Some(links);
    }

    /// Set the per-frame bump arena for temporary allocations.
    ///
    /// Widgets can access the arena via [`arena()`](Self::arena) to
    /// perform scratch allocations that only live for the current frame.
    pub fn set_arena(&mut self, arena: &'a FrameArena) {
        self.arena = Some(arena);
    }

    /// Returns the per-frame bump arena, if set.
    ///
    /// Widgets should use this for temporary allocations (formatted strings,
    /// scratch slices) to avoid per-frame allocator churn.
    pub fn arena(&self) -> Option<&FrameArena> {
        self.arena
    }

    /// Register a hyperlink URL and return its ID.
    ///
    /// Returns 0 if link registry is not available or full.
    pub fn register_link(&mut self, url: &str) -> u32 {
        if let Some(ref mut links) = self.links {
            links.register(url)
        } else {
            0
        }
    }

    /// Set the widget render budget for this frame.
    pub fn set_widget_budget(&mut self, budget: WidgetBudget) {
        self.widget_budget = budget;
    }

    /// Check whether a widget should be rendered under the current budget.
    #[inline]
    pub fn should_render_widget(&self, widget_id: u64, essential: bool) -> bool {
        self.widget_budget.allows(widget_id, essential)
    }

    /// Register a widget scheduling signal for this frame.
    pub fn register_widget_signal(&mut self, signal: WidgetSignal) {
        self.widget_signals.push(signal);
    }

    /// Borrow the collected widget signals.
    #[inline]
    pub fn widget_signals(&self) -> &[WidgetSignal] {
        &self.widget_signals
    }

    /// Take the collected widget signals, leaving an empty list.
    #[inline]
    pub fn take_widget_signals(&mut self) -> Vec<WidgetSignal> {
        std::mem::take(&mut self.widget_signals)
    }

    /// Intern a string in the grapheme pool.
    ///
    /// Returns a `GraphemeId` that can be used to create a `Cell`.
    /// The width is calculated automatically or can be provided if already known.
    ///
    /// # Panics
    ///
    /// Panics if width > 127.
    pub fn intern(&mut self, text: &str) -> GraphemeId {
        let width = display_width(text).min(127) as u8;
        self.pool.intern(text, width)
    }

    /// Intern a string with explicit width.
    pub fn intern_with_width(&mut self, text: &str, width: u8) -> GraphemeId {
        self.pool.intern(text, width)
    }

    /// Enable hit testing on an existing frame.
    pub fn enable_hit_testing(&mut self) {
        if self.hit_grid.is_none() {
            self.hit_grid = Some(HitGrid::new(self.width(), self.height()));
        }
    }

    /// Frame width in cells.
    #[inline]
    pub fn width(&self) -> u16 {
        self.buffer.width()
    }

    /// Frame height in cells.
    #[inline]
    pub fn height(&self) -> u16 {
        self.buffer.height()
    }

    /// Clear frame for next render.
    ///
    /// Resets both the buffer and hit grid (if present).
    pub fn clear(&mut self) {
        self.buffer.clear();
        if let Some(ref mut grid) = self.hit_grid {
            grid.clear();
        }
        self.cursor_position = None;
        self.widget_signals.clear();
    }

    /// Set cursor position.
    ///
    /// Pass `None` to indicate no cursor should be shown at a specific position.
    #[inline]
    pub fn set_cursor(&mut self, position: Option<(u16, u16)>) {
        self.cursor_position = position;
    }

    /// Set cursor visibility.
    #[inline]
    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    /// Set the degradation level for this frame.
    ///
    /// Propagates to the buffer so widgets can read `buf.degradation`
    /// during rendering without needing access to the full Frame.
    #[inline]
    pub fn set_degradation(&mut self, level: DegradationLevel) {
        self.degradation = level;
        self.buffer.degradation = level;
    }

    /// Get the bounding rectangle of the frame.
    #[inline]
    pub fn bounds(&self) -> Rect {
        self.buffer.bounds()
    }

    /// Register a hit region (if hit grid is enabled).
    ///
    /// Returns `true` if the region was registered, `false` if no hit grid.
    ///
    /// # Clipping
    ///
    /// The region is intersected with the current scissor stack of the
    /// internal buffer. Parts of the region outside the scissor are
    /// ignored.
    pub fn register_hit(
        &mut self,
        rect: Rect,
        id: HitId,
        region: HitRegion,
        data: HitData,
    ) -> bool {
        if let Some(ref mut grid) = self.hit_grid {
            // Clip against current scissor
            let clipped = rect.intersection(&self.buffer.current_scissor());
            if !clipped.is_empty() {
                grid.register(clipped, id, region, data);
            }
            true
        } else {
            false
        }
    }

    /// Hit test at the given position (if hit grid is enabled).
    #[must_use]
    pub fn hit_test(&self, x: u16, y: u16) -> Option<(HitId, HitRegion, HitData)> {
        self.hit_grid.as_ref().and_then(|grid| grid.hit_test(x, y))
    }

    /// Register a hit region with default metadata (Content, data=0).
    pub fn register_hit_region(&mut self, rect: Rect, id: HitId) -> bool {
        self.register_hit(rect, id, HitRegion::Content, 0)
    }
}

impl<'a> Draw for Frame<'a> {
    fn draw_horizontal_line(&mut self, x: u16, y: u16, width: u16, cell: Cell) {
        self.buffer.draw_horizontal_line(x, y, width, cell);
    }

    fn draw_vertical_line(&mut self, x: u16, y: u16, height: u16, cell: Cell) {
        self.buffer.draw_vertical_line(x, y, height, cell);
    }

    fn draw_rect_filled(&mut self, rect: Rect, cell: Cell) {
        self.buffer.draw_rect_filled(rect, cell);
    }

    fn draw_rect_outline(&mut self, rect: Rect, cell: Cell) {
        self.buffer.draw_rect_outline(rect, cell);
    }

    fn print_text(&mut self, x: u16, y: u16, text: &str, base_cell: Cell) -> u16 {
        self.print_text_clipped(x, y, text, base_cell, self.width())
    }

    fn print_text_clipped(
        &mut self,
        x: u16,
        y: u16,
        text: &str,
        base_cell: Cell,
        max_x: u16,
    ) -> u16 {
        let mut cx = x;
        for grapheme in text.graphemes(true) {
            let width = grapheme_width(grapheme);
            if width == 0 {
                continue;
            }

            if cx >= max_x {
                break;
            }

            // Don't start a wide char if it won't fit
            if cx as u32 + width as u32 > max_x as u32 {
                break;
            }

            // Intern grapheme if needed (unlike Buffer::print_text, we have the pool!)
            let content = if width > 1 || grapheme.chars().count() > 1 {
                let id = self.intern_with_width(grapheme, width as u8);
                CellContent::from_grapheme(id)
            } else if let Some(c) = grapheme.chars().next() {
                CellContent::from_char(c)
            } else {
                continue;
            };

            let cell = Cell {
                content,
                fg: base_cell.fg,
                bg: base_cell.bg,
                attrs: base_cell.attrs,
            };
            self.buffer.set_fast(cx, y, cell);

            cx = cx.saturating_add(width as u16);
        }
        cx
    }

    fn draw_border(&mut self, rect: Rect, chars: BorderChars, base_cell: Cell) {
        self.buffer.draw_border(rect, chars, base_cell);
    }

    fn draw_box(&mut self, rect: Rect, chars: BorderChars, border_cell: Cell, fill_cell: Cell) {
        self.buffer.draw_box(rect, chars, border_cell, fill_cell);
    }

    fn paint_area(
        &mut self,
        rect: Rect,
        fg: Option<crate::cell::PackedRgba>,
        bg: Option<crate::cell::PackedRgba>,
    ) {
        self.buffer.paint_area(rect, fg, bg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    #[test]
    fn frame_creation() {
        let mut pool = GraphemePool::new();
        let frame = Frame::new(80, 24, &mut pool);
        assert_eq!(frame.width(), 80);
        assert_eq!(frame.height(), 24);
        assert!(frame.hit_grid.is_none());
        assert!(frame.cursor_position.is_none());
        assert!(frame.cursor_visible);
    }

    #[test]
    fn frame_with_hit_grid() {
        let mut pool = GraphemePool::new();
        let frame = Frame::with_hit_grid(80, 24, &mut pool);
        assert!(frame.hit_grid.is_some());
        assert_eq!(frame.width(), 80);
        assert_eq!(frame.height(), 24);
    }

    #[test]
    fn frame_cursor() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        assert!(frame.cursor_position.is_none());
        assert!(frame.cursor_visible);

        frame.set_cursor(Some((10, 5)));
        assert_eq!(frame.cursor_position, Some((10, 5)));

        frame.set_cursor_visible(false);
        assert!(!frame.cursor_visible);

        frame.set_cursor(None);
        assert!(frame.cursor_position.is_none());
    }

    #[test]
    fn frame_clear() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(10, 10, &mut pool);

        // Add some content
        frame.buffer.set_raw(5, 5, Cell::from_char('X'));
        frame.register_hit_region(Rect::new(0, 0, 5, 5), HitId::new(1));

        // Verify content exists
        assert_eq!(frame.buffer.get(5, 5).unwrap().content.as_char(), Some('X'));
        assert_eq!(
            frame.hit_test(2, 2),
            Some((HitId::new(1), HitRegion::Content, 0))
        );

        // Clear
        frame.clear();

        // Verify cleared
        assert!(frame.buffer.get(5, 5).unwrap().is_empty());
        assert!(frame.hit_test(2, 2).is_none());
    }

    #[test]
    fn frame_bounds() {
        let mut pool = GraphemePool::new();
        let frame = Frame::new(80, 24, &mut pool);
        let bounds = frame.bounds();
        assert_eq!(bounds.x, 0);
        assert_eq!(bounds.y, 0);
        assert_eq!(bounds.width, 80);
        assert_eq!(bounds.height, 24);
    }

    #[test]
    fn hit_grid_creation() {
        let grid = HitGrid::new(80, 24);
        assert_eq!(grid.width(), 80);
        assert_eq!(grid.height(), 24);
    }

    #[test]
    fn hit_grid_registration() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(80, 24, &mut pool);
        let hit_id = HitId::new(42);
        let rect = Rect::new(10, 5, 20, 3);

        frame.register_hit(rect, hit_id, HitRegion::Button, 99);

        // Inside rect
        assert_eq!(frame.hit_test(15, 6), Some((hit_id, HitRegion::Button, 99)));
        assert_eq!(frame.hit_test(10, 5), Some((hit_id, HitRegion::Button, 99))); // Top-left corner
        assert_eq!(frame.hit_test(29, 7), Some((hit_id, HitRegion::Button, 99))); // Bottom-right corner

        // Outside rect
        assert!(frame.hit_test(5, 5).is_none()); // Left of rect
        assert!(frame.hit_test(30, 6).is_none()); // Right of rect (exclusive)
        assert!(frame.hit_test(15, 8).is_none()); // Below rect
        assert!(frame.hit_test(15, 4).is_none()); // Above rect
    }

    #[test]
    fn hit_grid_overlapping_regions() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(20, 20, &mut pool);

        // Register two overlapping regions
        frame.register_hit(
            Rect::new(0, 0, 10, 10),
            HitId::new(1),
            HitRegion::Content,
            1,
        );
        frame.register_hit(Rect::new(5, 5, 10, 10), HitId::new(2), HitRegion::Border, 2);

        // Non-overlapping region from first
        assert_eq!(
            frame.hit_test(2, 2),
            Some((HitId::new(1), HitRegion::Content, 1))
        );

        // Overlapping region - second wins (last registered)
        assert_eq!(
            frame.hit_test(7, 7),
            Some((HitId::new(2), HitRegion::Border, 2))
        );

        // Non-overlapping region from second
        assert_eq!(
            frame.hit_test(12, 12),
            Some((HitId::new(2), HitRegion::Border, 2))
        );
    }

    #[test]
    fn hit_grid_out_of_bounds() {
        let mut pool = GraphemePool::new();
        let frame = Frame::with_hit_grid(10, 10, &mut pool);

        // Out of bounds returns None
        assert!(frame.hit_test(100, 100).is_none());
        assert!(frame.hit_test(10, 0).is_none()); // Exclusive bound
        assert!(frame.hit_test(0, 10).is_none()); // Exclusive bound
    }

    #[test]
    fn hit_id_properties() {
        let id = HitId::new(42);
        assert_eq!(id.id(), 42);
        assert_eq!(id, HitId(42));
    }

    #[test]
    fn register_hit_region_no_grid() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        let result = frame.register_hit_region(Rect::new(0, 0, 5, 5), HitId::new(1));
        assert!(!result); // No hit grid, returns false
    }

    #[test]
    fn register_hit_region_with_grid() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(10, 10, &mut pool);
        let result = frame.register_hit_region(Rect::new(0, 0, 5, 5), HitId::new(1));
        assert!(result); // Has hit grid, returns true
    }

    #[test]
    fn hit_grid_clear() {
        let mut grid = HitGrid::new(10, 10);
        grid.register(Rect::new(0, 0, 5, 5), HitId::new(1), HitRegion::Content, 0);

        assert_eq!(
            grid.hit_test(2, 2),
            Some((HitId::new(1), HitRegion::Content, 0))
        );

        grid.clear();

        assert!(grid.hit_test(2, 2).is_none());
    }

    #[test]
    fn hit_grid_boundary_clipping() {
        let mut grid = HitGrid::new(10, 10);

        // Register region that extends beyond grid
        grid.register(
            Rect::new(8, 8, 10, 10),
            HitId::new(1),
            HitRegion::Content,
            0,
        );

        // Inside clipped region
        assert_eq!(
            grid.hit_test(9, 9),
            Some((HitId::new(1), HitRegion::Content, 0))
        );

        // Outside grid
        assert!(grid.hit_test(10, 10).is_none());
    }

    #[test]
    fn hit_grid_edge_and_corner_cells() {
        let mut grid = HitGrid::new(4, 4);
        grid.register(Rect::new(3, 0, 1, 4), HitId::new(7), HitRegion::Border, 11);

        // Right-most column corners
        assert_eq!(
            grid.hit_test(3, 0),
            Some((HitId::new(7), HitRegion::Border, 11))
        );
        assert_eq!(
            grid.hit_test(3, 3),
            Some((HitId::new(7), HitRegion::Border, 11))
        );

        // Neighboring cells remain empty
        assert!(grid.hit_test(2, 0).is_none());
        assert!(grid.hit_test(4, 0).is_none());
        assert!(grid.hit_test(3, 4).is_none());

        let mut grid = HitGrid::new(4, 4);
        grid.register(Rect::new(0, 3, 4, 1), HitId::new(9), HitRegion::Content, 21);

        // Bottom row corners
        assert_eq!(
            grid.hit_test(0, 3),
            Some((HitId::new(9), HitRegion::Content, 21))
        );
        assert_eq!(
            grid.hit_test(3, 3),
            Some((HitId::new(9), HitRegion::Content, 21))
        );

        // Outside bottom row
        assert!(grid.hit_test(0, 2).is_none());
        assert!(grid.hit_test(0, 4).is_none());
    }

    #[test]
    fn frame_register_hit_respects_nested_scissor() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(10, 10, &mut pool);

        let outer = Rect::new(1, 1, 8, 8);
        frame.buffer.push_scissor(outer);
        assert_eq!(frame.buffer.current_scissor(), outer);

        let inner = Rect::new(4, 4, 10, 10);
        frame.buffer.push_scissor(inner);
        let clipped = outer.intersection(&inner);
        let current = frame.buffer.current_scissor();
        assert_eq!(current, clipped);

        // Monotonic intersection: inner scissor must stay within outer.
        assert!(outer.contains(current.x, current.y));
        assert!(outer.contains(
            current.right().saturating_sub(1),
            current.bottom().saturating_sub(1)
        ));

        frame.register_hit(
            Rect::new(0, 0, 10, 10),
            HitId::new(3),
            HitRegion::Button,
            99,
        );

        assert_eq!(
            frame.hit_test(4, 4),
            Some((HitId::new(3), HitRegion::Button, 99))
        );
        assert_eq!(
            frame.hit_test(8, 8),
            Some((HitId::new(3), HitRegion::Button, 99))
        );
        assert!(frame.hit_test(3, 3).is_none()); // inside outer, outside inner
        assert!(frame.hit_test(0, 0).is_none()); // outside all scissor

        frame.buffer.pop_scissor();
        assert_eq!(frame.buffer.current_scissor(), outer);
    }

    #[test]
    fn hit_grid_hits_in_area() {
        let mut grid = HitGrid::new(5, 5);
        grid.register(Rect::new(0, 0, 2, 2), HitId::new(1), HitRegion::Content, 10);
        grid.register(Rect::new(1, 1, 2, 2), HitId::new(2), HitRegion::Button, 20);

        let hits = grid.hits_in(Rect::new(0, 0, 3, 3));
        assert!(hits.contains(&(HitId::new(1), HitRegion::Content, 10)));
        assert!(hits.contains(&(HitId::new(2), HitRegion::Button, 20)));
    }

    #[test]
    fn frame_intern() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);

        let id = frame.intern("👋");
        assert_eq!(frame.pool.get(id), Some("👋"));
    }

    #[test]
    fn frame_intern_with_width() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);

        let id = frame.intern_with_width("🧪", 2);
        assert_eq!(id.width(), 2);
        assert_eq!(frame.pool.get(id), Some("🧪"));
    }

    #[test]
    fn frame_print_text_emoji_presentation_sets_continuation() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);

        // Use a skin-tone modifier sequence (width 2, multi-codepoint, no VS16
        // dependency) so the test is independent of ftui-core's VS16 policy.
        frame.print_text(0, 0, "👍🏽", Cell::from_char(' '));

        let head = frame.buffer.get(0, 0).unwrap();
        let tail = frame.buffer.get(1, 0).unwrap();

        assert_eq!(head.content.width(), 2);
        assert!(tail.content.is_continuation());
    }

    #[test]
    fn frame_enable_hit_testing() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        assert!(frame.hit_grid.is_none());

        frame.enable_hit_testing();
        assert!(frame.hit_grid.is_some());

        // Calling again is idempotent
        frame.enable_hit_testing();
        assert!(frame.hit_grid.is_some());
    }

    #[test]
    fn frame_enable_hit_testing_then_register() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        frame.enable_hit_testing();

        let registered = frame.register_hit_region(Rect::new(0, 0, 5, 5), HitId::new(1));
        assert!(registered);
        assert_eq!(
            frame.hit_test(2, 2),
            Some((HitId::new(1), HitRegion::Content, 0))
        );
    }

    #[test]
    fn hit_cell_default_is_empty() {
        let cell = HitCell::default();
        assert!(cell.is_empty());
        assert_eq!(cell.widget_id, None);
        assert_eq!(cell.region, HitRegion::None);
        assert_eq!(cell.data, 0);
    }

    #[test]
    fn hit_cell_new_is_not_empty() {
        let cell = HitCell::new(HitId::new(1), HitRegion::Button, 42);
        assert!(!cell.is_empty());
        assert_eq!(cell.widget_id, Some(HitId::new(1)));
        assert_eq!(cell.region, HitRegion::Button);
        assert_eq!(cell.data, 42);
    }

    #[test]
    fn hit_region_variants() {
        assert_eq!(HitRegion::default(), HitRegion::None);

        // All variants are distinct
        let variants = [
            HitRegion::None,
            HitRegion::Content,
            HitRegion::Border,
            HitRegion::Scrollbar,
            HitRegion::Handle,
            HitRegion::Button,
            HitRegion::Link,
            HitRegion::Custom(0),
            HitRegion::Custom(1),
            HitRegion::Custom(255),
        ];
        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                assert_ne!(
                    variants[i], variants[j],
                    "variants {i} and {j} should differ"
                );
            }
        }
    }

    #[test]
    fn hit_id_default() {
        let id = HitId::default();
        assert_eq!(id.id(), 0);
    }

    #[test]
    fn hit_grid_initial_cells_empty() {
        let grid = HitGrid::new(5, 5);
        for y in 0..5 {
            for x in 0..5 {
                let cell = grid.get(x, y).unwrap();
                assert!(cell.is_empty());
            }
        }
    }

    #[test]
    fn hit_grid_zero_dimensions() {
        let grid = HitGrid::new(0, 0);
        assert_eq!(grid.width(), 0);
        assert_eq!(grid.height(), 0);
        assert!(grid.get(0, 0).is_none());
        assert!(grid.hit_test(0, 0).is_none());
    }

    #[test]
    fn hit_grid_hits_in_empty_area() {
        let grid = HitGrid::new(10, 10);
        let hits = grid.hits_in(Rect::new(0, 0, 5, 5));
        // All cells are empty, so no actual HitId hits
        assert!(hits.is_empty());
    }

    #[test]
    fn hit_grid_hits_in_clipped_area() {
        let mut grid = HitGrid::new(5, 5);
        grid.register(Rect::new(0, 0, 5, 5), HitId::new(1), HitRegion::Content, 0);

        // Query area extends beyond grid — should be clipped
        let hits = grid.hits_in(Rect::new(3, 3, 10, 10));
        assert_eq!(hits.len(), 4); // 2x2 cells inside grid
    }

    #[test]
    fn hit_test_no_grid_returns_none() {
        let mut pool = GraphemePool::new();
        let frame = Frame::new(10, 10, &mut pool);
        assert!(frame.hit_test(0, 0).is_none());
    }

    #[test]
    fn frame_cursor_operations() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        // Set position at edge of frame
        frame.set_cursor(Some((79, 23)));
        assert_eq!(frame.cursor_position, Some((79, 23)));

        // Set position at origin
        frame.set_cursor(Some((0, 0)));
        assert_eq!(frame.cursor_position, Some((0, 0)));

        // Toggle visibility
        frame.set_cursor_visible(false);
        assert!(!frame.cursor_visible);
        frame.set_cursor_visible(true);
        assert!(frame.cursor_visible);
    }

    #[test]
    fn hit_data_large_values() {
        let mut grid = HitGrid::new(5, 5);
        // HitData is u64, test max value
        grid.register(
            Rect::new(0, 0, 1, 1),
            HitId::new(1),
            HitRegion::Content,
            u64::MAX,
        );
        let result = grid.hit_test(0, 0);
        assert_eq!(result, Some((HitId::new(1), HitRegion::Content, u64::MAX)));
    }

    #[test]
    fn hit_id_large_value() {
        let id = HitId::new(u32::MAX);
        assert_eq!(id.id(), u32::MAX);
    }

    #[test]
    fn frame_print_text_interns_complex_graphemes() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);

        // Flag emoji (complex grapheme)
        let flag = "🇺🇸";
        assert!(flag.chars().count() > 1);

        frame.print_text(0, 0, flag, Cell::default());

        let cell = frame.buffer.get(0, 0).unwrap();
        assert!(cell.content.is_grapheme());

        let id = cell.content.grapheme_id().unwrap();
        assert_eq!(frame.pool.get(id), Some(flag));
    }

    // --- HitId trait coverage ---

    #[test]
    fn hit_id_debug_clone_copy_hash() {
        let id = HitId::new(99);
        let dbg = format!("{:?}", id);
        assert!(dbg.contains("99"), "Debug: {dbg}");
        let copied: HitId = id; // Copy
        assert_eq!(id, copied);
        // Hash: insert into set
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(id);
        set.insert(HitId::new(99));
        assert_eq!(set.len(), 1);
        set.insert(HitId::new(100));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn hit_id_eq_and_ne() {
        assert_eq!(HitId::new(0), HitId::new(0));
        assert_ne!(HitId::new(0), HitId::new(1));
        assert_ne!(HitId::new(u32::MAX), HitId::default());
    }

    // --- HitRegion trait coverage ---

    #[test]
    fn hit_region_debug_clone_copy_hash() {
        let r = HitRegion::Custom(42);
        let dbg = format!("{:?}", r);
        assert!(dbg.contains("Custom"), "Debug: {dbg}");
        let copied: HitRegion = r; // Copy
        assert_eq!(r, copied);
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(r);
        set.insert(HitRegion::Custom(42));
        assert_eq!(set.len(), 1);
    }

    // --- HitCell trait coverage ---

    #[test]
    fn hit_cell_debug_clone_copy_eq() {
        let cell = HitCell::new(HitId::new(5), HitRegion::Link, 123);
        let dbg = format!("{:?}", cell);
        assert!(dbg.contains("Link"), "Debug: {dbg}");
        let copied: HitCell = cell; // Copy
        assert_eq!(cell, copied);
        // ne
        assert_ne!(cell, HitCell::default());
    }

    // --- HitGrid edge cases ---

    #[test]
    fn hit_grid_clone() {
        let mut grid = HitGrid::new(5, 5);
        grid.register(Rect::new(0, 0, 2, 2), HitId::new(1), HitRegion::Content, 7);
        let clone = grid.clone();
        assert_eq!(clone.width(), 5);
        assert_eq!(
            clone.hit_test(0, 0),
            Some((HitId::new(1), HitRegion::Content, 7))
        );
    }

    #[test]
    fn hit_grid_get_mut() {
        let mut grid = HitGrid::new(5, 5);
        // Mutate a cell directly
        if let Some(cell) = grid.get_mut(2, 3) {
            *cell = HitCell::new(HitId::new(77), HitRegion::Handle, 55);
        }
        assert_eq!(
            grid.hit_test(2, 3),
            Some((HitId::new(77), HitRegion::Handle, 55))
        );
        // Out of bounds returns None
        assert!(grid.get_mut(5, 5).is_none());
    }

    #[test]
    fn hit_grid_zero_width_nonzero_height() {
        let grid = HitGrid::new(0, 10);
        assert_eq!(grid.width(), 0);
        assert_eq!(grid.height(), 10);
        assert!(grid.get(0, 0).is_none());
        assert!(grid.hit_test(0, 5).is_none());
    }

    #[test]
    fn hit_grid_nonzero_width_zero_height() {
        let grid = HitGrid::new(10, 0);
        assert_eq!(grid.width(), 10);
        assert_eq!(grid.height(), 0);
        assert!(grid.get(0, 0).is_none());
    }

    #[test]
    fn hit_grid_register_zero_width_rect() {
        let mut grid = HitGrid::new(10, 10);
        grid.register(Rect::new(2, 2, 0, 5), HitId::new(1), HitRegion::Content, 0);
        // Nothing should be registered
        assert!(grid.hit_test(2, 2).is_none());
    }

    #[test]
    fn hit_grid_register_zero_height_rect() {
        let mut grid = HitGrid::new(10, 10);
        grid.register(Rect::new(2, 2, 5, 0), HitId::new(1), HitRegion::Content, 0);
        assert!(grid.hit_test(2, 2).is_none());
    }

    #[test]
    fn hit_grid_register_past_bounds() {
        let mut grid = HitGrid::new(10, 10);
        // Rect starts past the grid boundary
        grid.register(
            Rect::new(10, 10, 5, 5),
            HitId::new(1),
            HitRegion::Content,
            0,
        );
        assert!(grid.hit_test(9, 9).is_none());
    }

    #[test]
    fn hit_grid_full_coverage() {
        let mut grid = HitGrid::new(3, 3);
        grid.register(Rect::new(0, 0, 3, 3), HitId::new(1), HitRegion::Content, 0);
        // Every cell should be filled
        for y in 0..3 {
            for x in 0..3 {
                assert_eq!(
                    grid.hit_test(x, y),
                    Some((HitId::new(1), HitRegion::Content, 0))
                );
            }
        }
    }

    #[test]
    fn hit_grid_single_cell() {
        let mut grid = HitGrid::new(1, 1);
        grid.register(Rect::new(0, 0, 1, 1), HitId::new(1), HitRegion::Button, 42);
        assert_eq!(
            grid.hit_test(0, 0),
            Some((HitId::new(1), HitRegion::Button, 42))
        );
        assert!(grid.hit_test(1, 0).is_none());
        assert!(grid.hit_test(0, 1).is_none());
    }

    #[test]
    fn hit_grid_hits_in_outside_rect() {
        let mut grid = HitGrid::new(5, 5);
        grid.register(Rect::new(0, 0, 2, 2), HitId::new(1), HitRegion::Content, 0);
        // Query area completely outside registered region
        let hits = grid.hits_in(Rect::new(3, 3, 2, 2));
        assert!(hits.is_empty());
    }

    #[test]
    fn hit_grid_hits_in_zero_rect() {
        let mut grid = HitGrid::new(5, 5);
        grid.register(Rect::new(0, 0, 5, 5), HitId::new(1), HitRegion::Content, 0);
        let hits = grid.hits_in(Rect::new(2, 2, 0, 0));
        assert!(hits.is_empty());
    }

    // --- CostEstimateSource ---

    #[test]
    fn cost_estimate_source_traits() {
        let a = CostEstimateSource::Measured;
        let b = CostEstimateSource::AreaFallback;
        let c = CostEstimateSource::FixedDefault;
        let dbg = format!("{:?}", a);
        assert!(dbg.contains("Measured"), "Debug: {dbg}");

        // Default
        assert_eq!(
            CostEstimateSource::default(),
            CostEstimateSource::FixedDefault
        );

        // Clone/Copy
        let copied: CostEstimateSource = a;
        assert_eq!(a, copied);

        // All variants distinct
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    // --- WidgetSignal ---

    #[test]
    fn widget_signal_default() {
        let sig = WidgetSignal::default();
        assert_eq!(sig.widget_id, 0);
        assert!(!sig.essential);
        assert!((sig.priority - 0.5).abs() < f32::EPSILON);
        assert_eq!(sig.staleness_ms, 0);
        assert!((sig.focus_boost - 0.0).abs() < f32::EPSILON);
        assert!((sig.interaction_boost - 0.0).abs() < f32::EPSILON);
        assert_eq!(sig.area_cells, 1);
        assert!((sig.cost_estimate_us - 5.0).abs() < f32::EPSILON);
        assert!((sig.recent_cost_us - 5.0).abs() < f32::EPSILON);
        assert_eq!(sig.estimate_source, CostEstimateSource::FixedDefault);
    }

    #[test]
    fn widget_signal_new() {
        let sig = WidgetSignal::new(42);
        assert_eq!(sig.widget_id, 42);
        // Other fields should be default
        assert!(!sig.essential);
        assert!((sig.priority - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn widget_signal_debug_clone() {
        let sig = WidgetSignal::new(7);
        let dbg = format!("{:?}", sig);
        assert!(dbg.contains("widget_id"), "Debug: {dbg}");
        let cloned = sig.clone();
        assert_eq!(cloned.widget_id, 7);
    }

    // --- WidgetBudget ---

    #[test]
    fn widget_budget_default_is_allow_all() {
        let budget = WidgetBudget::default();
        assert!(budget.allows(0, false));
        assert!(budget.allows(u64::MAX, false));
        assert!(budget.allows(42, true));
    }

    #[test]
    fn widget_budget_allow_only() {
        let budget = WidgetBudget::allow_only(vec![10, 20, 30]);
        assert!(budget.allows(10, false));
        assert!(budget.allows(20, false));
        assert!(budget.allows(30, false));
        assert!(!budget.allows(15, false));
        assert!(!budget.allows(0, false));
    }

    #[test]
    fn widget_budget_essential_always_allowed() {
        let budget = WidgetBudget::allow_only(vec![10]);
        // Essential widgets bypass the allow list
        assert!(budget.allows(999, true));
        assert!(budget.allows(0, true));
    }

    #[test]
    fn widget_budget_allow_only_dedup() {
        let budget = WidgetBudget::allow_only(vec![5, 5, 5, 10, 10]);
        assert!(budget.allows(5, false));
        assert!(budget.allows(10, false));
        assert!(!budget.allows(7, false));
    }

    #[test]
    fn widget_budget_allow_only_empty() {
        let budget = WidgetBudget::allow_only(vec![]);
        // No widgets allowed (except essential)
        assert!(!budget.allows(0, false));
        assert!(!budget.allows(1, false));
        assert!(budget.allows(1, true)); // essential always passes
    }

    #[test]
    fn widget_budget_debug_clone() {
        let budget = WidgetBudget::allow_only(vec![1, 2, 3]);
        let dbg = format!("{:?}", budget);
        assert!(dbg.contains("allow_list"), "Debug: {dbg}");
        let cloned = budget.clone();
        assert!(cloned.allows(2, false));
    }

    // --- Frame construction variants ---

    #[test]
    #[should_panic(expected = "buffer width must be > 0")]
    fn frame_zero_dimensions_panics() {
        let mut pool = GraphemePool::new();
        let _frame = Frame::new(0, 0, &mut pool);
    }

    #[test]
    fn frame_from_buffer() {
        let mut pool = GraphemePool::new();
        let mut buf = Buffer::new(20, 10);
        buf.set_raw(5, 5, Cell::from_char('Z'));
        let frame = Frame::from_buffer(buf, &mut pool);
        assert_eq!(frame.width(), 20);
        assert_eq!(frame.height(), 10);
        assert_eq!(frame.buffer.get(5, 5).unwrap().content.as_char(), Some('Z'));
        assert!(frame.hit_grid.is_none());
        assert!(frame.cursor_visible);
    }

    #[test]
    fn frame_with_links() {
        let mut pool = GraphemePool::new();
        let mut links = LinkRegistry::new();
        let frame = Frame::with_links(10, 5, &mut pool, &mut links);
        assert!(frame.links.is_some());
        assert_eq!(frame.width(), 10);
        assert_eq!(frame.height(), 5);
    }

    #[test]
    fn frame_set_links() {
        let mut pool = GraphemePool::new();
        let mut links = LinkRegistry::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        assert!(frame.links.is_none());
        frame.set_links(&mut links);
        assert!(frame.links.is_some());
    }

    #[test]
    fn frame_register_link_no_registry() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        // No link registry => returns 0
        let id = frame.register_link("https://example.com");
        assert_eq!(id, 0);
    }

    #[test]
    fn frame_register_link_with_registry() {
        let mut pool = GraphemePool::new();
        let mut links = LinkRegistry::new();
        let mut frame = Frame::with_links(10, 5, &mut pool, &mut links);
        let id = frame.register_link("https://example.com");
        assert!(id > 0);
        // Same URL should return same ID
        let id2 = frame.register_link("https://example.com");
        assert_eq!(id, id2);
        // Different URL should return different ID
        let id3 = frame.register_link("https://other.com");
        assert_ne!(id, id3);
    }

    // --- Frame widget budget integration ---

    #[test]
    fn frame_set_widget_budget() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);

        // Default allows all
        assert!(frame.should_render_widget(42, false));

        // Set restricted budget
        frame.set_widget_budget(WidgetBudget::allow_only(vec![1, 2]));
        assert!(frame.should_render_widget(1, false));
        assert!(!frame.should_render_widget(42, false));
        assert!(frame.should_render_widget(42, true)); // essential
    }

    // --- Frame widget signals ---

    #[test]
    fn frame_widget_signals_lifecycle() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        assert!(frame.widget_signals().is_empty());

        frame.register_widget_signal(WidgetSignal::new(1));
        frame.register_widget_signal(WidgetSignal::new(2));
        assert_eq!(frame.widget_signals().len(), 2);
        assert_eq!(frame.widget_signals()[0].widget_id, 1);
        assert_eq!(frame.widget_signals()[1].widget_id, 2);

        let taken = frame.take_widget_signals();
        assert_eq!(taken.len(), 2);
        assert!(frame.widget_signals().is_empty());
    }

    #[test]
    fn frame_clear_resets_signals_and_cursor() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        frame.set_cursor(Some((5, 5)));
        frame.register_widget_signal(WidgetSignal::new(1));
        assert!(frame.cursor_position.is_some());
        assert!(!frame.widget_signals().is_empty());

        frame.clear();
        assert!(frame.cursor_position.is_none());
        assert!(frame.widget_signals().is_empty());
    }

    // --- Frame degradation ---

    #[test]
    fn frame_set_degradation_propagates_to_buffer() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        assert_eq!(frame.degradation, DegradationLevel::Full);
        assert_eq!(frame.buffer.degradation, DegradationLevel::Full);

        frame.set_degradation(DegradationLevel::SimpleBorders);
        assert_eq!(frame.degradation, DegradationLevel::SimpleBorders);
        assert_eq!(frame.buffer.degradation, DegradationLevel::SimpleBorders);

        frame.set_degradation(DegradationLevel::EssentialOnly);
        assert_eq!(frame.degradation, DegradationLevel::EssentialOnly);
        assert_eq!(frame.buffer.degradation, DegradationLevel::EssentialOnly);
    }

    // --- Frame hit grid with zero-size screen ---

    #[test]
    #[should_panic(expected = "buffer width must be > 0")]
    fn frame_with_hit_grid_zero_size_panics() {
        let mut pool = GraphemePool::new();
        let _frame = Frame::with_hit_grid(0, 0, &mut pool);
    }

    // --- Frame register_hit returns true/false correctly ---

    #[test]
    fn frame_register_hit_with_all_regions() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(20, 20, &mut pool);
        let regions = [
            HitRegion::Content,
            HitRegion::Border,
            HitRegion::Scrollbar,
            HitRegion::Handle,
            HitRegion::Button,
            HitRegion::Link,
            HitRegion::Custom(0),
            HitRegion::Custom(255),
        ];
        for (i, &region) in regions.iter().enumerate() {
            let y = i as u16;
            frame.register_hit(Rect::new(0, y, 1, 1), HitId::new(i as u32), region, 0);
        }
        for (i, &region) in regions.iter().enumerate() {
            let y = i as u16;
            assert_eq!(
                frame.hit_test(0, y),
                Some((HitId::new(i as u32), region, 0))
            );
        }
    }

    // --- Frame Draw trait ---

    #[test]
    fn frame_draw_horizontal_line() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let cell = Cell::from_char('-');
        frame.draw_horizontal_line(2, 1, 5, cell);
        for x in 2..7 {
            assert_eq!(frame.buffer.get(x, 1).unwrap().content.as_char(), Some('-'));
        }
        // Neighbors untouched
        assert!(frame.buffer.get(1, 1).unwrap().is_empty());
        assert!(frame.buffer.get(7, 1).unwrap().is_empty());
    }

    #[test]
    fn frame_draw_vertical_line() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        let cell = Cell::from_char('|');
        frame.draw_vertical_line(3, 2, 4, cell);
        for y in 2..6 {
            assert_eq!(frame.buffer.get(3, y).unwrap().content.as_char(), Some('|'));
        }
        assert!(frame.buffer.get(3, 1).unwrap().is_empty());
        assert!(frame.buffer.get(3, 6).unwrap().is_empty());
    }

    #[test]
    fn frame_draw_rect_filled() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        let cell = Cell::from_char('#');
        frame.draw_rect_filled(Rect::new(1, 1, 3, 3), cell);
        for y in 1..4 {
            for x in 1..4 {
                assert_eq!(frame.buffer.get(x, y).unwrap().content.as_char(), Some('#'));
            }
        }
        // Outside
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
        assert!(frame.buffer.get(4, 4).unwrap().is_empty());
    }

    #[test]
    fn frame_paint_area() {
        use crate::cell::PackedRgba;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 5, &mut pool);
        let red = PackedRgba::rgb(255, 0, 0);
        frame.paint_area(Rect::new(0, 0, 2, 2), Some(red), None);
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.fg, red);
    }

    // --- Frame print_text_clipped ---

    #[test]
    fn frame_print_text_clipped_at_boundary() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        // "Hello World" should be clipped at width 5
        let end = frame.print_text(0, 0, "Hello World", Cell::from_char(' '));
        assert_eq!(end, 5);
        for x in 0..5 {
            assert!(!frame.buffer.get(x, 0).unwrap().is_empty());
        }
    }

    #[test]
    fn frame_print_text_empty_string() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let end = frame.print_text(0, 0, "", Cell::from_char(' '));
        assert_eq!(end, 0);
    }

    #[test]
    fn frame_print_text_at_right_edge() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        // Start at x=4, only 1 cell fits
        let end = frame.print_text(4, 0, "AB", Cell::from_char(' '));
        assert_eq!(end, 5);
        assert_eq!(frame.buffer.get(4, 0).unwrap().content.as_char(), Some('A'));
    }

    // --- Frame Debug ---

    #[test]
    fn frame_debug() {
        let mut pool = GraphemePool::new();
        let frame = Frame::new(5, 3, &mut pool);
        let dbg = format!("{:?}", frame);
        assert!(dbg.contains("Frame"), "Debug: {dbg}");
    }

    // --- HitGrid Debug ---

    #[test]
    fn hit_grid_debug() {
        let grid = HitGrid::new(3, 3);
        let dbg = format!("{:?}", grid);
        assert!(dbg.contains("HitGrid"), "Debug: {dbg}");
    }

    // --- Frame cursor beyond bounds ---

    #[test]
    fn frame_cursor_beyond_bounds() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        // Setting cursor beyond frame is allowed (no clipping)
        frame.set_cursor(Some((100, 200)));
        assert_eq!(frame.cursor_position, Some((100, 200)));
    }

    // --- HitGrid large data values ---

    #[test]
    fn hit_grid_register_overwrite() {
        let mut grid = HitGrid::new(5, 5);
        grid.register(Rect::new(0, 0, 3, 3), HitId::new(1), HitRegion::Content, 10);
        grid.register(Rect::new(0, 0, 3, 3), HitId::new(2), HitRegion::Button, 20);
        // Second registration overwrites first
        assert_eq!(
            grid.hit_test(1, 1),
            Some((HitId::new(2), HitRegion::Button, 20))
        );
    }
}

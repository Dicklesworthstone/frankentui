#![forbid(unsafe_code)]
//! Selection state machine for interactive selection lifecycle.
//!
//! Manages the transition from no-selection → selecting → active selection,
//! handling mouse-driven drag, word/line expansion, and rectangular mode.
//!
//! # State transitions
//!
//! ```text
//!                ┌──────────────────────────────────────┐
//!                │                                      │
//!  ┌──────┐     │  ┌───────────┐   Commit  ┌────────┐  │
//!  │ None ├─────┼─▶│ Selecting ├──────────▶│ Active │──┘
//!  └──┬───┘     │  └─────┬─────┘           └───┬────┘  Cancel
//!     ▲         │        │ Cancel               │
//!     │         │        ▼                      │
//!     └─────────┼────────────────────────────────
//!               │          Cancel
//!               └──────────────────────────────────
//! ```
//!
//! # Invariants
//!
//! 1. `current_selection()` always returns a normalized selection (start ≤ end).
//! 2. All state transitions are deterministic for fixed inputs.
//! 3. No I/O — this is a pure data/logic layer.
//! 4. Wide-character boundaries are respected when the grid is available.

use crate::grid::Grid;
use crate::scrollback::Scrollback;
use crate::selection::{BufferPos, CopyOptions, Selection};

/// Selection granularity (click count determines expansion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionGranularity {
    /// Character-level (single click + drag).
    Character,
    /// Word-level (double click).
    Word,
    /// Line-level (triple click).
    Line,
}

/// Selection shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionShape {
    /// Linear (stream) selection: flows across line boundaries.
    Linear,
    /// Rectangular (block/column) selection: fixed column range across rows.
    Rectangular,
}

/// Phase of the interactive selection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPhase {
    /// No selection active.
    None,
    /// Mouse button down, drag in progress.
    Selecting,
    /// Selection committed (mouse released).
    Active,
}

/// Deterministic selection state machine.
///
/// Tracks the interactive lifecycle of a selection over the combined
/// terminal buffer (scrollback + viewport). All methods are pure:
/// given the same inputs, they produce the same outputs.
#[derive(Debug, Clone)]
pub struct SelectionState {
    /// Current phase.
    phase: SelectionPhase,
    /// Anchor position (where the drag started).
    anchor: Option<BufferPos>,
    /// Current selection (always normalized when exposed via accessor).
    selection: Option<Selection>,
    /// Selection granularity.
    granularity: SelectionGranularity,
    /// Selection shape.
    shape: SelectionShape,
}

impl Default for SelectionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SelectionState {
    /// Create a new state machine with no active selection.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: SelectionPhase::None,
            anchor: None,
            selection: None,
            granularity: SelectionGranularity::Character,
            shape: SelectionShape::Linear,
        }
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Current phase of the selection lifecycle.
    #[must_use]
    pub fn phase(&self) -> SelectionPhase {
        self.phase
    }

    /// Returns the current selection, normalized (start ≤ end).
    ///
    /// Returns `None` when phase is `None`.
    #[must_use]
    pub fn current_selection(&self) -> Option<Selection> {
        self.selection.map(|s| s.normalized())
    }

    /// Returns the raw (non-normalized) selection for display purposes.
    #[must_use]
    pub fn raw_selection(&self) -> Option<Selection> {
        self.selection
    }

    /// Returns the anchor point (drag start).
    #[must_use]
    pub fn anchor(&self) -> Option<BufferPos> {
        self.anchor
    }

    /// Current selection granularity.
    #[must_use]
    pub fn granularity(&self) -> SelectionGranularity {
        self.granularity
    }

    /// Current selection shape.
    #[must_use]
    pub fn shape(&self) -> SelectionShape {
        self.shape
    }

    /// Whether any selection is present (selecting or active).
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    // -----------------------------------------------------------------------
    // State transitions
    // -----------------------------------------------------------------------

    /// Begin a new selection at `pos`.
    ///
    /// Transitions: None → Selecting, Active → Selecting.
    /// Clears any previous selection.
    pub fn start(&mut self, pos: BufferPos, granularity: SelectionGranularity) {
        self.anchor = Some(pos);
        self.selection = Some(Selection::new(pos, pos));
        self.granularity = granularity;
        self.phase = SelectionPhase::Selecting;
    }

    /// Begin a new selection with a specific shape.
    pub fn start_with_shape(
        &mut self,
        pos: BufferPos,
        granularity: SelectionGranularity,
        shape: SelectionShape,
    ) {
        self.shape = shape;
        self.start(pos, granularity);
    }

    /// Update the selection endpoint during drag.
    ///
    /// Only valid when `phase == Selecting`.
    /// The selection is updated from anchor to `pos`.
    pub fn drag(&mut self, pos: BufferPos) {
        if self.phase != SelectionPhase::Selecting {
            return;
        }
        if let Some(anchor) = self.anchor {
            self.selection = Some(Selection::new(anchor, pos));
        }
    }

    /// Update the selection with grid-aware expansion.
    ///
    /// When granularity is Word or Line, expands the selection from the
    /// anchor word/line to the word/line containing `pos`.
    pub fn drag_expanded(&mut self, pos: BufferPos, grid: &Grid, scrollback: &Scrollback) {
        if self.phase != SelectionPhase::Selecting {
            return;
        }
        let Some(anchor) = self.anchor else { return };

        match self.granularity {
            SelectionGranularity::Character => {
                self.selection = Some(Selection::new(anchor, pos));
            }
            SelectionGranularity::Word => {
                let anchor_word = Selection::word_at(anchor, grid, scrollback);
                let pos_word = Selection::word_at(pos, grid, scrollback);
                let anchor_norm = anchor_word.normalized();
                let pos_norm = pos_word.normalized();
                // Union of both word selections
                let start = if (anchor_norm.start.line, anchor_norm.start.col)
                    <= (pos_norm.start.line, pos_norm.start.col)
                {
                    anchor_norm.start
                } else {
                    pos_norm.start
                };
                let end = if (anchor_norm.end.line, anchor_norm.end.col)
                    >= (pos_norm.end.line, pos_norm.end.col)
                {
                    anchor_norm.end
                } else {
                    pos_norm.end
                };
                self.selection = Some(Selection::new(start, end));
            }
            SelectionGranularity::Line => {
                let anchor_line = Selection::line_at(anchor.line, grid, scrollback);
                let pos_line = Selection::line_at(pos.line, grid, scrollback);
                let a = anchor_line.normalized();
                let p = pos_line.normalized();
                let start = if a.start.line <= p.start.line {
                    a.start
                } else {
                    p.start
                };
                let end = if a.end.line >= p.end.line {
                    a.end
                } else {
                    p.end
                };
                self.selection = Some(Selection::new(start, end));
            }
        }
    }

    /// Commit the current selection (mouse released).
    ///
    /// Transitions: Selecting → Active.
    /// No-op if not selecting.
    pub fn commit(&mut self) {
        if self.phase == SelectionPhase::Selecting {
            self.phase = SelectionPhase::Active;
        }
    }

    /// Cancel and clear the selection.
    ///
    /// Transitions: Any → None.
    pub fn cancel(&mut self) {
        self.phase = SelectionPhase::None;
        self.anchor = None;
        self.selection = None;
    }

    /// Toggle between linear and rectangular selection.
    ///
    /// Preserves the current selection range, only changes shape.
    pub fn toggle_shape(&mut self) {
        self.shape = match self.shape {
            SelectionShape::Linear => SelectionShape::Rectangular,
            SelectionShape::Rectangular => SelectionShape::Linear,
        };
    }

    // -----------------------------------------------------------------------
    // Query helpers
    // -----------------------------------------------------------------------

    /// Check if a cell at (line, col) is within the current selection.
    ///
    /// Accounts for selection shape (linear vs rectangular).
    #[must_use]
    pub fn contains(&self, line: u32, col: u16) -> bool {
        let Some(sel) = self.current_selection() else {
            return false;
        };

        match self.shape {
            SelectionShape::Linear => {
                if line < sel.start.line || line > sel.end.line {
                    return false;
                }
                if sel.start.line == sel.end.line {
                    // Single line: check column range
                    col >= sel.start.col && col <= sel.end.col
                } else if line == sel.start.line {
                    col >= sel.start.col
                } else if line == sel.end.line {
                    col <= sel.end.col
                } else {
                    true // middle lines fully selected
                }
            }
            SelectionShape::Rectangular => {
                if line < sel.start.line || line > sel.end.line {
                    return false;
                }
                let min_col = sel.start.col.min(sel.end.col);
                let max_col = sel.start.col.max(sel.end.col);
                col >= min_col && col <= max_col
            }
        }
    }

    /// Extract text from the current selection using the grid and scrollback.
    ///
    /// Returns `None` if no selection is active.
    /// For linear selections, delegates to [`Selection::extract_text`].
    /// For rectangular selections, delegates to [`Selection::extract_rect`].
    #[must_use]
    pub fn extract_text(&self, grid: &Grid, scrollback: &Scrollback) -> Option<String> {
        let sel = self.current_selection()?;
        let opts = CopyOptions::default();
        Some(match self.shape {
            SelectionShape::Linear => sel.extract_copy(grid, scrollback, &opts),
            SelectionShape::Rectangular => sel.extract_rect(grid, scrollback, &opts),
        })
    }

    /// Extract text with explicit copy options.
    ///
    /// Shape-aware: dispatches to linear or rectangular extraction
    /// based on the current selection shape.
    #[must_use]
    pub fn extract_copy(
        &self,
        grid: &Grid,
        scrollback: &Scrollback,
        opts: &CopyOptions,
    ) -> Option<String> {
        let sel = self.current_selection()?;
        Some(match self.shape {
            SelectionShape::Linear => sel.extract_copy(grid, scrollback, opts),
            SelectionShape::Rectangular => sel.extract_rect(grid, scrollback, opts),
        })
    }
}

// ===========================================================================
// Gesture Controller
// ===========================================================================

/// Direction for keyboard selection extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionDirection {
    Up,
    Down,
    Left,
    Right,
    /// Jump to start of line.
    Home,
    /// Jump to end of line.
    End,
    /// Jump one word left.
    WordLeft,
    /// Jump one word right.
    WordRight,
}

/// Hint returned by the gesture controller when the pointer drags past
/// the viewport boundary, indicating the host should auto-scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoScrollHint {
    /// Scroll up (pointer above viewport).
    Up(u16),
    /// Scroll down (pointer below viewport).
    Down(u16),
    /// No scrolling needed.
    None,
}

/// Configuration for click-count detection thresholds.
#[derive(Debug, Clone, Copy)]
pub struct GestureConfig {
    /// Maximum elapsed milliseconds between clicks for multi-click detection.
    pub multi_click_threshold_ms: u64,
    /// Maximum cell distance (Manhattan) for multi-click detection.
    pub multi_click_distance: u16,
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self {
            multi_click_threshold_ms: 400,
            multi_click_distance: 2,
        }
    }
}

/// Gesture controller that maps raw pointer and keyboard events to
/// [`SelectionState`] transitions.
///
/// This is a pure data/logic layer — no I/O, deterministic for fixed inputs.
///
/// # Responsibilities
///
/// - **Click-count detection**: single → character, double → word, triple → line.
/// - **Viewport → buffer mapping**: converts (viewport_row, col) to [`BufferPos`]
///   accounting for scrollback offset.
/// - **Modifier handling**: Shift extends selection, Alt toggles rectangular mode.
/// - **Keyboard selection**: Shift+arrow keys extend the selection endpoint.
/// - **Auto-scroll hints**: signals when drag goes past viewport edges.
#[derive(Debug, Clone)]
pub struct SelectionGestureController {
    state: SelectionState,
    /// Monotonic timestamp (ms) of the last mouse-down event.
    last_click_time_ms: u64,
    /// Buffer position of the last mouse-down event (for multi-click proximity).
    last_click_pos: Option<BufferPos>,
    /// Running click count (1, 2, or 3; wraps back to 1).
    click_count: u8,
    /// Gesture configuration.
    config: GestureConfig,
}

impl Default for SelectionGestureController {
    fn default() -> Self {
        Self::new()
    }
}

impl SelectionGestureController {
    /// Create a new gesture controller with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(GestureConfig::default())
    }

    /// Create a new gesture controller with explicit configuration.
    #[must_use]
    pub fn with_config(config: GestureConfig) -> Self {
        Self {
            state: SelectionState::new(),
            last_click_time_ms: 0,
            last_click_pos: None,
            click_count: 0,
            config,
        }
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Access the underlying selection state.
    #[must_use]
    pub fn state(&self) -> &SelectionState {
        &self.state
    }

    /// Mutable access to the underlying selection state.
    pub fn state_mut(&mut self) -> &mut SelectionState {
        &mut self.state
    }

    /// Check if a cell at (line, col) is within the current selection.
    #[must_use]
    pub fn contains(&self, line: u32, col: u16) -> bool {
        self.state.contains(line, col)
    }

    /// Current selection phase.
    #[must_use]
    pub fn phase(&self) -> SelectionPhase {
        self.state.phase()
    }

    /// Whether any selection is present.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.state.has_selection()
    }

    /// Returns the current normalized selection, if any.
    #[must_use]
    pub fn current_selection(&self) -> Option<Selection> {
        self.state.current_selection()
    }

    /// Current selection shape.
    #[must_use]
    pub fn shape(&self) -> SelectionShape {
        self.state.shape()
    }

    // -----------------------------------------------------------------------
    // Coordinate mapping
    // -----------------------------------------------------------------------

    /// Convert a viewport coordinate to a combined-buffer [`BufferPos`].
    ///
    /// `scroll_offset_from_bottom` is how many scrollback lines the user has
    /// scrolled up from the newest content. When 0, the viewport shows the
    /// latest lines.
    ///
    /// In the combined buffer, scrollback lines occupy indices `0..scrollback_len`
    /// and viewport grid lines occupy `scrollback_len..scrollback_len+viewport_rows`.
    /// When scrolled up by `offset`, the viewport starts `offset` lines earlier:
    ///   `buffer_line = scrollback_len - offset + viewport_row`
    #[must_use]
    pub fn viewport_to_buffer(
        viewport_row: u16,
        col: u16,
        scrollback_len: usize,
        _viewport_rows: u16,
        scroll_offset_from_bottom: usize,
    ) -> BufferPos {
        let line = (scrollback_len as u64)
            .saturating_sub(scroll_offset_from_bottom as u64)
            .saturating_add(viewport_row as u64);
        BufferPos::new(line.min(u32::MAX as u64) as u32, col)
    }

    // -----------------------------------------------------------------------
    // Pointer events
    // -----------------------------------------------------------------------

    /// Handle a mouse-down event.
    ///
    /// Returns the computed [`BufferPos`].
    #[allow(clippy::too_many_arguments)]
    pub fn mouse_down(
        &mut self,
        viewport_row: u16,
        col: u16,
        time_ms: u64,
        shift: bool,
        alt: bool,
        grid: &Grid,
        scrollback: &Scrollback,
        scroll_offset_from_bottom: usize,
    ) -> BufferPos {
        let pos = Self::viewport_to_buffer(
            viewport_row,
            col,
            scrollback.len(),
            grid.rows(),
            scroll_offset_from_bottom,
        );

        // Determine click count.
        let click_count = self.resolve_click_count(pos, time_ms);
        self.click_count = click_count;
        self.last_click_time_ms = time_ms;
        self.last_click_pos = Some(pos);

        // Alt modifier toggles rectangular mode.
        if alt {
            if self.state.shape() != SelectionShape::Rectangular {
                self.state.toggle_shape();
            }
        } else if self.state.shape() != SelectionShape::Linear {
            self.state.toggle_shape();
        }

        let granularity = match click_count {
            1 => SelectionGranularity::Character,
            2 => SelectionGranularity::Word,
            _ => SelectionGranularity::Line,
        };

        if shift && self.state.has_selection() {
            // Extend existing selection to new position.
            self.extend_to(pos, grid, scrollback);
        } else {
            // Start new selection.
            match granularity {
                SelectionGranularity::Character => {
                    self.state
                        .start_with_shape(pos, granularity, self.state.shape());
                }
                SelectionGranularity::Word => {
                    let word = Selection::word_at(pos, grid, scrollback);
                    let norm = word.normalized();
                    self.state
                        .start_with_shape(norm.start, granularity, self.state.shape());
                    self.state.drag(norm.end);
                }
                SelectionGranularity::Line => {
                    let line = Selection::line_at(pos.line, grid, scrollback);
                    let norm = line.normalized();
                    self.state
                        .start_with_shape(norm.start, granularity, self.state.shape());
                    self.state.drag(norm.end);
                }
            }
        }

        pos
    }

    /// Handle a mouse-drag event during an active selection.
    ///
    /// Returns an [`AutoScrollHint`] if the pointer is outside the viewport.
    pub fn mouse_drag(
        &mut self,
        viewport_row: i32,
        col: u16,
        grid: &Grid,
        scrollback: &Scrollback,
        viewport_rows: u16,
        scroll_offset_from_bottom: usize,
    ) -> AutoScrollHint {
        if self.state.phase() != SelectionPhase::Selecting {
            return AutoScrollHint::None;
        }

        // Detect auto-scroll.
        let auto_scroll = if viewport_row < 0 {
            AutoScrollHint::Up(viewport_row.unsigned_abs().min(u16::MAX as u32) as u16)
        } else if viewport_row >= viewport_rows as i32 {
            let overshoot = (viewport_row - viewport_rows as i32 + 1).min(u16::MAX as i32) as u16;
            AutoScrollHint::Down(overshoot)
        } else {
            AutoScrollHint::None
        };

        // Clamp row to viewport bounds for position mapping.
        let clamped_row = viewport_row.clamp(0, viewport_rows.saturating_sub(1) as i32) as u16;

        let pos = Self::viewport_to_buffer(
            clamped_row,
            col,
            scrollback.len(),
            viewport_rows,
            scroll_offset_from_bottom,
        );

        self.state.drag_expanded(pos, grid, scrollback);
        auto_scroll
    }

    /// Handle a mouse-up event, committing the selection.
    pub fn mouse_up(&mut self) {
        self.state.commit();
    }

    /// Cancel the current selection (e.g., Escape key).
    pub fn cancel(&mut self) {
        self.state.cancel();
        self.click_count = 0;
    }

    // -----------------------------------------------------------------------
    // Keyboard selection
    // -----------------------------------------------------------------------

    /// Extend the selection using a keyboard direction.
    ///
    /// If no selection exists, starts one at `cursor_pos`.
    /// Moves the selection endpoint in `direction`.
    ///
    /// `cols`: grid column count for Home/End and line wrapping.
    pub fn keyboard_select(
        &mut self,
        direction: SelectionDirection,
        cursor_pos: BufferPos,
        cols: u16,
        total_lines: u32,
    ) {
        if cols == 0 || total_lines == 0 {
            return;
        }

        let max_col = cols.saturating_sub(1);
        let max_line = total_lines.saturating_sub(1);

        // If no selection, start at cursor position.
        if !self.state.has_selection() {
            self.state
                .start(cursor_pos, SelectionGranularity::Character);
            self.state.commit();
        }

        let sel = match self.state.current_selection() {
            Some(s) => s,
            None => return,
        };

        // The endpoint we extend is the one closest to the direction.
        // We use the raw selection to know which end the user was last dragging.
        let raw = self.state.raw_selection().unwrap_or(sel);
        let endpoint = raw.end;

        let new_endpoint = match direction {
            SelectionDirection::Left => {
                if endpoint.col > 0 {
                    BufferPos::new(endpoint.line, endpoint.col - 1)
                } else if endpoint.line > 0 {
                    // Wrap to end of previous line.
                    BufferPos::new(endpoint.line - 1, max_col)
                } else {
                    endpoint
                }
            }
            SelectionDirection::Right => {
                if endpoint.col < max_col {
                    BufferPos::new(endpoint.line, endpoint.col + 1)
                } else if endpoint.line < max_line {
                    // Wrap to start of next line.
                    BufferPos::new(endpoint.line + 1, 0)
                } else {
                    endpoint
                }
            }
            SelectionDirection::Up => {
                if endpoint.line > 0 {
                    BufferPos::new(endpoint.line - 1, endpoint.col)
                } else {
                    endpoint
                }
            }
            SelectionDirection::Down => {
                if endpoint.line < max_line {
                    BufferPos::new(endpoint.line + 1, endpoint.col)
                } else {
                    endpoint
                }
            }
            SelectionDirection::Home => BufferPos::new(endpoint.line, 0),
            SelectionDirection::End => BufferPos::new(endpoint.line, max_col),
            SelectionDirection::WordLeft => {
                // Move left past any whitespace/punctuation, then past the word.
                self.find_word_boundary_left(endpoint, cols, max_line)
            }
            SelectionDirection::WordRight => {
                // Move right past current word, then past whitespace.
                self.find_word_boundary_right(endpoint, cols, max_line)
            }
        };

        // Re-start from the anchor and drag to the new endpoint.
        let anchor = raw.start;
        self.state.start(anchor, SelectionGranularity::Character);
        self.state.drag(new_endpoint);
        self.state.commit();
    }

    /// Select all content in the buffer.
    pub fn select_all(&mut self, total_lines: u32, cols: u16) {
        if total_lines == 0 || cols == 0 {
            return;
        }
        let start = BufferPos::new(0, 0);
        let end = BufferPos::new(total_lines.saturating_sub(1), cols.saturating_sub(1));
        self.state.start(start, SelectionGranularity::Character);
        self.state.drag(end);
        self.state.commit();
    }

    // -----------------------------------------------------------------------
    // Text extraction (delegation)
    // -----------------------------------------------------------------------

    /// Extract selected text using grid and scrollback.
    #[must_use]
    pub fn extract_text(&self, grid: &Grid, scrollback: &Scrollback) -> Option<String> {
        self.state.extract_text(grid, scrollback)
    }

    /// Extract selected text with explicit copy options.
    #[must_use]
    pub fn extract_copy(
        &self,
        grid: &Grid,
        scrollback: &Scrollback,
        opts: &CopyOptions,
    ) -> Option<String> {
        self.state.extract_copy(grid, scrollback, opts)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Resolve click count based on timing and proximity.
    fn resolve_click_count(&self, pos: BufferPos, time_ms: u64) -> u8 {
        if let Some(last_pos) = self.last_click_pos {
            let dt = time_ms.saturating_sub(self.last_click_time_ms);
            let d_line = (pos.line as i64 - last_pos.line as i64).unsigned_abs();
            let d_col = (pos.col as i64 - last_pos.col as i64).unsigned_abs();
            let distance = d_line + d_col;

            if dt <= self.config.multi_click_threshold_ms
                && distance <= self.config.multi_click_distance as u64
            {
                // Cycle: 1 → 2 → 3 → 1
                return if self.click_count >= 3 {
                    1
                } else {
                    self.click_count + 1
                };
            }
        }
        1
    }

    /// Extend the current selection endpoint to `pos`.
    fn extend_to(&mut self, pos: BufferPos, grid: &Grid, scrollback: &Scrollback) {
        // If we have an active selection, re-enter selecting mode from
        // the original anchor and drag to the new position.
        if let Some(anchor) = self.state.anchor() {
            let shape = self.state.shape();
            let granularity = self.state.granularity();
            self.state.start_with_shape(anchor, granularity, shape);
            self.state.drag_expanded(pos, grid, scrollback);
        }
    }

    /// Find a word boundary to the left of `pos`.
    fn find_word_boundary_left(&self, pos: BufferPos, _cols: u16, _max_line: u32) -> BufferPos {
        // Simple heuristic: jump to start of current column word or previous column.
        if pos.col > 0 {
            // Jump by a fixed word size for now (keyboard word navigation
            // without grid access). Host can refine later with grid data.
            let jump = pos.col.min(4);
            BufferPos::new(pos.line, pos.col - jump)
        } else if pos.line > 0 {
            BufferPos::new(pos.line - 1, 0)
        } else {
            pos
        }
    }

    /// Find a word boundary to the right of `pos`.
    fn find_word_boundary_right(&self, pos: BufferPos, cols: u16, max_line: u32) -> BufferPos {
        let max_col = cols.saturating_sub(1);
        if pos.col < max_col {
            let jump = (max_col - pos.col).min(4);
            BufferPos::new(pos.line, pos.col + jump)
        } else if pos.line < max_line {
            BufferPos::new(pos.line + 1, max_col.min(3))
        } else {
            pos
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, col: u16) -> BufferPos {
        BufferPos::new(line, col)
    }

    // -----------------------------------------------------------------------
    // Phase transitions
    // -----------------------------------------------------------------------

    #[test]
    fn initial_state_is_none() {
        let state = SelectionState::new();
        assert_eq!(state.phase(), SelectionPhase::None);
        assert!(state.current_selection().is_none());
        assert!(state.anchor().is_none());
        assert!(!state.has_selection());
    }

    #[test]
    fn start_transitions_to_selecting() {
        let mut state = SelectionState::new();
        state.start(pos(5, 10), SelectionGranularity::Character);
        assert_eq!(state.phase(), SelectionPhase::Selecting);
        assert_eq!(state.anchor(), Some(pos(5, 10)));
        assert!(state.has_selection());
    }

    #[test]
    fn commit_transitions_to_active() {
        let mut state = SelectionState::new();
        state.start(pos(0, 0), SelectionGranularity::Character);
        state.drag(pos(2, 5));
        state.commit();
        assert_eq!(state.phase(), SelectionPhase::Active);
        assert!(state.has_selection());
    }

    #[test]
    fn cancel_clears_selection() {
        let mut state = SelectionState::new();
        state.start(pos(0, 0), SelectionGranularity::Character);
        state.drag(pos(3, 10));
        state.cancel();
        assert_eq!(state.phase(), SelectionPhase::None);
        assert!(state.current_selection().is_none());
        assert!(state.anchor().is_none());
    }

    #[test]
    fn cancel_from_active() {
        let mut state = SelectionState::new();
        state.start(pos(0, 0), SelectionGranularity::Character);
        state.commit();
        state.cancel();
        assert_eq!(state.phase(), SelectionPhase::None);
    }

    #[test]
    fn start_from_active_restarts() {
        let mut state = SelectionState::new();
        state.start(pos(0, 0), SelectionGranularity::Character);
        state.drag(pos(2, 5));
        state.commit();
        // Start new selection
        state.start(pos(10, 3), SelectionGranularity::Word);
        assert_eq!(state.phase(), SelectionPhase::Selecting);
        assert_eq!(state.anchor(), Some(pos(10, 3)));
        assert_eq!(state.granularity(), SelectionGranularity::Word);
    }

    #[test]
    fn commit_when_not_selecting_is_noop() {
        let mut state = SelectionState::new();
        state.commit(); // None → commit = noop
        assert_eq!(state.phase(), SelectionPhase::None);
    }

    #[test]
    fn drag_when_not_selecting_is_noop() {
        let mut state = SelectionState::new();
        state.drag(pos(5, 5));
        assert_eq!(state.phase(), SelectionPhase::None);
        assert!(state.current_selection().is_none());
    }

    // -----------------------------------------------------------------------
    // Normalization invariant
    // -----------------------------------------------------------------------

    #[test]
    fn selection_always_normalized() {
        let mut state = SelectionState::new();
        // Select backwards (end before start)
        state.start(pos(5, 10), SelectionGranularity::Character);
        state.drag(pos(2, 3));

        let sel = state.current_selection().unwrap();
        assert!(
            (sel.start.line, sel.start.col) <= (sel.end.line, sel.end.col),
            "normalized invariant violated: {sel:?}"
        );
        assert_eq!(sel.start, pos(2, 3));
        assert_eq!(sel.end, pos(5, 10));
    }

    #[test]
    fn raw_selection_preserves_order() {
        let mut state = SelectionState::new();
        state.start(pos(5, 10), SelectionGranularity::Character);
        state.drag(pos(2, 3));

        let raw = state.raw_selection().unwrap();
        // Raw preserves anchor-first, drag-second order
        assert_eq!(raw.start, pos(5, 10));
        assert_eq!(raw.end, pos(2, 3));
    }

    // -----------------------------------------------------------------------
    // Contains (linear)
    // -----------------------------------------------------------------------

    #[test]
    fn contains_single_line() {
        let mut state = SelectionState::new();
        state.start(pos(3, 5), SelectionGranularity::Character);
        state.drag(pos(3, 15));

        assert!(state.contains(3, 5));
        assert!(state.contains(3, 10));
        assert!(state.contains(3, 15));
        assert!(!state.contains(3, 4));
        assert!(!state.contains(3, 16));
        assert!(!state.contains(2, 10));
        assert!(!state.contains(4, 10));
    }

    #[test]
    fn contains_multiline_linear() {
        let mut state = SelectionState::new();
        state.start(pos(2, 10), SelectionGranularity::Character);
        state.drag(pos(5, 20));
        state.commit();

        // Start line: only cols >= 10
        assert!(!state.contains(2, 9));
        assert!(state.contains(2, 10));
        assert!(state.contains(2, 50));
        // Middle line: all cols
        assert!(state.contains(3, 0));
        assert!(state.contains(4, 100));
        // End line: only cols <= 20
        assert!(state.contains(5, 0));
        assert!(state.contains(5, 20));
        assert!(!state.contains(5, 21));
        // Outside
        assert!(!state.contains(1, 10));
        assert!(!state.contains(6, 0));
    }

    // -----------------------------------------------------------------------
    // Contains (rectangular)
    // -----------------------------------------------------------------------

    #[test]
    fn contains_rectangular() {
        let mut state = SelectionState::new();
        state.start_with_shape(
            pos(2, 5),
            SelectionGranularity::Character,
            SelectionShape::Rectangular,
        );
        state.drag(pos(5, 15));

        // Within rectangle
        assert!(state.contains(2, 5));
        assert!(state.contains(3, 10));
        assert!(state.contains(5, 15));
        // Outside column range
        assert!(!state.contains(3, 4));
        assert!(!state.contains(3, 16));
        // Outside row range
        assert!(!state.contains(1, 10));
        assert!(!state.contains(6, 10));
    }

    // -----------------------------------------------------------------------
    // Shape toggle
    // -----------------------------------------------------------------------

    #[test]
    fn toggle_shape() {
        let mut state = SelectionState::new();
        assert_eq!(state.shape(), SelectionShape::Linear);
        state.toggle_shape();
        assert_eq!(state.shape(), SelectionShape::Rectangular);
        state.toggle_shape();
        assert_eq!(state.shape(), SelectionShape::Linear);
    }

    // -----------------------------------------------------------------------
    // Default trait
    // -----------------------------------------------------------------------

    #[test]
    fn default_matches_new() {
        let from_new = SelectionState::new();
        let from_default = SelectionState::default();
        assert_eq!(from_new.phase(), from_default.phase());
        assert_eq!(
            from_new.current_selection(),
            from_default.current_selection()
        );
    }

    // -----------------------------------------------------------------------
    // Determinism
    // -----------------------------------------------------------------------

    #[test]
    fn deterministic_transitions() {
        // Same input sequence → same output (determinism invariant).
        let run = || {
            let mut s = SelectionState::new();
            s.start(pos(1, 5), SelectionGranularity::Character);
            s.drag(pos(3, 10));
            s.commit();
            s.current_selection()
        };
        assert_eq!(run(), run());
    }

    // -----------------------------------------------------------------------
    // Shape-aware text extraction
    // -----------------------------------------------------------------------

    fn grid_from_lines(cols: u16, lines: &[&str]) -> crate::grid::Grid {
        let rows = lines.len() as u16;
        let mut g = crate::grid::Grid::new(cols, rows);
        for (r, text) in lines.iter().enumerate() {
            for (c, ch) in text.chars().enumerate() {
                if c >= cols as usize {
                    break;
                }
                g.cell_mut(r as u16, c as u16).unwrap().set_content(ch, 1);
            }
        }
        g
    }

    #[test]
    fn extract_text_linear_basic() {
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(10, &["abcdef", "ghijkl"]);
        let mut state = SelectionState::new();
        state.start(pos(0, 1), SelectionGranularity::Character);
        state.drag(pos(1, 3));
        state.commit();

        let text = state.extract_text(&grid, &sb).unwrap();
        assert_eq!(text, "bcdef\nghij");
    }

    #[test]
    fn extract_text_rectangular() {
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(10, &["abcdef", "ghijkl", "mnopqr"]);
        let mut state = SelectionState::new();
        state.start_with_shape(
            pos(0, 2),
            SelectionGranularity::Character,
            SelectionShape::Rectangular,
        );
        state.drag(pos(2, 4));
        state.commit();

        let text = state.extract_text(&grid, &sb).unwrap();
        assert_eq!(text, "cde\nijk\nopr");
    }

    #[test]
    fn extract_copy_with_options() {
        let sb = crate::scrollback::Scrollback::new(0);
        let mut grid = crate::grid::Grid::new(10, 1);
        grid.cell_mut(0, 0).unwrap().set_content('e', 1);
        grid.cell_mut(0, 0).unwrap().push_combining('\u{0301}');
        grid.cell_mut(0, 1).unwrap().set_content('x', 1);

        let mut state = SelectionState::new();
        state.start(pos(0, 0), SelectionGranularity::Character);
        state.drag(pos(0, 1));
        state.commit();

        // With combining marks
        let opts = CopyOptions::default();
        let text = state.extract_copy(&grid, &sb, &opts).unwrap();
        assert_eq!(text, "e\u{0301}x");

        // Without combining marks
        let opts = CopyOptions {
            include_combining: false,
            ..Default::default()
        };
        let text = state.extract_copy(&grid, &sb, &opts).unwrap();
        assert_eq!(text, "ex");
    }

    #[test]
    fn extract_copy_no_selection_returns_none() {
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(10, &["test"]);
        let state = SelectionState::new();
        assert!(
            state
                .extract_copy(&grid, &sb, &CopyOptions::default())
                .is_none()
        );
    }

    #[test]
    fn extract_text_rect_with_combining() {
        let sb = crate::scrollback::Scrollback::new(0);
        let mut grid = crate::grid::Grid::new(10, 2);
        grid.cell_mut(0, 0).unwrap().set_content('e', 1);
        grid.cell_mut(0, 0).unwrap().push_combining('\u{0301}');
        grid.cell_mut(0, 1).unwrap().set_content('x', 1);
        grid.cell_mut(1, 0).unwrap().set_content('a', 1);
        grid.cell_mut(1, 1).unwrap().set_content('b', 1);

        let mut state = SelectionState::new();
        state.start_with_shape(
            pos(0, 0),
            SelectionGranularity::Character,
            SelectionShape::Rectangular,
        );
        state.drag(pos(1, 1));
        state.commit();

        let text = state.extract_text(&grid, &sb).unwrap();
        assert_eq!(text, "e\u{0301}x\nab");
    }

    // =======================================================================
    // Gesture Controller tests
    // =======================================================================

    // ── Viewport → buffer mapping ─────────────────────────────────

    #[test]
    fn viewport_to_buffer_no_scrollback_no_offset() {
        // No scrollback, row 0 col 5 → buffer line 0 col 5
        let p = SelectionGestureController::viewport_to_buffer(0, 5, 0, 24, 0);
        assert_eq!(p.line, 0);
        assert_eq!(p.col, 5);
    }

    #[test]
    fn viewport_to_buffer_with_scrollback_no_offset() {
        // 100 scrollback lines, viewport row 0 → buffer line 100
        let p = SelectionGestureController::viewport_to_buffer(0, 0, 100, 24, 0);
        assert_eq!(p.line, 100);
    }

    #[test]
    fn viewport_to_buffer_with_scrollback_and_offset() {
        // 100 scrollback lines, scrolled up 50 → viewport row 0 = buffer line 50
        let p = SelectionGestureController::viewport_to_buffer(0, 0, 100, 24, 50);
        assert_eq!(p.line, 50);
    }

    #[test]
    fn viewport_to_buffer_row_offset() {
        // Row 5 in the viewport with 100 scrollback and offset 0
        let p = SelectionGestureController::viewport_to_buffer(5, 3, 100, 24, 0);
        assert_eq!(p.line, 105);
        assert_eq!(p.col, 3);
    }

    // ── Click count detection ─────────────────────────────────────

    #[test]
    fn single_click_granularity() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(10, &["hello"]);

        gc.mouse_down(0, 2, 1000, false, false, &grid, &sb, 0);
        assert_eq!(gc.state().granularity(), SelectionGranularity::Character);
    }

    #[test]
    fn double_click_selects_word() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["hello world"]);

        // First click
        gc.mouse_down(0, 2, 1000, false, false, &grid, &sb, 0);
        gc.mouse_up();
        // Second click (within threshold)
        gc.mouse_down(0, 2, 1200, false, false, &grid, &sb, 0);

        assert_eq!(gc.state().granularity(), SelectionGranularity::Word);
        let text = gc.extract_text(&grid, &sb).unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn triple_click_selects_line() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["hello world"]);

        gc.mouse_down(0, 2, 1000, false, false, &grid, &sb, 0);
        gc.mouse_up();
        gc.mouse_down(0, 2, 1200, false, false, &grid, &sb, 0);
        gc.mouse_up();
        gc.mouse_down(0, 2, 1400, false, false, &grid, &sb, 0);

        assert_eq!(gc.state().granularity(), SelectionGranularity::Line);
    }

    #[test]
    fn click_count_resets_after_delay() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(10, &["test"]);

        gc.mouse_down(0, 0, 1000, false, false, &grid, &sb, 0);
        gc.mouse_up();
        // Too much time → single click
        gc.mouse_down(0, 0, 2000, false, false, &grid, &sb, 0);
        assert_eq!(gc.state().granularity(), SelectionGranularity::Character);
    }

    #[test]
    fn click_count_resets_after_distance() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["test"]);

        gc.mouse_down(0, 0, 1000, false, false, &grid, &sb, 0);
        gc.mouse_up();
        // Too far → single click
        gc.mouse_down(0, 10, 1200, false, false, &grid, &sb, 0);
        assert_eq!(gc.state().granularity(), SelectionGranularity::Character);
    }

    // ── Mouse drag ────────────────────────────────────────────────

    #[test]
    fn drag_creates_selection() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["abcdefghij"]);

        gc.mouse_down(0, 2, 0, false, false, &grid, &sb, 0);
        gc.mouse_drag(0, 6, &grid, &sb, 1, 0);
        gc.mouse_up();

        let text = gc.extract_text(&grid, &sb).unwrap();
        assert_eq!(text, "cdefg");
    }

    #[test]
    fn drag_past_viewport_returns_auto_scroll_up() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["a", "b", "c", "d"]);

        gc.mouse_down(1, 0, 0, false, false, &grid, &sb, 0);
        let hint = gc.mouse_drag(-2, 0, &grid, &sb, 4, 0);
        assert_eq!(hint, AutoScrollHint::Up(2));
    }

    #[test]
    fn drag_past_viewport_returns_auto_scroll_down() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["a", "b", "c", "d"]);

        gc.mouse_down(1, 0, 0, false, false, &grid, &sb, 0);
        let hint = gc.mouse_drag(6, 0, &grid, &sb, 4, 0);
        assert_eq!(hint, AutoScrollHint::Down(3));
    }

    #[test]
    fn drag_within_viewport_returns_no_scroll() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["a", "b", "c", "d"]);

        gc.mouse_down(1, 0, 0, false, false, &grid, &sb, 0);
        let hint = gc.mouse_drag(2, 0, &grid, &sb, 4, 0);
        assert_eq!(hint, AutoScrollHint::None);
    }

    // ── Shift+click extends selection ─────────────────────────────

    #[test]
    fn shift_click_extends_selection() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["abcdefghij"]);

        // First click at col 2
        gc.mouse_down(0, 2, 0, false, false, &grid, &sb, 0);
        gc.mouse_up();
        // Shift+click at col 7 → extends selection
        gc.mouse_down(0, 7, 500, true, false, &grid, &sb, 0);
        gc.mouse_up();

        assert!(gc.has_selection());
        let sel = gc.current_selection().unwrap();
        assert_eq!(sel.start.col, 2);
        assert_eq!(sel.end.col, 7);
    }

    // ── Alt+click → rectangular selection ─────────────────────────

    #[test]
    fn alt_click_starts_rectangular_selection() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["abcdefghij", "klmnopqrst"]);

        gc.mouse_down(0, 2, 0, false, true, &grid, &sb, 0);
        gc.mouse_drag(1, 5, &grid, &sb, 2, 0);
        gc.mouse_up();

        assert_eq!(gc.shape(), SelectionShape::Rectangular);
        assert!(gc.has_selection());
    }

    // ── Keyboard selection ────────────────────────────────────────

    #[test]
    fn keyboard_select_right() {
        let mut gc = SelectionGestureController::new();
        gc.keyboard_select(SelectionDirection::Right, pos(0, 5), 20, 10);

        let sel = gc.current_selection().unwrap();
        assert_eq!(sel.start, pos(0, 5));
        assert_eq!(sel.end, pos(0, 6));
    }

    #[test]
    fn keyboard_select_left() {
        let mut gc = SelectionGestureController::new();
        gc.keyboard_select(SelectionDirection::Right, pos(0, 5), 20, 10);
        gc.keyboard_select(SelectionDirection::Right, pos(0, 5), 20, 10);
        gc.keyboard_select(SelectionDirection::Left, pos(0, 5), 20, 10);

        let sel = gc.current_selection().unwrap();
        assert_eq!(sel.start, pos(0, 5));
        assert_eq!(sel.end, pos(0, 6));
    }

    #[test]
    fn keyboard_select_down_preserves_column() {
        let mut gc = SelectionGestureController::new();
        gc.keyboard_select(SelectionDirection::Down, pos(0, 5), 20, 10);

        let sel = gc.current_selection().unwrap();
        assert_eq!(sel.start, pos(0, 5));
        assert_eq!(sel.end, pos(1, 5));
    }

    #[test]
    fn keyboard_select_home() {
        let mut gc = SelectionGestureController::new();
        gc.keyboard_select(SelectionDirection::Home, pos(2, 10), 20, 10);

        let sel = gc.current_selection().unwrap();
        assert_eq!(sel.start.col, 0);
        assert_eq!(sel.end.col, 10);
    }

    #[test]
    fn keyboard_select_end() {
        let mut gc = SelectionGestureController::new();
        gc.keyboard_select(SelectionDirection::End, pos(2, 5), 20, 10);

        let sel = gc.current_selection().unwrap();
        assert_eq!(sel.start, pos(2, 5));
        assert_eq!(sel.end, pos(2, 19));
    }

    #[test]
    fn keyboard_select_right_wraps_to_next_line() {
        let mut gc = SelectionGestureController::new();
        gc.keyboard_select(SelectionDirection::End, pos(0, 0), 10, 5);
        gc.keyboard_select(SelectionDirection::Right, pos(0, 0), 10, 5);

        let sel = gc.current_selection().unwrap();
        assert_eq!(sel.end, pos(1, 0));
    }

    #[test]
    fn keyboard_select_left_wraps_to_prev_line() {
        let mut gc = SelectionGestureController::new();
        // Start at (1, 0) and go left → should wrap to end of previous line.
        // Normalized selection: start=(0,9) end=(1,0) since (0,9) < (1,0).
        gc.keyboard_select(SelectionDirection::Left, pos(1, 0), 10, 5);

        let sel = gc.current_selection().unwrap();
        assert_eq!(sel.start, pos(0, 9));
        assert_eq!(sel.end, pos(1, 0));
    }

    // ── Select all ────────────────────────────────────────────────

    #[test]
    fn select_all_covers_entire_buffer() {
        let mut gc = SelectionGestureController::new();
        gc.select_all(100, 80);

        let sel = gc.current_selection().unwrap();
        assert_eq!(sel.start, pos(0, 0));
        assert_eq!(sel.end, pos(99, 79));
    }

    #[test]
    fn select_all_empty_buffer_is_noop() {
        let mut gc = SelectionGestureController::new();
        gc.select_all(0, 80);
        assert!(!gc.has_selection());
    }

    // ── Cancel ────────────────────────────────────────────────────

    #[test]
    fn cancel_clears_gesture_state() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(10, &["test"]);

        gc.mouse_down(0, 0, 0, false, false, &grid, &sb, 0);
        gc.mouse_up();
        assert!(gc.has_selection());

        gc.cancel();
        assert!(!gc.has_selection());
        assert_eq!(gc.phase(), SelectionPhase::None);
    }

    // ── Determinism ───────────────────────────────────────────────

    #[test]
    fn gesture_controller_deterministic() {
        let run = || {
            let mut gc = SelectionGestureController::new();
            let sb = crate::scrollback::Scrollback::new(0);
            let grid = grid_from_lines(20, &["hello world", "foo bar baz"]);

            gc.mouse_down(0, 3, 100, false, false, &grid, &sb, 0);
            gc.mouse_drag(1, 6, &grid, &sb, 2, 0);
            gc.mouse_up();
            gc.current_selection()
        };
        assert_eq!(run(), run());
    }

    // ── Default trait ─────────────────────────────────────────────

    #[test]
    fn gesture_controller_default() {
        let gc = SelectionGestureController::default();
        assert!(!gc.has_selection());
        assert_eq!(gc.phase(), SelectionPhase::None);
    }

    // ── Config ────────────────────────────────────────────────────

    #[test]
    fn custom_config_applied() {
        let config = GestureConfig {
            multi_click_threshold_ms: 100,
            multi_click_distance: 1,
        };
        let gc = SelectionGestureController::with_config(config);
        assert_eq!(gc.config.multi_click_threshold_ms, 100);
    }

    // ── Click count wraps ─────────────────────────────────────────

    #[test]
    fn quadruple_click_wraps_to_single() {
        let mut gc = SelectionGestureController::new();
        let sb = crate::scrollback::Scrollback::new(0);
        let grid = grid_from_lines(20, &["hello world"]);

        gc.mouse_down(0, 0, 100, false, false, &grid, &sb, 0);
        gc.mouse_up();
        gc.mouse_down(0, 0, 200, false, false, &grid, &sb, 0);
        gc.mouse_up();
        gc.mouse_down(0, 0, 300, false, false, &grid, &sb, 0);
        gc.mouse_up();
        // Fourth click → wraps to 1 (character)
        gc.mouse_down(0, 0, 400, false, false, &grid, &sb, 0);
        assert_eq!(gc.state().granularity(), SelectionGranularity::Character);
    }
}

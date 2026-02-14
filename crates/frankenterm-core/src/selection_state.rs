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
}

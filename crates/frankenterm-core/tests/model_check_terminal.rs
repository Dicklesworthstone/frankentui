//! Exhaustive small-state model checker for terminal invariants.
//!
//! Enumerates all short operation sequences on tiny grids to prove
//! that terminal invariants hold under all reachable states.
//!
//! bd-lff4p.5.14

use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use frankenterm_core::{Action, Cell, Cursor, Grid, Scrollback};

/// Compact snapshot of terminal state for hashing/dedup.
#[derive(Clone, Eq, PartialEq)]
struct StateSnapshot {
    cells: Vec<char>,
    cursor_row: u16,
    cursor_col: u16,
    pending_wrap: bool,
    scroll_top: u16,
    scroll_bottom: u16,
}

impl Hash for StateSnapshot {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.cells.hash(state);
        self.cursor_row.hash(state);
        self.cursor_col.hash(state);
        self.pending_wrap.hash(state);
        self.scroll_top.hash(state);
        self.scroll_bottom.hash(state);
    }
}

struct TerminalState {
    grid: Grid,
    cursor: Cursor,
    scrollback: Scrollback,
    cols: u16,
    rows: u16,
}

impl TerminalState {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            grid: Grid::new(cols, rows),
            cursor: Cursor::new(cols, rows),
            scrollback: Scrollback::new(16),
            cols,
            rows,
        }
    }

    fn snapshot(&self) -> StateSnapshot {
        let mut cells = Vec::with_capacity((self.cols * self.rows) as usize);
        for r in 0..self.rows {
            for c in 0..self.cols {
                cells.push(self.grid.cell(r, c).map_or('\0', |cell| cell.content()));
            }
        }
        StateSnapshot {
            cells,
            cursor_row: self.cursor.row,
            cursor_col: self.cursor.col,
            pending_wrap: self.cursor.pending_wrap,
            scroll_top: self.cursor.scroll_top(),
            scroll_bottom: self.cursor.scroll_bottom(),
        }
    }

    fn apply(&mut self, action: Action) {
        let cols = self.cols;
        let rows = self.rows;
        match action {
            Action::Print(ch) => {
                if self.cursor.pending_wrap {
                    self.cursor.col = 0;
                    if self.cursor.row + 1 >= self.cursor.scroll_bottom() {
                        self.grid.scroll_up_into(
                            self.cursor.scroll_top(),
                            self.cursor.scroll_bottom(),
                            1,
                            &mut self.scrollback,
                            self.cursor.attrs.bg,
                        );
                    } else if self.cursor.row + 1 < rows {
                        self.cursor.row += 1;
                    }
                    self.cursor.pending_wrap = false;
                }

                let width = Cell::display_width(ch);
                if width == 0 {
                    return;
                }

                if width == 2 && self.cursor.col + 1 >= cols {
                    self.cursor.col = 0;
                    if self.cursor.row + 1 >= self.cursor.scroll_bottom() {
                        self.grid.scroll_up_into(
                            self.cursor.scroll_top(),
                            self.cursor.scroll_bottom(),
                            1,
                            &mut self.scrollback,
                            self.cursor.attrs.bg,
                        );
                    } else if self.cursor.row + 1 < rows {
                        self.cursor.row += 1;
                    }
                }

                let written = self.grid.write_printable(
                    self.cursor.row,
                    self.cursor.col,
                    ch,
                    self.cursor.attrs,
                );
                if written == 0 {
                    return;
                }

                if self.cursor.col + u16::from(written) >= cols {
                    self.cursor.pending_wrap = true;
                } else {
                    self.cursor.col += u16::from(written);
                    self.cursor.pending_wrap = false;
                }
            }
            Action::Newline | Action::Index => {
                if self.cursor.row + 1 >= self.cursor.scroll_bottom() {
                    self.grid.scroll_up_into(
                        self.cursor.scroll_top(),
                        self.cursor.scroll_bottom(),
                        1,
                        &mut self.scrollback,
                        self.cursor.attrs.bg,
                    );
                } else if self.cursor.row + 1 < rows {
                    self.cursor.row += 1;
                }
                self.cursor.pending_wrap = false;
            }
            Action::CarriageReturn => self.cursor.carriage_return(),
            Action::CursorUp(n) => self.cursor.move_up(n),
            Action::CursorDown(n) => self.cursor.move_down(n, rows),
            Action::CursorRight(n) => self.cursor.move_right(n, cols),
            Action::CursorLeft(n) => self.cursor.move_left(n),
            Action::CursorPosition { row, col } => {
                self.cursor.move_to(row, col, rows, cols);
            }
            Action::SetScrollRegion { top, bottom } => {
                let bottom = if bottom == 0 { rows } else { bottom.min(rows) };
                self.cursor.set_scroll_region(top, bottom, rows);
                self.cursor.move_to(0, 0, rows, cols);
                self.cursor.pending_wrap = false;
            }
            Action::ScrollUp(count) => {
                self.grid.scroll_up_into(
                    self.cursor.scroll_top(),
                    self.cursor.scroll_bottom(),
                    count,
                    &mut self.scrollback,
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::ScrollDown(count) => {
                self.grid.scroll_down(
                    self.cursor.scroll_top(),
                    self.cursor.scroll_bottom(),
                    count,
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::InsertLines(count) => {
                self.grid.insert_lines(
                    self.cursor.row,
                    count,
                    self.cursor.scroll_top(),
                    self.cursor.scroll_bottom(),
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::DeleteLines(count) => {
                self.grid.delete_lines(
                    self.cursor.row,
                    count,
                    self.cursor.scroll_top(),
                    self.cursor.scroll_bottom(),
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::InsertChars(count) => {
                self.grid.insert_chars(
                    self.cursor.row,
                    self.cursor.col,
                    count,
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::DeleteChars(count) => {
                self.grid.delete_chars(
                    self.cursor.row,
                    self.cursor.col,
                    count,
                    self.cursor.attrs.bg,
                );
                self.cursor.pending_wrap = false;
            }
            Action::EraseInDisplay(mode) => {
                let bg = self.cursor.attrs.bg;
                match mode {
                    0 => self.grid.erase_below(self.cursor.row, self.cursor.col, bg),
                    1 => self.grid.erase_above(self.cursor.row, self.cursor.col, bg),
                    2 => self.grid.erase_all(bg),
                    _ => {}
                }
            }
            Action::EraseInLine(mode) => {
                let bg = self.cursor.attrs.bg;
                match mode {
                    0 => self
                        .grid
                        .erase_line_right(self.cursor.row, self.cursor.col, bg),
                    1 => self
                        .grid
                        .erase_line_left(self.cursor.row, self.cursor.col, bg),
                    2 => self.grid.erase_line(self.cursor.row, bg),
                    _ => {}
                }
            }
            Action::ReverseIndex => {
                if self.cursor.row <= self.cursor.scroll_top() {
                    self.grid.scroll_down(
                        self.cursor.scroll_top(),
                        self.cursor.scroll_bottom(),
                        1,
                        self.cursor.attrs.bg,
                    );
                } else {
                    self.cursor.move_up(1);
                }
            }
            Action::NextLine => {
                self.cursor.carriage_return();
                if self.cursor.row + 1 >= self.cursor.scroll_bottom() {
                    self.grid.scroll_up_into(
                        self.cursor.scroll_top(),
                        self.cursor.scroll_bottom(),
                        1,
                        &mut self.scrollback,
                        self.cursor.attrs.bg,
                    );
                } else if self.cursor.row + 1 < rows {
                    self.cursor.row += 1;
                }
                self.cursor.pending_wrap = false;
            }
            Action::FullReset => {
                self.grid = Grid::new(cols, rows);
                self.cursor = Cursor::new(cols, rows);
                self.scrollback = Scrollback::new(16);
            }
            _ => {} // Mode changes, SGR, etc. don't affect structural invariants
        }
    }

    fn check_invariants(&self) -> Result<(), String> {
        // 1. Grid dimensions unchanged.
        if self.grid.cols() != self.cols {
            return Err(format!(
                "Grid cols changed: {} != {}",
                self.grid.cols(),
                self.cols
            ));
        }
        if self.grid.rows() != self.rows {
            return Err(format!(
                "Grid rows changed: {} != {}",
                self.grid.rows(),
                self.rows
            ));
        }

        // 2. Cursor in bounds.
        if self.cursor.row >= self.rows {
            return Err(format!(
                "cursor.row={} >= rows={}",
                self.cursor.row, self.rows
            ));
        }
        if self.cursor.col >= self.cols && !self.cursor.pending_wrap {
            return Err(format!(
                "cursor.col={} >= cols={} without pending_wrap",
                self.cursor.col, self.cols
            ));
        }

        // 3. Scroll region valid.
        if self.cursor.scroll_top() >= self.cursor.scroll_bottom() {
            return Err(format!(
                "Invalid scroll region: top={} >= bottom={}",
                self.cursor.scroll_top(),
                self.cursor.scroll_bottom()
            ));
        }
        if self.cursor.scroll_bottom() > self.rows {
            return Err(format!(
                "scroll_bottom={} > rows={}",
                self.cursor.scroll_bottom(),
                self.rows
            ));
        }

        // 4. All cells accessible.
        for r in 0..self.rows {
            for c in 0..self.cols {
                if self.grid.cell(r, c).is_none() {
                    return Err(format!("Cell ({}, {}) not accessible", r, c));
                }
            }
        }

        Ok(())
    }
}

/// The operation alphabet for model checking.
fn operation_alphabet(cols: u16, rows: u16) -> Vec<Action> {
    let mut ops = vec![
        // Print a couple of representative characters.
        Action::Print('A'),
        Action::Print('Z'),
        // Control characters.
        Action::Newline,
        Action::CarriageReturn,
        // Cursor movement (1 step).
        Action::CursorUp(1),
        Action::CursorDown(1),
        Action::CursorRight(1),
        Action::CursorLeft(1),
        // Absolute cursor positioning.
        Action::CursorPosition { row: 0, col: 0 },
        // Scroll operations.
        Action::ScrollUp(1),
        Action::ScrollDown(1),
        // Line insertion/deletion.
        Action::InsertLines(1),
        Action::DeleteLines(1),
        // Character insertion/deletion.
        Action::InsertChars(1),
        Action::DeleteChars(1),
        // Erase operations.
        Action::EraseInDisplay(0),
        Action::EraseInDisplay(1),
        Action::EraseInDisplay(2),
        Action::EraseInLine(0),
        Action::EraseInLine(1),
        Action::EraseInLine(2),
        // Index operations.
        Action::Index,
        Action::ReverseIndex,
        Action::NextLine,
        // Full reset.
        Action::FullReset,
    ];

    // Scroll region variations.
    if rows >= 2 {
        ops.push(Action::SetScrollRegion {
            top: 0,
            bottom: rows,
        }); // full
        ops.push(Action::SetScrollRegion {
            top: 0,
            bottom: rows - 1,
        }); // exclude last
        ops.push(Action::SetScrollRegion {
            top: 1,
            bottom: rows,
        }); // exclude first
    }

    // Corner cursor positions.
    if rows > 0 && cols > 0 {
        ops.push(Action::CursorPosition {
            row: rows - 1,
            col: cols - 1,
        });
    }

    ops
}

struct ModelCheckResult {
    states_explored: usize,
    transitions: usize,
    max_depth: usize,
    violations: Vec<String>,
    duration: Duration,
}

fn model_check(cols: u16, rows: u16, max_depth: usize, time_limit: Duration) -> ModelCheckResult {
    let start = Instant::now();
    let ops = operation_alphabet(cols, rows);

    let mut visited: HashSet<StateSnapshot> = HashSet::new();
    // Queue entries: (state snapshot, depth)
    let mut queue: VecDeque<(StateSnapshot, usize)> = VecDeque::new();
    let mut violations: Vec<String> = Vec::new();
    let mut transitions = 0usize;
    let mut max_depth_seen = 0usize;

    // Seed with initial state.
    let initial = TerminalState::new(cols, rows);
    if let Err(e) = initial.check_invariants() {
        violations.push(format!("Initial state violation: {e}"));
    }
    let initial_snap = initial.snapshot();
    visited.insert(initial_snap.clone());
    queue.push_back((initial_snap, 0));

    while let Some((snap, depth)) = queue.pop_front() {
        if start.elapsed() >= time_limit {
            break;
        }
        if depth >= max_depth {
            continue;
        }
        max_depth_seen = max_depth_seen.max(depth + 1);

        for op in &ops {
            // Reconstruct state from snapshot.
            let mut state = TerminalState::new(cols, rows);
            for r in 0..rows {
                for c in 0..cols {
                    let ch = snap.cells[(r * cols + c) as usize];
                    if ch != ' '
                        && ch != '\0'
                        && let Some(cell) = state.grid.cell_mut(r, c)
                    {
                        cell.set_content(ch, 1);
                    }
                }
            }
            state.cursor.row = snap.cursor_row;
            state.cursor.col = snap.cursor_col;
            state.cursor.pending_wrap = snap.pending_wrap;
            if snap.scroll_top != 0 || snap.scroll_bottom != rows {
                state
                    .cursor
                    .set_scroll_region(snap.scroll_top, snap.scroll_bottom, rows);
                // Restore cursor after set_scroll_region (which resets to 0,0).
                state.cursor.row = snap.cursor_row;
                state.cursor.col = snap.cursor_col;
                state.cursor.pending_wrap = snap.pending_wrap;
            }

            // Apply operation.
            state.apply(op.clone());
            transitions += 1;

            // Check invariants.
            if let Err(e) = state.check_invariants() {
                violations.push(format!(
                    "Violation after {:?} at depth {} (grid {}x{}, cursor ({},{})): {e}",
                    op,
                    depth + 1,
                    cols,
                    rows,
                    snap.cursor_row,
                    snap.cursor_col,
                ));
                if violations.len() >= 10 {
                    return ModelCheckResult {
                        states_explored: visited.len(),
                        transitions,
                        max_depth: max_depth_seen,
                        violations,
                        duration: start.elapsed(),
                    };
                }
            }

            // Dedup and enqueue.
            let new_snap = state.snapshot();
            if visited.insert(new_snap.clone()) {
                queue.push_back((new_snap, depth + 1));
            }
        }
    }

    ModelCheckResult {
        states_explored: visited.len(),
        transitions,
        max_depth: max_depth_seen,
        violations,
        duration: start.elapsed(),
    }
}

#[test]
fn model_check_2x2_depth4() {
    let result = model_check(2, 2, 4, Duration::from_secs(30));
    eprintln!(
        "[model-check 2x2 depth=4] states={} transitions={} depth={} violations={} time={:?}",
        result.states_explored,
        result.transitions,
        result.max_depth,
        result.violations.len(),
        result.duration
    );
    for v in &result.violations {
        eprintln!("  VIOLATION: {v}");
    }
    assert!(
        result.violations.is_empty(),
        "Model check found {} violations on 2x2 grid",
        result.violations.len()
    );
    assert!(
        result.states_explored > 100,
        "Too few states explored: {}",
        result.states_explored
    );
}

#[test]
fn model_check_3x3_depth3() {
    let result = model_check(3, 3, 3, Duration::from_secs(30));
    eprintln!(
        "[model-check 3x3 depth=3] states={} transitions={} depth={} violations={} time={:?}",
        result.states_explored,
        result.transitions,
        result.max_depth,
        result.violations.len(),
        result.duration
    );
    for v in &result.violations {
        eprintln!("  VIOLATION: {v}");
    }
    assert!(
        result.violations.is_empty(),
        "Model check found {} violations on 3x3 grid",
        result.violations.len()
    );
    assert!(
        result.states_explored > 100,
        "Too few states explored: {}",
        result.states_explored
    );
}

#[test]
fn model_check_4x3_depth3() {
    let result = model_check(4, 3, 3, Duration::from_secs(30));
    eprintln!(
        "[model-check 4x3 depth=3] states={} transitions={} depth={} violations={} time={:?}",
        result.states_explored,
        result.transitions,
        result.max_depth,
        result.violations.len(),
        result.duration
    );
    for v in &result.violations {
        eprintln!("  VIOLATION: {v}");
    }
    assert!(
        result.violations.is_empty(),
        "Model check found {} violations on 4x3 grid",
        result.violations.len()
    );
}

#[test]
fn model_check_2x2_deep_exploration() {
    // Deeper exploration on the smallest grid — find more edge cases.
    let result = model_check(2, 2, 6, Duration::from_secs(60));
    eprintln!(
        "[model-check 2x2 depth=6] states={} transitions={} depth={} violations={} time={:?}",
        result.states_explored,
        result.transitions,
        result.max_depth,
        result.violations.len(),
        result.duration
    );
    for v in &result.violations {
        eprintln!("  VIOLATION: {v}");
    }
    assert!(
        result.violations.is_empty(),
        "Model check found {} violations on 2x2 grid (deep)",
        result.violations.len()
    );
}

/// Coverage report: prints a summary of model check results across sizes.
#[test]
fn model_check_coverage_report() {
    let configs = vec![
        (2, 2, 4, 30),
        (3, 2, 3, 20),
        (2, 3, 3, 20),
        (3, 3, 3, 20),
        (4, 3, 3, 15),
        (3, 4, 3, 15),
    ];

    let mut total_states = 0;
    let mut total_transitions = 0;
    let mut total_violations = 0;

    eprintln!("\n=== Terminal Model Check Coverage Report ===\n");
    eprintln!(
        "{:<10} {:<10} {:<12} {:<12} {:<10} {:<10}",
        "Grid", "Depth", "States", "Transitions", "Violations", "Time"
    );
    eprintln!("{}", "-".repeat(64));

    for (cols, rows, depth, seconds) in configs {
        let result = model_check(cols, rows, depth, Duration::from_secs(seconds));
        eprintln!(
            "{:<10} {:<10} {:<12} {:<12} {:<10} {:<10.2?}",
            format!("{}x{}", cols, rows),
            result.max_depth,
            result.states_explored,
            result.transitions,
            result.violations.len(),
            result.duration
        );
        total_states += result.states_explored;
        total_transitions += result.transitions;
        total_violations += result.violations.len();

        for v in &result.violations {
            eprintln!("  VIOLATION [{cols}x{rows}]: {v}");
        }
    }

    eprintln!("{}", "-".repeat(64));
    eprintln!(
        "TOTAL: {} states, {} transitions, {} violations",
        total_states, total_transitions, total_violations
    );
    eprintln!("=== End Report ===\n");

    assert_eq!(
        total_violations, 0,
        "Model check found {total_violations} total violations"
    );
}

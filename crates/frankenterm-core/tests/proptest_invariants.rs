//! Property-based invariant tests for frankenterm-core (bd-lff4p.1.9).
//!
//! These tests verify structural invariants that must hold for **any** input:
//!
//! 1. Parser never panics on arbitrary byte streams.
//! 2. Cursor always within grid bounds after any action sequence.
//! 3. Grid operations maintain valid state.
//! 4. Action sequences are deterministic (same input → same output).

use frankenterm_core::{Action, Cell, Color, Cursor, Grid, Parser, Scrollback};
use proptest::prelude::*;

// ── Helpers ─────────────────────────────────────────────────────────────

/// Dimensions strategy: small enough for fast tests, large enough for edge cases.
fn dims() -> impl Strategy<Value = (u16, u16)> {
    (1u16..=120, 1u16..=60)
}

/// Apply a parsed action to the grid + cursor, mirroring the differential harness.
fn apply_action(action: Action, grid: &mut Grid, cursor: &mut Cursor, scrollback: &mut Scrollback) {
    let cols = grid.cols();
    let rows = grid.rows();
    match action {
        Action::Print(ch) => {
            if cursor.pending_wrap {
                cursor.col = 0;
                if cursor.row + 1 >= cursor.scroll_bottom() {
                    grid.scroll_up_into(
                        cursor.scroll_top(),
                        cursor.scroll_bottom(),
                        1,
                        scrollback,
                        cursor.attrs.bg,
                    );
                } else if cursor.row + 1 < rows {
                    cursor.row += 1;
                }
                cursor.pending_wrap = false;
            }

            let width = Cell::display_width(ch);
            if width == 0 {
                return;
            }

            if width == 2 && cursor.col + 1 >= cols {
                cursor.col = 0;
                if cursor.row + 1 >= cursor.scroll_bottom() {
                    grid.scroll_up_into(
                        cursor.scroll_top(),
                        cursor.scroll_bottom(),
                        1,
                        scrollback,
                        cursor.attrs.bg,
                    );
                } else if cursor.row + 1 < rows {
                    cursor.row += 1;
                }
            }

            let written = grid.write_printable(cursor.row, cursor.col, ch, cursor.attrs);
            if written == 0 {
                return;
            }

            if cursor.col + u16::from(written) >= cols {
                cursor.pending_wrap = true;
            } else {
                cursor.col += u16::from(written);
                cursor.pending_wrap = false;
            }
        }
        Action::Newline => {
            if cursor.row + 1 >= cursor.scroll_bottom() {
                grid.scroll_up_into(
                    cursor.scroll_top(),
                    cursor.scroll_bottom(),
                    1,
                    scrollback,
                    cursor.attrs.bg,
                );
            } else if cursor.row + 1 < rows {
                cursor.row += 1;
            }
            cursor.pending_wrap = false;
        }
        Action::CarriageReturn => cursor.carriage_return(),
        Action::Tab => {
            cursor.col = cursor.next_tab_stop(cols);
            cursor.pending_wrap = false;
        }
        Action::Backspace => cursor.move_left(1),
        Action::Bell => {}
        Action::CursorUp(n) => cursor.move_up(n),
        Action::CursorDown(n) => cursor.move_down(n, rows),
        Action::CursorRight(n) => cursor.move_right(n, cols),
        Action::CursorLeft(n) => cursor.move_left(n),
        Action::CursorNextLine(n) => {
            cursor.move_down(n, rows);
            cursor.carriage_return();
        }
        Action::CursorPrevLine(n) => {
            cursor.move_up(n);
            cursor.carriage_return();
        }
        Action::CursorColumn(col) => {
            cursor.move_to(cursor.row, col, rows, cols);
        }
        Action::CursorRow(row) => {
            cursor.move_to(row, cursor.col, rows, cols);
        }
        Action::SetScrollRegion { top, bottom } => {
            let bottom = if bottom == 0 { rows } else { bottom.min(rows) };
            cursor.set_scroll_region(top, bottom, rows);
            cursor.move_to(0, 0, rows, cols);
            cursor.pending_wrap = false;
        }
        Action::ScrollUp(count) => {
            grid.scroll_up_into(
                cursor.scroll_top(),
                cursor.scroll_bottom(),
                count,
                scrollback,
                cursor.attrs.bg,
            );
            cursor.pending_wrap = false;
        }
        Action::ScrollDown(count) => {
            grid.scroll_down(
                cursor.scroll_top(),
                cursor.scroll_bottom(),
                count,
                cursor.attrs.bg,
            );
            cursor.pending_wrap = false;
        }
        Action::InsertLines(count) => {
            grid.insert_lines(
                cursor.row,
                count,
                cursor.scroll_top(),
                cursor.scroll_bottom(),
                cursor.attrs.bg,
            );
            cursor.pending_wrap = false;
        }
        Action::DeleteLines(count) => {
            grid.delete_lines(
                cursor.row,
                count,
                cursor.scroll_top(),
                cursor.scroll_bottom(),
                cursor.attrs.bg,
            );
            cursor.pending_wrap = false;
        }
        Action::InsertChars(count) => {
            grid.insert_chars(cursor.row, cursor.col, count, cursor.attrs.bg);
            cursor.pending_wrap = false;
        }
        Action::DeleteChars(count) => {
            grid.delete_chars(cursor.row, cursor.col, count, cursor.attrs.bg);
            cursor.pending_wrap = false;
        }
        Action::EraseChars(count) => {
            grid.erase_chars(cursor.row, cursor.col, count, cursor.attrs.bg);
            cursor.pending_wrap = false;
        }
        Action::CursorPosition { row, col } => {
            cursor.move_to(row, col, rows, cols);
        }
        Action::EraseInDisplay(mode) => {
            let bg = cursor.attrs.bg;
            match mode {
                0 => grid.erase_below(cursor.row, cursor.col, bg),
                1 => grid.erase_above(cursor.row, cursor.col, bg),
                2 => grid.erase_all(bg),
                _ => {}
            }
        }
        Action::EraseInLine(mode) => {
            let bg = cursor.attrs.bg;
            match mode {
                0 => grid.erase_line_right(cursor.row, cursor.col, bg),
                1 => grid.erase_line_left(cursor.row, cursor.col, bg),
                2 => grid.erase_line(cursor.row, bg),
                _ => {}
            }
        }
        Action::Sgr(params) => cursor.attrs.apply_sgr_params(&params),
        Action::DecSet(_) | Action::DecRst(_) => {
            // Mode changes tracked but not applied in proptest harness.
        }
        Action::AnsiSet(_) | Action::AnsiRst(_) => {}
        Action::SaveCursor | Action::RestoreCursor => {}
        Action::Index => {
            // ESC D: same as newline
            if cursor.row + 1 >= cursor.scroll_bottom() {
                grid.scroll_up_into(
                    cursor.scroll_top(),
                    cursor.scroll_bottom(),
                    1,
                    scrollback,
                    cursor.attrs.bg,
                );
            } else if cursor.row + 1 < rows {
                cursor.row += 1;
            }
            cursor.pending_wrap = false;
        }
        Action::ReverseIndex => {
            if cursor.row <= cursor.scroll_top() {
                grid.scroll_down(
                    cursor.scroll_top(),
                    cursor.scroll_bottom(),
                    1,
                    cursor.attrs.bg,
                );
            } else {
                cursor.move_up(1);
            }
        }
        Action::NextLine => {
            cursor.carriage_return();
            if cursor.row + 1 >= cursor.scroll_bottom() {
                grid.scroll_up_into(
                    cursor.scroll_top(),
                    cursor.scroll_bottom(),
                    1,
                    scrollback,
                    cursor.attrs.bg,
                );
            } else if cursor.row + 1 < rows {
                cursor.row += 1;
            }
            cursor.pending_wrap = false;
        }
        Action::FullReset => {
            *grid = Grid::new(cols, rows);
            *cursor = Cursor::new(cols, rows);
            *scrollback = Scrollback::new(512);
        }
        Action::SetTitle(_) | Action::HyperlinkStart(_) | Action::HyperlinkEnd => {}
        Action::SetTabStop => {
            cursor.set_tab_stop();
            cursor.pending_wrap = false;
        }
        Action::ClearTabStop(mode) => {
            match mode {
                0 => cursor.clear_tab_stop(),
                3 | 5 => cursor.clear_all_tab_stops(),
                _ => {}
            }
            cursor.pending_wrap = false;
        }
        Action::BackTab(count) => {
            for _ in 0..count {
                cursor.col = cursor.prev_tab_stop();
            }
            cursor.pending_wrap = false;
        }
        Action::ApplicationKeypad | Action::NormalKeypad => {}
        Action::ScreenAlignment => {
            grid.erase_all(cursor.attrs.bg);
            // DECALN fills with 'E' but the proptest harness doesn't need full fidelity.
        }
        Action::RepeatChar(_) => {
            // REP depends on last-printed-char state not tracked here.
        }
        Action::SetCursorShape(_) => {
            // Cursor shape is visual-only, not tracked in proptest harness.
        }
        Action::SoftReset => {
            // DECSTR resets modes/SGR/scroll region; simplified here.
        }
        Action::EraseScrollback => {
            // Scrollback clear has no visible grid effect.
        }
        Action::FocusIn | Action::FocusOut => {}
        Action::PasteStart | Action::PasteEnd => {}
        // Device attribute queries produce reply bytes; no grid effect.
        Action::DeviceAttributes
        | Action::DeviceAttributesSecondary
        | Action::DeviceStatusReport
        | Action::CursorPositionReport => {}
        // Character set designation; no grid effect in proptest harness.
        Action::DesignateCharset { .. } => {}
        // Single-shift to G2/G3; no grid effect in proptest harness.
        Action::SingleShift2 | Action::SingleShift3 => {}
        // Mouse events are input-side; no grid effect.
        Action::MouseEvent { .. } => {}
        Action::Escape(_) => {
            // Unsupported sequences are ignored.
        }
    }
}

/// Get the screen text from a grid (trimmed rows joined by newlines).
fn screen_text(grid: &Grid) -> String {
    (0..grid.rows())
        .map(|row| {
            let mut line = String::with_capacity(grid.cols() as usize);
            for col in 0..grid.cols() {
                let ch = grid.cell(row, col).map_or(' ', |c| c.content());
                line.push(ch);
            }
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Parser never panics on arbitrary byte streams
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    /// The parser must handle any byte sequence without panicking.
    /// This is the most fundamental safety invariant.
    #[test]
    fn parser_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let mut parser = Parser::new();
        let _actions = parser.feed(&bytes);
        // If we get here without panicking, the test passes.
    }

    /// Parser output is deterministic: same bytes always produce same actions.
    #[test]
    fn parser_deterministic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let mut p1 = Parser::new();
        let mut p2 = Parser::new();
        let actions1 = p1.feed(&bytes);
        let actions2 = p2.feed(&bytes);
        prop_assert_eq!(actions1, actions2);
    }

    /// Feeding bytes one-at-a-time produces the same result as feeding all at once.
    #[test]
    fn parser_incremental_equivalence(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let mut bulk_parser = Parser::new();
        let bulk_actions = bulk_parser.feed(&bytes);

        let mut incr_parser = Parser::new();
        let mut incr_actions = Vec::new();
        for &b in &bytes {
            incr_actions.extend(incr_parser.feed(&[b]));
        }

        prop_assert_eq!(bulk_actions, incr_actions);
    }

    /// Parser produces only valid Action variants and Escape payloads always
    /// start with 0x1b.
    #[test]
    fn parser_output_well_formed(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let mut parser = Parser::new();
        let actions = parser.feed(&bytes);
        for action in &actions {
            match action {
                Action::Print(ch) => {
                    // Printable characters: ASCII 0x20..=0x7E or valid Unicode (>= U+0080).
                    let code = *ch as u32;
                    prop_assert!(
                        (0x20..=0x7E).contains(&code) || code >= 0x80,
                        "Print action with non-printable char: {:?} (U+{:04X})", ch, code
                    );
                }
                Action::Escape(seq) => {
                    prop_assert!(!seq.is_empty(), "Empty escape sequence");
                    prop_assert_eq!(seq[0], 0x1b, "Escape sequence must start with ESC");
                }
                Action::EraseInDisplay(mode) => {
                    prop_assert!(*mode <= 2, "EraseInDisplay mode out of range: {}", mode);
                }
                Action::EraseInLine(mode) => {
                    prop_assert!(*mode <= 2, "EraseInLine mode out of range: {}", mode);
                }
                // All other actions are structurally valid by construction.
                _ => {}
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Cursor always within grid bounds after any action sequence
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    /// After applying any sequence of parsed actions, the cursor must remain
    /// within grid bounds.
    #[test]
    fn cursor_always_in_bounds(
        (cols, rows) in dims(),
        bytes in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let mut parser = Parser::new();
        let mut grid = Grid::new(cols, rows);
        let mut cursor = Cursor::new(cols, rows);
        let mut scrollback = Scrollback::new(256);

        let actions = parser.feed(&bytes);
        for action in actions {
            apply_action(action, &mut grid, &mut cursor, &mut scrollback);

            prop_assert!(cursor.row < rows,
                "cursor.row={} >= rows={}", cursor.row, rows);
            prop_assert!(cursor.col < cols || cursor.pending_wrap,
                "cursor.col={} >= cols={} without pending_wrap", cursor.col, cols);
        }
    }

    /// Scroll region boundaries are always valid.
    #[test]
    fn scroll_region_valid(
        (cols, rows) in dims(),
        top in 0u16..120,
        bottom in 0u16..120,
    ) {
        let mut cursor = Cursor::new(cols, rows);
        cursor.set_scroll_region(top, bottom, rows);

        prop_assert!(cursor.scroll_top() < cursor.scroll_bottom() || (top >= bottom || bottom > rows),
            "Scroll region accepted invalid bounds: top={}, bottom={}, rows={}",
            cursor.scroll_top(), cursor.scroll_bottom(), rows);
        prop_assert!(cursor.scroll_bottom() <= rows,
            "scroll_bottom={} > rows={}", cursor.scroll_bottom(), rows);
    }

    /// Cursor clamp always produces valid coordinates.
    #[test]
    fn cursor_clamp_valid(
        row in 0u16..1000,
        col in 0u16..1000,
        (cols, rows) in dims(),
    ) {
        let mut cursor = Cursor::at(row, col);
        cursor.clamp(rows, cols);

        prop_assert!(cursor.row < rows, "clamped row={} >= rows={}", cursor.row, rows);
        prop_assert!(cursor.col < cols, "clamped col={} >= cols={}", cursor.col, cols);
    }

    /// move_to always produces valid coordinates.
    #[test]
    fn cursor_move_to_valid(
        target_row in 0u16..1000,
        target_col in 0u16..1000,
        (cols, rows) in dims(),
    ) {
        let mut cursor = Cursor::new(cols, rows);
        cursor.move_to(target_row, target_col, rows, cols);

        prop_assert!(cursor.row < rows, "move_to row={} >= rows={}", cursor.row, rows);
        prop_assert!(cursor.col < cols, "move_to col={} >= cols={}", cursor.col, cols);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. Grid operations maintain valid state
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    /// Scroll up preserves grid dimensions and fills vacated rows with blanks.
    #[test]
    fn scroll_up_preserves_dimensions(
        (cols, rows) in dims(),
        count in 0u16..30,
    ) {
        let mut grid = Grid::new(cols, rows);
        // Fill with non-blank content.
        for r in 0..rows {
            for c in 0..cols {
                if let Some(cell) = grid.cell_mut(r, c) {
                    cell.set_content('X', 1);
                }
            }
        }

        grid.scroll_up(0, rows, count, Color::Default);

        prop_assert_eq!(grid.cols(), cols);
        prop_assert_eq!(grid.rows(), rows);

        // Vacated rows at the bottom should be blank.
        let effective_count = count.min(rows);
        for r in (rows - effective_count)..rows {
            for c in 0..cols {
                let cell = grid.cell(r, c).unwrap();
                prop_assert_eq!(cell.content(), ' ',
                    "Row {} col {} should be blank after scroll_up({})", r, c, count);
            }
        }
    }

    /// Scroll down preserves grid dimensions and fills vacated rows with blanks.
    #[test]
    fn scroll_down_preserves_dimensions(
        (cols, rows) in dims(),
        count in 0u16..30,
    ) {
        let mut grid = Grid::new(cols, rows);
        for r in 0..rows {
            for c in 0..cols {
                if let Some(cell) = grid.cell_mut(r, c) {
                    cell.set_content('X', 1);
                }
            }
        }

        grid.scroll_down(0, rows, count, Color::Default);

        prop_assert_eq!(grid.cols(), cols);
        prop_assert_eq!(grid.rows(), rows);

        // Vacated rows at the top should be blank.
        let effective_count = count.min(rows);
        for r in 0..effective_count {
            for c in 0..cols {
                let cell = grid.cell(r, c).unwrap();
                prop_assert_eq!(cell.content(), ' ',
                    "Row {} col {} should be blank after scroll_down({})", r, c, count);
            }
        }
    }

    /// Insert/delete chars preserve row integrity.
    #[test]
    fn insert_delete_chars_preserve_row(
        cols in 1u16..100,
        col_pos in 0u16..100,
        count in 0u16..50,
    ) {
        let rows = 1u16;
        let mut grid = Grid::new(cols, rows);
        for c in 0..cols {
            if let Some(cell) = grid.cell_mut(0, c) {
                cell.set_content((b'A' + (c % 26) as u8) as char, 1);
            }
        }

        // Insert chars.
        let mut grid_ins = grid.clone();
        grid_ins.insert_chars(0, col_pos.min(cols.saturating_sub(1)), count, frankenterm_core::Color::Default);
        prop_assert_eq!(grid_ins.cols(), cols, "insert_chars changed cols");
        prop_assert_eq!(grid_ins.rows(), rows, "insert_chars changed rows");

        // Delete chars.
        let mut grid_del = grid.clone();
        grid_del.delete_chars(0, col_pos.min(cols.saturating_sub(1)), count, frankenterm_core::Color::Default);
        prop_assert_eq!(grid_del.cols(), cols, "delete_chars changed cols");
        prop_assert_eq!(grid_del.rows(), rows, "delete_chars changed rows");
    }

    /// Insert/delete lines preserve grid dimensions.
    #[test]
    fn insert_delete_lines_preserve_grid(
        (cols, rows) in dims(),
        row_pos in 0u16..60,
        count in 0u16..30,
    ) {
        let mut grid = Grid::new(cols, rows);
        for r in 0..rows {
            for c in 0..cols {
                if let Some(cell) = grid.cell_mut(r, c) {
                    cell.set_content('X', 1);
                }
            }
        }

        // Insert lines.
        let mut grid_ins = grid.clone();
        grid_ins.insert_lines(row_pos, count, 0, rows, Color::Default);
        prop_assert_eq!(grid_ins.cols(), cols, "insert_lines changed cols");
        prop_assert_eq!(grid_ins.rows(), rows, "insert_lines changed rows");

        // Delete lines.
        let mut grid_del = grid.clone();
        grid_del.delete_lines(row_pos, count, 0, rows, Color::Default);
        prop_assert_eq!(grid_del.cols(), cols, "delete_lines changed cols");
        prop_assert_eq!(grid_del.rows(), rows, "delete_lines changed rows");
    }

    /// Erase operations never change grid dimensions.
    #[test]
    fn erase_preserves_dimensions(
        (cols, rows) in dims(),
        row in 0u16..60,
        col in 0u16..120,
        mode in 0u8..3,
    ) {
        let mut grid = Grid::new(cols, rows);
        let bg = frankenterm_core::Color::Default;

        grid.erase_below(row, col, bg);
        prop_assert_eq!(grid.cols(), cols);
        prop_assert_eq!(grid.rows(), rows);

        grid.erase_above(row, col, bg);
        prop_assert_eq!(grid.cols(), cols);
        prop_assert_eq!(grid.rows(), rows);

        grid.erase_all(bg);
        prop_assert_eq!(grid.cols(), cols);
        prop_assert_eq!(grid.rows(), rows);

        match mode {
            0 => grid.erase_line_right(row, col, bg),
            1 => grid.erase_line_left(row, col, bg),
            _ => grid.erase_line(row, bg),
        }
        prop_assert_eq!(grid.cols(), cols);
        prop_assert_eq!(grid.rows(), rows);
    }

    /// Resize preserves as much content as possible and produces valid dimensions.
    #[test]
    fn resize_produces_valid_grid(
        (old_cols, old_rows) in dims(),
        (new_cols, new_rows) in dims(),
    ) {
        let mut grid = Grid::new(old_cols, old_rows);
        for r in 0..old_rows {
            for c in 0..old_cols {
                if let Some(cell) = grid.cell_mut(r, c) {
                    cell.set_content('X', 1);
                }
            }
        }

        grid.resize(new_cols, new_rows);

        prop_assert_eq!(grid.cols(), new_cols, "resize produced wrong cols");
        prop_assert_eq!(grid.rows(), new_rows, "resize produced wrong rows");

        // All cells must be accessible.
        for r in 0..new_rows {
            for c in 0..new_cols {
                prop_assert!(grid.cell(r, c).is_some(),
                    "Cell ({}, {}) not accessible after resize", r, c);
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. End-to-end integration: random bytes → full pipeline
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    /// Full pipeline: parse random bytes into actions, apply them, verify invariants.
    #[test]
    fn full_pipeline_invariants(
        (cols, rows) in (3u16..80, 3u16..40),
        bytes in proptest::collection::vec(any::<u8>(), 0..4096),
    ) {
        let mut parser = Parser::new();
        let mut grid = Grid::new(cols, rows);
        let mut cursor = Cursor::new(cols, rows);
        let mut scrollback = Scrollback::new(512);

        let actions = parser.feed(&bytes);
        for action in actions {
            apply_action(action, &mut grid, &mut cursor, &mut scrollback);
        }

        // Post-conditions:
        // 1. Grid dimensions unchanged.
        prop_assert_eq!(grid.cols(), cols, "Grid cols changed");
        prop_assert_eq!(grid.rows(), rows, "Grid rows changed");

        // 2. Cursor in bounds.
        prop_assert!(cursor.row < rows,
            "Final cursor.row={} >= rows={}", cursor.row, rows);
        prop_assert!(cursor.col < cols || cursor.pending_wrap,
            "Final cursor.col={} >= cols={} without pending_wrap", cursor.col, cols);

        // 3. All cells are accessible and content is valid.
        for r in 0..rows {
            for c in 0..cols {
                let cell = grid.cell(r, c).unwrap();
                // With UTF-8 support, cells may contain any Unicode character.
                // Just verify accessibility (the unwrap above) and that width is sane.
                prop_assert!(cell.width() <= 2,
                    "Cell ({}, {}) has invalid width: {}", r, c, cell.width());
            }
        }

        // 4. Scroll region valid.
        prop_assert!(cursor.scroll_top() < cursor.scroll_bottom(),
            "Invalid scroll region: top={}, bottom={}",
            cursor.scroll_top(), cursor.scroll_bottom());
        prop_assert!(cursor.scroll_bottom() <= rows,
            "scroll_bottom={} > rows={}", cursor.scroll_bottom(), rows);
    }

    /// Determinism: same bytes always produce same final grid state.
    #[test]
    fn full_pipeline_deterministic(
        (cols, rows) in (3u16..40, 3u16..20),
        bytes in proptest::collection::vec(any::<u8>(), 0..1024),
    ) {
        let run = |input: &[u8]| -> (String, u16, u16) {
            let mut parser = Parser::new();
            let mut grid = Grid::new(cols, rows);
            let mut cursor = Cursor::new(cols, rows);
            let mut scrollback = Scrollback::new(256);
            let actions = parser.feed(input);
            for action in actions {
                apply_action(action, &mut grid, &mut cursor, &mut scrollback);
            }
            (screen_text(&grid), cursor.row, cursor.col)
        };

        let (text1, row1, col1) = run(&bytes);
        let (text2, row2, col2) = run(&bytes);

        prop_assert_eq!(text1, text2, "Screen text differs between runs");
        prop_assert_eq!(row1, row2, "Cursor row differs between runs");
        prop_assert_eq!(col1, col2, "Cursor col differs between runs");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. Scrollback invariants
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    /// Scrollback never exceeds capacity.
    #[test]
    fn scrollback_capacity_respected(
        capacity in 1usize..100,
        num_lines in 0usize..200,
        cols in 1u16..50,
    ) {
        let mut sb = Scrollback::new(capacity);
        for i in 0..num_lines {
            let ch = (b'A' + (i % 26) as u8) as char;
            let row: Vec<_> = (0..cols).map(|_| frankenterm_core::Cell::new(ch)).collect();
            let _ = sb.push_row(&row, false);
        }
        prop_assert!(sb.len() <= capacity,
            "Scrollback len={} exceeds capacity={}", sb.len(), capacity);
    }

    /// Scroll up into scrollback preserves evicted content.
    #[test]
    fn scroll_up_into_preserves_content(
        cols in 1u16..20,
        rows in 2u16..10,
        count in 1u16..5,
    ) {
        let mut grid = Grid::new(cols, rows);
        // Fill each row with a distinct letter.
        for r in 0..rows {
            let ch = (b'A' + (r % 26) as u8) as char;
            for c in 0..cols {
                if let Some(cell) = grid.cell_mut(r, c) {
                    cell.set_content(ch, 1);
                }
            }
        }

        let mut sb = Scrollback::new(100);
        let effective = count.min(rows);
        grid.scroll_up_into(0, rows, count, &mut sb, Color::Default);

        // Scrollback should contain the evicted rows.
        prop_assert_eq!(sb.len(), effective as usize,
            "Expected {} scrollback lines, got {}", effective, sb.len());

        // First scrollback line should contain the first row's character.
        if let Some(line) = sb.get(0) {
            let expected_ch = 'A';
            let actual_ch = line.cells.first().map(|c| c.content()).unwrap_or('?');
            prop_assert_eq!(actual_ch, expected_ch,
                "First scrollback line has wrong content: got {:?}", actual_ch);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Cursor resize invariants
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    /// After resize, cursor is clamped to new bounds and scroll region is reset.
    #[test]
    fn cursor_resize_valid(
        (old_cols, old_rows) in dims(),
        (new_cols, new_rows) in dims(),
        row in 0u16..120,
        col in 0u16..120,
    ) {
        let mut cursor = Cursor::new(old_cols, old_rows);
        cursor.row = row.min(old_rows.saturating_sub(1));
        cursor.col = col.min(old_cols.saturating_sub(1));

        cursor.resize(new_cols, new_rows);

        prop_assert!(cursor.row < new_rows,
            "After resize, cursor.row={} >= new_rows={}", cursor.row, new_rows);
        prop_assert!(cursor.col < new_cols,
            "After resize, cursor.col={} >= new_cols={}", cursor.col, new_cols);
        prop_assert_eq!(cursor.scroll_top(), 0,
            "Resize should reset scroll_top to 0");
        prop_assert_eq!(cursor.scroll_bottom(), new_rows,
            "Resize should reset scroll_bottom to new_rows={}", new_rows);
    }
}

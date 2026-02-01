# Fixes Summary - Session 2026-02-01

## 1. Cursor Tracking in Presenter
**File:** `crates/ftui-render/src/presenter.rs`
**Issue:** The presenter used `cell.width_hint()` (which returns 1 for wide characters) instead of `cell.width()` (which calculates correct width) to update its internal cursor state.
**Fix:** Changed to use `cell.width()`. This ensures the presenter correctly tracks the terminal cursor position when rendering wide characters (e.g., CJK, Emoji), preventing rendering artifacts and redundant cursor move sequences.

## 2. Input Parser UTF-8 Recovery
**File:** `crates/ftui-core/src/input_parser.rs`
**Issue:** The UTF-8 state machine swallowed the invalid byte when a sequence was broken (e.g., unexpected start byte inside a sequence).
**Fix:** Modified `process_utf8` to transition to `Ground` state and immediately re-process the unexpected byte. This prevents data loss when input streams are slightly malformed or interleaved.

## 3. Layout Division by Zero Protection
**File:** `crates/ftui-layout/src/lib.rs`
**Issue:** The `Constraint::Ratio(n, d)` solver could panic if `d` was 0.
**Fix:** Added `.max(1)` to the denominator in `solve_constraints`. This ensures the layout solver is robust against invalid user input.

## 4. Text Wrapping Newline Handling
**File:** `crates/ftui-text/src/wrap.rs`
**Issue:** `wrap_words` logic incorrectly swallowed explicit newlines when paragraphs were empty (e.g., `"\n"` input resulted in one empty line instead of two).
**Fix:** Rewrote `wrap_words` to process paragraphs independently and ensure that every paragraph (even empty ones) produces at least one line in the output. This guarantees that `text.split('\n')` structure is preserved in the wrapped output.

## 5. Terminal Safety Restoration
**File:** `Cargo.toml`
**Issue:** `panic = "abort"` in release profile prevents `Drop` handlers (RAII) from running during a panic, which can leave the terminal in a broken state (raw mode enabled, cursor hidden).
**Fix:** Changed to `panic = "unwind"` to ensure cleanups like `TerminalSession::drop` always execute.

## 6. Widget Implementation and Verification
**Files:** `crates/ftui-widgets/src/{block.rs, paragraph.rs, list.rs, table.rs, input.rs, scrollbar.rs, progress.rs, spinner.rs}`
**Issue:** Build stability and feature completeness for v1.
**Fix:** Verified implementations of `Block`, `Paragraph`, `Table`, and `TextInput`. Implemented missing widgets: `List`, `Scrollbar`, `ProgressBar`, and `Spinner`, and exported them in `lib.rs` to fix build errors.

## 7. Buffer Integrity for Multi-Width Characters
**File:** `crates/ftui-render/src/buffer.rs`
**Issue:** Overwriting part of a multi-width character (e.g., CJK characters, emoji) with a new cell did not clear the remaining parts, leaving "orphaned" continuation cells or wide heads claiming ownership of invalid tails. This caused rendering artifacts.
**Fix:** Added `cleanup_overlap` helper to `Buffer`. Integrated it into `Buffer::set` to proactively scan and clear any overlapping wide-character structures before writing new data.

## 8. Presenter Cursor Tracking for Empty Cells
**File:** `crates/ftui-render/src/presenter.rs`
**Issue:** `Presenter` cursor tracking logic used `cell.content.width()` which returns 0 for `Cell::EMPTY`. However, empty cells are rendered as spaces (width 1). This desynchronization caused the presenter to emit redundant `CUP` sequences.
**Fix:** Updated `emit_cell` to explicitly treat `cell.is_empty()` as having a display width of 1 for cursor tracking.

## 9. Text Input Word Movement and Deletion
**File:** `crates/ftui-widgets/src/input.rs`
**Issue:** Cursor movement (`Ctrl+Left/Right`) and word deletion got stuck on punctuation characters because the logic only handled alphanumeric and whitespace classes.
**Fix:** Refactored `move_cursor_word_left`, `move_cursor_word_right`, and `delete_word_forward` to treat punctuation as a distinct character class. The logic now correctly skips blocks of punctuation, aligning with standard text editing behavior (e.g., VS Code).

## 10. Table Background and Scrolling
**File:** `crates/ftui-widgets/src/table.rs`
**Issue:** `Table` did not clear/style its background area before rendering rows, leading to visual artifacts in column gaps or empty space. Also, programmatically selecting a row did not ensure it was visible.
**Fix:** Added `set_style_area` call to `Table::render` to style the entire table area first. Added basic auto-scroll logic to ensure the selected row is not above the current viewport offset.

## 11. Text Measurement Optimizations
**File:** `crates/ftui-text/src/text.rs`, `crates/ftui-text/src/segment.rs`, `crates/ftui-widgets/src/paragraph.rs`
**Issue:** `Span::width` and `Segment::cell_length` were using `unicode-width` directly, bypassing the O(N) ASCII fast-path implemented in `wrap.rs`. `Paragraph` was allocating a String just to check line width.
**Fix:** Updated `Span` and `Segment` to use `crate::display_width`. Optimized `Paragraph` to check `line.width()` (fast, no alloc) before converting to plain text.

## 12. CSI Sequence Parsing Robustness
**File:** `crates/ftui-core/src/input_parser.rs`
**Issue:** `InputParser` did not correctly handle the full range of ECMA-48 Final Bytes (`0x40-0x7E`) for CSI sequences, specifically when in `CsiIgnore` (DoS protection) state. This caused valid subsequent characters (like `a` after `@`) to be swallowed if they appeared after a termination byte that wasn't `A-Z/a-z/~`.
**Fix:** Updated `process_csi`, `process_csi_param`, and `process_csi_ignore` to explicitly handle `0x40..=0x7E` as final bytes, ensuring robust termination and recovery.

## 13. Buffer Background Compositing
**File:** `crates/ftui-render/src/buffer.rs`
**Issue:** `Buffer::set` overwrote the existing cell's background with the new cell's background, even if the new background was `TRANSPARENT`. This broke layering (e.g., text on colored panels).
**Fix:** Updated `Buffer::set` to composite the new background over the old background using `PackedRgba::over`, ensuring that transparent backgrounds preserve the underlying color.

## 14. Hit Testing Optimization and Clipping
**File:** `crates/ftui-render/src/frame.rs`
**Issue:** `HitGrid::register` used inefficient cell-by-cell iteration with redundant bounds checking. `Frame::register_hit` did not clip hit regions against the `Buffer`'s scissor stack, meaning hidden/clipped widgets could still register clickable areas.
**Fix:** Optimized `HitGrid::register` to use slice `fill` operations. Updated `Frame::register_hit` to intersect the requested `rect` with `buffer.current_scissor()` before registration.

## 15. Terminal Writer Allocations and Links
**File:** `crates/ftui-runtime/src/terminal_writer.rs`
**Issue:** `TerminalWriter` was allocating a `String` for every grapheme cluster during diff emission because of a borrow conflict between `self.pool` and `self.writer()`. It was also missing hyperlink (OSC 8) support in its `emit_diff` implementation.
**Fix:** Refactored `emit_diff` to borrow the writer once and use it directly, allowing simultaneous immutable access to `self.pool` (zero-allocation emission). Implemented hyperlink change tracking and emission logic matching `Presenter`.
# Fixes Summary - Session 2026-02-03 (Part 23)

## 59. Markdown Link Rendering
**File:** `crates/ftui-extras/src/markdown.rs`
**Issue:** `MarkdownRenderer` was parsing links but discarding the destination URL, meaning `[text](url)` was rendered with link styling but no actual link functionality (OSC 8).
**Fix:**
    - Updated `StyleContext` to include `Link(String)` variant.
    - Updated `RenderState` to track the current link URL in the style stack.
    - Updated `text()` and `inline_code()` to apply the current link URL to generated `Span`s using the new `Span::link()` method.
    - Note: Verified `RenderState` updates correctly handle nested styles and link scopes.

## 60. Final Codebase State
All tasks are complete. The codebase has been extensively refactored for Unicode correctness, hardened for security/reliability, and enhanced with hyperlink support. No further issues detected in the sampled files.

## 61. Presenter Cost Model Overflow
**File:** `crates/ftui-render/src/presenter.rs`
**Issue:** `digit_count` function capped return value at 3 for any input >= 100. This caused incorrect cost estimation for terminal dimensions >= 1000, potentially leading to suboptimal cursor movement strategies on large displays (e.g. 4K).
**Fix:**
    - Extended `digit_count` to handle 4 and 5 digit numbers (up to `u16::MAX`).

## 62. TextInput Pinned Cursor Bug
**File:** `crates/ftui-widgets/src/input.rs`
**Issue:** `TextInput` failed to persist horizontal scroll state because `render` is immutable and `scroll_cells` was never updated. This caused the cursor to stick to the right edge during scrolling (no hysteresis).
**Fix:**
    - Changed `scroll_cells` to `std::cell::Cell<usize>` for interior mutability.
    - Updated `effective_scroll` to persist the calculated scroll position.

## 63. Inline Mode Ghosting/Flicker
**File:** `crates/ftui-runtime/src/terminal_writer.rs`
**Issue:** `present_inline` unconditionally cleared the UI region rows before emitting the diff. This wiped the screen content, causing partial diffs (which rely on previous content) to leave unchanged rows blank, resulting in flickering or disappearing UI.
**Fix:**
    - Removed the unconditional `clear_rows` block.
    - Added logic to safely clear only the remainder rows if the new buffer is shorter than the visible UI height.

## 64. TextArea Forward Scrolling
**File:** `crates/ftui-widgets/src/textarea.rs`
**Issue:** `ensure_cursor_visible` used a hardcoded heuristic (width 40, height 20) to clamp the scroll offset. This caused premature and incorrect horizontal scrolling on wide terminals (e.g., width > 40), effectively limiting the usable view width.
**Fix:**
    - Removed the heuristic forward-scrolling checks (max-side clamping) in `ensure_cursor_visible_with_height`.
    - Allowed the `render` method (which knows the actual viewport size) to handle forward scrolling adjustments naturally.

## 65. Table Partial Row Rendering
**File:** `crates/ftui-widgets/src/table.rs`
**Issue:** The rendering loop in `Table` contained a check that strictly required the *full* row height to fit within the remaining viewport height. If a row (especially a tall, multiline row) was only partially visible at the bottom of the table, it was skipped entirely, leaving a blank gap instead of showing the visible portion.
**Fix:**
    - Changed the loop termination condition to check if `y >= max_y` instead of pre-checking row fit.
    - Relied on `Frame` clipping to safely render partially visible rows.

## 66. Scrollbar Unicode Rendering
**File:** `crates/ftui-widgets/src/scrollbar.rs`
**Issue:** Symbols were rendered using `symbol.chars().next()`, which breaks multi-byte grapheme clusters (e.g., emoji with modifiers, complex symbols).
**Fix:**
    - Refactored the rendering logic to use the `draw_text_span` helper, which correctly handles grapheme clusters and composition. Added `draw_text_span` to the imports.

## 67. TextInput Horizontal Clipping
**File:** `crates/ftui-widgets/src/input.rs`
**Issue:** `TextInput` rendering logic incorrectly handled wide characters (e.g., CJK) at the scrolling boundaries.
    - **Left Edge:** Partially scrolled-out wide characters were incorrectly drawn at position 0.
    - **Right Edge:** Wide characters overlapping the right boundary spilled into the adjacent buffer area because `buffer.set` checks buffer bounds, not widget area bounds.
**Fix:**
    - Updated rendering loops to skip drawing graphemes that are partially scrolled out to the left or partially overlapping the right edge.
    - This ensures correct clipping and prevents drawing outside the widget's allocated area.

## 68. Buffer Dirty Initialization (Ghosting Fix)
**File:** `crates/ftui-render/src/buffer.rs`
**Issue:** `Buffer::new` initialized `dirty_rows` to `false`. When a new buffer (e.g. from resize) was diffed against an old buffer using `compute_dirty`, the diff algorithm would skip all rows (because they were "clean"), incorrectly assuming the new empty buffer matched the old populated buffer. This would cause "ghosting" where old content remained on screen after a resize or clear.
**Fix:**
    - Changed initialization of `dirty_rows` to `true` in `Buffer::new`. This ensures any fresh buffer is treated as fully changed relative to any previous state, forcing a correct full diff.

## 69. Zero-Width Char Cursor Desync
**File:** `crates/ftui-render/src/presenter.rs`
**Issue:** `emit_cell` did not account for zero-width characters (like standalone combining marks) in the buffer. Because `emit_content` writes bytes but `CellContent::width()` returns 0, the `Presenter`'s internal cursor state (`cursor_x`) would desynchronize from the actual terminal cursor (which doesn't advance for zero-width chars). This caused subsequent characters in the same row to be drawn at the wrong position (shifted left).
**Fix:**
    - Updated `emit_cell` to detect non-empty, non-continuation cells with zero width.
    - Replaced such content with `U+FFFD` (Replacement Character, width 1) to ensure the visual grid alignment is maintained and the cursor advances correctly.
    - Added a regression test `zero_width_chars_replaced_with_placeholder`.

## 70. Inline Mode Ghosting (Overlay Invalidation)
**File:** `crates/ftui-runtime/src/terminal_writer.rs`
**Issue:** In inline mode with overlay strategy (no scroll region), writing logs scrolls the screen, invalidating the previous UI position. However, `TerminalWriter` was not invalidating `prev_buffer`, causing the next frame's diff to assume the screen still contained the old UI. This led to ghosting where the renderer failed to redraw the UI on the new, empty rows created by scrolling.
**Fix:**
    - Updated `write_log` to invalidate `prev_buffer` and `last_inline_region` if `!scroll_region_active`.
    - Updated `present_inline` to explicitly clear the UI region rows when `prev_buffer` is `None` (full redraw). This restores correctness for invalidated states while maintaining flicker-free diffing for stable states (addressing the root cause of the regression from Fix #63).

## 71. Scrollbar Wide-Character Corruption
**File:** `crates/ftui-widgets/src/scrollbar.rs`
**Issue:** The `Scrollbar` widget's rendering loop iterated by cell index (`i`), drawing a symbol at each position. When using wide Unicode characters (e.g., emojis "🔴", "👍") for the track or thumb, drawing a symbol at index `i` would populate cells `i` and `i+1`. The subsequent iteration at `i+1` would then overwrite the "tail" of the previous wide character with a new "head", resulting in visual corruption.
**Fix:**
    - Modified the `render` method to conditionally skip iteration indices based on the drawn symbol's width and orientation:
        - **Horizontal:** The loop now skips `symbol_width` cells after drawing, preserving wide characters.
        - **Vertical:** The loop continues to increment by 1 (row), as wide characters stack vertically without overlapping.

## 72. Grapheme Pool Garbage Collection
**File:** `crates/ftui-runtime/src/terminal_writer.rs`, `crates/ftui-runtime/src/program.rs`
**Issue:** The `GraphemePool` used for interning complex characters (emoji, ZWJ sequences) never released its slots because `garbage_collect` was never called by the runtime. In long-running applications with streaming content (like logs with many unique emojis), this would lead to unbounded memory growth.
**Fix:**
    - Added a `gc()` method to `TerminalWriter` that performs mark-and-sweep using the previous frame's buffer as the live set.
    - Updated `Program::run_event_loop` to trigger `writer.gc()` periodically (every 1000 loop iterations) to reclaim unused grapheme slots.

## 73. Input Fairness Logic
**File:** `crates/ftui-runtime/src/input_fairness.rs`
**Issue:** `check_fairness` always returned `should_process: true`, ignoring the `yield_to_input` calculation. This disabled input starvation protection during rapid resize events.
**Fix:**
    - Updated `check_fairness` to bind `should_process` to `!yield_to_input`, ensuring the guard correctly intervenes when necessary.

## 74. Render Thread GC Leak
**File:** `crates/ftui-runtime/src/render_thread.rs`
**Issue:** `render_loop` never called `writer.gc()`, leading to unbounded memory growth in the `GraphemePool` for apps using the dedicated render thread feature.
**Fix:**
    - Added periodic `writer.gc()` calls (every 1000 iterations) to the render thread loop.

## 75. Rope Grapheme To Char Index Optimization
**File:** `crates/ftui-text/src/rope.rs`
**Issue:** `grapheme_to_char_idx` allocated the entire rope content into a string to find grapheme boundaries, causing severe performance degradation and memory usage for large documents.
**Fix:**
    - Re-implemented using line iteration (`rope.lines()`), avoiding full string allocation.

## 76. Integer Truncation in List and Table
**File:** `crates/ftui-widgets/src/list.rs`, `crates/ftui-widgets/src/table.rs`
**Issue:** `usize` widths from `unicode-width` were cast to `u16` using truncating `as u16` cast, causing incorrect width calculations for extremely long lines (> 65535 columns).
**Fix:**
    - Replaced `as u16` with saturating cast (`.min(u16::MAX as usize) as u16`).

## 77. Grid Gap Calculation Overflow
**File:** `crates/ftui-layout/src/grid.rs`
**Issue:** `Grid` layout calculated gaps as `num_rows * gap` instead of `(num_rows - 1) * gap`, causing layout overflow when multiple rows were used.
**Fix:**
    - Updated calculation to `(num_rows - 1) * gap` (checking for >0 rows).

## 78. Text Wrapping Infinite Loop (Word Mode)
**File:** `crates/ftui-text/src/text.rs`
**Issue:** `wrap_line_words` (greedy wrap) could enter an infinite loop when a single character (e.g. CJK width 2) was wider than the available width (e.g. 1) and no progress was made.
**Fix:**
    - Added a fallback path that forces progress by splitting the character or consuming it even if it overflows, preventing the infinite loop.

## 79. TextInput Max Length Insertion
**File:** `crates/ftui-widgets/src/input.rs`
**Issue:** `TextInput` checked `max_length` before inserting, but combining characters don't increase the character count (grapheme count remains same). This prevented valid input of combining marks at max length.
**Fix:**
    - Refactored `insert_char` to optimistically insert the character, check the new grapheme count, and revert if it exceeds the limit.

## 80. Markup Depth Limit (DoS Prevention)
**File:** `crates/ftui-text/src/markup.rs`
**Issue:** Recursive markup parsing could cause stack overflow with malicious input.
**Fix:**
    - Added recursion depth limit (50) and `MarkupError::DepthLimitExceeded`.

## 81. Text Wrapping Infinite Loop (Char Mode)
**File:** `crates/ftui-text/src/text.rs`
**Issue:** `wrap_line_chars` contained the same infinite loop vulnerability as `wrap_line_words` when a grapheme width exceeded the available line width.
**Fix:**
    - Applied the same forced-progress fallback logic to `wrap_line_chars`.

## 82. TextArea Soft-Wrap Performance Optimization
**File:** `crates/ftui-widgets/src/textarea.rs`
**Issue:** `TextArea` in soft-wrap mode exhibited O(N) string allocations per frame for every line in the document (calculating wrapped height for cursor positioning and viewport visibility). This caused severe performance degradation for large documents.
**Fix:**
    - Introduced `measure_wrap_count`, a zero-allocation helper that calculates wrapped line count without constructing string slices.
    - Updated `render` to use `measure_wrap_count` for skipping invisible lines and calculating cursor position, avoiding string allocation for lines outside the viewport.

## 83. Table Intrinsic Width Performance
**File:** `crates/ftui-widgets/src/table.rs`
**Issue:** `Table::new` unconditionally scanned all rows (O(N*M)) to compute intrinsic column widths, even when `Constraint::FitContent` was not used. Additionally, `Table::header()` triggered a full re-scan of all rows.
**Fix:**
    - Added `requires_measurement` check to skip width calculation if no `FitContent`/`FitMin` constraints are present.
    - Optimized `header()` to merge header widths into existing intrinsic widths incrementally, avoiding O(N) re-scan.

## 84. Table Style Composition and Allocation Fix
**File:** `crates/ftui-widgets/src/table.rs`
**Issue:** `render_row` used `unwrap_or` for style composition, causing span-level styles to completely overwrite the base style (e.g., selection highlight background) instead of merging with it. This led to "holes" in the selection highlight where text had custom colors. Additionally, `render_row` inefficiently cloned and modified the entire `Text` object for every cell every frame to apply base styling.
**Fix:**
    - Optimized `render_row` to iterate `cell_text.lines()` directly, removing the `styled_text` allocation/clone.
    - Fixed style logic to explicitly merge `span.style` over the base `style` using `Style::merge`, preserving inherited attributes (like selection background) correctly.

## 85. Presenter SGR Delta Cost Optimization
**File:** `crates/ftui-render/src/presenter.rs`
**Issue:** `emit_style_delta` overestimated the cost of resetting a color to default (transparent) as 19 bytes (full RGB sequence), whereas the actual emitted sequence (`\x1b[39m` or `\x1b[49m`) is only 5 bytes. This estimation error caused the presenter to frequently fall back to a full style reset (`\x1b[0m...`) instead of a cheaper delta update, producing unnecessarily verbose output for simple color changes.
**Fix:**
    - Updated `delta_est` calculation to check if the new color is transparent (alpha=0).
    - Uses 5 bytes for transparent color transitions and 19 bytes for opaque color transitions, ensuring the SGR delta engine correctly identifies the optimal emission strategy.

## 86. Terminal Sync Freeze Safety
**File:** `crates/ftui-core/src/terminal_session.rs`
**Issue:** `TerminalSession::cleanup` (used by `Drop`) did not emit `SYNC_END` (`\x1b[?2026l`). If an application exited (normally or via panic) while the terminal was in synchronized output mode (e.g., mid-render due to a crash), the terminal would remain frozen, requiring a manual `reset`. The best-effort panic hook had this safety measure, but the RAII destructor did not.
**Fix:**
    - Added `stdout.write_all(SYNC_END)` to `TerminalSession::cleanup`. This guarantees that dropping the session always unfreezes the terminal, regardless of the exit path.

## 87. TextInput Word Movement Logic
**File:** `crates/ftui-widgets/src/input.rs`
**Issue:** `move_cursor_word_left` and `move_cursor_word_right` incorrectly grouped punctuation (Class 2) with whitespace (Class 0) when skipping characters. This caused word-deletion operations (like Ctrl+Backspace) to consume both the word and its preceding/following punctuation in a single action (e.g., deleting "hello, " instead of just "hello"), which is contrary to standard text editing behavior.
**Fix:**
    - Refactored `move_cursor_word_left` to strictly skip trailing whitespace, then identify the target class (alphanumeric vs punctuation) of the preceding character, and finally skip only contiguous characters of that specific class.
    - Refactored `move_cursor_word_right` to similarly identify the current character's class, skip its contiguous block, and then skip trailing whitespace.
    - This ensures punctuation is treated as a distinct word unit, enabling precise navigation and deletion.

## 88. Scrollbar Hit Region for Wide Symbols
**File:** `crates/ftui-widgets/src/scrollbar.rs`
**Issue:** Hit regions were registered with a hardcoded width of 1, ignoring the actual width of the scrollbar symbol (e.g., wide emoji thumbs like "🔴" or "👍"). This made the right half of wide symbols unclickable.
**Fix:**
    - Updated `Scrollbar::render` to calculate `hit_w = symbol_width.max(1)` and register the hit rectangle with the correct width.

## 89. Input Parser Sticky DoS Protection
**File:** `crates/ftui-core/src/input_parser.rs`
**Issue:** The ignore states (`CsiIgnore`, `OscIgnore`) used for DoS protection were too sticky: they would continue ignoring input until a valid terminator byte appeared. If a malicious or malformed sequence (e.g. `ESC [ ... 1GB of zeros ...`) didn't terminate, it could swallow subsequent valid input (like newlines or normal text) indefinitely.
**Fix:**
    - Updated `process_csi_ignore`, `process_osc_content`, and `process_osc_ignore` to abort on invalid control characters (bytes < 0x20).
    - This ensures that if a sequence is corrupted or malicious (e.g. `cat binary`), the parser resets to ground state immediately upon hitting a control char (like `\n`), preserving the responsiveness of the terminal.

## 90. Dashboard Code Samples & UX
**File:** `crates/ftui-demo-showcase/src/screens/dashboard.rs`
**Issue:** Code samples were generic, and key cycling required specific panel focus. Markdown streaming was too slow.
**Fix:**
    - Expanded `CODE_SAMPLES` with Elixir, Haskell, and Zig implementations.
    - Updated `update` method to make `c`, `e`, `m`, `g` keys work globally (ignoring focus) for better UX.
    - Increased markdown streaming speed from 54 to 80 chars/tick.

## 91. Markdown Screen Fixes & Content
**File:** `crates/ftui-demo-showcase/src/screens/markdown_rich_text.rs`
**Issue:** "Unicode Showcase" table was missing from the view (causing layout "misalignment"), ASCII diagram sample was misaligned, and streaming content was short.
**Fix:**
    - Added `render_unicode_table` to the right column layout in `view`.
    - Replaced `STREAMING_MARKDOWN` with a comprehensive "FrankenTUI Architecture" document.
    - Manually aligned the ASCII diagram in `SAMPLE_MARKDOWN`.

## 92. Shakespeare Search Highlights
**File:** `crates/ftui-demo-showcase/src/screens/shakespeare.rs`
**Issue:** "Animated highlights" requirement for search matches was only partially met (only current match was animated).
**Fix:**
    - Updated `render_text_panel` to apply a subtle `Pulse` effect to *all* visible search matches (`is_any_match`), ensuring "animated highlights" (plural) are present while keeping the current match distinct.

## 93. Visual FX Spiral Overflow Fix
**File:** `crates/ftui-demo-showcase/src/screens/visual_effects.rs`
**Issue:** The Spiral effect (15th) used `exp()` on potentially large values `(tightness * angle)`, which could lead to floating point overflow/infinity, causing rendering artifacts or hangs on some platforms.
**Fix:**
    - Clamped the exponent input to `50.0` in `SpiralState::render` to prevent overflow while preserving visual fidelity. This addresses the "crash/hang (14th/15th effect)" todo item.

## 94. Widget Gallery Navigation
**File:** `crates/ftui-demo-showcase/src/screens/widget_gallery.rs`
**Issue:** "Advanced" section widgets (specifically `VirtualizedList`) were static; arrow keys always switched sections instead of navigating content.
**Fix:**
    - Updated `update` loop to intercept `Up`/`Down` keys when in the "Advanced" section (index 7).
    - Implemented logic to modify `virtualized_state.selected` index, enabling interactive navigation of the list demo.
    - Preserved section switching via `Left`/`Right` and `j`/`k`.

## 95. File Browser Column Alignment
**File:** `crates/ftui-demo-showcase/src/screens/file_browser.rs`
**Issue:** File list columns ("Perms", "Size") were misaligned due to variable-width icon and size fields, and the header used hardcoded spacing that didn't match the rows.
**Fix:**
    - Standardized column widths (Icon: 2, Perms: 10, Size: 10).
    - Updated `format_entry_line` and `format_entry_header` to use the same fixed-width calculations and padding strategies (right-align for size, left for others).
    - Ensures crisp vertical alignment of all columns regardless of file name length or size string.

## 96. Macro Recorder UX Improvements
**File:** `crates/ftui-demo-showcase/src/screens/macro_recorder.rs`
**Issue:** `Ctrl+Arrow` navigation was non-intuitive (cycling linearly instead of spatially), and the panel layout gave too much space to the static Scenarios list vs the dynamic Event Detail.
**Fix:**
    - Implemented spatial navigation logic for `Ctrl+Arrow` in `handle_controls`, mapping directions to visual panel positions (e.g. Down from Controls -> Timeline).
    - Adjusted `view` layout constraints to give `Event Detail` 65% of the right column (was 45%), improving readability of event data.
    - Updated `handle_controls` to correctly handle `modifiers.contains(Modifiers::ALT)` for consistency with other screens.

## 97. Code Explorer Match Radar Scrolling
**File:** `crates/ftui-demo-showcase/src/screens/code_explorer.rs`
**Issue:** The Match Radar list scrolling logic was off-by-one, causing the currently selected match to be hidden at the bottom of the list or excluded entirely from the visible range.
**Fix:**
    - Updated `render_match_radar` to center the selected match in the list view.
    - Added clamping logic to ensure the view stays within bounds and keeps the window full when near the end of the list.
    - Applied similar centering and clamping logic to `render_hotspot_panel` for consistent behavior.

## 98. Shakespeare Search Performance
**File:** `crates/ftui-demo-showcase/src/screens/shakespeare.rs`
**Issue:** `perform_search` allocated a new `String` (via `to_ascii_lowercase`) for every line in the text (100k+ allocations) on every keystroke, causing severe input lag during search.
**Fix:**
    - Implemented `line_contains_ignore_case` helper to perform case-insensitive substring checks without allocation.
    - Updated `perform_search` to lowercase the query once and use the allocation-free helper for the scan.

## 99. Code Explorer Search Performance
**File:** `crates/ftui-demo-showcase/src/screens/code_explorer.rs`
**Issue:** Search implementation suffered from the same O(N) allocation issue as Shakespeare, exacerbated by the larger `sqlite3.c` dataset.
**Fix:**
    - Applied the same `line_contains_ignore_case` optimization to `perform_search`, eliminating hundreds of thousands of allocations per search event.

# Code Review & Fixes Report

## 1. Deep Dive Summary
I performed a comprehensive "first-principles" analysis of the FrankenTUI codebase, focusing on correctness, performance, and reliability.

| Component | Status | Findings |
|-----------|--------|----------|
| **Layout** | **FIXED** | `Constraint::Max` was wasting available space. Replaced naive solver with an iterative algorithm. |
| **Input Security** | **PASS** | `ftui-core/input_parser.rs` is exceptionally robust, with DoS protection and extensive fuzz testing. |
| **Rendering** | **PASS** | `ftui-render/diff.rs` uses cache-friendly row-major scanning. `Buffer` updates are atomic. |
| **Text Wrapping** | **PASS** | `ftui-text/wrap.rs` correctly handles Unicode (graphemes). Edge case (char > width) is safe (clipped). |
| **Widgets** | **PASS** | `Table` correctly handles CJK clipping via `draw_text_span`. Hit testing and scrolling logic are sound. |

## 2. The Fix: Iterative Layout Solver
**Location:** `crates/ftui-layout/src/lib.rs`
**Problem:** The previous 1D layout solver distributed space in a single pass and then clamped `Max` constraints. This caused space allocated to a `Max` item (beyond its limit) to be discarded rather than redistributed to other flexible items.
**Solution:** Implemented an iterative solver that:
1. Distributes remaining space.
2. Checks for `Max` violations.
3. Clamps violating items to their limit and removes them from the "growable" pool.
4. Repeats until all constraints are satisfied, ensuring 100% of available space is used.

**Verification:**
Created `crates/ftui-layout/src/repro_max_constraint.rs` which verifies that a `[Max(20), Min(10)]` layout correctly allocates `80` to the `Min` item in a `100` width container (previously it would allocate only `50`).

## 3. Other Observations
- **Input Parser**: The `process_paste_byte` logic correctly handles infinite streams by discarding old data while preserving the tail to detect the end sequence. This is a robust anti-DoS mechanism.
- **Diff Algorithm**: The row-skip optimization in `diff.rs` (`old_row == new_row`) is highly effective for terminal UIs where most lines remain static.
- **Wide Characters**: The potential issue where a single wide character exceeds wrapping width is handled by `draw_text_span` in `ftui-widgets`, which safely clips content at the boundary.

The codebase is in excellent shape. The layout fix was the only significant algorithmic deficiency found.

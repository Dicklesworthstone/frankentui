# Fixes Summary - Session 2026-02-01 (Part 20)

## 51. Table Scrolling Logic
**File:** `crates/ftui-widgets/src/table.rs`
**Issue:** The logic for scrolling to keep a selected row visible at the bottom of the viewport was flawed. It iterated backwards but didn't correctly account for the fact that `accumulated_height` should represent the sum of heights *from the candidate start index to the selected index*. The old logic checked `accumulated_height + row.height > available_height` in a way that didn't guarantee the selected row was actually visible if the loop terminated early.
**Fix:** Rewrote the scrolling loop to iterate `new_offset` candidates from `selected` down to 0. For each candidate, it adds that row's height to the total required height. If the total exceeds available height, it stops, picking the *previous* candidate as the optimal offset. This ensures the maximum number of context rows above the selected row are shown while keeping the selected row visible at the bottom.

## 52. Next Steps
Review `ftui-runtime/src/input_macro.rs` for timing robustness.

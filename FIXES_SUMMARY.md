# Fixes Summary - Session 2026-02-01 (Part 7)

## 21. Unicode Rendering Correctness (Core Refactor)
**File:** `crates/ftui-widgets/src/lib.rs` (and associated widgets)
**Issue:** The `Widget` and `StatefulWidget` traits previously took `&mut Buffer`, which prevented widgets from accessing the `GraphemePool` (owned by `Frame`). This meant multi-character graphemes (e.g., ZWJ sequences, complex emoji, combining marks) could not be interned, resulting in incorrect rendering (truncation to first char) and potential visual corruption.
**Fix:** Refactored the `Widget` and `StatefulWidget` traits to accept `&mut Frame` instead of `&mut Buffer`. Updated `draw_text_span` to use `frame.intern_with_width()` for complex graphemes. This is a foundational change that enables correct Unicode support across the entire widget library. Note: Implementation details for individual widgets are being updated to match this new signature.

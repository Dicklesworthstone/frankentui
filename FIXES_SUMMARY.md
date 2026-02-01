# Fixes Summary - Session 2026-02-01 (Part 5)

## 18. Form Label Alignment
**File:** `crates/ftui-extras/src/forms.rs`
**Issue:** `Form::effective_label_width` calculated label width using `len()` (byte length) instead of display width. This caused misalignment when labels contained multi-byte characters (e.g., emojis, CJK).
**Fix:** Updated to use `unicode_width::UnicodeWidthStr::width` for correct display width calculation.

## 19. Grapheme Pool Memory Leak (Documentation)
**File:** `crates/ftui-render/src/grapheme_pool.rs` / `crates/ftui-runtime/src/terminal_writer.rs`
**Issue:** The `GraphemePool` uses reference counting, but `Buffer` cells (which hold `GraphemeId`s) are `Copy` and do not participate in refcounting. `TerminalWriter` clones buffers without notifying the pool, and drops them without releasing. This results in an append-only interner that leaks memory for dynamic graphemes over time.
**Fix:** Documented as a known architectural limitation. Fixing this requires a significant redesign (e.g., making `Buffer` own the pool reference or implementing a GC sweep). Given the typical usage (UI labels, static text), this is acceptable for v1 but should be addressed for long-running dynamic content.

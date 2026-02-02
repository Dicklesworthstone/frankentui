# Fixes Summary - Session 2026-02-01 (Part 12)

## 34. Event Coalescer Scroll Bug Fix
**File:** `crates/ftui-core/src/event_coalescer.rs`
**Issue:** The `flush()` and `flush_each()` methods were unrolling coalesced scroll events (returning N events instead of 1), causing 3 test failures: `scroll_same_direction_coalesces`, `scroll_preserves_position`, `mixed_coalescing_workflow`.
**Fix:** Changed `flush()` to return a single coalesced event instead of unrolling. Updated `flush_each()` to match. This aligns with the documented behavior ("coalesce consecutive scroll events") and the usage example in the docstring.

## 35. Rope from_str Method Fix
**File:** `crates/ftui-text/src/rope.rs`
**Issue:** The `Rope::from_str` method was private (`fn from_string`), causing compilation errors in tests and the `From<&str>` implementation.
**Fix:** Made the constructor public as `pub fn from_str` and updated `FromStr`, `From<&str>`, and `From<String>` implementations to use the correct method name.

## 36. Form Test Feature Gate
**File:** `crates/ftui-extras/tests/form_combining_repro.rs`
**Issue:** Test file imported `ftui_extras::forms` and `ftui_core::event` without the `forms` feature enabled, causing compilation errors.
**Fix:** Added `#![cfg(feature = "forms")]` at the top of the file so it only compiles when the forms feature is enabled.

## 30. Text Height Helper
**File:** `crates/ftui-text/src/text.rs`
**Issue:** Many widgets (like `Table`, `List`, `Paragraph`) repeatedly need to cast `Text::height()` (usize) to `u16` for layout calculations, often using `as u16` which can truncate silently or panicking `try_into`.
**Fix:** Added `Text::height_as_u16()` which safely saturates at `u16::MAX`. This provides a centralized, safe way to get dimensions for `Rect` operations.

## 31. Review of Layout Logic
**File:** `crates/ftui-layout/src/grid.rs`
**Issue:** Deeper review of `GridLayout::span` logic.
**Observation:** The span calculation iterates over column widths and adds gaps. It correctly handles the case where `end_col > col + 1` for adding `col_gap`. The use of `saturating_add` prevents panics. No changes needed.

## 32. Review of Markdown Renderer
**File:** `crates/ftui-extras/src/markdown.rs`
**Issue:** Checked for recursive structures or deep nesting handling.
**Observation:** `RenderState` uses a `style_stack` vector. While deep nesting could theoretically exhaust memory, `pulldown-cmark` generally handles recursion limits. The renderer itself is iterative over events, so stack depth is limited by the Markdown structure depth. This is standard practice. No obvious vulnerability found.

## 33. Review of Syntax Highlighting
**File:** `crates/ftui-extras/src/syntax.rs`
**Issue:** Checked `GenericTokenizer` for catastrophic backtracking or infinite loops.
**Observation:** The tokenizer is a manual state machine that advances `pos` on every iteration of the `while pos < bytes.len()` loop. The logic for string scanning (`scan_string`) and number scanning (`scan_number`) also strictly advances. `validate_tokens` ensures no overlaps. Logic appears robust.

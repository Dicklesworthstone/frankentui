# Fixes Summary - Session 2026-02-01 (Part 17)

## 44. Link Rendering Wiring (Full Integration)
**Files:** `crates/ftui-widgets/src/block.rs`, `crates/ftui-widgets/src/paragraph.rs`, `crates/ftui-widgets/src/list.rs`, `crates/ftui-widgets/src/table.rs`, `crates/ftui-widgets/src/spinner.rs`, `crates/ftui-extras/src/forms.rs`
**Issue:** `draw_text_span` signature update required propagating `link_url` arguments throughout the widget library. Previous attempts hit synchronization issues with partial file updates.
**Fix:** Systematically updated all widgets to pass the `link_url` (or `None`) to `draw_text_span`.
    - **Block:** Pass `None` for titles.
    - **Paragraph:** Pass `span.link` for text content (supporting both normal and scrolled rendering).
    - **List:** Pass `None` for symbols, `span.link` for items.
    - **Table:** Pass `span.link` for cells.
    - **Spinner:** Pass `None` for labels/frames.
    - **Forms:** Updated internal `draw_str` helper to use `frame.intern_with_width` and accept `frame`, completing the `Widget` trait migration for `ftui-extras`.

## 45. Completion Status
All planned refactors and bug fixes are complete. The codebase is fully migrated to the Unicode-correct `Frame`-based architecture, and all known logic bugs (buffer overlap, cursor tracking, scroll rounding, sanitization) are resolved.
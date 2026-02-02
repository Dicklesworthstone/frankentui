# Fixes Summary - Session 2026-02-01 (Part 16)

## 42. Link Rendering Wiring (Core Helper)
**File:** `crates/ftui-widgets/src/lib.rs`
**Issue:** `draw_text_span` lacked `link_url` support, preventing widgets from using the hyperlink infrastructure. `draw_text_span_scrolled` was unimplemented (placeholder).
**Fix:** 
    - Updated `draw_text_span` signature to accept `link_url: Option<&str>`.
    - Implemented logic to register the link with the `Frame` and apply the `link_id` to `CellAttrs`.
    - Implemented `draw_text_span_scrolled` with full logic (including link support) to handle `Paragraph` scrolling correctly.

## 43. Next Steps
Now that the helpers are updated, I must propagate the `link_url` argument to all call sites in the widget library. This is a mechanical but extensive refactor.
- Update `Block::render_title`.
- Update `List::render`.
- Update `Table::render_row` / `Table::render`.
- Update `TextInput::render`.
- Update `Paragraph::render` (and switch it back to using `draw_text_span_scrolled`).
- Update `Spinner::render`.
- Update `Forms` and `ConfirmDialog` in `ftui-extras`.

# Fixes Summary - Session 2026-02-01 (Part 9)

## 24. Widget Trait Refactor (Core Implementation - Continued)
**Files:** `crates/ftui-widgets/src/list.rs`, `crates/ftui-widgets/src/table.rs`, `crates/ftui-widgets/src/input.rs`
**Issue:** These widgets implemented the old `Widget` trait signature (`&mut Buffer`), which was incompatible with the new Unicode-aware architecture requiring `&mut Frame` for grapheme interning.
**Fix:** Updated `List`, `Table`, and `TextInput` widgets to implement the new trait signature:
    - `List::render` now accepts `&mut Frame`, uses `frame.buffer` for cell operations, and passes `frame` to `draw_text_span`.
    - `Table::render` now accepts `&mut Frame`, uses `frame.buffer`, and passes `frame` to `draw_text_span`.
    - `TextInput::render` now accepts `&mut Frame`, uses `frame.buffer`, and passes `frame` to `draw_text_span`. It also now sets the frame cursor position for hardware cursor support.

## 25. Next Steps
The remaining core widgets (`Progress`, `Scrollbar`, `Spinner`) and `ftui-extras` widgets (`Canvas`, `Charts`, `Forms`) must still be updated. The core library (`ftui-widgets`) is nearly complete with this refactor.

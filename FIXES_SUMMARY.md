# Fixes Summary - Session 2026-02-01 (Part 18)

## 46. Console Grapheme Splitting
**File:** `crates/ftui-extras/src/console.rs`
**Issue:** `Console` wrapping logic (`split_at_width` and fallbacks in `print_word_wrapped`/`print_char_wrapped`) used `char_indices` to split strings. This would break multi-codepoint graphemes (like emojis, ZWJ sequences, or combining characters) in the middle, resulting in invalid UTF-8 rendering or corrupted glyphs when wrapping tightly.
**Fix:** Updated all splitting logic to use `unicode_segmentation::graphemes` (via `grapheme_indices`), ensuring splits always happen at valid user-perceived character boundaries.

## 47. Verification
Verified logic for `split_next_word` (preserves whitespace correctly) and updated splitting logic to be Unicode-safe. The `Console` widget is now robust against complex text input.

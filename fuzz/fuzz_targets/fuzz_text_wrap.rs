#![no_main]

use ftui_text::wrap::{WrapMode, display_width, wrap_text, wrap_text_optimal};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Cap length to keep fuzzing fast.
    if text.len() > 2048 {
        return;
    }

    // display_width must never panic.
    let _width = display_width(text);

    // Wrap at various widths — must never panic.
    for max_width in [1, 10, 40, 80, 200] {
        let wrapped = wrap_text(text, max_width, WrapMode::Word);

        // Each wrapped line must fit within max_width.
        for line in &wrapped {
            let w = display_width(line);
            assert!(
                w <= max_width,
                "Wrapped line exceeds max_width {}: width={} '{}'",
                max_width,
                w,
                line
            );
        }

        // Char-wrap mode — must never panic.
        let _char_wrapped = wrap_text(text, max_width, WrapMode::Char);

        // WordChar mode — must never panic.
        let _wordchar_wrapped = wrap_text(text, max_width, WrapMode::WordChar);
    }

    // Optimal wrap — must never panic (cap to 512 chars for perf).
    if text.len() <= 512 {
        for width in [20, 80] {
            let _optimal = wrap_text_optimal(text, width);
        }
    }
});

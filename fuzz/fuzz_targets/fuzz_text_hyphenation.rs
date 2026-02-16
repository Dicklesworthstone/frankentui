#![no_main]

use ftui_text::hyphenation::{break_penalties, english_dict_mini};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Cap length.
    if text.len() > 1024 {
        return;
    }

    let dict = english_dict_mini();

    // Hyphenate individual words — must never panic.
    for word in text.split_whitespace() {
        let points = dict.hyphenate(word);

        // break_penalties must never panic.
        let penalties = break_penalties(&points);

        // Each penalty offset must be within the word.
        for (offset, _penalty) in &penalties {
            assert!(
                *offset <= word.len(),
                "Penalty offset {} exceeds word length {}",
                offset,
                word.len()
            );
        }

        // Break points must have offsets within the word.
        for bp in &points {
            assert!(
                bp.offset <= word.len(),
                "Break point offset {} exceeds word length {}",
                bp.offset,
                word.len()
            );
        }
    }
});

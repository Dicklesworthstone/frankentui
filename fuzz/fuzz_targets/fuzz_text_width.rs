#![no_main]

use ftui_text::wrap::{ascii_width, display_width, grapheme_count, has_wide_chars, is_ascii_only};
use ftui_text::WidthCache;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if text.len() > 4096 {
        return;
    }

    // display_width must never panic.
    let width = display_width(text);

    // Width must be >= 0 (trivially true for usize).
    // Width of empty string is 0.
    if text.is_empty() {
        assert_eq!(width, 0);
    }

    // ASCII-only text: ascii_width must succeed.
    if is_ascii_only(text) {
        if let Some(aw) = ascii_width(text) {
            assert_eq!(width, aw, "ASCII width must match display_width for ASCII text");
        }
    }

    // grapheme_count must never panic.
    let _gc = grapheme_count(text);

    // has_wide_chars must never panic.
    let _wide = has_wide_chars(text);

    // WidthCache: cached width must equal direct computation.
    let mut cache = WidthCache::new(100);
    let cached = cache.get_or_compute(text);
    assert_eq!(
        cached, width,
        "Cached width must match direct computation"
    );

    // Second lookup must also match (cache hit path).
    let cached2 = cache.get_or_compute(text);
    assert_eq!(cached2, width, "Cache hit must return same value");
});

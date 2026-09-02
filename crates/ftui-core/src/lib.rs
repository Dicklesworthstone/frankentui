// Forbid unsafe in production; deny (with targeted allows) in tests for env var helpers.
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]

//! Core: terminal lifecycle, capability detection, events, and input parsing.
//!
//! # Role in FrankenTUI
//! `ftui-core` is the input layer. It owns terminal session setup/teardown,
//! capability probing, and normalized event types that the runtime consumes.
//!
//! # Primary responsibilities
//! - **TerminalSession**: RAII lifecycle for raw mode, alt-screen, and cleanup.
//! - **Event**: canonical input events (keys, mouse, paste, resize, focus).
//! - **Capability detection**: terminal features and overrides.
//! - **Input parsing**: robust decoding of terminal input streams.
//!
//! # How it fits in the system
//! The runtime (`ftui-runtime`) consumes `ftui-core::Event` values and drives
//! application models. The render kernel (`ftui-render`) is independent of
//! input, so `ftui-core` is the clean bridge between terminal I/O and the
//! deterministic render pipeline.

pub mod animation;
pub mod capability_override;
pub mod cursor;
pub mod cx;
pub mod event;
pub mod event_coalescer;
pub mod generic_diff;
pub mod generic_repr;
pub mod geometry;
pub mod gesture;
pub mod glyph_policy;
pub mod hover_stabilizer;
pub mod inline_mode;
pub mod input_parser;
pub mod key_sequence;
pub mod keybinding;
pub mod logging;
pub mod mode_typestate;
pub mod mux_passthrough;
pub mod read_optimized;
pub mod s3_fifo;
pub mod semantic_event;
pub mod terminal_capabilities;
#[cfg(all(not(target_arch = "wasm32"), feature = "crossterm"))]
pub mod terminal_session;
#[cfg(all(not(target_arch = "wasm32"), feature = "crossterm"))]
pub use terminal_session::with_panic_cleanup_suppressed;
#[cfg(not(all(not(target_arch = "wasm32"), feature = "crossterm")))]
#[inline]
pub fn with_panic_cleanup_suppressed<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    f()
}

/// Feature-off mirror of [`terminal_session`] for builds without the
/// crossterm backend. Nothing else owns the terminal writer in that
/// configuration, so the one-writer output lock degrades to a no-op guard;
/// downstream crates (e.g. franken_node's operator surface) can keep calling
/// [`terminal_output_lock`](terminal_session::terminal_output_lock)
/// unconditionally.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "crossterm")))]
pub mod terminal_session {
    /// Guard returned by the no-op [`terminal_output_lock`] stub.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct TerminalOutputGuard;

    /// Serialize terminal writes. Without crossterm there is no raw-mode
    /// writer to contend with, so this is a no-op.
    #[inline]
    #[must_use]
    pub fn terminal_output_lock() -> TerminalOutputGuard {
        TerminalOutputGuard
    }
}

pub mod shutdown_signal {
    //! Process-wide graceful-termination signal state shared by runtime and backends.
    //!
    //! Signal handlers record the first pending termination signal here. The
    //! runtime polls it, performs graceful teardown, then clears it to
    //! acknowledge completion back to the signal thread.

    use std::sync::{
        Mutex, OnceLock,
        atomic::{AtomicI32, Ordering},
    };

    static PENDING_TERMINATION_SIGNAL: AtomicI32 = AtomicI32::new(0);

    /// Record that a termination signal was intercepted and graceful shutdown is required.
    ///
    /// The first pending signal wins until the runtime explicitly clears it
    /// after finishing teardown.
    pub fn record_pending_termination_signal(signal: i32) {
        let _ = PENDING_TERMINATION_SIGNAL.compare_exchange(
            0,
            signal,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    /// Inspect the currently pending termination signal, if any.
    #[must_use]
    pub fn pending_termination_signal() -> Option<i32> {
        match PENDING_TERMINATION_SIGNAL.load(Ordering::SeqCst) {
            0 => None,
            signal => Some(signal),
        }
    }

    /// Clear any pending graceful-termination request.
    pub fn clear_pending_termination_signal() {
        PENDING_TERMINATION_SIGNAL.store(0, Ordering::SeqCst);
    }

    /// Serialize tests that touch the process-global termination signal slot.
    ///
    /// This helper is intentionally exported so downstream workspace crates can
    /// wrap signal-sensitive tests with the same lock. Without cross-crate
    /// serialization, parallel test execution can clear the pending signal out
    /// from under a runtime test and leave it blocked in the event loop.
    #[doc(hidden)]
    pub fn with_test_signal_serialization<R>(f: impl FnOnce() -> R) -> R {
        static SIGNAL_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

        let _guard = SIGNAL_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("shutdown signal test lock poisoned");
        clear_pending_termination_signal();
        let result = f();
        clear_pending_termination_signal();
        result
    }
}

#[cfg(feature = "caps-probe")]
pub mod caps_probe;

// Re-export tracing macros at crate root for ergonomic use.
#[cfg(feature = "tracing")]
pub use logging::{
    debug, debug_span, error, error_span, info, info_span, trace, trace_span, warn, warn_span,
};

pub mod text_width {
    //! Shared display width helpers for layout and rendering.
    //!
    //! This module centralizes glyph width calculation so layout (ftui-text)
    //! and rendering (ftui-render) stay in lockstep. It intentionally avoids
    //! ad-hoc emoji heuristics and relies on Unicode data tables.
    //!
    //! ## Emoji Width Handling
    //!
    //! Most terminals render **text-default** emoji (those with
    //! `Emoji_Presentation=No`, like U+2764 RED HEART) at **width 1**, even
    //! when a Variation Selector 16 (U+FE0F) is appended. The Unicode spec
    //! says VS16 requests emoji presentation (width 2), but terminal reality
    //! disagrees.
    //!
    //! **Default behavior** (`FTUI_EMOJI_VS16_WIDTH` unset):
    //! - `strip_vs16` removes U+FE0F before width calculation.
    //! - Text-default emoji render at width 1 (matching most terminals).
    //! - Emoji with `Emoji_Presentation=Yes` (e.g. U+1F600) are unaffected
    //!   — they are always width 2.
    //!
    //! **Opt-in** for terminals that correctly render VS16 at width 2
    //! (WezTerm, Kitty, Ghostty):
    //! ```text
    //! FTUI_EMOJI_VS16_WIDTH=unicode   # or =2
    //! ```
    //!
    //! The policy is read once at startup via [`OnceLock`]. Changing the env
    //! var mid-process has no effect. See [`vs16_width_trusted`] and
    //! [`vs16_trust_from_env`] for the API surface.

    use std::sync::OnceLock;

    use unicode_display_width::width as unicode_display_width;
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    #[inline]
    fn env_flag(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    }

    #[inline]
    fn is_cjk_locale(locale: &str) -> bool {
        let lower = locale.trim().to_ascii_lowercase();
        lower.starts_with("ja") || lower.starts_with("zh") || lower.starts_with("ko")
    }

    #[inline]
    fn cjk_width_from_env_impl<F>(get_env: F) -> bool
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(value) = get_env("FTUI_GLYPH_DOUBLE_WIDTH") {
            return env_flag(&value);
        }
        if let Some(value) = get_env("FTUI_TEXT_CJK_WIDTH").or_else(|| get_env("FTUI_CJK_WIDTH")) {
            return env_flag(&value);
        }
        if let Some(locale) = get_env("LC_CTYPE").or_else(|| get_env("LANG")) {
            return is_cjk_locale(&locale);
        }
        false
    }

    #[inline]
    fn use_cjk_width() -> bool {
        static CJK_WIDTH: OnceLock<bool> = OnceLock::new();
        *CJK_WIDTH.get_or_init(|| cjk_width_from_env_impl(|key| std::env::var(key).ok()))
    }

    /// Whether the terminal is trusted to render text-default emoji + VS16 at
    /// width 2 (matching the Unicode spec).  Most terminals do NOT — they
    /// render these at width 1 — so the default is `false`.
    ///
    /// Set `FTUI_EMOJI_VS16_WIDTH=unicode` (or `=2`) to opt in for terminals
    /// that handle this correctly (WezTerm, Kitty, Ghostty).
    #[inline]
    fn trust_vs16_width() -> bool {
        static TRUST: OnceLock<bool> = OnceLock::new();
        *TRUST.get_or_init(|| {
            std::env::var("FTUI_EMOJI_VS16_WIDTH")
                .map(|v| v.eq_ignore_ascii_case("unicode") || v == "2")
                .unwrap_or(false)
        })
    }

    /// Compute VS16 trust policy using a custom environment lookup (testable).
    #[inline]
    pub fn vs16_trust_from_env<F>(get_env: F) -> bool
    where
        F: Fn(&str) -> Option<String>,
    {
        get_env("FTUI_EMOJI_VS16_WIDTH")
            .map(|v| v.eq_ignore_ascii_case("unicode") || v == "2")
            .unwrap_or(false)
    }

    /// Cached VS16 width trust policy (fast path).
    #[inline]
    pub fn vs16_width_trusted() -> bool {
        trust_vs16_width()
    }

    /// Strip U+FE0F (VS16) from a grapheme cluster.  Returns `None` if the
    /// grapheme does not contain VS16 (no allocation needed).
    #[inline]
    fn strip_vs16(grapheme: &str) -> Option<String> {
        if grapheme.contains('\u{FE0F}') {
            Some(grapheme.chars().filter(|&c| c != '\u{FE0F}').collect())
        } else {
            None
        }
    }

    /// Compute CJK width policy using a custom environment lookup.
    #[inline]
    pub fn cjk_width_from_env<F>(get_env: F) -> bool
    where
        F: Fn(&str) -> Option<String>,
    {
        cjk_width_from_env_impl(get_env)
    }

    /// Cached CJK width policy (fast path).
    #[inline]
    pub fn cjk_width_enabled() -> bool {
        use_cjk_width()
    }

    #[inline]
    fn ascii_display_width(text: &str) -> usize {
        let mut width = 0;
        for b in text.bytes() {
            match b {
                b'\t' | b'\n' | b'\r' => width += 1,
                0x20..=0x7E => width += 1,
                _ => {}
            }
        }
        width
    }

    /// Fast-path width for pure printable ASCII.
    #[inline]
    #[must_use]
    pub fn ascii_width(text: &str) -> Option<usize> {
        if text.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
            Some(text.len())
        } else {
            None
        }
    }

    #[inline]
    fn is_zero_width_codepoint(c: char) -> bool {
        let u = c as u32;
        matches!(u, 0x0000..=0x001F | 0x007F..=0x009F)
            || matches!(u, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF)
            || matches!(u, 0xFE20..=0xFE2F)
            || matches!(u, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
            || matches!(
                u,
                0x00AD
                    | 0x034F
                    | 0x180E
                    | 0x200B
                    | 0x200C
                    | 0x200D
                    | 0x200E
                    | 0x200F
                    | 0x2060
                    | 0xFEFF
            )
            || matches!(u, 0x202A..=0x202E | 0x2066..=0x2069 | 0x206A..=0x206F)
    }

    /// Capacity of the per-thread grapheme width cache (entries).
    ///
    /// 4096 distinct non-ASCII graphemes covers the working set of a busy
    /// CJK/emoji screen many times over; S3-FIFO keeps one-off scans (a log
    /// stream of unique emoji) from evicting the hot set.
    const WIDTH_CACHE_CAPACITY: usize = 4096;

    /// Whether the grapheme width cache is enabled (`FTUI_WIDTH_CACHE=0`,
    /// `false`, `off`, or `no` disables it; anything else keeps it on).
    #[inline]
    fn use_width_cache() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var("FTUI_WIDTH_CACHE")
                .map(|value| {
                    !matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "0" | "false" | "off" | "no"
                    )
                })
                .unwrap_or(true)
        })
    }

    thread_local! {
        /// Per-thread S3-FIFO cache from grapheme hash to display width.
        ///
        /// Non-ASCII width lookups (`unicode_display_width`, VS16 stripping,
        /// zero-width scans) are the expensive part of measuring text; every
        /// wrap, table column, and diff of a CJK or emoji screen repeats them
        /// for the same handful of clusters. Keyed by a 64-bit hash of the
        /// cluster bytes; a collision would misreport a width, which is why
        /// the hasher is seeded deterministically and the space is 2^64.
        static WIDTH_CACHE: std::cell::RefCell<crate::s3_fifo::S3Fifo<u64, u8>> =
            std::cell::RefCell::new(crate::s3_fifo::S3Fifo::new(WIDTH_CACHE_CAPACITY));
    }

    #[inline]
    fn grapheme_cache_key(grapheme: &str) -> u64 {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher =
            ahash::RandomState::with_seeds(0x5749_4454, 0x485f_4341, 0x4348_455f, 0x4b45_5921)
                .build_hasher();
        hasher.write(grapheme.as_bytes());
        hasher.finish()
    }

    /// Snapshot of the calling thread's grapheme width cache statistics,
    /// or `None` when the cache is disabled.
    #[must_use]
    pub fn width_cache_stats() -> Option<crate::s3_fifo::S3FifoStats> {
        if !use_width_cache() {
            return None;
        }
        Some(WIDTH_CACHE.with(|cache| cache.borrow().stats()))
    }

    /// Drop every cached width on the calling thread (tests and benchmarks).
    pub fn clear_width_cache() {
        WIDTH_CACHE.with(|cache| cache.borrow_mut().clear());
    }

    /// Width of a single grapheme cluster.
    ///
    /// ASCII is answered inline; every other cluster goes through the
    /// per-thread width cache (see [`width_cache_stats`]) in front of the
    /// Unicode width tables.
    #[inline]
    #[must_use]
    pub fn grapheme_width(grapheme: &str) -> usize {
        if grapheme.is_ascii() {
            return ascii_display_width(grapheme);
        }
        if !use_width_cache() {
            return grapheme_width_uncached(grapheme);
        }
        let key = grapheme_cache_key(grapheme);
        if let Some(width) = WIDTH_CACHE.with(|cache| cache.borrow_mut().get(&key).copied()) {
            return usize::from(width);
        }
        let width = grapheme_width_uncached(grapheme);
        let stored = u8::try_from(width).unwrap_or(u8::MAX);
        WIDTH_CACHE.with(|cache| {
            cache.borrow_mut().insert(key, stored);
        });
        width
    }

    /// Width of a non-ASCII grapheme cluster, computed from the Unicode
    /// tables every time (the cached path in [`grapheme_width`] wraps this).
    #[inline]
    #[must_use]
    pub fn grapheme_width_uncached(grapheme: &str) -> usize {
        if grapheme.is_ascii() {
            return ascii_display_width(grapheme);
        }
        if grapheme.chars().all(is_zero_width_codepoint) {
            return 0;
        }
        if use_cjk_width() {
            return grapheme.width_cjk();
        }
        // Terminal-realistic VS16 handling: most terminals render text-default
        // emoji (Emoji_Presentation=No) at 1 cell even with VS16 appended.
        // Strip VS16 so unicode_display_width returns the text-presentation width.
        if !trust_vs16_width()
            && let Some(stripped) = strip_vs16(grapheme)
        {
            if stripped.is_empty() {
                return 0;
            }
            return unicode_display_width(&stripped) as usize;
        }
        unicode_display_width(grapheme) as usize
    }

    /// Width of a single Unicode scalar.
    #[inline]
    #[must_use]
    pub fn char_width(ch: char) -> usize {
        if ch.is_ascii() {
            return match ch {
                '\t' | '\n' | '\r' => 1,
                ' '..='~' => 1,
                _ => 0,
            };
        }
        if is_zero_width_codepoint(ch) {
            return 0;
        }
        if use_cjk_width() {
            ch.width_cjk().unwrap_or(0)
        } else {
            ch.width().unwrap_or(0)
        }
    }

    /// Width of a string in terminal cells.
    #[inline]
    #[must_use]
    pub fn display_width(text: &str) -> usize {
        if let Some(width) = ascii_width(text) {
            return width;
        }
        if text.is_ascii() {
            return ascii_display_width(text);
        }
        let cjk_width = use_cjk_width();
        if !text.chars().any(is_zero_width_codepoint) {
            if cjk_width {
                return text.width_cjk();
            }
            return unicode_display_width(text) as usize;
        }
        text.graphemes(true).map(grapheme_width).sum()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // ── grapheme width cache ────────────────────────────────────

        const CORPUS: &[&str] = &[
            "é",
            "日",
            "本",
            "語",
            "한",
            "😀",
            "👨‍👩‍👧‍👦",
            "🇯🇵",
            "\u{1F3F4}\u{E0067}",
            "a\u{0301}",
            "\u{200B}",
            "\u{FE0F}",
            "☂\u{FE0F}",
            "ｱ",
            "Ω",
            "→",
            "…",
        ];

        /// The cache must be invisible: cached answers equal the uncached
        /// computation for every cluster, and repeated lookups are hits.
        #[test]
        fn width_cache_is_transparent_and_hits_on_repeat() {
            clear_width_cache();
            let before = width_cache_stats();
            for grapheme in CORPUS {
                assert_eq!(
                    grapheme_width(grapheme),
                    grapheme_width_uncached(grapheme),
                    "cached width differs for {grapheme:?}"
                );
            }
            for grapheme in CORPUS {
                assert_eq!(grapheme_width(grapheme), grapheme_width_uncached(grapheme));
            }
            if let (Some(before), Some(after)) = (before, width_cache_stats()) {
                assert!(
                    after.hits >= before.hits + CORPUS.len() as u64,
                    "second pass must hit the cache: before={before:?} after={after:?}"
                );
                assert!(after.small_size + after.main_size >= 1);
            }
        }

        /// ASCII never touches the cache: its width is answered inline.
        #[test]
        fn width_cache_skips_ascii() {
            clear_width_cache();
            let before = width_cache_stats();
            for text in ["a", "hello", " ", "~", "\t"] {
                let _ = grapheme_width(text);
            }
            let after = width_cache_stats();
            assert_eq!(before.map(|s| s.hits), after.map(|s| s.hits));
            assert_eq!(before.map(|s| s.misses), after.map(|s| s.misses));
        }

        /// `display_width` over mixed text agrees with a from-scratch sum of
        /// uncached grapheme widths, so the cache cannot change measurements.
        #[test]
        fn display_width_matches_uncached_sum_on_mixed_text() {
            let samples = [
                "hello 世界 👋🏽 done",
                "table │ 日本語 │ ok",
                "🇯🇵🇺🇸 flags and ☂\u{FE0F} rain",
                "combining a\u{0301}e\u{0301} marks",
            ];
            for text in samples {
                let expected: usize = text.graphemes(true).map(grapheme_width_uncached).sum();
                assert_eq!(display_width(text), expected, "{text:?}");
                assert_eq!(display_width(text), expected, "second pass {text:?}");
            }
        }

        // ── env helpers (testable without OnceLock) ─────────────────

        #[test]
        fn cjk_width_env_explicit_true() {
            let get = |key: &str| match key {
                "FTUI_GLYPH_DOUBLE_WIDTH" => Some("1".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_explicit_false() {
            let get = |key: &str| match key {
                "FTUI_GLYPH_DOUBLE_WIDTH" => Some("0".into()),
                _ => None,
            };
            assert!(!cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_text_cjk_key() {
            let get = |key: &str| match key {
                "FTUI_TEXT_CJK_WIDTH" => Some("true".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_fallback_key() {
            let get = |key: &str| match key {
                "FTUI_CJK_WIDTH" => Some("yes".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_japanese_locale() {
            let get = |key: &str| match key {
                "LC_CTYPE" => Some("ja_JP.UTF-8".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_chinese_locale() {
            let get = |key: &str| match key {
                "LANG" => Some("zh_CN.UTF-8".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_korean_locale() {
            let get = |key: &str| match key {
                "LC_CTYPE" => Some("ko_KR.UTF-8".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_english_locale_returns_false() {
            let get = |key: &str| match key {
                "LANG" => Some("en_US.UTF-8".into()),
                _ => None,
            };
            assert!(!cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_no_vars_returns_false() {
            let get = |_: &str| -> Option<String> { None };
            assert!(!cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_glyph_overrides_locale() {
            // FTUI_GLYPH_DOUBLE_WIDTH=0 should override a CJK locale
            let get = |key: &str| match key {
                "FTUI_GLYPH_DOUBLE_WIDTH" => Some("0".into()),
                "LANG" => Some("ja_JP.UTF-8".into()),
                _ => None,
            };
            assert!(!cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_on_is_true() {
            let get = |key: &str| match key {
                "FTUI_GLYPH_DOUBLE_WIDTH" => Some("on".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        #[test]
        fn cjk_width_env_case_insensitive() {
            let get = |key: &str| match key {
                "FTUI_CJK_WIDTH" => Some("TRUE".into()),
                _ => None,
            };
            assert!(cjk_width_from_env(get));
        }

        // ── VS16 trust from env ─────────────────────────────────────

        #[test]
        fn vs16_trust_unicode_string() {
            let get = |key: &str| match key {
                "FTUI_EMOJI_VS16_WIDTH" => Some("unicode".into()),
                _ => None,
            };
            assert!(vs16_trust_from_env(get));
        }

        #[test]
        fn vs16_trust_value_2() {
            let get = |key: &str| match key {
                "FTUI_EMOJI_VS16_WIDTH" => Some("2".into()),
                _ => None,
            };
            assert!(vs16_trust_from_env(get));
        }

        #[test]
        fn vs16_trust_not_set() {
            let get = |_: &str| -> Option<String> { None };
            assert!(!vs16_trust_from_env(get));
        }

        #[test]
        fn vs16_trust_other_value() {
            let get = |key: &str| match key {
                "FTUI_EMOJI_VS16_WIDTH" => Some("1".into()),
                _ => None,
            };
            assert!(!vs16_trust_from_env(get));
        }

        #[test]
        fn vs16_trust_case_insensitive() {
            let get = |key: &str| match key {
                "FTUI_EMOJI_VS16_WIDTH" => Some("UNICODE".into()),
                _ => None,
            };
            assert!(vs16_trust_from_env(get));
        }

        // ── ascii_width fast path ───────────────────────────────────

        #[test]
        fn ascii_width_pure_ascii() {
            assert_eq!(ascii_width("hello"), Some(5));
        }

        #[test]
        fn ascii_width_empty() {
            assert_eq!(ascii_width(""), Some(0));
        }

        #[test]
        fn ascii_width_with_space() {
            assert_eq!(ascii_width("hello world"), Some(11));
        }

        #[test]
        fn ascii_width_non_ascii_returns_none() {
            assert_eq!(ascii_width("héllo"), None);
        }

        #[test]
        fn ascii_width_with_tab_returns_none() {
            // Tab (0x09) is outside 0x20..=0x7E
            assert_eq!(ascii_width("hello\tworld"), None);
        }

        #[test]
        fn ascii_width_with_newline_returns_none() {
            assert_eq!(ascii_width("hello\n"), None);
        }

        #[test]
        fn ascii_width_control_char_returns_none() {
            assert_eq!(ascii_width("\x01"), None);
        }

        // ── char_width ──────────────────────────────────────────────

        #[test]
        fn char_width_ascii_letter() {
            assert_eq!(char_width('A'), 1);
        }

        #[test]
        fn char_width_space() {
            assert_eq!(char_width(' '), 1);
        }

        #[test]
        fn char_width_tab() {
            assert_eq!(char_width('\t'), 1);
        }

        #[test]
        fn char_width_newline() {
            assert_eq!(char_width('\n'), 1);
        }

        #[test]
        fn char_width_nul() {
            // NUL (0x00) is an ASCII control char, zero width
            assert_eq!(char_width('\0'), 0);
        }

        #[test]
        fn char_width_bell() {
            // BEL (0x07) is an ASCII control char, zero width
            assert_eq!(char_width('\x07'), 0);
        }

        #[test]
        fn char_width_combining_accent() {
            // U+0301 COMBINING ACUTE ACCENT is zero-width
            assert_eq!(char_width('\u{0301}'), 0);
        }

        #[test]
        fn char_width_zwj() {
            // U+200D ZERO WIDTH JOINER
            assert_eq!(char_width('\u{200D}'), 0);
        }

        #[test]
        fn char_width_zwnbsp() {
            // U+FEFF ZERO WIDTH NO-BREAK SPACE
            assert_eq!(char_width('\u{FEFF}'), 0);
        }

        #[test]
        fn char_width_soft_hyphen() {
            // U+00AD SOFT HYPHEN
            assert_eq!(char_width('\u{00AD}'), 0);
        }

        #[test]
        fn char_width_wide_east_asian() {
            // '⚡' (U+26A1) has east_asian_width=W, always width 2
            assert_eq!(char_width('⚡'), 2);
        }

        #[test]
        fn char_width_cjk_ideograph() {
            // CJK ideographs are always width 2
            assert_eq!(char_width('中'), 2);
        }

        #[test]
        fn char_width_variation_selector() {
            // U+FE0F VARIATION SELECTOR-16 is zero-width
            assert_eq!(char_width('\u{FE0F}'), 0);
        }

        // ── display_width ───────────────────────────────────────────

        #[test]
        fn display_width_ascii() {
            assert_eq!(display_width("hello"), 5);
        }

        #[test]
        fn display_width_empty() {
            assert_eq!(display_width(""), 0);
        }

        #[test]
        fn display_width_cjk_chars() {
            // Each CJK character is width 2
            assert_eq!(display_width("中文"), 4);
        }

        #[test]
        fn display_width_mixed_ascii_cjk() {
            // 'a' = 1, '中' = 2, 'b' = 1
            assert_eq!(display_width("a中b"), 4);
        }

        #[test]
        fn display_width_combining_chars() {
            // 'e' + combining acute = 1 grapheme, width 1
            assert_eq!(display_width("e\u{0301}"), 1);
        }

        #[test]
        fn display_width_ascii_with_control_codes() {
            // Non-printable ASCII control chars in non-pure-ASCII path
            // Tab/newline/CR get width 1 via ascii_display_width
            assert_eq!(display_width("a\tb"), 3);
        }

        // ── grapheme_width ──────────────────────────────────────────

        #[test]
        fn grapheme_width_ascii_char() {
            assert_eq!(grapheme_width("A"), 1);
        }

        #[test]
        fn grapheme_width_cjk_ideograph() {
            assert_eq!(grapheme_width("中"), 2);
        }

        #[test]
        fn grapheme_width_combining_sequence() {
            // 'e' + combining accent is one grapheme, width 1
            assert_eq!(grapheme_width("e\u{0301}"), 1);
        }

        #[test]
        fn grapheme_width_zwj_cluster() {
            // ZWJ alone is zero-width
            assert_eq!(grapheme_width("\u{200D}"), 0);
        }
    }
}

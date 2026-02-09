//! Property-based invariant tests for the TTY backend (public API only).
//!
//! Verifies structural guarantees of cleanup sequences and headless backend:
//!
//! 1.  write_cleanup_sequence always includes SYNC_END and CURSOR_SHOW
//! 2.  write_cleanup_sequence deterministic
//! 3.  write_cleanup_sequence ordering: sync_end < cursor_show < alt_screen_leave
//! 4.  write_cleanup_sequence without alt_screen omits ALT_SCREEN_LEAVE
//! 5.  write_cleanup_sequence includes per-feature disable sequences
//! 6.  TtyEventSource headless: size matches constructor
//! 7.  TtyEventSource headless: set_features roundtrip
//! 8.  TtyEventSource headless: poll returns false, read returns None
//! 9.  TtyBackend headless: is_live returns false
//! 10. TtyBackend headless: size matches constructor

use std::time::Duration;

use ftui_backend::{Backend, BackendEventSource, BackendFeatures};
use ftui_tty::{TtyBackend, TtyEventSource, write_cleanup_sequence};
use proptest::prelude::*;

// ── Helpers ──────────────────────────────────────────────────────────

fn arb_features() -> impl Strategy<Value = BackendFeatures> {
    (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(|(m, b, f, k)| {
        BackendFeatures {
            mouse_capture: m,
            bracketed_paste: b,
            focus_events: f,
            kitty_keyboard: k,
        }
    })
}

// Known escape sequences
const SYNC_END: &[u8] = b"\x1b[?2026l";
const CURSOR_SHOW: &[u8] = b"\x1b[?25h";
const ALT_SCREEN_LEAVE: &[u8] = b"\x1b[?1049l";
const MOUSE_DISABLE: &[u8] = b"\x1b[?1000;1002;1006l";
const BRACKETED_PASTE_DISABLE: &[u8] = b"\x1b[?2004l";
const FOCUS_DISABLE: &[u8] = b"\x1b[?1004l";
const KITTY_KEYBOARD_DISABLE: &[u8] = b"\x1b[<u";

fn contains_seq(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn find_seq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ═════════════════════════════════════════════════════════════════════════
// 1. write_cleanup_sequence always includes SYNC_END and CURSOR_SHOW
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cleanup_always_has_sync_and_cursor(
        features in arb_features(),
        alt_screen in any::<bool>(),
    ) {
        let mut buf = Vec::new();
        write_cleanup_sequence(&features, alt_screen, &mut buf).unwrap();
        prop_assert!(contains_seq(&buf, SYNC_END), "must include SYNC_END");
        prop_assert!(contains_seq(&buf, CURSOR_SHOW), "must include CURSOR_SHOW");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. write_cleanup_sequence deterministic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cleanup_deterministic(
        features in arb_features(),
        alt_screen in any::<bool>(),
    ) {
        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();
        write_cleanup_sequence(&features, alt_screen, &mut buf1).unwrap();
        write_cleanup_sequence(&features, alt_screen, &mut buf2).unwrap();
        prop_assert_eq!(buf1, buf2, "cleanup should be deterministic");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. write_cleanup_sequence ordering
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cleanup_ordering(features in arb_features()) {
        let mut buf = Vec::new();
        write_cleanup_sequence(&features, true, &mut buf).unwrap();

        let sync_pos = find_seq(&buf, SYNC_END).expect("sync_end present");
        let cursor_pos = find_seq(&buf, CURSOR_SHOW).expect("cursor_show present");
        let alt_pos = find_seq(&buf, ALT_SCREEN_LEAVE).expect("alt_screen_leave present");

        prop_assert!(
            sync_pos < cursor_pos,
            "sync_end ({}) must precede cursor_show ({})",
            sync_pos, cursor_pos
        );
        prop_assert!(
            cursor_pos < alt_pos,
            "cursor_show ({}) must precede alt_screen_leave ({})",
            cursor_pos, alt_pos
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. write_cleanup_sequence without alt_screen omits ALT_SCREEN_LEAVE
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cleanup_no_alt_omits_leave(features in arb_features()) {
        let mut buf = Vec::new();
        write_cleanup_sequence(&features, false, &mut buf).unwrap();
        prop_assert!(
            !contains_seq(&buf, ALT_SCREEN_LEAVE),
            "should not include alt_screen_leave when alt_screen=false"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. write_cleanup_sequence includes per-feature disable sequences
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cleanup_disables_active_features(features in arb_features()) {
        let mut buf = Vec::new();
        write_cleanup_sequence(&features, false, &mut buf).unwrap();

        if features.mouse_capture {
            prop_assert!(contains_seq(&buf, MOUSE_DISABLE), "should disable mouse");
        } else {
            prop_assert!(!contains_seq(&buf, MOUSE_DISABLE), "should not disable mouse when off");
        }

        if features.bracketed_paste {
            prop_assert!(contains_seq(&buf, BRACKETED_PASTE_DISABLE));
        } else {
            prop_assert!(!contains_seq(&buf, BRACKETED_PASTE_DISABLE));
        }

        if features.focus_events {
            prop_assert!(contains_seq(&buf, FOCUS_DISABLE));
        } else {
            prop_assert!(!contains_seq(&buf, FOCUS_DISABLE));
        }

        if features.kitty_keyboard {
            prop_assert!(contains_seq(&buf, KITTY_KEYBOARD_DISABLE));
        } else {
            prop_assert!(!contains_seq(&buf, KITTY_KEYBOARD_DISABLE));
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. TtyEventSource headless: size matches constructor
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn headless_size_matches(
        width in 1u16..=500,
        height in 1u16..=500,
    ) {
        let src = TtyEventSource::new(width, height);
        let (w, h) = src.size().unwrap();
        prop_assert_eq!(w, width);
        prop_assert_eq!(h, height);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. TtyEventSource headless: set_features roundtrip
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn headless_features_roundtrip(features in arb_features()) {
        let mut src = TtyEventSource::new(80, 24);
        src.set_features(features).unwrap();
        prop_assert_eq!(src.features(), features);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. TtyEventSource headless: poll returns false, read returns None
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn headless_no_events(
        width in 1u16..=200,
        height in 1u16..=200,
    ) {
        let mut src = TtyEventSource::new(width, height);
        let has_event = src.poll_event(Duration::from_millis(0)).unwrap();
        prop_assert!(!has_event, "headless poll should return false");
        let event = src.read_event().unwrap();
        prop_assert!(event.is_none(), "headless read should return None");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. TtyBackend headless: is_live returns false
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn headless_not_live(
        width in 1u16..=200,
        height in 1u16..=200,
    ) {
        let backend = TtyBackend::new(width, height);
        prop_assert!(!backend.is_live());
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. TtyBackend headless: size matches constructor
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn headless_backend_size(
        width in 1u16..=500,
        height in 1u16..=500,
    ) {
        let mut backend = TtyBackend::new(width, height);
        let (w, h) = backend.events().size().unwrap();
        prop_assert_eq!(w, width);
        prop_assert_eq!(h, height);
    }
}

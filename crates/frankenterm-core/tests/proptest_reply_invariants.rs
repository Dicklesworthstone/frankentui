//! Property-based invariant tests for the terminal query/reply engine.
//!
//! Verifies:
//! 1. parse_escape rejects sequences that don't start with ESC [
//! 2. CursorPosition reply always has 1-indexed row/col (>= 1)
//! 3. DECRPM replies always have valid status bytes (0, 1, or 2)
//! 4. parse_escape → reply_for_query produces valid VT sequences (start with ESC [)
//! 5. DeviceStatus reply is always the fixed response
//! 6. DA1 reply is constant regardless of context
//! 7. ExtendedCursorPosition includes the '?' marker
//! 8. Determinism: same query + context → same reply

use frankenterm_core::Modes;

use frankenterm_core::reply::{
    ReplyContext, ReplyEngine, TerminalQuery, parse_terminal_query,
};
use proptest::prelude::*;

// ═════════════════════════════════════════════════════════════════════════
// 1. parse_escape rejects non-CSI sequences
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn parse_rejects_non_csi(bytes in proptest::collection::vec(any::<u8>(), 0..=50)) {
        // If first two bytes aren't ESC [, should return None
        if bytes.len() < 2 || bytes[0] != 0x1b || bytes[1] != b'[' {
            prop_assert_eq!(
                parse_terminal_query(&bytes), None,
                "should reject non-CSI sequence: {:?}",
                &bytes[..bytes.len().min(10)]
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. CursorPosition reply always has 1-indexed coordinates
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cursor_position_reply_one_indexed(row in 0u16..=500, col in 0u16..=500) {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: row,
            cursor_col: col,
            modes: None,
        };
        let reply = engine.reply_for_query(TerminalQuery::CursorPosition, context);
        let reply_str = String::from_utf8_lossy(&reply);

        // Reply format: ESC [ row ; col R
        // row and col should be >= 1
        let inner = reply_str
            .strip_prefix("\x1b[")
            .and_then(|s| s.strip_suffix('R'));
        prop_assert!(inner.is_some(), "unexpected format: {}", reply_str);
        let inner = inner.unwrap();
        let parts: Vec<&str> = inner.split(';').collect();
        prop_assert_eq!(parts.len(), 2, "expected 2 parts in {}", inner);
        let r: u16 = parts[0].parse().unwrap();
        let c: u16 = parts[1].parse().unwrap();
        prop_assert!(r >= 1, "row {} < 1 for input row={}", r, row);
        prop_assert!(c >= 1, "col {} < 1 for input col={}", c, col);
        // Verify exact values
        prop_assert_eq!(r, row.saturating_add(1));
        prop_assert_eq!(c, col.saturating_add(1));
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. ExtendedCursorPosition includes '?' marker
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn extended_cursor_has_question_mark(row in 0u16..=500, col in 0u16..=500) {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: row,
            cursor_col: col,
            modes: None,
        };
        let reply = engine.reply_for_query(TerminalQuery::ExtendedCursorPosition, context);
        let reply_str = String::from_utf8_lossy(&reply);
        prop_assert!(
            reply_str.starts_with("\x1b[?"),
            "DECXCPR should start with ESC[?: {}",
            reply_str
        );
        prop_assert!(reply_str.ends_with('R'), "should end with R: {}", reply_str);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. DeviceStatus reply is always the fixed response
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn device_status_reply_constant(row in any::<u16>(), col in any::<u16>()) {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: row,
            cursor_col: col,
            modes: None,
        };
        let reply = engine.reply_for_query(TerminalQuery::DeviceStatus, context);
        prop_assert_eq!(
            reply, b"\x1b[0n".to_vec(),
            "DeviceStatus reply should always be ESC[0n"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. DA1 reply is constant regardless of context
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn da1_reply_constant(row in any::<u16>(), col in any::<u16>()) {
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: row,
            cursor_col: col,
            modes: None,
        };
        let reply = engine.reply_for_query(TerminalQuery::PrimaryDeviceAttributes, context);
        prop_assert_eq!(
            reply,
            b"\x1b[?64;1;2;4;6;9;15;18;21;22c".to_vec(),
            "DA1 should be constant"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. DA2 reply uses engine's configured values
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn da2_reply_uses_config(term_id in 0u16..=100, version in 0u16..=1000, rom in 0u16..=10) {
        let engine = ReplyEngine {
            da2_terminal_id: term_id,
            da2_version: version,
            da2_rom: rom,
        };
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: None,
        };
        let reply = engine.reply_for_query(TerminalQuery::SecondaryDeviceAttributes, context);
        let expected = format!("\x1b[>{};{};{}c", term_id, version, rom).into_bytes();
        prop_assert_eq!(reply, expected);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. DECRPM status byte is 0, 1, or 2
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn decrpm_status_byte_valid(mode in 0u16..=10000) {
        let engine = ReplyEngine::default();
        let modes = Modes::new();
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: Some(&modes),
        };
        let reply = engine.reply_for_query(
            TerminalQuery::DecModeReport { mode },
            context,
        );
        let reply_str = String::from_utf8_lossy(&reply);
        // Format: ESC [ ? mode ; status $ y
        let inner = reply_str
            .strip_prefix("\x1b[?")
            .and_then(|s| s.strip_suffix("$y"));
        prop_assert!(inner.is_some(), "bad format: {}", reply_str);
        let parts: Vec<&str> = inner.unwrap().split(';').collect();
        prop_assert_eq!(parts.len(), 2);
        let status: u8 = parts[1].parse().unwrap();
        prop_assert!(
            status <= 2,
            "DECRPM status {} > 2 for mode {}",
            status, mode
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. Determinism: same input → same output
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn reply_deterministic(row in 0u16..=500, col in 0u16..=500, mode in 0u16..=10000) {
        let engine = ReplyEngine::default();
        let modes = Modes::new();
        let context = ReplyContext {
            cursor_row: row,
            cursor_col: col,
            modes: Some(&modes),
        };

        // Test each query type for determinism
        let queries = [
            TerminalQuery::DeviceStatus,
            TerminalQuery::CursorPosition,
            TerminalQuery::ExtendedCursorPosition,
            TerminalQuery::PrimaryDeviceAttributes,
            TerminalQuery::SecondaryDeviceAttributes,
            TerminalQuery::DecModeReport { mode },
        ];

        for query in queries {
            let r1 = engine.reply_for_query(query, context);
            let r2 = engine.reply_for_query(query, context);
            prop_assert_eq!(r1, r2, "non-deterministic reply for {:?}", query);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. All replies start with ESC [
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn all_replies_valid_vt(row in 0u16..=500, col in 0u16..=500) {
        let engine = ReplyEngine::default();
        let modes = Modes::new();
        let context = ReplyContext {
            cursor_row: row,
            cursor_col: col,
            modes: Some(&modes),
        };
        let queries = [
            TerminalQuery::DeviceStatus,
            TerminalQuery::CursorPosition,
            TerminalQuery::ExtendedCursorPosition,
            TerminalQuery::PrimaryDeviceAttributes,
            TerminalQuery::SecondaryDeviceAttributes,
            TerminalQuery::DecModeReport { mode: 7 },
        ];
        for query in queries {
            let reply = engine.reply_for_query(query, context);
            prop_assert!(
                reply.len() >= 3 && reply[0] == 0x1b && reply[1] == b'[',
                "reply for {:?} doesn't start with ESC[: {:?}",
                query, &reply[..reply.len().min(10)]
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. parse roundtrip: known query bytes → parse → reply → valid
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn known_queries_roundtrip() {
    let queries = [
        (b"\x1b[5n".as_slice(), TerminalQuery::DeviceStatus),
        (b"\x1b[6n".as_slice(), TerminalQuery::CursorPosition),
        (
            b"\x1b[?6n".as_slice(),
            TerminalQuery::ExtendedCursorPosition,
        ),
        (b"\x1b[c".as_slice(), TerminalQuery::PrimaryDeviceAttributes),
        (
            b"\x1b[0c".as_slice(),
            TerminalQuery::PrimaryDeviceAttributes,
        ),
        (
            b"\x1b[>c".as_slice(),
            TerminalQuery::SecondaryDeviceAttributes,
        ),
        (
            b"\x1b[>0c".as_slice(),
            TerminalQuery::SecondaryDeviceAttributes,
        ),
    ];

    for (bytes, expected_query) in queries {
        let parsed = parse_terminal_query(bytes);
        assert_eq!(
            parsed,
            Some(expected_query),
            "failed to parse {:?}",
            String::from_utf8_lossy(bytes)
        );

        // Verify the reply is non-empty and valid VT
        let engine = ReplyEngine::default();
        let context = ReplyContext {
            cursor_row: 0,
            cursor_col: 0,
            modes: None,
        };
        let reply = engine.reply_for_query(expected_query, context);
        assert!(reply.len() >= 3, "reply too short for {:?}", expected_query);
        assert_eq!(reply[0], 0x1b, "reply missing ESC for {:?}", expected_query);
        assert_eq!(reply[1], b'[', "reply missing [ for {:?}", expected_query);
    }
}

#![forbid(unsafe_code)]

//! Multiplexer passthrough wrappers for escape sequences.
//!
//! Terminal multiplexers (tmux, GNU screen, Zellij) intercept escape sequences.
//! Some features like OSC 8 hyperlinks and synchronized output require
//! passthrough wrapping so the inner terminal receives them.
//!
//! # tmux Passthrough
//!
//! tmux uses DCS (Device Control String) passthrough:
//! ```text
//! ESC P tmux; <escaped-sequence> ESC \
//! ```
//! All ESC bytes inside the sequence must be doubled (`ESC ESC`).
//!
//! # GNU screen Passthrough
//!
//! screen uses a similar DCS passthrough, split over as many DCS strings as
//! it takes to keep each one short and free of `ESC \`:
//! ```text
//! ESC P <chunk> ESC \ ESC P <chunk> ESC \ ...
//! ```
//!
//! # Zellij
//!
//! Zellij (0.39+) generally has better passthrough than tmux/screen
//! and doesn't require explicit wrapping for most sequences.

use std::io::{self, Write};

use crate::terminal_capabilities::TerminalCapabilities;

/// Escape byte (0x1B).
const ESC: u8 = 0x1b;

/// String Terminator: ESC \ (used to close DCS sequences).
const ST: &[u8] = b"\x1b\\";

/// Write a sequence wrapped in tmux DCS passthrough.
///
/// tmux intercepts most escape sequences. To pass them through to the
/// inner terminal, they must be wrapped in a DCS passthrough block:
///
/// ```text
/// ESC P tmux; <sequence-with-doubled-escapes> ESC \
/// ```
///
/// All ESC (0x1B) bytes within the sequence are doubled so tmux
/// doesn't interpret them as its own escape sequences.
pub fn tmux_wrap<W: Write>(w: &mut W, sequence: &[u8]) -> io::Result<()> {
    // DCS passthrough header: ESC P tmux;
    w.write_all(b"\x1bPtmux;")?;

    // Write sequence with doubled escapes
    for &byte in sequence {
        if byte == ESC {
            w.write_all(&[ESC, ESC])?;
        } else {
            w.write_all(&[byte])?;
        }
    }

    // String Terminator: ESC \
    w.write_all(ST)
}

/// Longest DCS string GNU screen forwards.
///
/// screen 4.00 (the one macOS ships) buffers a DCS string in 256 bytes. A
/// 255-byte string is forwarded; at 256 it forwards nothing and prints the
/// rest of the string into the window as text.
const SCREEN_DCS_MAX: usize = 255;

/// Write a sequence wrapped in GNU screen DCS passthrough.
///
/// screen uses a simpler DCS passthrough:
///
/// ```text
/// ESC P <sequence> ESC \
/// ```
///
/// Unlike tmux, screen does not require doubling of ESC bytes
/// within the passthrough block. It forwards the contents of consecutive
/// DCS strings back to back, so the sequence goes out in pieces:
///
/// - at most 255 bytes each, since screen drops a longer string. An OSC 52
///   clipboard write of more than 183 bytes is longer.
/// - cut between the two bytes of any `ESC \` inside the sequence, which
///   would otherwise end its DCS string early and leave, say, an
///   ST-terminated OSC 8 link open on the outer terminal. screen keeps an
///   ESC that ends a string, so the terminal still receives `ESC \`.
pub fn screen_wrap<W: Write>(w: &mut W, sequence: &[u8]) -> io::Result<()> {
    let mut rest = sequence;
    loop {
        let mut len = rest.len().min(SCREEN_DCS_MAX);
        if let Some(esc) = rest[..len].windows(2).position(|pair| pair == ST) {
            len = esc + 1;
        }
        w.write_all(b"\x1bP")?;
        w.write_all(&rest[..len])?;
        w.write_all(ST)?;
        rest = &rest[len..];
        if rest.is_empty() {
            return Ok(());
        }
    }
}

/// Write a sequence with appropriate mux passthrough wrapping.
///
/// Selects the correct passthrough wrapper based on detected capabilities:
/// - In tmux: uses [`tmux_wrap`]
/// - In GNU screen: uses [`screen_wrap`]
/// - In Zellij or no mux: writes directly (no wrapping needed)
pub fn mux_wrap<W: Write>(
    w: &mut W,
    caps: &TerminalCapabilities,
    sequence: &[u8],
) -> io::Result<()> {
    if caps.in_tmux {
        tmux_wrap(w, sequence)
    } else if caps.in_screen {
        screen_wrap(w, sequence)
    } else {
        // Zellij and bare terminals don't need wrapping
        w.write_all(sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_bytes<F: FnOnce(&mut Vec<u8>) -> io::Result<()>>(f: F) -> Vec<u8> {
        let mut buf = Vec::new();
        f(&mut buf).unwrap();
        buf
    }

    #[test]
    fn tmux_wrap_doubles_escapes() {
        // OSC 8 hyperlink: ESC ] 8 ; ; url BEL
        let osc8 = b"\x1b]8;;https://example.com\x07";
        let wrapped = to_bytes(|w| tmux_wrap(w, osc8));

        // Should start with DCS header
        assert!(wrapped.starts_with(b"\x1bPtmux;"));

        // Should end with ST
        assert!(wrapped.ends_with(b"\x1b\\"));

        // Original ESC bytes should be doubled
        // The sequence has 2 ESC bytes (one for OSC start, one for ST)
        // Each should become ESC ESC
        let inner = &wrapped[7..wrapped.len() - 2]; // strip header and ST
        let esc_count = inner.windows(2).filter(|w| w == &[ESC, ESC]).count();
        assert_eq!(
            esc_count, 1,
            "Only the ESC from OSC start should be doubled"
        );
    }

    #[test]
    fn tmux_wrap_no_escape_passthrough() {
        // Plain text (no ESC bytes)
        let plain = b"hello world";
        let wrapped = to_bytes(|w| tmux_wrap(w, plain));

        assert_eq!(wrapped, b"\x1bPtmux;hello world\x1b\\");
    }

    #[test]
    fn tmux_wrap_empty_sequence() {
        let wrapped = to_bytes(|w| tmux_wrap(w, b""));
        assert_eq!(wrapped, b"\x1bPtmux;\x1b\\");
    }

    #[test]
    fn screen_wrap_basic() {
        let seq = b"\x1b]8;;https://example.com\x07";
        let wrapped = to_bytes(|w| screen_wrap(w, seq));

        // Should start with DCS header
        assert!(wrapped.starts_with(b"\x1bP"));

        // Should end with ST
        assert!(wrapped.ends_with(b"\x1b\\"));

        // Should contain original sequence unmodified between header and ST
        let inner = &wrapped[2..wrapped.len() - 2];
        assert_eq!(inner, seq);
    }

    #[test]
    fn screen_wrap_does_not_double_escapes() {
        let seq = b"\x1b[?2026h"; // sync output begin
        let wrapped = to_bytes(|w| screen_wrap(w, seq));

        // ESC should NOT be doubled (unlike tmux)
        assert_eq!(wrapped, b"\x1bP\x1b[?2026h\x1b\\");
    }

    #[test]
    fn mux_wrap_selects_tmux() {
        let mut caps = TerminalCapabilities::basic();
        caps.in_tmux = true;

        let seq = b"\x1b[?2026h";
        let result = to_bytes(|w| mux_wrap(w, &caps, seq));

        // Should use tmux wrapping
        assert!(result.starts_with(b"\x1bPtmux;"));
    }

    #[test]
    fn mux_wrap_selects_screen() {
        let mut caps = TerminalCapabilities::basic();
        caps.in_screen = true;

        let seq = b"\x1b[?2026h";
        let result = to_bytes(|w| mux_wrap(w, &caps, seq));

        // Should use screen wrapping
        assert!(result.starts_with(b"\x1bP"));
        assert!(!result.starts_with(b"\x1bPtmux;")); // Not tmux format
    }

    #[test]
    fn mux_wrap_passthrough_for_zellij() {
        let mut caps = TerminalCapabilities::basic();
        caps.in_zellij = true;

        let seq = b"\x1b[?2026h";
        let result = to_bytes(|w| mux_wrap(w, &caps, seq));

        // Should write directly (no wrapping)
        assert_eq!(result, seq);
    }

    #[test]
    fn mux_wrap_passthrough_for_bare_terminal() {
        let caps = TerminalCapabilities::basic();

        let seq = b"\x1b[?2026h";
        let result = to_bytes(|w| mux_wrap(w, &caps, seq));

        // Should write directly
        assert_eq!(result, seq);
    }

    #[test]
    fn tmux_priority_over_screen() {
        // If both tmux and screen are detected, tmux takes priority
        let mut caps = TerminalCapabilities::basic();
        caps.in_tmux = true;
        caps.in_screen = true;

        let seq = b"test";
        let result = to_bytes(|w| mux_wrap(w, &caps, seq));

        assert!(result.starts_with(b"\x1bPtmux;"));
    }

    // --- tmux_wrap: additional escape handling ---

    #[test]
    fn tmux_wrap_multiple_escapes() {
        // Sequence with 3 ESC bytes
        let seq = &[ESC, b'[', b'm', ESC, b']', b'0', ESC, b'\\'];
        let wrapped = to_bytes(|w| tmux_wrap(w, seq));
        let inner = &wrapped[7..wrapped.len() - 2];
        // Each ESC should be doubled → 6 ESC bytes in inner
        let esc_count = inner.iter().filter(|&&b| b == ESC).count();
        assert_eq!(esc_count, 6, "3 ESC bytes should become 6 (doubled)");
    }

    #[test]
    fn tmux_wrap_all_escape_bytes() {
        // Every byte is ESC
        let seq = &[ESC, ESC, ESC];
        let wrapped = to_bytes(|w| tmux_wrap(w, seq));
        let inner = &wrapped[7..wrapped.len() - 2];
        // 3 ESCs → 6 ESC bytes
        assert_eq!(inner.len(), 6);
        assert!(inner.iter().all(|&b| b == ESC));
    }

    #[test]
    fn tmux_wrap_preserves_non_escape_bytes() {
        let seq = b"ABCDEF";
        let wrapped = to_bytes(|w| tmux_wrap(w, seq));
        assert_eq!(wrapped, b"\x1bPtmux;ABCDEF\x1b\\");
    }

    #[test]
    fn tmux_wrap_binary_data() {
        // Sequence with all byte values 0-255 except ESC
        let seq: Vec<u8> = (0u8..=255).filter(|&b| b != ESC).collect();
        let wrapped = to_bytes(|w| tmux_wrap(w, &seq));
        // Should contain all those bytes in the inner portion
        let inner = &wrapped[7..wrapped.len() - 2];
        assert_eq!(inner.len(), seq.len());
    }

    // --- screen_wrap: additional tests ---

    #[test]
    fn screen_wrap_empty_sequence() {
        let wrapped = to_bytes(|w| screen_wrap(w, b""));
        assert_eq!(wrapped, b"\x1bP\x1b\\");
    }

    #[test]
    fn screen_wrap_preserves_all_bytes() {
        // Even ESC bytes pass through unmodified
        let seq = &[ESC, ESC, 0x00, 0xFF];
        let wrapped = to_bytes(|w| screen_wrap(w, seq));
        let inner = &wrapped[2..wrapped.len() - 2];
        assert_eq!(inner, seq);
    }

    // --- mux_wrap: priority and combination tests ---

    #[test]
    fn mux_wrap_tmux_priority_over_zellij() {
        let mut caps = TerminalCapabilities::basic();
        caps.in_tmux = true;
        caps.in_zellij = true;
        let result = to_bytes(|w| mux_wrap(w, &caps, b"x"));
        assert!(result.starts_with(b"\x1bPtmux;"));
    }

    #[test]
    fn mux_wrap_screen_priority_over_zellij() {
        let mut caps = TerminalCapabilities::basic();
        caps.in_screen = true;
        caps.in_zellij = true;
        let result = to_bytes(|w| mux_wrap(w, &caps, b"x"));
        assert!(result.starts_with(b"\x1bP"));
        assert!(!result.starts_with(b"\x1bPtmux;"));
    }

    // --- Constants ---

    #[test]
    fn esc_constant_value() {
        assert_eq!(ESC, 0x1b);
    }

    #[test]
    fn st_constant_value() {
        assert_eq!(ST, b"\x1b\\");
    }

    // --- Large sequence ---

    #[test]
    fn tmux_wrap_large_sequence() {
        let seq = vec![b'A'; 10_000];
        let wrapped = to_bytes(|w| tmux_wrap(w, &seq));
        // header (7) + data (10000) + ST (2) = 10009
        assert_eq!(wrapped.len(), 10_009);
    }

    /// The contents of each DCS string `screen_wrap` wrote, in order.
    fn screen_strings(wrapped: &[u8]) -> Vec<&[u8]> {
        let mut strings = Vec::new();
        let mut rest = wrapped;
        while !rest.is_empty() {
            let body = rest.strip_prefix(b"\x1bP").expect("opens with ESC P");
            let end = body
                .windows(2)
                .position(|pair| pair == ST)
                .expect("closes with ST");
            strings.push(&body[..end]);
            rest = &body[end + ST.len()..];
        }
        strings
    }

    #[test]
    fn screen_wrap_splits_a_largest_clipboard_write_into_strings_screen_forwards() {
        // One DCS string of 75,000 bytes: screen forwarded none of it and
        // printed the base64 into the window.
        let seq = crate::osc52::encode_set(
            crate::osc52::ClipboardSelection::Clipboard,
            &vec![0; 56_244],
        )
        .unwrap();
        let wrapped = to_bytes(|w| screen_wrap(w, &seq));
        let strings = screen_strings(&wrapped);
        assert!(strings.iter().all(|s| s.len() <= SCREEN_DCS_MAX));
        assert_eq!(strings.concat(), seq);
    }

    #[test]
    fn screen_wrap_cuts_an_st_inside_the_sequence_between_its_bytes() {
        // Left whole, the link's ST closed the DCS string and the outer
        // terminal got an OSC 8 with no terminator.
        let seq = b"\x1b]8;;https://example.com\x1b\\";
        let wrapped = to_bytes(|w| screen_wrap(w, seq));
        assert_eq!(
            screen_strings(&wrapped),
            [&b"\x1b]8;;https://example.com\x1b"[..], b"\\"]
        );
    }
}

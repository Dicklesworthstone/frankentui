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
//! screen uses a similar DCS passthrough:
//! ```text
//! ESC P <sequence> ESC \
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

/// Write a sequence wrapped in GNU screen DCS passthrough.
///
/// screen uses a simpler DCS passthrough:
///
/// ```text
/// ESC P <sequence> ESC \
/// ```
///
/// Unlike tmux, screen does not require doubling of ESC bytes
/// within the passthrough block.
pub fn screen_wrap<W: Write>(w: &mut W, sequence: &[u8]) -> io::Result<()> {
    // DCS passthrough header: ESC P
    w.write_all(b"\x1bP")?;

    // Write sequence as-is
    w.write_all(sequence)?;

    // String Terminator: ESC \
    w.write_all(ST)
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
        // OSC 8 hyperlink: ESC ] 8 ; ; url ESC \
        let osc8 = b"\x1b]8;;https://example.com\x1b\\";
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
        assert_eq!(esc_count, 2, "Both ESC bytes should be doubled");
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
        let seq = b"\x1b]8;;https://example.com\x1b\\";
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
}

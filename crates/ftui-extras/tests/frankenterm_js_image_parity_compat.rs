//! Addon-parity compatibility harness — inline-image arm (bd-2vr05.14.6).
//!
//! Drives the **production** inline-image protocol detection
//! (`ftui_extras::image`, the in-tree home of bd-2vr05.14.3) and asserts the
//! deterministic protocol-priority parity an embedder relies on when choosing
//! between the Kitty graphics, iTerm2 inline, Sixel, and ASCII-fallback paths —
//! the same capability-priority ladder the xterm.js image addon walks.
//!
//! Each cell prints one structured `FTUI_ADDON_PARITY_COMPAT ...` JSONL line so
//! the aggregator (`scripts/frankenterm_js_addon_parity_compat.sh`) folds it
//! into the compatibility manifest. Determinism: hints are constructed
//! explicitly (never read from the environment), so detection is reproducible.
//!
//! Requires the `image` feature (the inline-image module + decoders).
#![cfg(feature = "image")]
#![forbid(unsafe_code)]

use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_extras::image::{DetectionHints, ImageProtocol, ProtocolCache, detect_protocol};

fn protocol_token(p: ImageProtocol) -> &'static str {
    match p {
        ImageProtocol::Kitty => "kitty",
        ImageProtocol::Iterm2 => "iterm2",
        ImageProtocol::Sixel => "sixel",
        ImageProtocol::Ascii => "ascii",
    }
}

fn emit(scenario: &str, case: &str, correlation_id: &str, passed: bool, extra: &str) {
    println!(
        "FTUI_ADDON_PARITY_COMPAT {{\"subsystem\":\"image\",\"scenario\":\"{scenario}\",\
\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\"passed\":{passed},{extra}}}"
    );
}

/// Each explicitly-advertised protocol resolves to itself when it is the only
/// one available.
#[test]
fn single_capability_resolves_to_that_protocol() {
    let caps = TerminalCapabilities::basic();
    let cases = [
        (
            DetectionHints::default().with_kitty_graphics(true),
            ImageProtocol::Kitty,
        ),
        (
            DetectionHints::default().with_iterm2_inline(true),
            ImageProtocol::Iterm2,
        ),
        (
            DetectionHints::default().with_sixel(true),
            ImageProtocol::Sixel,
        ),
    ];
    for (hints, expected) in cases {
        let got = detect_protocol(caps, &hints);
        assert_eq!(got, expected, "single-capability detection");
        emit(
            "single_capability",
            protocol_token(expected),
            &format!("img-single-{}", protocol_token(expected)),
            got == expected,
            &format!("\"protocol\":\"{}\"", protocol_token(got)),
        );
    }
}

/// With no advertised capability, detection falls back to ASCII rather than
/// guessing — the safe, always-renderable default.
#[test]
fn no_capability_falls_back_to_ascii() {
    let caps = TerminalCapabilities::basic();
    let hints = DetectionHints::default()
        .with_kitty_graphics(false)
        .with_iterm2_inline(false)
        .with_sixel(false);
    let got = detect_protocol(caps, &hints);
    assert_eq!(got, ImageProtocol::Ascii, "fallback must be ASCII");
    emit(
        "fallback",
        "no_capability",
        "img-fallback",
        got == ImageProtocol::Ascii,
        "\"protocol\":\"ascii\"",
    );
}

/// Protocol priority is deterministic: when several are advertised, Kitty wins
/// over iTerm2 wins over Sixel (the highest-fidelity available path).
#[test]
fn priority_ladder_is_deterministic() {
    let caps = TerminalCapabilities::basic();

    let all = DetectionHints::default()
        .with_kitty_graphics(true)
        .with_iterm2_inline(true)
        .with_sixel(true);
    assert_eq!(
        detect_protocol(caps, &all),
        ImageProtocol::Kitty,
        "kitty wins"
    );

    let iterm_and_sixel = DetectionHints::default()
        .with_kitty_graphics(false)
        .with_iterm2_inline(true)
        .with_sixel(true);
    assert_eq!(
        detect_protocol(caps, &iterm_and_sixel),
        ImageProtocol::Iterm2,
        "iterm2 beats sixel"
    );

    emit(
        "priority_ladder",
        "kitty_gt_iterm2_gt_sixel",
        "img-priority",
        true,
        "\"order\":\"kitty>iterm2>sixel>ascii\"",
    );
}

/// The cache returns the same protocol as the stateless detector, and repeated
/// detection is stable (no flapping).
#[test]
fn protocol_cache_matches_and_is_stable() {
    let caps = TerminalCapabilities::basic();
    let hints = DetectionHints::default().with_iterm2_inline(true);
    let mut cache = ProtocolCache::default();

    let first = cache.detect(caps, &hints);
    let second = cache.detect(caps, &hints);
    assert_eq!(first, second, "cache is stable");
    assert_eq!(
        first,
        detect_protocol(caps, &hints),
        "cache matches detector"
    );

    emit(
        "cache_stability",
        "iterm2_repeated",
        "img-cache",
        first == second,
        &format!("\"protocol\":\"{}\"", protocol_token(first)),
    );
}

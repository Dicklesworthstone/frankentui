//! Adversarial security/reliability compatibility harness — clipboard-policy
//! arm (bd-2vr05.11.6).
//!
//! This is the clipboard-policy arm of the cross-subsystem security/reliability
//! harness. It drives the **production** OSC-52 clipboard helper
//! (`ftui_extras::clipboard::Clipboard`) — the host-managed clipboard gate — to
//! prove its bounded-payload policy: a clipboard write whose base64 payload
//! exceeds the cap is rejected (no bytes emitted), the cap is host-tunable
//! downward per deployment policy, and a terminal that never advertised OSC-52
//! support gets no clipboard sequence at all.
//!
//! Every cell prints one structured `FTUI_SECURITY_RELIABILITY_COMPAT ...` JSONL
//! line so the aggregator (`scripts/frankenterm_js_security_reliability_compat.sh`)
//! folds it into the incident-grade compatibility manifest alongside the
//! flow-control and link-policy arms.
//!
//! Requires the `clipboard` feature (the OSC-52 clipboard module).
#![cfg(feature = "clipboard")]
#![forbid(unsafe_code)]

use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_extras::clipboard::{Clipboard, ClipboardError, ClipboardSelection};

const PREFIX: &str = "FTUI_SECURITY_RELIABILITY_COMPAT";

/// Attempt a clipboard write into a `Vec<u8>` sink, returning the result and the
/// number of bytes that reached the (simulated) terminal.
fn try_set(clip: &Clipboard, content: &str) -> (Result<(), ClipboardError>, usize) {
    let mut buf = Vec::new();
    let result = clip.set(content, ClipboardSelection::Clipboard, &mut buf);
    (result, buf.len())
}

fn emit(
    scenario: &str,
    case: &str,
    correlation_id: &str,
    passed: bool,
    failure_injection: bool,
    extra: &str,
) {
    println!(
        "{PREFIX} {{\"subsystem\":\"clipboard_policy\",\"scenario\":\"{scenario}\",\
\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\"passed\":{passed},\
\"failure_injection\":{failure_injection},{extra}}}"
    );
}

/// A small clipboard write on a clipboard-capable terminal succeeds and emits a
/// well-formed OSC-52 sequence.
#[test]
fn small_payload_is_written() {
    let clip = Clipboard::new(TerminalCapabilities::modern());
    let (result, bytes) = try_set(&clip, "hello clipboard");
    let passed = result.is_ok() && bytes > 0;
    assert!(
        passed,
        "a small payload must be written on a clipboard-capable terminal"
    );
    emit(
        "write_allowed",
        "small_payload",
        "clip-small",
        passed,
        false,
        &format!("\"bytes_emitted\":{bytes}"),
    );
}

/// The bound is exact: a payload whose base64 length equals the cap is written;
/// one base64 chunk over the cap is rejected with `InvalidInput` and emits
/// nothing.
#[test]
fn payload_cap_boundary_is_exact() {
    // base64 length = 4 * ceil(n / 3). With cap = 8: 6 bytes -> 8 (allowed),
    // 9 bytes -> 12 (rejected).
    let clip = Clipboard::with_max_payload(TerminalCapabilities::modern(), 8);

    let (at_cap, at_bytes) = try_set(&clip, "abcdef"); // base64 len 8
    let (over_cap, over_bytes) = try_set(&clip, "abcdefghi"); // base64 len 12

    let passed = at_cap.is_ok()
        && at_bytes > 0
        && matches!(over_cap, Err(ClipboardError::InvalidInput(_)))
        && over_bytes == 0;
    assert!(
        passed,
        "the payload cap must admit exactly up to the limit and no further"
    );
    emit(
        "payload_cap",
        "exact_boundary",
        "clip-boundary",
        passed,
        false,
        &format!(
            "\"cap\":8,\"at_cap_bytes\":{at_bytes},\"over_cap_bytes\":{over_bytes},\
\"over_rejected\":{}",
            over_cap.is_err()
        ),
    );
}

/// Hosts can tighten the cap below the default per deployment policy; a payload
/// the default would accept is then refused.
#[test]
fn host_can_tighten_cap() {
    let content = "x".repeat(256);

    let default_clip = Clipboard::new(TerminalCapabilities::modern());
    let (default_result, default_bytes) = try_set(&default_clip, &content);

    let tight_clip = Clipboard::with_max_payload(TerminalCapabilities::modern(), 16);
    let (tight_result, tight_bytes) = try_set(&tight_clip, &content);

    let passed =
        default_result.is_ok() && default_bytes > 0 && tight_result.is_err() && tight_bytes == 0;
    assert!(
        passed,
        "a tightened host cap must refuse a payload the default allows"
    );
    emit(
        "host_policy",
        "tighten_below_default",
        "clip-tighten",
        passed,
        false,
        &format!(
            "\"default_cap\":{},\"tightened_cap\":16,\"default_bytes\":{default_bytes},\
\"tightened_bytes\":{tight_bytes}",
            Clipboard::DEFAULT_MAX_OSC52_PAYLOAD
        ),
    );
}

/// A terminal that never advertised OSC-52 support gets no clipboard sequence —
/// the helper does not silently emit to an unsupporting terminal.
#[test]
fn unsupported_terminal_emits_nothing() {
    let clip = Clipboard::new(TerminalCapabilities::basic());
    let (result, bytes) = try_set(&clip, "hello");
    let passed = matches!(result, Err(ClipboardError::NotAvailable)) && bytes == 0;
    assert!(
        passed,
        "no OSC-52 must be emitted to a terminal lacking clipboard support"
    );
    emit(
        "capability_gate",
        "unsupported_terminal",
        "clip-unsupported",
        passed,
        false,
        &format!(
            "\"bytes_emitted\":{bytes},\"available\":{}",
            clip.is_available()
        ),
    );
}

/// FAILURE INJECTION: a hostile actor attempts to exfiltrate / inject a very
/// large payload through the clipboard channel, far exceeding the documented
/// default OSC-52 bound. The bounded-payload policy must reject it outright with
/// zero bytes emitted — the channel cannot be abused as an unbounded conduit.
#[test]
fn oversized_payload_exfil_is_blocked() {
    let clip = Clipboard::new(TerminalCapabilities::modern());
    // base64 length = 4 * ceil(n / 3); 60_000 bytes -> 80_000 base64 chars,
    // comfortably over the 74_994-byte default cap.
    let hostile = "A".repeat(60_000);
    let (result, bytes) = try_set(&clip, &hostile);

    let blocked = matches!(result, Err(ClipboardError::InvalidInput(_))) && bytes == 0;
    assert!(
        blocked,
        "an oversized clipboard payload must be rejected with no emission"
    );
    emit(
        "oversized_exfil",
        "payload_over_default_cap",
        "clip-exfil",
        blocked,
        true,
        &format!(
            "\"content_bytes\":{},\"default_cap\":{},\"bytes_emitted\":{bytes}",
            hostile.len(),
            Clipboard::DEFAULT_MAX_OSC52_PAYLOAD
        ),
    );
}

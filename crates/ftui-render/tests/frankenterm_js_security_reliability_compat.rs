//! Adversarial security/reliability compatibility harness — link-policy arm
//! (bd-2vr05.11.6).
//!
//! This is the link-policy arm of the cross-subsystem security/reliability
//! harness. It drives the **production** OSC-8 hyperlink emitter
//! (`ftui_render::ansi::{hyperlink_start, hyperlink_start_with_id}`) — the gate
//! every rendered link passes through — with hostile URLs and IDs that try to
//! break out of the OSC-8 string and inject arbitrary terminal control
//! sequences (the classic "OSC-8 escape breakout" attack). A safe HTTPS link
//! must render to a well-formed sequence; any field carrying a control byte,
//! an embedded string terminator, or an over-length payload must render to
//! nothing at all.
//!
//! Every cell prints one structured `FTUI_SECURITY_RELIABILITY_COMPAT ...` JSONL
//! line so the aggregator (`scripts/frankenterm_js_security_reliability_compat.sh`)
//! folds it into the incident-grade compatibility manifest alongside the
//! flow-control and clipboard arms.
//!
//! Determinism: inputs are fixed literals and rendering is pure (a `Vec<u8>`
//! sink), so the emitted-byte counts are reproducible.
#![forbid(unsafe_code)]

use ftui_render::ansi::{hyperlink_end, hyperlink_start, hyperlink_start_with_id};

const PREFIX: &str = "FTUI_SECURITY_RELIABILITY_COMPAT";

fn render_link(url: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    hyperlink_start(&mut buf, url).expect("writing to a Vec is infallible");
    buf
}

fn render_link_with_id(id: &str, url: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    hyperlink_start_with_id(&mut buf, id, url).expect("writing to a Vec is infallible");
    buf
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
        "{PREFIX} {{\"subsystem\":\"link_policy\",\"scenario\":\"{scenario}\",\
\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\"passed\":{passed},\
\"failure_injection\":{failure_injection},{extra}}}"
    );
}

/// A safe HTTPS link renders to exactly the documented OSC-8 sequence.
#[test]
fn safe_https_link_renders_sequence() {
    let url = "https://example.com/path?q=1";
    let out = render_link(url);
    let expected = format!("\x1b]8;;{url}\x07").into_bytes();
    let passed = out == expected && !out.is_empty();
    assert!(
        passed,
        "a safe HTTPS link must render a well-formed OSC-8 sequence"
    );
    emit(
        "safe_link",
        "https_url",
        "link-safe-https",
        passed,
        false,
        &format!("\"bytes_emitted\":{}", out.len()),
    );
}

/// Control bytes in the URL field (BEL, ESC, newline, carriage return, NUL)
/// each abort emission entirely — none can smuggle a sequence into the stream.
#[test]
fn control_bytes_in_url_are_sanitized() {
    let hostile = [
        ("bel", "https://x\u{07}evil"),
        ("esc", "https://x\u{1b}]0;pwned"),
        ("newline", "https://x\ninjected"),
        ("carriage_return", "https://x\rinjected"),
        ("nul", "https://x\u{00}evil"),
        ("tab", "https://x\tinjected"),
    ];
    let mut all_blocked = true;
    for (case, url) in hostile {
        let out = render_link(url);
        let blocked = out.is_empty();
        all_blocked &= blocked;
        emit(
            "control_byte_sanitization",
            case,
            &format!("link-ctrl-{case}"),
            blocked,
            false,
            &format!("\"bytes_emitted\":{}", out.len()),
        );
    }
    assert!(
        all_blocked,
        "no control byte may survive into an OSC-8 URL field"
    );
}

/// An over-length URL (past the 4 KiB field bound) is rejected, so a hostile
/// link cannot be used as an unbounded write amplifier.
#[test]
fn oversized_url_is_rejected() {
    let url = format!("https://example.com/{}", "a".repeat(5_000));
    let out = render_link(&url);
    let passed = out.is_empty();
    assert!(passed, "an over-length URL must not be emitted");
    emit(
        "bounded_field",
        "url_over_4kib",
        "link-oversized",
        passed,
        false,
        &format!("\"url_len\":{},\"bytes_emitted\":{}", url.len(), out.len()),
    );
}

/// A hostile `id` field (control byte, or the `;` parameter separator) aborts
/// emission, blocking parameter-injection into the OSC-8 params section.
#[test]
fn hostile_id_field_is_rejected() {
    let url = "https://example.com";
    let safe = render_link_with_id("group-1", url);
    let safe_expected = format!("\x1b]8;id=group-1;{url}\x07").into_bytes();
    let safe_ok = safe == safe_expected;

    let semicolon = render_link_with_id("id;injected=1", url);
    let esc_id = render_link_with_id("id\u{1b}evil", url);
    let blocked = semicolon.is_empty() && esc_id.is_empty();

    let passed = safe_ok && blocked;
    assert!(
        passed,
        "id field must reject ';' and control bytes while passing a clean id"
    );
    emit(
        "id_param_injection",
        "semicolon_and_control",
        "link-id-injection",
        passed,
        false,
        &format!(
            "\"safe_bytes\":{},\"semicolon_bytes\":{},\"esc_id_bytes\":{}",
            safe.len(),
            semicolon.len(),
            esc_id.len()
        ),
    );
}

/// FAILURE INJECTION: a crafted URL tries to close the OSC-8 string early with
/// a BEL terminator and then inject a raw OSC sequence to rewrite the window
/// title (`\x07\x1b]0;pwned\x07`). The sanitizer must drop the entire field —
/// nothing reaches the terminal.
#[test]
fn osc8_breakout_injection_is_blocked() {
    let payload = "https://example.com\u{07}\u{1b}]0;pwned\u{07}";
    let out = render_link(payload);

    // Defense in depth: not only must the buffer be empty, the dangerous title
    // sequence must not appear anywhere in any rendered bytes.
    let no_title_injection = !out.windows(4).any(|w| w == b"]0;p");
    let blocked = out.is_empty() && no_title_injection;
    assert!(
        blocked,
        "OSC-8 breakout into a title sequence must be impossible"
    );
    emit(
        "breakout_injection",
        "title_rewrite_via_bel",
        "link-breakout",
        blocked,
        true,
        &format!(
            "\"bytes_emitted\":{},\"title_injection\":{}",
            out.len(),
            !no_title_injection
        ),
    );
}

/// The link terminator is a fixed, parameter-free sequence (no attacker-
/// controlled bytes), so closing a link can never carry an injection.
#[test]
fn link_terminator_is_constant() {
    let mut buf = Vec::new();
    hyperlink_end(&mut buf).expect("writing to a Vec is infallible");
    let passed = buf == b"\x1b]8;;\x07";
    assert!(passed, "the OSC-8 terminator must be a constant sequence");
    emit(
        "constant_terminator",
        "hyperlink_end",
        "link-terminator",
        passed,
        false,
        &format!("\"bytes_emitted\":{}", buf.len()),
    );
}

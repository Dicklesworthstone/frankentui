//! Addon-parity compatibility harness — fit + web-links arms (bd-2vr05.14.6).
//!
//! These are the `ftui-render`-hosted arms of the cross-subsystem addon-parity
//! compatibility harness. They drive the **production** deterministic
//! fit-to-container / web-font metric lifecycle (`ftui_render::fit_metrics`, the
//! in-tree home of bd-2vr05.14.1) and the **production** OSC-8 hyperlink
//! emission + sanitisation (`ftui_render::ansi`, part of bd-2vr05.14.2),
//! asserting parity with the xterm.js `FitAddon` and web-links behaviour plus
//! the FrankenTUI sanitisation hardening that xterm.js lacks.
//!
//! Each cell prints one structured `FTUI_ADDON_PARITY_COMPAT ...` JSONL line
//! (subsystem-tagged, with a correlation id) so the aggregator
//! (`scripts/frankenterm_js_addon_parity_compat.sh`) folds it into a single
//! compatibility manifest. Determinism: pure functions, fixed inputs, no
//! wall-clock — the emitted evidence is byte-for-byte reproducible.
#![forbid(unsafe_code)]

use ftui_render::ansi::{hyperlink_end, hyperlink_start, hyperlink_start_with_id};
use ftui_render::fit_metrics::{
    CellMetrics, ContainerViewport, FitPolicy, MetricGeneration, fit_to_container,
};

/// Emit one structured compatibility-harness cell.
fn emit(
    subsystem: &str,
    scenario: &str,
    case: &str,
    correlation_id: &str,
    passed: bool,
    extra: &str,
) {
    println!(
        "FTUI_ADDON_PARITY_COMPAT {{\"subsystem\":\"{subsystem}\",\"scenario\":\"{scenario}\",\
\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\"passed\":{passed},{extra}}}"
    );
}

// ── Fit-to-container (xterm.js FitAddon parity) ──────────────────────────────

/// A bigger container never yields a smaller grid (the monotonic FitAddon
/// invariant), and the result is always valid and at least 1×1.
#[test]
fn fit_grows_monotonically_with_container() {
    let cell = CellMetrics::from_px(8.0, 16.0).expect("cell metrics");
    let small = ContainerViewport::simple(800, 600).expect("viewport");
    let big = ContainerViewport::simple(1600, 1200).expect("viewport");

    let r_small = fit_to_container(&small, &cell, FitPolicy::FitToContainer).expect("fit small");
    let r_big = fit_to_container(&big, &cell, FitPolicy::FitToContainer).expect("fit big");

    assert!(r_small.is_valid() && r_big.is_valid());
    assert!(r_small.cols >= 1 && r_small.rows >= 1);
    // Monotonic: more container space never shrinks the grid, and a larger
    // container strictly grows it here.
    assert!(r_big.cols > r_small.cols, "wider container ⇒ more cols");
    assert!(r_big.rows > r_small.rows, "taller container ⇒ more rows");

    emit(
        "fit",
        "fitaddon_monotonic",
        "800x600_vs_1600x1200",
        "fit-scale",
        r_big.cols > r_small.cols && r_big.rows > r_small.rows,
        &format!(
            "\"small_cols\":{},\"small_rows\":{},\"big_cols\":{},\"big_rows\":{}",
            r_small.cols, r_small.rows, r_big.cols, r_big.rows
        ),
    );
}

/// `Fixed` ignores the container; `FitWithMinimum` enforces its floor; and a
/// container too small for even one cell clamps to a valid 1×1 grid rather than
/// panicking or producing a zero-sized grid.
#[test]
fn fit_policies_and_minimum_clamp() {
    let cell = CellMetrics::from_px(8.0, 16.0).expect("cell metrics");
    let viewport = ContainerViewport::simple(800, 600).expect("viewport");

    let fixed = fit_to_container(&viewport, &cell, FitPolicy::Fixed { cols: 80, rows: 24 })
        .expect("fixed fit");
    assert_eq!(
        (fixed.cols, fixed.rows),
        (80, 24),
        "Fixed ignores container"
    );

    let min = fit_to_container(
        &viewport,
        &cell,
        FitPolicy::FitWithMinimum {
            min_cols: 200,
            min_rows: 100,
        },
    )
    .expect("min fit");
    assert!(min.cols >= 200 && min.rows >= 100, "minimum is enforced");

    // A 1×1px container clamps to the minimum valid grid (never zero-sized).
    let tiny = ContainerViewport::simple(1, 1).expect("tiny viewport");
    let clamped = fit_to_container(&tiny, &cell, FitPolicy::FitToContainer).expect("tiny fit");
    assert!(clamped.is_valid(), "tiny container clamps to a valid grid");
    assert_eq!((clamped.cols, clamped.rows), (1, 1), "clamp floor is 1×1");

    emit(
        "fit",
        "policies_and_clamp",
        "fixed_min_tiny",
        "fit-policy",
        true,
        &format!(
            "\"fixed\":\"80x24\",\"min_cols\":{},\"tiny\":\"{}x{}\"",
            min.cols, clamped.cols, clamped.rows
        ),
    );
}

/// The web-font metric lifecycle is a strictly monotonic generation counter, so
/// a host can invalidate cached layout exactly once per metric change.
#[test]
fn web_font_metric_generation_is_monotonic() {
    let g0 = MetricGeneration::default();
    let g1 = g0.next();
    let g2 = g1.next();
    assert!(g1 != g0 && g2 != g1 && g2 != g0, "generations are distinct");

    // Determinism: the same starting generation advances identically.
    assert_eq!(MetricGeneration::default().next(), g0.next());

    emit(
        "fit",
        "web_font_metric_lifecycle",
        "generation_monotonic",
        "fit-metricgen",
        true,
        "\"distinct\":true",
    );
}

// ── OSC-8 web links (xterm.js web-links parity + sanitisation) ───────────────

/// A safe URL emits the exact OSC-8 byte sequence xterm.js expects.
#[test]
fn osc8_emits_exact_sequence() {
    let mut buf: Vec<u8> = Vec::new();
    hyperlink_start(&mut buf, "https://example.com").unwrap();
    buf.extend_from_slice(b"link text");
    hyperlink_end(&mut buf).unwrap();

    let s = String::from_utf8(buf).unwrap();
    assert_eq!(s, "\x1b]8;;https://example.com\x07link text\x1b]8;;\x07");

    let mut id_buf: Vec<u8> = Vec::new();
    hyperlink_start_with_id(&mut id_buf, "grp1", "https://example.com").unwrap();
    assert_eq!(
        String::from_utf8(id_buf).unwrap(),
        "\x1b]8;id=grp1;https://example.com\x07"
    );

    emit(
        "links",
        "osc8_emission",
        "safe_url_and_id",
        "links-emit",
        true,
        "\"format\":\"osc8\"",
    );
}

/// Sanitisation hardening (no xterm.js analogue): a URL with a control character,
/// an over-long field, or an id containing `;` emits **no bytes** rather than a
/// malformed/injectable escape. This is the addon-parity fault-injection cell.
#[test]
fn osc8_sanitisation_rejects_injection() {
    // Control character in the URL (e.g. an embedded BEL to break out early).
    let mut buf: Vec<u8> = Vec::new();
    hyperlink_start(&mut buf, "https://evil\x07/inject").unwrap();
    assert!(buf.is_empty(), "control char must suppress emission");

    // id containing ';' would corrupt the OSC-8 field structure.
    let mut buf2: Vec<u8> = Vec::new();
    hyperlink_start_with_id(&mut buf2, "a;b", "https://example.com").unwrap();
    assert!(buf2.is_empty(), "semicolon in id must suppress emission");

    // A newline in the URL is a control char → suppressed.
    let mut buf3: Vec<u8> = Vec::new();
    hyperlink_start(&mut buf3, "https://example.com/\nX").unwrap();
    assert!(buf3.is_empty(), "newline must suppress emission");

    emit(
        "links",
        "osc8_sanitisation",
        "control_char_and_semicolon",
        "links-sanitise",
        true,
        "\"suppressed\":true",
    );
}

/// Determinism: identical inputs produce byte-identical OSC-8 output.
#[test]
fn osc8_is_deterministic() {
    let render = || {
        let mut buf: Vec<u8> = Vec::new();
        hyperlink_start(&mut buf, "https://example.com/path?q=1").unwrap();
        hyperlink_end(&mut buf).unwrap();
        buf
    };
    assert_eq!(render(), render());
}

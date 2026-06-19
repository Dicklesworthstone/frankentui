//! Addon-parity compatibility harness — ligature/shaping arm (bd-2vr05.14.6).
//!
//! Drives the **production** ligature/shaping fallback path
//! (`ftui_text::ShapingFallback`, the in-tree home of bd-2vr05.14.5) and asserts
//! its core guarantee: when a real shaper is unavailable (the common terminal
//! case), shaping falls back **deterministically** to canonical cell boundaries
//! regardless of the requested ligature mode — no glyph dropping, no
//! reordering, byte-exact text round-trip. This is the parity counterpart to
//! xterm.js's optional ligatures addon, which silently no-ops when unsupported.
//!
//! Each cell prints one structured `FTUI_ADDON_PARITY_COMPAT ...` JSONL line so
//! the aggregator (`scripts/frankenterm_js_addon_parity_compat.sh`) folds it
//! into the compatibility manifest. Determinism: no shaper, no randomness — the
//! fallback layout is reproducible.
#![forbid(unsafe_code)]

use ftui_text::{
    FallbackEvent, FallbackStats, LigatureMode, RunDirection, Script, ShapingFallback,
};

fn mode_token(m: LigatureMode) -> &'static str {
    match m {
        LigatureMode::Disabled => "disabled",
        LigatureMode::Auto => "auto",
        LigatureMode::Enabled => "enabled",
    }
}

fn emit(scenario: &str, case: &str, correlation_id: &str, passed: bool, extra: &str) {
    println!(
        "FTUI_ADDON_PARITY_COMPAT {{\"subsystem\":\"ligature\",\"scenario\":\"{scenario}\",\
\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\"passed\":{passed},{extra}}}"
    );
}

/// With no primary shaper, every ligature mode falls back to canonical cell
/// boundaries: the layout has one cell per ASCII char, the text round-trips
/// byte-for-byte, and the event is the deterministic `NoopUsed`.
#[test]
fn fallback_is_canonical_for_every_mode() {
    let text = "file->x"; // contains a would-be "fi" ligature and "->"
    for mode in [
        LigatureMode::Disabled,
        LigatureMode::Auto,
        LigatureMode::Enabled,
    ] {
        let mut fb = ShapingFallback::terminal();
        fb.set_ligature_mode(mode);
        let (layout, event) = fb.shape_line(text, Script::Latin, RunDirection::Ltr);

        assert_eq!(event, FallbackEvent::NoopUsed, "no shaper ⇒ NoopUsed");
        assert_eq!(
            layout.total_cells(),
            text.chars().count(),
            "one cell per char"
        );
        // Byte-exact round-trip over the whole line.
        assert_eq!(
            layout.extract_text(text, 0, layout.total_cells()),
            text,
            "fallback must not drop or reorder glyphs"
        );

        emit(
            "deterministic_fallback",
            mode_token(mode),
            &format!("lig-fallback-{}", mode_token(mode)),
            event == FallbackEvent::NoopUsed,
            &format!(
                "\"mode\":\"{}\",\"cells\":{},\"event\":\"noop_used\"",
                mode_token(mode),
                layout.total_cells()
            ),
        );
    }
}

/// `FallbackStats` accounts a known event sequence deterministically, so an
/// operator can read the shaping-vs-fallback split from structured telemetry.
#[test]
fn fallback_stats_accounting_is_exact() {
    let mut stats = FallbackStats::default();
    stats.record(FallbackEvent::ShapedSuccessfully);
    stats.record(FallbackEvent::NoopUsed);
    stats.record(FallbackEvent::NoopUsed);
    stats.record(FallbackEvent::SkippedByPolicy);

    assert_eq!(stats.total_lines, 4);
    assert_eq!(stats.shaped_lines, 1);
    assert_eq!(stats.fallback_lines, 2);
    assert_eq!(stats.skipped_lines, 1);
    assert!((stats.shaping_rate() - 0.25).abs() < 1e-9);
    assert!((stats.fallback_rate() - 0.50).abs() < 1e-9);

    emit(
        "stats_accounting",
        "1shaped_2fallback_1skipped",
        "lig-stats",
        true,
        &format!(
            "\"shaping_rate\":{:.2},\"fallback_rate\":{:.2}",
            stats.shaping_rate(),
            stats.fallback_rate()
        ),
    );
}

/// Empty input is handled without panicking and produces an empty layout.
#[test]
fn empty_line_is_safe() {
    let fb = ShapingFallback::terminal();
    let (layout, _event) = fb.shape_line("", Script::Latin, RunDirection::Ltr);
    assert_eq!(layout.total_cells(), 0, "empty line ⇒ empty layout");

    emit(
        "edge_case",
        "empty_line",
        "lig-empty",
        layout.total_cells() == 0,
        "\"cells\":0",
    );
}

/// Determinism: the same line shaped twice yields an identical layout.
#[test]
fn fallback_is_deterministic() {
    let render = || {
        let mut fb = ShapingFallback::terminal();
        fb.set_ligature_mode(LigatureMode::Enabled);
        let (layout, event) = fb.shape_line("ligature->test", Script::Latin, RunDirection::Ltr);
        (
            layout.total_cells(),
            layout
                .extract_text("ligature->test", 0, layout.total_cells())
                .to_owned(),
            event,
        )
    };
    assert_eq!(render(), render(), "fallback shaping must be deterministic");
}

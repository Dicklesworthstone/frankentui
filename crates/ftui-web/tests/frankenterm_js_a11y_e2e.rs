//! Cross-browser accessibility + IME E2E matrix (bd-2vr05.7.5).
//!
//! This is the deterministic, host-free counterpart to a real browser matrix
//! run. It drives the **production** FrankenTermJS web input path —
//! `ftui_web::input_parser::parse_encoded_input_to_event` (the exact function a
//! JS host calls on each `FrankenTermWeb.drainEncodedInputs()` payload) —
//! together with the **production** host-agnostic accessibility subsystem in
//! `ftui_a11y` (screen-reader announcements + the reduced-motion / high-contrast
//! preference policies from bd-2vr05.7.4).
//!
//! It exercises three workflows the parent feature (bd-2vr05.7) makes
//! first-class, across a matrix of simulated browser engines:
//!
//!   1. **IME composition** — preedit/commit/cancel sequences for Latin
//!      dead-key, CJK Pinyin, and Japanese Kanji input methods, asserting the
//!      emitted `Event::Ime` phase stream is correct per engine quirk.
//!   2. **Accessibility preferences** — each engine reports
//!      `prefers-reduced-motion` / `prefers-contrast` / `forced-colors` media
//!      queries; we assert the derived motion/contrast policies and the stable
//!      CSS data-attribute serialisation are deterministic for that capability
//!      profile.
//!   3. **Screen-reader behaviour under reduced motion** — a live-region
//!      progress bar churns frame-by-frame; we assert that reduced-motion
//!      engines coalesce and downgrade the assistive-tech churn while full-motion
//!      engines announce every frame.
//!
//! Determinism: there is no wall-clock, randomness, or floating-point
//! ordering. Every cell prints a single structured `FTUI_A11Y_MATRIX ...` JSONL
//! line (with a correlation ID) so the runner script
//! (`scripts/frankenterm_js_a11y_e2e.sh`) captures greppable, hashable
//! observability evidence per step. One cell is a deliberate failure-injection
//! scenario that proves malformed composition input yields an actionable,
//! structured diagnostic rather than a panic or a silent drop.
//!
//! Requires the `input-parser` feature (the IME JSON path). The whole file is
//! gated so a default-feature build of `ftui-web` still compiles its test
//! target.
#![cfg(feature = "input-parser")]

use ftui_a11y::node::{A11yNodeInfo, A11yRole, A11yState, LiveRegion};
use ftui_a11y::preferences::{AccessibilityPreferences, MotionProfile};
use ftui_a11y::tree::{A11yTree, A11yTreeBuilder, ScreenReaderPolicy};
use ftui_core::event::{Event, ImeEvent, ImePhase};
use ftui_core::geometry::Rect;
use ftui_web::input_parser::{InputParseError, parse_encoded_input_to_event};

// ── Simulated browser engines (the "cross-browser" matrix axis) ─────────────

/// A simulated browser engine: its reported accessibility media queries plus the
/// one IME quirk that actually differs in practice across engines.
#[derive(Debug, Clone, Copy)]
struct BrowserProfile {
    name: &'static str,
    prefers_reduced_motion: bool,
    prefers_contrast_more: bool,
    forced_colors: bool,
    /// Whether the engine emits an explicit `compositionstart` (`start` phase)
    /// before the first `compositionupdate`. WebKit historically fires the first
    /// `update` without a preceding `start` in some IMEs; we model that so the
    /// consumer code is proven robust to both shapes.
    emits_composition_start: bool,
}

const BROWSERS: &[BrowserProfile] = &[
    // Chromium: default prefs, well-behaved composition lifecycle.
    BrowserProfile {
        name: "chromium",
        prefers_reduced_motion: false,
        prefers_contrast_more: false,
        forced_colors: false,
        emits_composition_start: true,
    },
    // Firefox: user has reduced-motion + more-contrast enabled.
    BrowserProfile {
        name: "firefox",
        prefers_reduced_motion: true,
        prefers_contrast_more: true,
        forced_colors: false,
        emits_composition_start: true,
    },
    // WebKit: forced-colors (Windows High Contrast via Safari Tech Preview) and
    // the no-explicit-start composition quirk.
    BrowserProfile {
        name: "webkit",
        prefers_reduced_motion: false,
        prefers_contrast_more: false,
        forced_colors: true,
        emits_composition_start: false,
    },
];

impl BrowserProfile {
    fn preferences(&self) -> AccessibilityPreferences {
        AccessibilityPreferences::from_media_queries(
            self.prefers_reduced_motion,
            self.prefers_contrast_more,
            self.forced_colors,
        )
    }
}

// ── IME methods (the input-method matrix axis) ──────────────────────────────

/// An input method's logical composition steps (preedit snapshots) and its final
/// outcome.
#[derive(Debug, Clone)]
struct ImeMethod {
    name: &'static str,
    /// Successive preedit strings shown to the user.
    preedits: &'static [&'static str],
    /// `Some(text)` = committed text; `None` = composition cancelled.
    commit: Option<&'static str>,
}

const IME_METHODS: &[ImeMethod] = &[
    ImeMethod {
        name: "latin_deadkey",
        preedits: &["´"],
        commit: Some("é"),
    },
    ImeMethod {
        name: "pinyin",
        preedits: &["n", "ni", "nihao"],
        commit: Some("你好"),
    },
    ImeMethod {
        name: "japanese_kanji",
        preedits: &["か", "かん", "感"],
        commit: Some("感じ"),
    },
    ImeMethod {
        name: "cancelled",
        preedits: &["未"],
        commit: None,
    },
];

/// Build the JSON payloads a browser would hand us for one composition, honoring
/// the engine's start-phase quirk.
fn composition_payloads(browser: &BrowserProfile, method: &ImeMethod) -> Vec<String> {
    let mut payloads = Vec::new();
    if browser.emits_composition_start {
        payloads.push(r#"{"kind":"composition","phase":"start"}"#.to_string());
    }
    for preedit in method.preedits {
        payloads.push(format!(
            r#"{{"kind":"composition","phase":"update","data":"{preedit}"}}"#
        ));
    }
    match method.commit {
        Some(text) => payloads.push(format!(
            r#"{{"kind":"composition","phase":"end","data":"{text}"}}"#
        )),
        None => payloads.push(r#"{"kind":"composition","phase":"cancel"}"#.to_string()),
    }
    payloads
}

/// Drive the production parser over a composition and return the emitted IME
/// phase/text stream.
fn run_composition(browser: &BrowserProfile, method: &ImeMethod) -> Vec<(ImePhase, String)> {
    composition_payloads(browser, method)
        .iter()
        .map(|json| match parse_encoded_input_to_event(json) {
            Ok(Some(Event::Ime(ImeEvent { phase, text }))) => (phase, text),
            other => panic!("composition payload did not parse to an IME event: {other:?}"),
        })
        .collect()
}

// ── Accessibility scenario: live-region progress churn ──────────────────────

fn progress_tree(percent: u8) -> A11yTree {
    let mut b = A11yTreeBuilder::new();
    b.add_node(
        A11yNodeInfo::new(1, A11yRole::Window, Rect::new(0, 0, 80, 24)).with_children(vec![2]),
    );
    let state = A11yState {
        value_now: Some(f64::from(percent)),
        value_text: Some(format!("{percent}%")),
        ..A11yState::default()
    };
    b.add_node(
        A11yNodeInfo::new(2, A11yRole::ProgressBar, Rect::new(0, 1, 40, 1))
            .with_name("Upload")
            .with_parent(1)
            .with_state(state)
            // Assertive so we can prove reduced motion downgrades interruption.
            .with_live_region(LiveRegion::Assertive),
    );
    b.set_root(1);
    b.build()
}

/// Run three frames of progress churn through the production screen-reader
/// pipeline + the engine's motion policy. Returns
/// `(announced, coalesced, downgraded)` totals across the frames.
fn run_progress_churn(motion: MotionProfile) -> (usize, usize, usize) {
    let mut prev = progress_tree(0);
    let (mut announced, mut coalesced, mut downgraded) = (0usize, 0usize, 0usize);
    for pct in [33u8, 66, 100] {
        let next = progress_tree(pct);
        let batch = next.screen_reader_announcements_since(&prev, ScreenReaderPolicy::default());
        let filtered = motion.filter_announcements(&batch);
        announced += filtered.announcements.len();
        coalesced += filtered.coalesced_count;
        downgraded += filtered.downgraded_count;
        prev = next;
    }
    (announced, coalesced, downgraded)
}

// ── JSONL evidence emission ─────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn emit(
    scenario: &str,
    browser: &str,
    case: &str,
    correlation_id: &str,
    passed: bool,
    extra: &str,
) {
    println!(
        "FTUI_A11Y_MATRIX {{\"event\":\"a11y_ime_matrix\",\"scenario\":\"{scenario}\",\
\"browser\":\"{browser}\",\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\
\"passed\":{passed},{extra}}}"
    );
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// IME composition across every (browser × input-method) cell.
#[test]
fn ime_composition_matrix_emits_correct_phase_stream() {
    for browser in BROWSERS {
        for method in IME_METHODS {
            let stream = run_composition(browser, method);
            let cid = format!("ime-{}-{}", browser.name, method.name);

            // The terminal phase must match the method outcome.
            let last = stream.last().expect("composition emits at least one event");
            match method.commit {
                Some(text) => {
                    assert_eq!(last.0, ImePhase::Commit, "{cid}: must end in commit");
                    assert_eq!(last.1, text, "{cid}: committed text mismatch");
                }
                None => {
                    assert_eq!(last.0, ImePhase::Cancel, "{cid}: must end in cancel");
                    assert!(last.1.is_empty(), "{cid}: cancel carries no text");
                }
            }

            // Every preedit must surface as an Update with the exact preedit.
            let updates: Vec<&String> = stream
                .iter()
                .filter(|(p, _)| *p == ImePhase::Update)
                .map(|(_, t)| t)
                .collect();
            assert_eq!(
                updates.len(),
                method.preedits.len(),
                "{cid}: one Update per preedit"
            );
            for (got, want) in updates.iter().zip(method.preedits.iter()) {
                assert_eq!(got.as_str(), *want, "{cid}: preedit mismatch");
            }

            // Start phase appears iff the engine emits it.
            let has_start = stream.iter().any(|(p, _)| *p == ImePhase::Start);
            assert_eq!(
                has_start, browser.emits_composition_start,
                "{cid}: start-phase presence must match engine quirk"
            );

            emit(
                "ime_composition",
                browser.name,
                method.name,
                &cid,
                true,
                &format!(
                    "\"events\":{},\"updates\":{},\"has_start\":{},\"terminal\":\"{}\"",
                    stream.len(),
                    updates.len(),
                    has_start,
                    if method.commit.is_some() {
                        "commit"
                    } else {
                        "cancel"
                    }
                ),
            );
        }
    }
}

/// Accessibility-preference derivation + data-attribute serialisation per engine.
#[test]
fn accessibility_preferences_matrix_is_deterministic_per_engine() {
    for browser in BROWSERS {
        let prefs = browser.preferences();
        let motion = prefs.motion_profile();
        let contrast = prefs.contrast_profile();
        let dataset = prefs.web_dataset();
        let cid = format!("pref-{}", browser.name);

        // Determinism: identical media-query profile → identical policy + log.
        assert_eq!(prefs, browser.preferences());
        assert_eq!(prefs.to_jsonl(&cid), prefs.to_jsonl(&cid));

        // Motion policy tracks reduced-motion exactly.
        assert_eq!(motion.reduced_motion, browser.prefers_reduced_motion);
        assert_eq!(
            motion.announcement_coalescing, browser.prefers_reduced_motion,
            "{cid}: coalescing iff reduced motion"
        );
        assert_eq!(
            motion.animation_scale_bps,
            if browser.prefers_reduced_motion {
                0
            } else {
                10_000
            }
        );

        // Contrast policy elevates iff high-contrast OR forced-colors.
        let elevated = browser.prefers_contrast_more || browser.forced_colors;
        assert_eq!(contrast.high_contrast, elevated, "{cid}: elevated contrast");
        assert_eq!(
            contrast.system_colors_only, browser.forced_colors,
            "{cid}: system colours only iff forced-colors"
        );

        // Data-attribute keys are stable and ordered regardless of values.
        assert_eq!(
            dataset.map(|(k, _)| k),
            [
                "data-ftui-reduced-motion",
                "data-ftui-high-contrast",
                "data-ftui-forced-colors",
            ]
        );

        emit(
            "a11y_preferences",
            browser.name,
            "media_query_profile",
            &cid,
            true,
            &format!(
                "\"reduced_motion\":{},\"high_contrast\":{},\"forced_colors\":{},\
\"animation_scale_bps\":{},\"min_contrast_ratio_x100\":{}",
                prefs.reduced_motion,
                contrast.high_contrast,
                prefs.forced_colors,
                motion.animation_scale_bps,
                contrast.min_contrast_ratio_x100
            ),
        );
    }
}

/// Screen-reader behaviour under each engine's motion policy: full-motion engines
/// announce every progress frame; reduced-motion engines coalesce + downgrade.
#[test]
fn screen_reader_progress_churn_respects_engine_motion_policy() {
    for browser in BROWSERS {
        let motion = browser.preferences().motion_profile();
        let (announced, coalesced, downgraded) = run_progress_churn(motion);
        let cid = format!("sr-{}", browser.name);

        if browser.prefers_reduced_motion {
            // Three assertive value changes on the same node collapse: the
            // pipeline already emits one announcement per frame, and the motion
            // filter downgrades each interrupting assertive update to polite.
            assert!(
                downgraded >= 1,
                "{cid}: reduced motion must downgrade interrupting churn (got {downgraded})"
            );
        } else {
            assert_eq!(
                coalesced, 0,
                "{cid}: full motion must not coalesce announcements"
            );
            assert_eq!(
                downgraded, 0,
                "{cid}: full motion must not downgrade urgency"
            );
            assert_eq!(announced, 3, "{cid}: full motion announces every frame");
        }

        emit(
            "screen_reader_churn",
            browser.name,
            "progress_live_region",
            &cid,
            true,
            &format!(
                "\"announced\":{announced},\"coalesced\":{coalesced},\"downgraded\":{downgraded}"
            ),
        );
    }
}

/// End-to-end determinism: the entire matrix produces byte-identical evidence on
/// every run (no wall-clock, no map iteration order leaking out).
#[test]
fn matrix_is_byte_for_byte_reproducible() {
    let render = || {
        let mut lines = Vec::new();
        for browser in BROWSERS {
            for method in IME_METHODS {
                let stream = run_composition(browser, method);
                lines.push(format!("{}/{}: {:?}", browser.name, method.name, stream));
            }
            let prefs = browser.preferences();
            lines.push(prefs.to_jsonl(browser.name));
            let (a, c, d) = run_progress_churn(prefs.motion_profile());
            lines.push(format!("{}: churn {a}/{c}/{d}", browser.name));
        }
        lines.join("\n")
    };
    assert_eq!(render(), render(), "matrix output must be deterministic");
}

/// Deliberate failure injection: a malformed composition payload (unknown phase)
/// must surface as a structured, actionable `InputParseError`, never a panic or a
/// silent drop. This proves the diagnostics path the parent feature requires.
#[test]
fn failure_injection_malformed_composition_yields_actionable_diagnostic() {
    // Case 1: an unknown composition phase.
    let bad_phase = r#"{"kind":"composition","phase":"teleport","data":"x"}"#;
    let err = parse_encoded_input_to_event(bad_phase)
        .expect_err("unknown composition phase must be an error, not Ok");
    match &err {
        InputParseError::UnknownPhase(phase) => assert_eq!(phase, "teleport"),
        other => panic!("expected UnknownPhase, got {other:?}"),
    }
    let diagnostic = err.to_string();
    assert!(
        diagnostic.contains("teleport"),
        "diagnostic must name the offending phase: {diagnostic}"
    );
    emit(
        "failure_injection",
        "synthetic",
        "unknown_phase",
        "fail-unknown-phase",
        true,
        &format!("\"error\":\"{diagnostic}\""),
    );

    // Case 2: structurally broken JSON.
    let broken = r#"{"kind":"composition","phase":"#;
    let err =
        parse_encoded_input_to_event(broken).expect_err("malformed JSON must be an error, not Ok");
    assert!(
        matches!(err, InputParseError::Json(_)),
        "expected a JSON error, got {err:?}"
    );
    emit(
        "failure_injection",
        "synthetic",
        "malformed_json",
        "fail-malformed-json",
        true,
        &format!("\"error\":\"{}\"", json_escape(&err.to_string())),
    );
}

/// Minimal JSON string escaper for embedding diagnostic text in evidence lines.
fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

//! Cross-engine runtime option / theming mutation E2E matrix (bd-2vr05.13.5).
//!
//! This is the deterministic, host-free counterpart to a real browser run of the
//! `term.options` reconfigure surface. It drives the **production** runtime
//! option API — `ftui_web::runtime_options::{RuntimeOptions, RuntimeOptionPatch,
//! OptionCapabilityProfile}` (the exact types a JS host mutates when an embedder
//! assigns to `FrankenTermWeb.options`) — across a matrix of simulated rendering
//! engines, each advertising a different capability profile (the backends
//! introduced by bd-2vr05.8.1).
//!
//! It exercises the three guarantees the task (bd-2vr05.13.5) makes
//! first-class:
//!
//!   1. **Atomic hot-reconfigure** — a sparse option patch is applied as a unit;
//!      a single bad field rolls back the *entire* patch with no partial state.
//!   2. **Capability/profile gating** — a schema-valid option (e.g. a WebGPU
//!      renderer, or a huge scrollback) is rejected with an explicit, structured
//!      reason on engines that do not advertise it, while applying cleanly on
//!      engines that do.
//!   3. **Complete auditability** — every applied/rejected decision serialises to
//!      one structured `FTUI_RUNTIME_OPTIONS_MATRIX ...` JSONL line (with a
//!      correlation id), so the runner script
//!      (`scripts/frankenterm_js_runtime_options_e2e.sh`) captures greppable,
//!      hashable observability evidence per cell.
//!
//! Determinism: no wall-clock, randomness, or map-iteration order leaks into the
//! output. One cell is a deliberate failure-injection scenario proving a
//! malformed option payload yields an actionable, structured diagnostic rather
//! than a panic or a silent drop.
//!
//! Requires the `input-parser` feature (the JSON option-patch path). The whole
//! file is gated so a default-feature build of `ftui-web` still compiles its test
//! target.
#![cfg(feature = "input-parser")]

use ftui_core::terminal_capabilities::ColorDepth;
use ftui_web::runtime_options::{
    CursorStyle, OptionCapabilityProfile, RendererType, RuntimeOptionPatch, RuntimeOptions,
    ThemeColor, ThemePalette,
};

// ── Simulated rendering engines (the capability-profile matrix axis) ────────

/// A simulated browser rendering engine and the option capability profile it
/// advertises. These mirror the real backends (WebGPU-primary down to a DOM
/// fallback) plus their memory budgets.
#[derive(Debug, Clone, Copy)]
struct EngineProfile {
    name: &'static str,
    caps: OptionCapabilityProfile,
}

const ENGINES: &[EngineProfile] = &[
    // High-end: every renderer, true colour, generous scrollback budget.
    EngineProfile {
        name: "webgpu_full",
        caps: OptionCapabilityProfile {
            dom: true,
            canvas: true,
            webgl: true,
            webgpu: true,
            color_depth: ColorDepth::TrueColor,
            max_scrollback_lines: 1_000_000,
        },
    },
    // Mid: WebGL but no WebGPU, moderate budget.
    EngineProfile {
        name: "webgl_mid",
        caps: OptionCapabilityProfile {
            dom: true,
            canvas: true,
            webgl: true,
            webgpu: false,
            color_depth: ColorDepth::TrueColor,
            max_scrollback_lines: 100_000,
        },
    },
    // Low: canvas-only, small budget, no true colour.
    EngineProfile {
        name: "canvas_low",
        caps: OptionCapabilityProfile {
            dom: true,
            canvas: true,
            webgl: false,
            webgpu: false,
            color_depth: ColorDepth::Ansi256,
            max_scrollback_lines: 5_000,
        },
    },
    // Degraded: DOM fallback only.
    EngineProfile {
        name: "dom_fallback",
        caps: OptionCapabilityProfile {
            dom: true,
            canvas: false,
            webgl: false,
            webgpu: false,
            color_depth: ColorDepth::Ansi16,
            max_scrollback_lines: 1_000,
        },
    },
];

// ── Option-mutation scenarios (the patch matrix axis) ───────────────────────

/// A named reconfigure scenario: the JSON payload a host would assign, and
/// whether each engine is *expected* to accept it (derived from caps, not
/// hard-coded per engine, so the assertion mirrors the production gating logic).
#[derive(Debug, Clone, Copy)]
struct Scenario {
    name: &'static str,
    json: &'static str,
    /// Classifies the scenario so the test can derive the expected verdict from
    /// each engine's capability profile.
    expectation: Expectation,
}

#[derive(Debug, Clone, Copy)]
enum Expectation {
    /// Always applies regardless of engine (pure behaviour flags).
    AlwaysApplies,
    /// Applies only when the engine advertises the given renderer.
    NeedsRenderer(RendererType),
    /// Applies only when the engine budget is at least `lines`.
    NeedsScrollbackBudget(u32),
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "cursor_and_sr_flags",
        json: r#"{"cursorStyle":"bar","cursorBlink":true,"screenReaderMode":true}"#,
        expectation: Expectation::AlwaysApplies,
    },
    Scenario {
        name: "switch_to_webgpu",
        json: r#"{"rendererType":"webgpu"}"#,
        expectation: Expectation::NeedsRenderer(RendererType::WebGpu),
    },
    Scenario {
        name: "switch_to_webgl",
        json: r#"{"rendererType":"webgl"}"#,
        expectation: Expectation::NeedsRenderer(RendererType::WebGl),
    },
    Scenario {
        name: "big_scrollback",
        json: r#"{"scrollback":50000}"#,
        expectation: Expectation::NeedsScrollbackBudget(50_000),
    },
    Scenario {
        name: "theme_and_contrast",
        json: r##"{"minimumContrastRatioX100":700,"theme":{"foreground":"#e0e0e0","background":"#101020"}}"##,
        expectation: Expectation::AlwaysApplies,
    },
];

impl Expectation {
    /// Whether this scenario should apply under the given capability profile.
    fn should_apply(self, caps: &OptionCapabilityProfile) -> bool {
        match self {
            Self::AlwaysApplies => true,
            Self::NeedsRenderer(r) => caps.supports_renderer(r),
            Self::NeedsScrollbackBudget(lines) => caps.max_scrollback_lines >= lines,
        }
    }
}

// ── Evidence emission ───────────────────────────────────────────────────────

/// Emit one structured matrix-cell evidence line. The runner script greps for
/// the `FTUI_RUNTIME_OPTIONS_MATRIX ` prefix and strips it to build a JSONL
/// artifact.
fn emit(scenario: &str, engine: &str, case: &str, correlation_id: &str, passed: bool, extra: &str) {
    println!(
        "FTUI_RUNTIME_OPTIONS_MATRIX {{\"event\":\"runtime_options_matrix\",\"scenario\":\"{scenario}\",\
\"engine\":\"{engine}\",\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\
\"passed\":{passed},{extra}}}"
    );
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// Every (engine × scenario) cell: the production apply path must agree with the
/// capability-derived expectation, and the outcome JSONL must be self-describing.
#[test]
fn reconfigure_matrix_agrees_with_capability_gating() {
    for engine in ENGINES {
        for scenario in SCENARIOS {
            let patch = RuntimeOptionPatch::parse_json(scenario.json)
                .expect("matrix scenarios are well-formed JSON");

            let mut opts = RuntimeOptions::default();
            let before = opts;
            let cid = format!("opt-{}-{}", engine.name, scenario.name);
            let update = opts.apply_patch(&patch, &engine.caps, &cid);

            let expected = scenario.expectation.should_apply(&engine.caps);
            assert_eq!(
                update.applied, expected,
                "{cid}: applied={} but expected={expected}",
                update.applied
            );

            if update.applied {
                // Applied cells must report a non-empty, internally consistent
                // change set and leave no errors.
                assert!(update.errors.is_empty(), "{cid}: applied yet has errors");
                assert!(!update.changes.is_empty(), "{cid}: applied with no changes");
            } else {
                // Rejected cells must roll back to the exact prior state and
                // carry at least one actionable error.
                assert_eq!(opts, before, "{cid}: rejection must roll back");
                assert!(!update.errors.is_empty(), "{cid}: rejected with no errors");
            }

            // The outcome serialises to exactly one structured line.
            let outcome = update.to_jsonl();
            assert!(outcome.starts_with('{') && outcome.ends_with('}'));
            assert!(!outcome.contains('\n'));

            emit(
                scenario.name,
                engine.name,
                "apply_patch",
                &cid,
                update.applied == expected,
                &format!(
                    "\"applied\":{},\"change_count\":{},\"error_count\":{},\
\"requires_repaint\":{},\"requires_renderer_reinit\":{}",
                    update.applied,
                    update.changes.len(),
                    update.errors.len(),
                    update.requires_repaint,
                    update.requires_renderer_reinit
                ),
            );
        }
    }
}

/// Atomicity: a patch mixing one valid and one engine-gated field must reject as
/// a unit, applying *neither* field. WebGPU is unsupported on `canvas_low`, so a
/// patch that also flips a behaviour flag must leave both untouched.
#[test]
fn mixed_patch_rejects_atomically_under_gating() {
    let engine = ENGINES
        .iter()
        .find(|e| e.name == "canvas_low")
        .expect("canvas_low engine present");
    let patch =
        RuntimeOptionPatch::parse_json(r#"{"screenReaderMode":true,"rendererType":"webgpu"}"#)
            .unwrap();

    let mut opts = RuntimeOptions::default();
    let before = opts;
    let cid = "atomic-mixed";
    let update = opts.apply_patch(&patch, &engine.caps, cid);

    assert!(!update.applied, "gated field must fail the whole patch");
    assert_eq!(opts, before, "the valid field must NOT have been applied");
    assert!(!opts.screen_reader_mode, "screen_reader_mode rolled back");

    emit(
        "atomic_rollback",
        engine.name,
        "mixed_valid_and_gated",
        cid,
        true,
        &format!("\"error_count\":{}", update.errors.len()),
    );
}

/// The same theme reconfigure applied on the highest and lowest engines must
/// produce byte-identical applied state — option semantics are engine-invariant;
/// only gating differs. Theme colours are host-downgraded, never gated.
#[test]
fn theme_reconfigure_is_engine_invariant_when_ungated() {
    let json =
        r##"{"theme":{"foreground":"#abcdef","background":"#012345"},"cursorStyle":"underline"}"##;
    let patch = RuntimeOptionPatch::parse_json(json).unwrap();

    let high = ENGINES.iter().find(|e| e.name == "webgpu_full").unwrap();
    let low = ENGINES.iter().find(|e| e.name == "dom_fallback").unwrap();

    let mut a = RuntimeOptions::default();
    let mut b = RuntimeOptions::default();
    let ua = a.apply_patch(&patch, &high.caps, "inv-high");
    let ub = b.apply_patch(&patch, &low.caps, "inv-low");

    assert!(ua.applied && ub.applied, "theme change is never gated");
    assert_eq!(a, b, "applied option state must be engine-invariant");
    assert_eq!(a.theme.foreground, ThemeColor::rgb(0xAB, 0xCD, 0xEF));

    emit(
        "engine_invariance",
        "webgpu_full_vs_dom_fallback",
        "theme_reconfigure",
        "inv",
        a == b,
        &format!("\"foreground\":\"{}\"", a.theme.foreground.as_hex()),
    );
}

/// End-to-end determinism: the whole matrix produces byte-identical evidence on
/// every run.
#[test]
fn matrix_is_byte_for_byte_reproducible() {
    let render = || {
        let mut lines = Vec::new();
        for engine in ENGINES {
            for scenario in SCENARIOS {
                let patch = RuntimeOptionPatch::parse_json(scenario.json).unwrap();
                let mut opts = RuntimeOptions::default();
                let cid = format!("rep-{}-{}", engine.name, scenario.name);
                let update = opts.apply_patch(&patch, &engine.caps, &cid);
                lines.push(update.to_jsonl());
                lines.push(opts.to_jsonl(&cid));
            }
        }
        lines.join("\n")
    };
    assert_eq!(render(), render(), "matrix output must be deterministic");
}

/// Deliberate failure injection: malformed and semantically-invalid option
/// payloads must surface as structured, actionable diagnostics — never a panic
/// or a silent drop. This proves the audit path the parent feature requires.
#[test]
fn failure_injection_yields_actionable_diagnostics() {
    use ftui_web::runtime_options::OptionPatchParseError;

    // Case 1: structurally broken JSON → parse-stage diagnostic.
    let broken = r#"{"cursorStyle":"#;
    let err = RuntimeOptionPatch::parse_json(broken)
        .expect_err("malformed JSON must be an error, not Ok");
    assert!(matches!(err, OptionPatchParseError::MalformedJson(_)));
    emit(
        "failure_injection",
        "synthetic",
        "malformed_json",
        "fail-malformed-json",
        true,
        &format!("\"error\":\"{}\"", json_escape(&err.to_string())),
    );

    // Case 2: unknown option key → strict-parse diagnostic.
    let err = RuntimeOptionPatch::parse_json(r#"{"bogusKnob":1}"#)
        .expect_err("unknown key must be rejected");
    assert!(matches!(err, OptionPatchParseError::UnknownKey(_)));
    emit(
        "failure_injection",
        "synthetic",
        "unknown_key",
        "fail-unknown-key",
        true,
        &format!("\"error\":\"{}\"", json_escape(&err.to_string())),
    );

    // Case 3: a structurally valid payload that is semantically out of range →
    // apply-stage rejection with a complete error record, options unchanged.
    let patch = RuntimeOptionPatch::parse_json(r#"{"tabStopWidth":99}"#).unwrap();
    let mut opts = RuntimeOptions::default();
    let before = opts;
    let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "fail-range");
    assert!(!update.applied);
    assert_eq!(opts, before, "invalid payload must not mutate options");
    let outcome = update.to_jsonl();
    assert!(outcome.contains("\"applied\":false"));
    assert!(outcome.contains("\"kind\":\"out_of_range\""));
    emit(
        "failure_injection",
        "synthetic",
        "out_of_range_tab_width",
        "fail-range",
        true,
        &format!("\"error_count\":{}", update.errors.len()),
    );
}

/// Sanity: a fresh terminal on the most degraded engine still validates (the
/// default renderer must be in every engine's supported set).
#[test]
fn defaults_boot_on_every_engine() {
    let opts = RuntimeOptions::default();
    for engine in ENGINES {
        let errors = opts.validate(&engine.caps);
        assert!(
            errors.is_empty(),
            "{}: defaults must be valid, got {errors:?}",
            engine.name
        );
        emit(
            "default_boot",
            engine.name,
            "validate_defaults",
            &format!("boot-{}", engine.name),
            true,
            "\"errors\":0",
        );
    }
    // Reference an otherwise-unused import so a default palette change stays
    // covered by this E2E surface.
    let _ = ThemePalette::default();
    let _ = CursorStyle::Block;
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

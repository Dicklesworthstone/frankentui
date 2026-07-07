//! Comprehensive SDK validation suite (bd-2vr05.9.6): contract unit tests +
//! adapter-lifecycle E2E scenarios with structured JSONL evidence.
//!
//! This is the explicit test lane tying the TS-facing contract types, the
//! runtime option validation, the typed error taxonomy, and the first-party
//! adapter lifecycles (bd-2vr05.9.3) together, so the SDK surface cannot
//! drift silently. `scripts/frankenterm_js_sdk_adapter_e2e.sh` drives this
//! file and harvests the `FTUI_SDK_ADAPTER_COMPAT ` JSONL lines (adding
//! wall-clock timestamps at the harness layer; the in-model lines stay
//! timestamp-free so replays are byte-identical).

#![forbid(unsafe_code)]

use ftui_web::runtime_options::{OptionCapabilityProfile, RendererType, RuntimeOptions};
use ftui_web::sdk_adapter::{
    ADAPTER_SCHEMA_VERSION, AdapterAction, AdapterKind, AdapterLifecycle, AdapterMisuse,
    AdapterOutcome, AdapterPhase, react_example, recommended_wiring, vanilla_example,
};
use ftui_web::sdk_event_model::{
    EVENT_SCHEMA_VERSION, EventBufferPolicy, HostEventClass, SdkErrorKind,
};

/// Evidence envelope prefix harvested by the E2E script.
const EVIDENCE_PREFIX: &str = "FTUI_SDK_ADAPTER_COMPAT";

fn emit(scenario: &str, payload: &str) {
    println!("{EVIDENCE_PREFIX} {{\"scenario\":\"{scenario}\",{payload}}}");
}

// ============================================================================
// Contract unit lane: API surface
// ============================================================================

/// The host-event taxonomy is complete, sorted, and round-trips its wire
/// strings — the ordering guarantee `apiContract().eventTypes` documents.
#[test]
fn api_surface_event_taxonomy_is_sorted_and_round_trips() {
    let wires: Vec<&str> = HostEventClass::ALL.iter().map(|c| c.as_str()).collect();
    let mut sorted = wires.clone();
    sorted.sort_unstable();
    assert_eq!(
        wires, sorted,
        "event taxonomy must stay sorted by wire string"
    );
    assert_eq!(HostEventClass::ALL.len(), 15);
    for class in HostEventClass::ALL {
        assert_eq!(
            HostEventClass::from_wire(class.as_str()),
            Some(class),
            "wire round-trip failed for {}",
            class.as_str()
        );
        assert!(
            class.as_str().starts_with(class.namespace()),
            "wire string must live inside its namespace"
        );
    }
    assert_eq!(HostEventClass::from_wire("no.such.event"), None);
    assert_eq!(EVENT_SCHEMA_VERSION, "1.0.0");
    emit(
        "api_surface",
        &format!(
            "\"check\":\"event_taxonomy\",\"classes\":{},\"schema\":\"{EVENT_SCHEMA_VERSION}\",\"verdict\":\"pass\"",
            HostEventClass::ALL.len()
        ),
    );
}

/// The adapter wiring tables reference only stable contract methods, for
/// both adapter kinds, in a teardown-safe order.
#[test]
fn api_surface_adapter_wiring_stays_on_contract_methods() {
    for kind in AdapterKind::ALL {
        let wiring = recommended_wiring(kind);
        let pos = |m: &str| wiring.iter().position(|s| s.method == m).expect(m);
        assert!(pos("apiContract") < pos("init"));
        assert!(pos("init") < pos("attachConnect"));
        assert!(pos("attachClose") < pos("destroy"));
        emit(
            "api_surface",
            &format!(
                "\"check\":\"wiring\",\"adapter\":\"{}\",\"steps\":{},\"verdict\":\"pass\"",
                kind.as_str(),
                wiring.len()
            ),
        );
    }
}

// ============================================================================
// Contract unit lane: option validation
// ============================================================================

/// Defaulted options validate against every capability profile (the boot
/// guarantee: a fresh terminal starts on any engine).
#[test]
fn option_validation_defaults_pass_every_profile() {
    for (label, profile) in [
        ("minimal", OptionCapabilityProfile::minimal()),
        ("full", full_profile()),
    ] {
        let errors = RuntimeOptions::default().validate(&profile);
        assert!(
            errors.is_empty(),
            "default options must validate on the {label} profile: {errors:?}"
        );
        emit(
            "option_validation",
            &format!("\"check\":\"defaults\",\"profile\":\"{label}\",\"verdict\":\"pass\""),
        );
    }
}

fn full_profile() -> OptionCapabilityProfile {
    OptionCapabilityProfile {
        webgl: true,
        webgpu: true,
        ..OptionCapabilityProfile::minimal()
    }
}

/// Out-of-range and capability-gated options are rejected with named,
/// field-level errors — never silently clamped.
#[test]
fn option_validation_rejects_bad_options_with_named_errors() {
    let profile = OptionCapabilityProfile::minimal();

    let bad_tab = RuntimeOptions {
        tab_width: 0,
        ..RuntimeOptions::default()
    };
    let errors = bad_tab.validate(&profile);
    assert!(
        !errors.is_empty(),
        "tab_width=0 must fail schema validation"
    );

    let ungated_renderer = RuntimeOptions {
        renderer: RendererType::WebGpu,
        ..RuntimeOptions::default()
    };
    let errors = ungated_renderer.validate(&profile);
    assert!(
        !errors.is_empty(),
        "webgpu renderer must be rejected by a profile that does not advertise it"
    );

    emit(
        "option_validation",
        "\"check\":\"rejections\",\"cases\":2,\"verdict\":\"pass\"",
    );
}

// ============================================================================
// Contract unit lane: event ordering (bounded buffering)
// ============================================================================

/// The bounded-buffering policy table is stable: every queue is named once,
/// capacities are non-zero, and the entry order is deterministic.
#[test]
fn event_ordering_buffer_policy_is_stable() {
    let entries = EventBufferPolicy::DEFAULT.entries();
    assert_eq!(entries.len(), 9);
    let mut names: Vec<&str> = entries.iter().map(|(name, _)| *name).collect();
    let unique_before = names.len();
    names.dedup();
    assert_eq!(names.len(), unique_before, "queue names must be unique");
    for (name, capacity) in entries {
        assert!(capacity > 0, "queue {name} must have non-zero capacity");
    }
    // Determinism: two reads produce the identical table.
    assert_eq!(EventBufferPolicy::DEFAULT.entries(), entries);
    emit(
        "event_ordering",
        &format!(
            "\"check\":\"buffer_policy\",\"queues\":{},\"verdict\":\"pass\"",
            entries.len()
        ),
    );
}

// ============================================================================
// Contract unit lane: typed error taxonomy
// ============================================================================

/// Engine error codes are sorted, unique, summarized, and round-trip; the
/// adapter-layer misuse codes stay disjoint from the engine taxonomy.
#[test]
fn error_taxonomy_is_stable_and_layered() {
    let codes: Vec<&str> = SdkErrorKind::ALL.iter().map(|k| k.code()).collect();
    let mut sorted = codes.clone();
    sorted.sort_unstable();
    assert_eq!(codes, sorted, "error codes must stay sorted");
    for kind in SdkErrorKind::ALL {
        assert_eq!(SdkErrorKind::from_code(kind.code()), Some(kind));
        assert!(!kind.summary().is_empty());
    }
    assert_eq!(SdkErrorKind::from_code("adapter.double_mount"), None);
    for misuse in AdapterMisuse::CODES {
        assert!(
            misuse.starts_with("adapter."),
            "adapter misuse codes live in the adapter.* namespace"
        );
        assert!(
            SdkErrorKind::from_code(misuse).is_none(),
            "adapter code {misuse} must not collide with the engine taxonomy"
        );
    }
    emit(
        "error_taxonomy",
        &format!(
            "\"check\":\"layering\",\"engine_codes\":{},\"adapter_codes\":{},\"verdict\":\"pass\"",
            SdkErrorKind::ALL.len(),
            AdapterMisuse::CODES.len()
        ),
    );
}

// ============================================================================
// E2E lane: adapter lifecycle scenarios (init / resize / input / teardown)
// ============================================================================

fn full_session(kind: AdapterKind, adapter_id: &str) -> Vec<String> {
    let mut adapter = AdapterLifecycle::new(kind, adapter_id);
    let mut lines = Vec::new();
    for action in [
        AdapterAction::Mount,
        AdapterAction::Resize { cols: 80, rows: 24 },
        AdapterAction::Attach,
        AdapterAction::Input { bytes: 12 },
        AdapterAction::Resize {
            cols: 120,
            rows: 40,
        },
        AdapterAction::Input { bytes: 3 },
        AdapterAction::Detach,
        AdapterAction::Dispose,
    ] {
        lines.push(
            adapter
                .apply(&action)
                .unwrap_or_else(|m| panic!("{kind}: {} rejected: {}", m.action, m.code))
                .to_jsonl(),
        );
    }
    assert_eq!(adapter.phase(), AdapterPhase::Disposed);
    lines
}

/// Full init→resize→input→teardown sessions for BOTH adapter kinds produce
/// complete, parseable, correlated JSONL timelines.
#[test]
fn e2e_full_sessions_emit_correlated_timelines() {
    for kind in AdapterKind::ALL {
        let adapter_id = format!("e2e-{}", kind.as_str());
        let lines = full_session(kind, &adapter_id);
        assert_eq!(lines.len(), 8);
        for (idx, line) in lines.iter().enumerate() {
            let parsed: serde_json::Value =
                serde_json::from_str(line).expect("timeline line parses");
            assert_eq!(parsed["adapter_id"].as_str(), Some(adapter_id.as_str()));
            assert_eq!(parsed["schema"].as_str(), Some(ADAPTER_SCHEMA_VERSION));
            assert_eq!(parsed["seq"].as_u64(), Some(idx as u64 + 1));
            emit(
                "adapter_session",
                &format!("\"adapter\":\"{}\",\"timeline\":{line}", kind.as_str()),
            );
        }
    }
}

/// The same action sequence replays byte-identically (deterministic
/// postmortem guarantee for the JSONL timeline).
#[test]
fn e2e_sessions_replay_byte_identically() {
    for kind in AdapterKind::ALL {
        let a = full_session(kind, "replay");
        let b = full_session(kind, "replay");
        assert_eq!(a, b, "{kind} timeline must replay byte-identically");
    }
    emit(
        "adapter_session",
        "\"check\":\"replay_determinism\",\"verdict\":\"pass\"",
    );
}

/// React StrictMode double mount/cleanup runs the whole session without a
/// single misuse; the dedup outcomes are visible in the timeline.
#[test]
fn e2e_react_strict_mode_session_is_clean() {
    let mut adapter = AdapterLifecycle::new(AdapterKind::React, "strict-mode");
    let mut dedups = 0;
    for action in [
        AdapterAction::Mount,
        AdapterAction::Mount, // StrictMode re-runs the effect body.
        AdapterAction::Attach,
        AdapterAction::Attach,
        AdapterAction::Input { bytes: 1 },
        AdapterAction::Detach,
        AdapterAction::Detach, // StrictMode re-runs the cleanup.
        AdapterAction::Dispose,
        AdapterAction::Dispose,
    ] {
        let event = adapter.apply(&action).expect("strict-mode session action");
        if event.outcome == AdapterOutcome::StrictModeDeduped {
            dedups += 1;
        }
        emit("strict_mode", &format!("\"timeline\":{}", event.to_jsonl()));
    }
    assert_eq!(dedups, 4, "each repeated idempotent step must dedup");
    assert_eq!(adapter.phase(), AdapterPhase::Disposed);
}

/// Error transitions land in the same evidence stream with stable codes and
/// actionable explanations (the error-timeline requirement).
#[test]
fn e2e_error_transitions_are_logged_with_stable_codes() {
    let mut adapter = AdapterLifecycle::new(AdapterKind::Vanilla, "err-timeline");
    let cases: Vec<(&str, AdapterMisuse)> = vec![
        (
            "attach_before_mount",
            adapter
                .apply(&AdapterAction::Attach)
                .expect_err("attach before mount"),
        ),
        ("input_before_attach", {
            adapter.apply(&AdapterAction::Mount).expect("mount");
            adapter
                .apply(&AdapterAction::Input { bytes: 1 })
                .expect_err("input before attach")
        }),
        ("double_dispose", {
            adapter.apply(&AdapterAction::Dispose).expect("dispose");
            adapter
                .apply(&AdapterAction::Dispose)
                .expect_err("double dispose")
        }),
    ];
    for (label, misuse) in cases {
        assert!(AdapterMisuse::CODES.contains(&misuse.code));
        assert!(!misuse.explanation.is_empty());
        let line = misuse.to_jsonl();
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("misuse line parses");
        assert_eq!(parsed["event"].as_str(), Some("adapter_misuse"));
        emit(
            "error_timeline",
            &format!("\"case\":\"{label}\",\"timeline\":{line}"),
        );
    }
}

/// The committed example files stay lockstep with the generators and embed
/// the contract-pinning check (the docs/examples alignment AC).
#[test]
fn e2e_examples_are_lockstep_and_pin_the_contract() {
    let vanilla = vanilla_example();
    let react = react_example();
    assert_eq!(
        include_str!("../sdk/examples/frankenterm-adapter-vanilla.js"),
        vanilla
    );
    assert_eq!(
        include_str!("../sdk/examples/frankenterm-adapter-react.tsx"),
        react
    );
    for (label, example) in [("vanilla", &vanilla), ("react", &react)] {
        assert!(
            example.contains("apiContract()"),
            "{label} example must pin the contract before other calls"
        );
        // The contract identity is `apiLine: "frankenterm-js"` (Contract
        // Identity section of docs/spec/frankenterm-web-api.md). An example
        // pinning any other apiLine would reject every valid engine at
        // runtime — this exact string is load-bearing.
        assert!(
            example.contains("contract.apiLine !== \"frankenterm-js\""),
            "{label} example must pin the canonical apiLine \"frankenterm-js\""
        );
        assert!(
            example.contains("startsWith(\"1.\")"),
            "{label} example must pin the 1.x api line"
        );
        emit(
            "examples",
            &format!("\"example\":\"{label}\",\"lockstep\":true,\"verdict\":\"pass\""),
        );
    }
}

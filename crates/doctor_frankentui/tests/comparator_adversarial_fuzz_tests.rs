use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use doctor_frankentui::accessibility_diff::{
    ACCESSIBILITY_DIFF_VALIDATOR_ID, AccessibilityAction, AccessibilityActionKind,
    AccessibilityDiffConfig, AccessibilityDiffVerdict, AccessibilityNode, AccessibilityRole,
    AccessibilityRun, AssistiveAnnouncement, FocusTransition, compare_accessibility_runs,
};
use doctor_frankentui::performance_diff::{
    PERFORMANCE_DIFF_VALIDATOR_ID, PerformanceDiffConfig, PerformanceDiffVerdict,
    PerformanceMetricKind, PerformanceRun, PerformanceSample, PerformanceWorkloadTrace,
    compare_performance_runs,
};
use doctor_frankentui::semantic_diff::{
    SEMANTIC_DIFF_VALIDATOR_ID, SemanticDiffVerdict, SemanticObservation, SemanticObservationKind,
    SemanticRun, compare_runs,
};
use doctor_frankentui::visual_diff::{
    TerminalCell, TerminalFrame, TerminalOutputRun, TerminalStyle, VISUAL_DIFF_VALIDATOR_ID,
    VisualDiffConfig, VisualDiffMode, VisualDiffVerdict, compare_terminal_runs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AdversarialKind {
    EventStorm,
    ResizeThrash,
    MalformedSourceConstruct,
}

#[derive(Debug)]
struct FuzzOutcome {
    comparator: &'static str,
    kind: AdversarialKind,
    seed: u64,
    verdict: &'static str,
    original_trace_len: usize,
    minimized_trace_len: usize,
    replay_key: String,
}

#[derive(Debug, Clone)]
struct ComparatorEvidence {
    comparator_id: String,
    fixture_id: String,
    threshold_class: String,
    verdict_reason: String,
    verdict_rank: u8,
}

impl ComparatorEvidence {
    fn assert_structured_log_fields(&self) {
        let log = serde_json::json!({
            "comparator_id": self.comparator_id,
            "fixture_id": self.fixture_id,
            "threshold_class": self.threshold_class,
            "verdict_reason": self.verdict_reason,
            "verdict_rank": self.verdict_rank,
        });

        assert!(
            log["comparator_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "structured comparator log must carry comparator_id"
        );
        assert!(
            log["fixture_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "structured comparator log must carry fixture_id"
        );
        assert!(
            log["threshold_class"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "structured comparator log must carry threshold_class"
        );
        assert!(
            log["verdict_reason"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "structured comparator log must carry verdict_reason"
        );
        assert!(
            log["verdict_rank"].as_u64().is_some(),
            "structured comparator log must carry machine-sortable verdict rank"
        );
    }
}

fn assert_panic_free<T>(case_id: &str, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => std::panic::resume_unwind(Box::new(format!(
            "adversarial comparator fuzz case panicked: {case_id}"
        ))),
    }
}

fn semantic_event_storm(seed: u64) -> FuzzOutcome {
    assert_panic_free("semantic-event-storm", || {
        let mut source = Vec::new();
        let mut translated = Vec::new();

        for index in 0..96 {
            let key = format!("state.slot.{}", index % 8);
            let value = format!("seed-{seed:x}-tick-{index}");
            let source_observation = SemanticObservation::new(
                index,
                u64::from(index) * 3,
                SemanticObservationKind::StateTransition,
                key,
                value,
            )
            .with_contract_clause_ids(vec!["ST-001".to_string()]);
            let mut translated_observation = source_observation.clone();
            if index == 37 {
                translated_observation.value = format!("seed-{seed:x}-tick-{index}-mutated");
            }
            source.push(source_observation);
            translated.push(translated_observation);
        }

        let source_run = SemanticRun::new(format!("source-event-storm-{seed:x}"), source)
            .with_replay_command(format!(
                "doctor_frankentui replay --seed {seed} --side source"
            ));
        let translated_run =
            SemanticRun::new(format!("translated-event-storm-{seed:x}"), translated)
                .with_replay_command(format!(
                    "doctor_frankentui replay --seed {seed} --side translated"
                ));
        let report = compare_runs(&source_run, &translated_run);
        assert_eq!(report.verdict, SemanticDiffVerdict::Violation);

        let counterexample = report
            .counterexample
            .as_ref()
            .expect("event storm violation must reduce to a replayable counterexample");
        assert_eq!(counterexample.divergence_index, 37);
        assert_eq!(counterexample.source_observations.len(), 1);
        assert_eq!(counterexample.translated_observations.len(), 1);
        assert!(counterexample.replay_command.contains(&seed.to_string()));

        FuzzOutcome {
            comparator: "semantic",
            kind: AdversarialKind::EventStorm,
            seed,
            verdict: "violation",
            original_trace_len: report.observations_compared,
            minimized_trace_len: counterexample.source_observations.len()
                + counterexample.translated_observations.len(),
            replay_key: counterexample.replay_command.clone(),
        }
    })
}

fn semantic_fixture_pair(
    fixture_id: &str,
    mutated_observations: u32,
) -> (SemanticRun, SemanticRun) {
    let mut source = Vec::new();
    let mut translated = Vec::new();

    for sequence in 0..4 {
        let observation = SemanticObservation::new(
            sequence,
            u64::from(sequence) * 10,
            SemanticObservationKind::StateTransition,
            format!("state.{sequence}"),
            format!("value.{sequence}"),
        )
        .with_contract_clause_ids(vec!["ST-001".to_string()]);
        let mut translated_observation = observation.clone();
        if sequence < mutated_observations {
            translated_observation.value = format!("mutated.{sequence}");
        }
        source.push(observation);
        translated.push(translated_observation);
    }

    (
        SemanticRun::new(format!("{fixture_id}:source"), source).with_replay_command(format!(
            "doctor_frankentui semantic-fixture {fixture_id} source"
        )),
        SemanticRun::new(format!("{fixture_id}:translated"), translated).with_replay_command(
            format!("doctor_frankentui semantic-fixture {fixture_id} translated"),
        ),
    )
}

fn semantic_evidence(fixture_id: &str, mutated_observations: u32) -> ComparatorEvidence {
    let (source, translated) = semantic_fixture_pair(fixture_id, mutated_observations);
    let report = compare_runs(&source, &translated);
    let threshold_class = report
        .violated_clause_ids
        .first()
        .or_else(|| report.covered_clause_ids.first())
        .cloned()
        .unwrap_or_else(|| "ST-001".to_string());
    let verdict_reason = report.differences.first().map_or_else(
        || "semantic observations are equivalent".to_string(),
        |diff| diff.message.clone(),
    );

    ComparatorEvidence {
        comparator_id: report.validator_id,
        fixture_id: report.source_run_id,
        threshold_class,
        verdict_reason,
        verdict_rank: semantic_verdict_rank(report.verdict),
    }
}

fn semantic_verdict_rank(verdict: SemanticDiffVerdict) -> u8 {
    match verdict {
        SemanticDiffVerdict::Equivalent => 0,
        SemanticDiffVerdict::AcceptableImprovement => 1,
        SemanticDiffVerdict::Violation => 2,
    }
}

fn decorative_style(fg: &str) -> TerminalStyle {
    TerminalStyle {
        fg: Some(fg.to_string()),
        bg: None,
        attrs: Vec::new(),
    }
}

fn decorative_frame(frame_index: u32, fg: &str) -> TerminalFrame {
    TerminalFrame::new(
        frame_index,
        1,
        1,
        vec![
            TerminalCell::new("x")
                .with_style(decorative_style(fg))
                .with_semantic_class("decorative_color"),
        ],
    )
}

fn visual_tolerance_config() -> VisualDiffConfig {
    VisualDiffConfig {
        mode: VisualDiffMode::Tolerance,
        strict_classes: vec!["command_output".to_string()],
        perceptual_classes: vec!["decorative_color".to_string()],
        max_perceptual_delta: 0.01,
    }
}

fn visual_evidence(fixture_id: &str, translated_color: &str) -> ComparatorEvidence {
    let source_run = TerminalOutputRun::new(
        format!("{fixture_id}:source"),
        vec![decorative_frame(0, "#000000")],
    )
    .with_replay_command(format!(
        "doctor_frankentui visual-fixture {fixture_id} source"
    ));
    let translated_run = TerminalOutputRun::new(
        format!("{fixture_id}:translated"),
        vec![decorative_frame(0, translated_color)],
    )
    .with_replay_command(format!(
        "doctor_frankentui visual-fixture {fixture_id} translated"
    ));
    let report = compare_terminal_runs(&source_run, &translated_run, &visual_tolerance_config());
    let threshold_class = report.differences.first().map_or_else(
        || "decorative_color<=0.01".to_string(),
        |diff| format!("{}>{}", diff.semantic_class, 0.01),
    );
    let verdict_reason = report.differences.first().map_or_else(
        || "decorative color delta stayed within tolerance".to_string(),
        |diff| diff.message.clone(),
    );

    ComparatorEvidence {
        comparator_id: report.validator_id,
        fixture_id: report.source_run_id,
        threshold_class,
        verdict_reason,
        verdict_rank: visual_verdict_rank(report.verdict),
    }
}

fn visual_verdict_rank(verdict: VisualDiffVerdict) -> u8 {
    match verdict {
        VisualDiffVerdict::Equivalent => 0,
        VisualDiffVerdict::WithinTolerance => 1,
        VisualDiffVerdict::Violation => 2,
    }
}

fn performance_fixture_run(fixture_id: &str, side: &str, latency_p99_ms: f64) -> PerformanceRun {
    let workload = PerformanceWorkloadTrace::new(
        "latency-workload",
        fixture_id,
        0x5EED,
        format!("{fixture_id}:trace"),
        128,
    )
    .with_controlled_inputs(vec![
        "viewport=80x24".to_string(),
        "events=stable".to_string(),
    ]);
    let samples = (0..5)
        .map(|sample_index| {
            PerformanceSample::new(
                fixture_id,
                PerformanceMetricKind::LatencyP99Ms,
                sample_index,
                latency_p99_ms,
                0x5EED,
                "latency-workload",
            )
            .with_artifact_id(format!("{fixture_id}:{side}:{sample_index}"))
        })
        .collect();

    PerformanceRun::new(format!("{fixture_id}:{side}"), vec![workload], samples)
        .with_replay_command(format!(
            "doctor_frankentui perf-fixture {fixture_id} {side}"
        ))
}

fn performance_evidence(fixture_id: &str, translated_latency_p99_ms: f64) -> ComparatorEvidence {
    let source = performance_fixture_run(fixture_id, "source", 100.0);
    let translated = performance_fixture_run(fixture_id, "translated", translated_latency_p99_ms);
    let report = compare_performance_runs(&source, &translated, &PerformanceDiffConfig::default());
    let threshold_class = report.comparisons.first().map_or_else(
        || "PD-002".to_string(),
        |comparison| {
            format!(
                "{}:{:.2}",
                comparison.policy_id, comparison.threshold.max_relative_regression
            )
        },
    );
    let verdict_reason = report.differences.first().map_or_else(
        || {
            report.comparisons.first().map_or_else(
                || "no comparable performance samples".to_string(),
                |comparison| comparison.message.clone(),
            )
        },
        |diff| diff.message.clone(),
    );

    ComparatorEvidence {
        comparator_id: report.validator_id,
        fixture_id: report.source_run_id,
        threshold_class,
        verdict_reason,
        verdict_rank: performance_verdict_rank(report.verdict),
    }
}

fn performance_verdict_rank(verdict: PerformanceDiffVerdict) -> u8 {
    match verdict {
        PerformanceDiffVerdict::Improvement => 0,
        PerformanceDiffVerdict::Equivalent => 1,
        PerformanceDiffVerdict::RegressionWithinPolicy => 2,
        PerformanceDiffVerdict::NeedsMoreEvidence => 3,
        PerformanceDiffVerdict::PolicyRegression => 4,
    }
}

fn accessible_button(name: &str, contrast_ratio: f32) -> AccessibilityNode {
    AccessibilityNode::new("submit", AccessibilityRole::Button)
        .with_name(name)
        .with_focus_order(1)
        .with_contrast_ratio(contrast_ratio)
        .with_action(AccessibilityAction::new(
            "activate-submit",
            AccessibilityActionKind::Activate,
            name,
        ))
        .with_source_ref("fixture.tsx:10")
}

fn accessibility_evidence(fixture_id: &str, translated_contrast_ratio: f32) -> ComparatorEvidence {
    let source = AccessibilityRun::new(
        format!("{fixture_id}:source"),
        vec![accessible_button("Submit", 4.8)],
    )
    .with_focus_transitions(vec![FocusTransition::new("submit", "submit", "Tab")])
    .with_replay_command(format!(
        "doctor_frankentui a11y-fixture {fixture_id} source"
    ));
    let translated = AccessibilityRun::new(
        format!("{fixture_id}:translated"),
        vec![accessible_button("Submit", translated_contrast_ratio)],
    )
    .with_focus_transitions(vec![FocusTransition::new("submit", "submit", "Tab")])
    .with_replay_command(format!(
        "doctor_frankentui a11y-fixture {fixture_id} translated"
    ));
    let report =
        compare_accessibility_runs(&source, &translated, &AccessibilityDiffConfig::default());
    let threshold_class = report
        .violated_policy_ids
        .first()
        .or_else(|| report.covered_policy_ids.first())
        .cloned()
        .unwrap_or_else(|| "AD-004".to_string());
    let verdict_reason = report.violations.first().map_or_else(
        || "accessibility policies are satisfied".to_string(),
        |violation| violation.message.clone(),
    );

    ComparatorEvidence {
        comparator_id: report.validator_id,
        fixture_id: report.source_run_id,
        threshold_class,
        verdict_reason,
        verdict_rank: accessibility_verdict_rank(report.verdict),
    }
}

fn accessibility_verdict_rank(verdict: AccessibilityDiffVerdict) -> u8 {
    match verdict {
        AccessibilityDiffVerdict::Equivalent => 0,
        AccessibilityDiffVerdict::Improved => 0,
        AccessibilityDiffVerdict::Violation => 1,
    }
}

fn visual_resize_thrash(seed: u64) -> FuzzOutcome {
    assert_panic_free("visual-resize-thrash", || {
        let mut source_frames = Vec::new();
        let mut translated_frames = Vec::new();

        for frame_index in 0..32 {
            let width = 2 + ((seed as u16).wrapping_add(frame_index as u16) % 19);
            let height = 1 + (frame_index as u16 % 5);
            let cell_count = usize::from(width) * usize::from(height);
            let source_hash = format!(
                "{:064x}",
                seed.wrapping_mul(131).wrapping_add(u64::from(frame_index))
            );
            let translated_hash = if frame_index == 19 {
                format!(
                    "{:064x}",
                    seed.wrapping_mul(197).wrapping_add(u64::from(frame_index))
                )
            } else {
                source_hash.clone()
            };
            source_frames.push(
                TerminalFrame::digest_only(frame_index, width, height, source_hash)
                    .with_source_artifact(format!("resize/source/{seed:x}/{frame_index}.ansi")),
            );
            translated_frames.push(
                TerminalFrame::new(frame_index, width, height, Vec::new())
                    .with_source_artifact(format!("resize/translated/{seed:x}/{frame_index}.ansi")),
            );
            translated_frames
                .last_mut()
                .expect("frame just pushed")
                .content_hash = Some(translated_hash);
            assert!(cell_count > 0);
        }

        let source_run = TerminalOutputRun::new(format!("source-resize-{seed:x}"), source_frames)
            .with_replay_command(format!("doctor_frankentui replay-visual --seed {seed}"));
        let translated_run =
            TerminalOutputRun::new(format!("translated-resize-{seed:x}"), translated_frames)
                .with_replay_command(format!(
                    "doctor_frankentui replay-visual --seed {seed} --translated"
                ));
        let report =
            compare_terminal_runs(&source_run, &translated_run, &VisualDiffConfig::strict());
        assert_eq!(report.verdict, VisualDiffVerdict::Violation);
        assert!(
            report
                .differences
                .iter()
                .any(|difference| difference.frame_index == 19),
            "resize thrash corpus should retain the seeded divergent frame"
        );

        let bundle = report
            .artifact_bundle
            .as_ref()
            .expect("visual violations must emit replay artifacts");
        assert!(bundle.replay_command.contains(&seed.to_string()));

        FuzzOutcome {
            comparator: "visual",
            kind: AdversarialKind::ResizeThrash,
            seed,
            verdict: "violation",
            original_trace_len: report.frames_compared,
            minimized_trace_len: report.differences.len(),
            replay_key: bundle.replay_command.clone(),
        }
    })
}

fn performance_malformed_source(seed: u64) -> FuzzOutcome {
    assert_panic_free("performance-malformed-source", || {
        let source_workload = PerformanceWorkloadTrace::new(
            "resize-loop",
            "malformed-source",
            seed,
            "source-trace-hash",
            128,
        )
        .with_controlled_inputs(vec!["resize:80x24".to_string(), "resize:1x1".to_string()]);
        let translated_workload = PerformanceWorkloadTrace::new(
            "resize-loop",
            "malformed-source",
            seed + 1,
            "translated-trace-hash",
            128,
        )
        .with_controlled_inputs(vec![
            "resize:80x24".to_string(),
            "resize:9999x1".to_string(),
        ]);

        let mut source_samples = Vec::new();
        let mut translated_samples = Vec::new();
        for sample_index in 0..5 {
            source_samples.push(PerformanceSample::new(
                "malformed-source",
                PerformanceMetricKind::LatencyP99Ms,
                sample_index,
                if sample_index == 2 { f64::NAN } else { 12.0 },
                seed,
                "resize-loop",
            ));
            translated_samples.push(PerformanceSample::new(
                "malformed-source",
                PerformanceMetricKind::LatencyP99Ms,
                sample_index,
                29.0 + f64::from(sample_index),
                seed + 1,
                "resize-loop",
            ));
        }

        let source_run = PerformanceRun::new(
            format!("source-performance-{seed:x}"),
            vec![source_workload],
            source_samples,
        )
        .with_replay_command(format!("doctor_frankentui perf-replay --seed {seed}"));
        let translated_run = PerformanceRun::new(
            format!("translated-performance-{seed:x}"),
            vec![translated_workload],
            translated_samples,
        )
        .with_replay_command(format!(
            "doctor_frankentui perf-replay --seed {seed} --translated"
        ));
        let report = compare_performance_runs(
            &source_run,
            &translated_run,
            &PerformanceDiffConfig::certification_default(),
        );
        assert_eq!(report.verdict, PerformanceDiffVerdict::PolicyRegression);
        assert!(
            !report.differences.is_empty(),
            "malformed performance controls should be reported as policy differences"
        );
        assert!(
            !report.violated_policy_ids.is_empty(),
            "malformed performance controls should identify violated policy IDs"
        );

        FuzzOutcome {
            comparator: "performance",
            kind: AdversarialKind::MalformedSourceConstruct,
            seed,
            verdict: "violation",
            original_trace_len: source_run.samples.len() + translated_run.samples.len(),
            minimized_trace_len: report.differences.len().max(1),
            replay_key: report
                .artifact_bundle
                .as_ref()
                .map_or_else(String::new, |bundle| bundle.replay_command.clone()),
        }
    })
}

fn accessibility_malformed_focus(seed: u64) -> FuzzOutcome {
    assert_panic_free("accessibility-malformed-focus", || {
        let source_nodes = vec![
            AccessibilityNode::new("root", AccessibilityRole::Group)
                .with_name("Root")
                .with_source_ref("source.tsx:1"),
            AccessibilityNode::new("submit", AccessibilityRole::Button)
                .with_name("Submit")
                .with_focus_order(1)
                .with_contrast_ratio(4.8)
                .with_action(AccessibilityAction::new(
                    "activate-submit",
                    AccessibilityActionKind::Activate,
                    "Submit",
                ))
                .with_source_ref("source.tsx:12"),
            AccessibilityNode::new("cancel", AccessibilityRole::Button)
                .with_name("Cancel")
                .with_focus_order(2)
                .with_contrast_ratio(4.7)
                .with_action(AccessibilityAction::new(
                    "activate-cancel",
                    AccessibilityActionKind::Activate,
                    "Cancel",
                ))
                .with_source_ref("source.tsx:13"),
        ];
        let translated_nodes = vec![
            AccessibilityNode::new("submit", AccessibilityRole::Button)
                .with_focus_order(1)
                .with_contrast_ratio(2.2)
                .with_action(
                    AccessibilityAction::new(
                        "activate-submit",
                        AccessibilityActionKind::Activate,
                        "",
                    )
                    .disabled(),
                )
                .with_source_ref("translated.rs:44"),
            AccessibilityNode::new("cancel", AccessibilityRole::Button)
                .with_name("Cancel")
                .with_focus_order(2)
                .with_contrast_ratio(4.7)
                .with_action(AccessibilityAction::new(
                    "activate-cancel",
                    AccessibilityActionKind::Activate,
                    "Cancel",
                ))
                .with_source_ref("translated.rs:45"),
        ];

        let source_run = AccessibilityRun::new(format!("source-a11y-{seed:x}"), source_nodes)
            .with_focus_transitions(vec![FocusTransition::new("submit", "cancel", "Tab")])
            .with_announcements(vec![AssistiveAnnouncement::new(
                "submit-ready",
                Some("submit".to_string()),
                "Submit ready",
                "polite",
            )])
            .with_replay_command(format!("doctor_frankentui a11y-replay --seed {seed}"));
        let translated_run =
            AccessibilityRun::new(format!("translated-a11y-{seed:x}"), translated_nodes)
                .with_focus_transitions(vec![
                    FocusTransition::new("submit", "missing-node", "Tab"),
                    FocusTransition::new("missing-node", "submit", "Shift+Tab"),
                ])
                .with_replay_command(format!(
                    "doctor_frankentui a11y-replay --seed {seed} --translated"
                ));
        let report = compare_accessibility_runs(
            &source_run,
            &translated_run,
            &AccessibilityDiffConfig::default(),
        );
        assert_eq!(report.verdict, AccessibilityDiffVerdict::Violation);
        assert!(
            report.violations.len() >= 3,
            "malformed accessibility artifacts should surface multiple ranked violations"
        );
        assert!(
            report
                .violations
                .windows(2)
                .all(|pair| pair[0].risk_level >= pair[1].risk_level)
        );

        FuzzOutcome {
            comparator: "accessibility",
            kind: AdversarialKind::MalformedSourceConstruct,
            seed,
            verdict: "violation",
            original_trace_len: source_run.nodes.len()
                + translated_run.nodes.len()
                + source_run.focus_transitions.len()
                + translated_run.focus_transitions.len(),
            minimized_trace_len: report.violations.len(),
            replay_key: source_run.replay_command.unwrap_or_default(),
        }
    })
}

fn run_adversarial_corpus() -> Vec<FuzzOutcome> {
    vec![
        semantic_event_storm(0x00C0_FFEE_0001),
        visual_resize_thrash(0x00C0_FFEE_0002),
        performance_malformed_source(0x00C0_FFEE_0003),
        accessibility_malformed_focus(0x00C0_FFEE_0004),
    ]
}

#[test]
fn adversarial_corpus_covers_required_scenarios() {
    let outcomes = run_adversarial_corpus();
    let kinds = outcomes
        .iter()
        .map(|outcome| outcome.kind)
        .collect::<BTreeSet<_>>();
    let comparators = outcomes
        .iter()
        .map(|outcome| outcome.comparator)
        .collect::<BTreeSet<_>>();
    let seeds = outcomes
        .iter()
        .map(|outcome| outcome.seed)
        .collect::<BTreeSet<_>>();

    assert!(kinds.contains(&AdversarialKind::EventStorm));
    assert!(kinds.contains(&AdversarialKind::ResizeThrash));
    assert!(kinds.contains(&AdversarialKind::MalformedSourceConstruct));
    assert_eq!(
        comparators,
        BTreeSet::from(["accessibility", "performance", "semantic", "visual"])
    );
    assert_eq!(
        seeds.len(),
        outcomes.len(),
        "each fuzz case needs a stable unique seed"
    );
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.verdict == "violation")
    );
}

#[test]
fn comparator_failures_are_reproducible_and_minimized() {
    for outcome in run_adversarial_corpus() {
        assert!(
            outcome.original_trace_len >= outcome.minimized_trace_len,
            "{} should not grow while reducing the failing trace",
            outcome.comparator
        );
        assert!(
            outcome.minimized_trace_len > 0,
            "{} should retain a non-empty minimized failure artifact",
            outcome.comparator
        );
        assert!(
            outcome.replay_key.contains(&outcome.seed.to_string())
                || outcome.comparator == "performance",
            "{} replay key should carry the deterministic seed",
            outcome.comparator
        );
    }
}

#[test]
fn malformed_artifacts_do_not_panic_any_comparator() {
    let outcomes = run_adversarial_corpus();
    assert_eq!(outcomes.len(), 4);
}

#[test]
fn synthetic_fixture_matrix_covers_positive_negative_and_tolerance_boundaries() {
    let evidence = vec![
        semantic_evidence("semantic-false-positive-guard", 0),
        semantic_evidence("semantic-true-positive-violation", 1),
        visual_evidence("visual-tolerance-boundary-pass", "#010101"),
        visual_evidence("visual-tolerance-boundary-fail", "#050505"),
        performance_evidence("performance-false-positive-guard", 100.0),
        performance_evidence("performance-threshold-policy-fail", 120.0),
        accessibility_evidence("accessibility-false-positive-guard", 4.8),
        accessibility_evidence("accessibility-threshold-policy-fail", 3.1),
    ];

    for record in &evidence {
        record.assert_structured_log_fields();
    }

    assert_eq!(evidence[0].comparator_id, SEMANTIC_DIFF_VALIDATOR_ID);
    assert_eq!(evidence[0].verdict_rank, 0);
    assert_eq!(evidence[1].verdict_rank, 2);
    assert_eq!(evidence[2].comparator_id, VISUAL_DIFF_VALIDATOR_ID);
    assert_eq!(evidence[2].verdict_rank, 1);
    assert_eq!(evidence[3].verdict_rank, 2);
    assert_eq!(evidence[4].comparator_id, PERFORMANCE_DIFF_VALIDATOR_ID);
    assert_eq!(evidence[4].verdict_rank, 1);
    assert_eq!(evidence[5].verdict_rank, 4);
    assert_eq!(evidence[6].comparator_id, ACCESSIBILITY_DIFF_VALIDATOR_ID);
    assert_eq!(evidence[6].verdict_rank, 0);
    assert_eq!(evidence[7].verdict_rank, 1);
}

#[test]
fn comparator_scores_are_deterministic_and_monotone_across_boundaries() {
    let semantic_scores = [0, 1, 2, 3]
        .into_iter()
        .map(|mutations| semantic_evidence("semantic-monotone", mutations).verdict_rank)
        .collect::<Vec<_>>();
    assert!(
        semantic_scores.windows(2).all(|pair| pair[0] <= pair[1]),
        "semantic verdict severity should not decrease as mutations increase: {semantic_scores:?}"
    );

    let performance_scores = [100.0, 108.0, 120.0]
        .into_iter()
        .map(|latency| performance_evidence("performance-monotone", latency).verdict_rank)
        .collect::<Vec<_>>();
    assert_eq!(
        performance_scores,
        vec![1, 2, 4],
        "performance scorer should classify equal, within-policy regression, then policy regression"
    );

    let replay_a = performance_evidence("performance-deterministic", 120.0);
    let replay_b = performance_evidence("performance-deterministic", 120.0);
    assert_eq!(replay_a.comparator_id, replay_b.comparator_id);
    assert_eq!(replay_a.fixture_id, replay_b.fixture_id);
    assert_eq!(replay_a.threshold_class, replay_b.threshold_class);
    assert_eq!(replay_a.verdict_reason, replay_b.verdict_reason);
    assert_eq!(replay_a.verdict_rank, replay_b.verdict_rank);
}

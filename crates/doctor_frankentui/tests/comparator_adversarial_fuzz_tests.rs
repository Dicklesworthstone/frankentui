use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use doctor_frankentui::accessibility_diff::{
    AccessibilityAction, AccessibilityActionKind, AccessibilityDiffConfig,
    AccessibilityDiffVerdict, AccessibilityNode, AccessibilityRole, AccessibilityRun,
    AssistiveAnnouncement, FocusTransition, compare_accessibility_runs,
};
use doctor_frankentui::performance_diff::{
    PerformanceDiffConfig, PerformanceDiffVerdict, PerformanceMetricKind, PerformanceRun,
    PerformanceSample, PerformanceWorkloadTrace, compare_performance_runs,
};
use doctor_frankentui::semantic_diff::{
    SemanticDiffVerdict, SemanticObservation, SemanticObservationKind, SemanticRun, compare_runs,
};
use doctor_frankentui::visual_diff::{
    TerminalFrame, TerminalOutputRun, VisualDiffConfig, VisualDiffVerdict, compare_terminal_runs,
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

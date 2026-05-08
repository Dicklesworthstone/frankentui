use doctor_frankentui::semantic_contract::TransformationRiskLevel;
use doctor_frankentui::semantic_diff::{
    SemanticDiffVerdict, SemanticDifferenceKind, compare_traces,
};
use doctor_frankentui::trace::{InteractionTrace, TraceBuilder, TracePayload, Viewport};

fn viewport() -> Viewport {
    Viewport {
        width: 80,
        height: 24,
    }
}

fn trace_with(
    trace_id: &str,
    run_id: &str,
    state_hash: &str,
    include_effect: bool,
    improvement: Option<&str>,
) -> InteractionTrace {
    let mut builder = TraceBuilder::new(trace_id, run_id, viewport()).with_metadata(
        "replay_command",
        format!("doctor_frankentui replay --trace-id {trace_id}"),
    );
    builder.record(
        0,
        TracePayload::Key {
            key: "Enter".to_string(),
            modifiers: vec![],
            action: doctor_frankentui::trace::KeyAction::Press,
        },
    );
    builder.record(
        5,
        TracePayload::StateCapture {
            state_hash: state_hash.to_string(),
            component: Some("Counter".to_string()),
        },
    );
    if include_effect {
        builder.record(
            8,
            TracePayload::Marker {
                name: "effect:network.fetch=/api/count".to_string(),
            },
        );
    }
    if let Some(name) = improvement {
        builder.record(
            10,
            TracePayload::Marker {
                name: name.to_string(),
            },
        );
    }
    builder.build()
}

#[test]
fn equivalent_traces_have_accepting_semantic_diff_report() {
    let source = trace_with("source-trace", "source-run", "state-a", true, None);
    let translated = trace_with("translated-trace", "translated-run", "state-a", true, None);

    let report = compare_traces(&source, &translated);

    assert_eq!(report.verdict, SemanticDiffVerdict::Equivalent);
    assert!(report.differences.is_empty());
    assert!(report.counterexample.is_none());
    assert_eq!(report.risk_level, TransformationRiskLevel::Low);
    assert_eq!(report.risk_score, 0.0);
    assert!(report.covered_clause_ids.contains(&"EO-001".to_string()));
    assert!(report.covered_clause_ids.contains(&"ST-001".to_string()));
    assert!(report.covered_clause_ids.contains(&"SE-001".to_string()));
    assert!(report.expected_loss.claim_id.is_none());
    assert_eq!(
        report.expected_loss.policy_id.as_deref(),
        Some("semantic_diff_validator")
    );
}

#[test]
fn translated_allowed_improvement_is_classified_without_violation() {
    let source = trace_with("source-trace", "source-run", "state-a", true, None);
    let translated = trace_with(
        "translated-trace",
        "translated-run",
        "state-a",
        true,
        Some("improvement:accessibility=aria-labels-added"),
    );

    let report = compare_traces(&source, &translated);

    assert_eq!(report.verdict, SemanticDiffVerdict::AcceptableImprovement);
    assert!(report.differences.is_empty());
    assert!(report.counterexample.is_none());
    assert_eq!(report.risk_level, TransformationRiskLevel::Low);
    assert!(report.covered_clause_ids.contains(&"IE-001".to_string()));
}

#[test]
fn state_transition_mismatch_reports_minimal_counterexample() {
    let source = trace_with("source-trace", "source-run", "state-a", true, None);
    let translated = trace_with("translated-trace", "translated-run", "state-b", true, None);

    let report = compare_traces(&source, &translated);

    assert_eq!(report.verdict, SemanticDiffVerdict::Violation);
    assert_eq!(report.risk_level, TransformationRiskLevel::Critical);
    assert!(report.risk_score > 0.0);
    assert_eq!(report.differences.len(), 1);
    assert_eq!(
        report.differences[0].difference_kind,
        SemanticDifferenceKind::ValueMismatch
    );
    assert!(report.violated_clause_ids.contains(&"ST-001".to_string()));
    assert_eq!(report.expected_loss.claim_id.as_deref(), Some("ST-001"));

    let counterexample = report
        .counterexample
        .expect("violation must include counterexample");
    assert_eq!(counterexample.divergence_index, 1);
    assert_eq!(counterexample.source_observations.len(), 1);
    assert_eq!(counterexample.translated_observations.len(), 1);
    assert_eq!(counterexample.source_observations[0].value, "state-a");
    assert_eq!(counterexample.translated_observations[0].value, "state-b");
    assert!(counterexample.replay_command.contains("source-trace"));
    assert!(counterexample.replay_command.contains("translated-trace"));
}

#[test]
fn dropped_side_effect_is_a_contract_violation() {
    let source = trace_with("source-trace", "source-run", "state-a", true, None);
    let translated = trace_with("translated-trace", "translated-run", "state-a", false, None);

    let report = compare_traces(&source, &translated);

    assert_eq!(report.verdict, SemanticDiffVerdict::Violation);
    assert_eq!(report.differences.len(), 1);
    assert_eq!(
        report.differences[0].difference_kind,
        SemanticDifferenceKind::MissingObservation
    );
    assert_eq!(report.differences[0].key, "network.fetch");
    assert!(report.violated_clause_ids.contains(&"SE-001".to_string()));
}

#[test]
fn forbidden_improvement_is_reported_against_improvement_envelope() {
    let source = trace_with("source-trace", "source-run", "state-a", true, None);
    let translated = trace_with(
        "translated-trace",
        "translated-run",
        "state-a",
        true,
        Some("improvement:behavioral_semantics_change_without_clause_exemption=faster"),
    );

    let report = compare_traces(&source, &translated);

    assert_eq!(report.verdict, SemanticDiffVerdict::Violation);
    assert_eq!(report.risk_level, TransformationRiskLevel::Critical);
    assert_eq!(
        report.differences[0].difference_kind,
        SemanticDifferenceKind::ForbiddenImprovement
    );
    assert_eq!(report.differences[0].clause_ids, vec!["IE-002".to_string()]);
    let counterexample = report
        .counterexample
        .expect("forbidden improvement must include counterexample");
    assert!(counterexample.source_observations.is_empty());
    assert_eq!(counterexample.translated_observations.len(), 1);
    assert_eq!(
        counterexample.translated_observations[0].key,
        "behavioral_semantics_change_without_clause_exemption"
    );
}

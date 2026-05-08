use std::collections::BTreeSet;

use doctor_frankentui::proof_artifacts::{
    ProofObligationStatus, ProofWitnessKind, SEMANTIC_PROOF_ARTIFACT_SCHEMA_VERSION, VerdictClause,
    build_semantic_proof_artifact, parse_and_verify_semantic_proof_artifact,
    verify_semantic_proof_artifact,
};
use doctor_frankentui::semantic_diff::{
    SemanticDiffVerdict, SemanticObservation, SemanticObservationKind, SemanticRun, compare_runs,
};

fn observation(
    sequence: u32,
    kind: SemanticObservationKind,
    key: &str,
    value: &str,
    clause_id: &str,
) -> SemanticObservation {
    SemanticObservation::new(sequence, u64::from(sequence) * 10, kind, key, value)
        .with_contract_clause_ids(vec![clause_id.to_string()])
}

fn equivalent_runs() -> (SemanticRun, SemanticRun) {
    let source = SemanticRun::new(
        "source-equivalent",
        vec![
            observation(
                1,
                SemanticObservationKind::EventOrdering,
                "key:enter",
                "pressed",
                "EO-001",
            ),
            observation(
                2,
                SemanticObservationKind::StateTransition,
                "state.count",
                "1",
                "ST-001",
            ),
        ],
    )
    .with_replay_command("doctor_frankentui replay --seed 11 --side source");
    let translated = SemanticRun::new(
        "translated-equivalent",
        vec![
            observation(
                1,
                SemanticObservationKind::EventOrdering,
                "key:enter",
                "pressed",
                "EO-001",
            ),
            observation(
                2,
                SemanticObservationKind::StateTransition,
                "state.count",
                "1",
                "ST-001",
            ),
        ],
    )
    .with_replay_command("doctor_frankentui replay --seed 11 --side translated");

    (source, translated)
}

#[test]
fn proof_artifact_links_every_passing_clause_to_concrete_witnesses() {
    let (source, translated) = equivalent_runs();
    let report = compare_runs(&source, &translated);
    assert_eq!(report.verdict, SemanticDiffVerdict::Equivalent);

    let artifact = build_semantic_proof_artifact(&source, &translated, &report)
        .expect("equivalent semantic report should build proof artifact");
    let validation =
        verify_semantic_proof_artifact(&artifact, &report).expect("proof artifact should verify");

    assert_eq!(
        artifact.schema_version,
        SEMANTIC_PROOF_ARTIFACT_SCHEMA_VERSION
    );
    assert!(validation.machine_verifiable);
    assert!(validation.certification_passed);
    assert!(artifact.certification_passed);

    let clause_ids = artifact
        .obligations
        .iter()
        .map(|obligation| obligation.clause_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(clause_ids, BTreeSet::from(["EO-001", "ST-001"]));

    for obligation in &artifact.obligations {
        assert_eq!(obligation.verdict_clause, VerdictClause::Covered);
        assert_eq!(obligation.status, ProofObligationStatus::Proven);
        assert!(!obligation.witnesses.is_empty());
        assert!(
            obligation
                .witnesses
                .iter()
                .all(|witness| witness.clause_id == obligation.clause_id)
        );
        assert!(
            obligation
                .witnesses
                .iter()
                .all(|witness| witness.witness_kind == ProofWitnessKind::MatchedObservation)
        );
    }
}

#[test]
fn acceptable_improvement_artifact_carries_improvement_witness() {
    let source = SemanticRun::new(
        "source-improvement",
        vec![observation(
            1,
            SemanticObservationKind::StateTransition,
            "state.count",
            "1",
            "ST-001",
        )],
    );
    let translated = SemanticRun::new(
        "translated-improvement",
        vec![
            observation(
                1,
                SemanticObservationKind::StateTransition,
                "state.count",
                "1",
                "ST-001",
            ),
            observation(
                2,
                SemanticObservationKind::Improvement,
                "latency",
                "p99-ms=12",
                "IE-001",
            ),
        ],
    )
    .with_replay_command("doctor_frankentui replay --seed 21 --translated");
    let report = compare_runs(&source, &translated);
    assert_eq!(report.verdict, SemanticDiffVerdict::AcceptableImprovement);

    let artifact = build_semantic_proof_artifact(&source, &translated, &report)
        .expect("acceptable improvement should build proof artifact");
    let validation = verify_semantic_proof_artifact(&artifact, &report)
        .expect("acceptable improvement proof should verify");

    assert!(validation.machine_verifiable);
    assert!(validation.certification_passed);
    assert!(artifact.certification_passed);
    let improvement_obligation = artifact
        .obligations
        .iter()
        .find(|obligation| obligation.clause_id == "IE-001")
        .expect("allowed improvement clause must have an obligation");
    assert!(
        improvement_obligation
            .witnesses
            .iter()
            .any(|witness| witness.witness_kind == ProofWitnessKind::ImprovementObservation)
    );
}

#[test]
fn violation_artifact_is_machine_verifiable_but_not_certification_passing() {
    let source = SemanticRun::new(
        "source-violation",
        vec![observation(
            1,
            SemanticObservationKind::StateTransition,
            "state.count",
            "1",
            "ST-001",
        )],
    )
    .with_replay_command("doctor_frankentui replay --seed 31 --source");
    let translated = SemanticRun::new(
        "translated-violation",
        vec![observation(
            1,
            SemanticObservationKind::StateTransition,
            "state.count",
            "2",
            "ST-001",
        )],
    )
    .with_replay_command("doctor_frankentui replay --seed 31 --translated");
    let report = compare_runs(&source, &translated);
    assert_eq!(report.verdict, SemanticDiffVerdict::Violation);

    let artifact = build_semantic_proof_artifact(&source, &translated, &report)
        .expect("violation should still build replayable proof artifact");
    let validation = verify_semantic_proof_artifact(&artifact, &report)
        .expect("violation proof should be structurally verifiable");

    assert!(validation.machine_verifiable);
    assert!(!validation.certification_passed);
    assert!(!artifact.certification_passed);
    let obligation = artifact
        .obligations
        .iter()
        .find(|obligation| obligation.clause_id == "ST-001")
        .expect("violated clause must have an obligation");
    assert_eq!(obligation.verdict_clause, VerdictClause::Violated);
    assert_eq!(obligation.status, ProofObligationStatus::Refuted);
    assert!(
        obligation
            .witnesses
            .iter()
            .any(|witness| witness.witness_kind == ProofWitnessKind::CounterexampleDifference)
    );
}

#[test]
fn proof_artifact_schema_is_replay_stable() {
    let (source, translated) = equivalent_runs();
    let report = compare_runs(&source, &translated);

    let artifact_a = build_semantic_proof_artifact(&source, &translated, &report)
        .expect("first proof artifact should build");
    let artifact_b = build_semantic_proof_artifact(&source, &translated, &report)
        .expect("second proof artifact should build");

    assert_eq!(artifact_a, artifact_b);
    assert_eq!(artifact_a.artifact_checksum, artifact_b.artifact_checksum);
    assert_eq!(
        serde_json::to_string(&artifact_a).expect("proof artifact should serialize"),
        serde_json::to_string(&artifact_b).expect("proof artifact should serialize")
    );

    let raw_json =
        serde_json::to_string(&artifact_a).expect("proof artifact should serialize for parse");
    let validation = parse_and_verify_semantic_proof_artifact(&raw_json, &report)
        .expect("serialized artifact should parse and verify");
    assert!(validation.machine_verifiable);
    assert!(validation.certification_passed);
}

#[test]
fn missing_or_invalid_obligations_force_non_passing_certification() {
    let (source, translated) = equivalent_runs();
    let report = compare_runs(&source, &translated);
    let artifact = build_semantic_proof_artifact(&source, &translated, &report)
        .expect("baseline artifact should build");

    let mut missing = artifact.clone();
    missing
        .obligations
        .retain(|obligation| obligation.clause_id != "ST-001");
    let missing_validation = verify_semantic_proof_artifact(&missing, &report)
        .expect("missing-obligation artifact should produce validation report");
    assert!(!missing_validation.machine_verifiable);
    assert!(!missing_validation.certification_passed);
    assert_eq!(missing_validation.missing_clause_ids, vec!["ST-001"]);
    assert!(
        missing_validation
            .issues
            .iter()
            .any(|issue| issue.code == "missing_obligation")
    );

    let mut invalid = artifact;
    let first_obligation = invalid
        .obligations
        .first_mut()
        .expect("baseline artifact must have at least one obligation");
    let first_witness = first_obligation
        .witnesses
        .first_mut()
        .expect("baseline obligation must have at least one witness");
    first_witness.message.clear();
    let invalid_validation = verify_semantic_proof_artifact(&invalid, &report)
        .expect("invalid-obligation artifact should produce validation report");
    assert!(!invalid_validation.machine_verifiable);
    assert!(!invalid_validation.certification_passed);
    assert!(
        invalid_validation
            .issues
            .iter()
            .any(|issue| issue.code == "empty_witness_message")
    );
    assert!(
        invalid_validation
            .issues
            .iter()
            .any(|issue| issue.code == "artifact_checksum_mismatch")
    );
}

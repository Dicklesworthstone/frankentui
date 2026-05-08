use doctor_frankentui::accessibility_diff::{
    AccessibilityAction, AccessibilityActionKind, AccessibilityDiffConfig, AccessibilityNode,
    AccessibilityRole, AccessibilityRun, compare_accessibility_runs,
};
use doctor_frankentui::certification_report::{
    CERTIFICATION_REPORT_SCHEMA_VERSION, CertificationClauseStatus, CertificationPolicyProfile,
    CertificationRemediationAction, CertificationReportInput, CertificationStageStatus,
    generate_certification_report, verify_certification_report_checksum,
};
use doctor_frankentui::performance_diff::{
    PerformanceDiffConfig, PerformanceMetricKind, PerformanceRun, PerformanceSample,
    PerformanceWorkloadTrace, compare_performance_runs,
};
use doctor_frankentui::proof_artifacts::{
    build_semantic_proof_artifact, verify_semantic_proof_artifact,
};
use doctor_frankentui::semantic_contract::{
    BayesianPosterior, ExpectedLossResult, IpArtifactRecord, IpArtifactStatus, MigrationDecision,
    ProvenanceChainRecord, ProvenanceReport, VerdictOutcome,
};
use doctor_frankentui::semantic_diff::{
    SemanticObservation, SemanticObservationKind, SemanticRun, compare_runs,
};
use doctor_frankentui::visual_diff::{
    TerminalCell, TerminalFrame, TerminalOutputRun, TerminalStyle, VisualDiffConfig,
    compare_terminal_runs,
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

fn semantic_runs() -> (SemanticRun, SemanticRun) {
    let source = SemanticRun::new(
        "source-semantic",
        vec![observation(
            1,
            SemanticObservationKind::StateTransition,
            "state.count",
            "1",
            "ST-001",
        )],
    )
    .with_replay_command("doctor_frankentui replay --source source-semantic");
    let translated = SemanticRun::new(
        "translated-semantic",
        vec![observation(
            1,
            SemanticObservationKind::StateTransition,
            "state.count",
            "1",
            "ST-001",
        )],
    )
    .with_replay_command("doctor_frankentui replay --translated translated-semantic");
    (source, translated)
}

fn visual_report(strict: bool) -> doctor_frankentui::visual_diff::VisualDiffReport {
    if strict {
        let source = TerminalOutputRun::new(
            "source-visual",
            vec![TerminalFrame::from_text(0, "status: ok")],
        );
        let translated = TerminalOutputRun::new(
            "translated-visual",
            vec![TerminalFrame::from_text(0, "status: ok")],
        );
        compare_terminal_runs(&source, &translated, &VisualDiffConfig::strict())
    } else {
        let source = TerminalOutputRun::new(
            "source-visual",
            vec![TerminalFrame::new(
                0,
                1,
                1,
                vec![
                    TerminalCell::new("x")
                        .with_style(TerminalStyle {
                            fg: Some("#ffffff".to_string()),
                            bg: None,
                            attrs: Vec::new(),
                        })
                        .with_semantic_class("decorative_color"),
                ],
            )],
        );
        let translated = TerminalOutputRun::new(
            "translated-visual",
            vec![TerminalFrame::new(
                0,
                1,
                1,
                vec![
                    TerminalCell::new("x")
                        .with_style(TerminalStyle {
                            fg: Some("#fefefe".to_string()),
                            bg: None,
                            attrs: Vec::new(),
                        })
                        .with_semantic_class("decorative_color"),
                ],
            )],
        );
        compare_terminal_runs(&source, &translated, &VisualDiffConfig::tolerance())
    }
}

fn performance_report() -> doctor_frankentui::performance_diff::PerformanceDiffReport {
    let workload =
        PerformanceWorkloadTrace::new("workload-scroll", "scroll", 42, "trace-hash-scroll", 128);
    let samples = |run_id: &str, value: f64| {
        let run_workload = workload.clone();
        let run_samples = (0..8)
            .map(|index| {
                PerformanceSample::new(
                    "scroll",
                    PerformanceMetricKind::LatencyP99Ms,
                    index,
                    value,
                    42,
                    "workload-scroll",
                )
            })
            .collect::<Vec<_>>();
        PerformanceRun::new(run_id, vec![run_workload], run_samples)
    };
    compare_performance_runs(
        &samples("source-performance", 100.0),
        &samples("translated-performance", 80.0),
        &PerformanceDiffConfig::certification_default(),
    )
}

fn accessibility_report() -> doctor_frankentui::accessibility_diff::AccessibilityDiffReport {
    let button = |run_id: &str| {
        AccessibilityRun::new(
            run_id,
            vec![
                AccessibilityNode::new("save", AccessibilityRole::Button)
                    .with_name("Save")
                    .with_focus_order(0)
                    .with_action(AccessibilityAction::new(
                        "activate",
                        AccessibilityActionKind::Activate,
                        "Activate",
                    ))
                    .with_contrast_ratio(5.0),
            ],
        )
    };
    compare_accessibility_runs(
        &button("source-accessibility"),
        &button("translated-accessibility"),
        &AccessibilityDiffConfig::default(),
    )
}

fn high_confidence() -> ExpectedLossResult {
    ExpectedLossResult {
        decision: MigrationDecision::AutoApprove,
        posterior: BayesianPosterior {
            alpha: 99.0,
            beta: 2.0,
            mean: 0.98,
            variance: 0.0001,
            credible_lower: 0.94,
            credible_upper: 0.99,
        },
        expected_loss_accept: 0.01,
        expected_loss_reject: 2.0,
        expected_loss_hold: 0.5,
        rationale: "high-confidence certification fixture".to_string(),
        claim_id: Some("confidence-fixture".to_string()),
        policy_id: Some("confidence-policy".to_string()),
    }
}

fn provenance(status: IpArtifactStatus) -> ProvenanceReport {
    ProvenanceReport {
        run_id: "migration-run".to_string(),
        chain: vec![
            ProvenanceChainRecord {
                stage_id: "extract".to_string(),
                input_hash: "sha256:source".to_string(),
                output_hash: "sha256:ir".to_string(),
                tool_version: "doctor_frankentui-test".to_string(),
                timestamp: "2026-05-08T00:00:00Z".to_string(),
            },
            ProvenanceChainRecord {
                stage_id: "translate".to_string(),
                input_hash: "sha256:ir".to_string(),
                output_hash: "sha256:ftui".to_string(),
                tool_version: "doctor_frankentui-test".to_string(),
                timestamp: "2026-05-08T00:00:01Z".to_string(),
            },
        ],
        ip_artifacts: vec![IpArtifactRecord {
            artifact_id: "component.tsx".to_string(),
            license_spdx: Some("MIT".to_string()),
            license_class: "permissive".to_string(),
            status,
            risk_flags: Vec::new(),
            design_around_notes: None,
        }],
        attribution_notice: "MIT fixture attribution".to_string(),
        unresolved_risk_flags: Vec::new(),
        overall_status: status,
    }
}

fn passing_input() -> CertificationReportInput {
    let (source, translated) = semantic_runs();
    let semantic = compare_runs(&source, &translated);
    let proof_artifact = build_semantic_proof_artifact(&source, &translated, &semantic)
        .expect("semantic proof artifact should build");
    let semantic_proof = verify_semantic_proof_artifact(&proof_artifact, &semantic)
        .expect("semantic proof artifact should verify");

    CertificationReportInput {
        report_id: "report-fixture".to_string(),
        migration_id: "migration-fixture".to_string(),
        semantic,
        semantic_proof,
        visual: visual_report(true),
        performance: performance_report(),
        accessibility: accessibility_report(),
        confidence: high_confidence(),
        provenance: provenance(IpArtifactStatus::Clear),
    }
}

#[test]
fn passing_inputs_generate_accept_report_with_matrix_and_intervals() {
    let input = passing_input();
    let report =
        generate_certification_report(&input, &CertificationPolicyProfile::strict_release())
            .expect("passing certification report should generate");

    assert_eq!(report.schema_version, CERTIFICATION_REPORT_SCHEMA_VERSION);
    assert_eq!(report.final_verdict, VerdictOutcome::Accept);
    assert!(report.certification_passed);
    assert!(verify_certification_report_checksum(&report).expect("checksum should verify"));
    assert!(
        report
            .clause_matrix
            .iter()
            .any(|row| row.clause_id == "ST-001" && row.status == CertificationClauseStatus::Passed)
    );
    assert!(
        report
            .confidence_intervals
            .iter()
            .any(|interval| interval.source_id == "overall" && interval.mean >= 0.90)
    );
    assert!(report.next_steps.is_empty());
    assert!(report.remediation_plan.actions.is_empty());
    assert!(report.remediation_plan.issue_exports.is_empty());
}

#[test]
fn semantic_violation_rejects_and_emits_actionable_next_steps() {
    let mut input = passing_input();
    let source = SemanticRun::new(
        "source-semantic",
        vec![observation(
            1,
            SemanticObservationKind::StateTransition,
            "state.count",
            "1",
            "ST-001",
        )],
    );
    let translated = SemanticRun::new(
        "translated-semantic",
        vec![observation(
            1,
            SemanticObservationKind::StateTransition,
            "state.count",
            "2",
            "ST-001",
        )],
    );
    let semantic = compare_runs(&source, &translated);
    let proof_artifact = build_semantic_proof_artifact(&source, &translated, &semantic)
        .expect("violation proof artifact should build");
    input.semantic_proof = verify_semantic_proof_artifact(&proof_artifact, &semantic)
        .expect("violation proof artifact should verify structurally");
    input.semantic = semantic;

    let report =
        generate_certification_report(&input, &CertificationPolicyProfile::strict_release())
            .expect("non-passing certification report should generate");

    assert_eq!(report.final_verdict, VerdictOutcome::Reject);
    assert!(!report.certification_passed);
    assert!(
        report
            .clause_matrix
            .iter()
            .any(|row| row.clause_id == "ST-001" && row.status == CertificationClauseStatus::Failed)
    );
    assert!(
        report
            .next_steps
            .iter()
            .any(|step| { step.target == "ST-001" && step.action.contains("fix") })
    );

    let plan = &report.remediation_plan;
    assert_eq!(plan.generated_for_verdict, VerdictOutcome::Reject);
    assert_eq!(plan.migration_id, input.migration_id);
    assert_ranked(&plan.actions);

    let semantic_action = plan
        .actions
        .iter()
        .find(|action| {
            action.target == "ST-001"
                && action
                    .failed_clause_ids
                    .iter()
                    .any(|clause_id| clause_id == "ST-001")
        })
        .expect("semantic failure should produce remediation action");
    assert_eq!(semantic_action.rank, 1);
    assert!(semantic_action.expected_confidence_impact > 0.0);
    assert!(semantic_action.expected_value_score > 0.0);
    assert!(!semantic_action.artifact_refs.is_empty());
    assert!(
        semantic_action
            .evidence_messages
            .iter()
            .any(|message| message.contains("state.count"))
    );

    let export = plan
        .issue_exports
        .iter()
        .find(|export| export.action_id == semantic_action.action_id)
        .expect("remediation action should have issue export");
    assert_eq!(export.issue_type, "task");
    assert!(export.labels.iter().any(|label| label == "remediation"));
    assert!(export.description.contains("Failed clauses: ST-001"));
    assert!(export.description.contains("Artifact refs:"));
}

#[test]
fn profile_configuration_controls_visual_tolerance_acceptance() {
    let mut input = passing_input();
    input.visual = visual_report(false);

    let tolerant =
        generate_certification_report(&input, &CertificationPolicyProfile::strict_release())
            .expect("tolerant profile should generate");
    assert_eq!(tolerant.final_verdict, VerdictOutcome::Hold);
    assert!(!tolerant.certification_passed);

    let mut rejecting_policy = CertificationPolicyProfile::strict_release();
    rejecting_policy.allow_visual_tolerance = false;
    let rejecting = generate_certification_report(&input, &rejecting_policy)
        .expect("rejecting profile should generate");
    assert_eq!(rejecting.final_verdict, VerdictOutcome::Reject);
    assert!(
        rejecting
            .stage_results
            .iter()
            .any(|stage| stage.status == CertificationStageStatus::Fail)
    );
}

#[test]
fn report_schema_is_replay_stable() {
    let input = passing_input();
    let policy = CertificationPolicyProfile::strict_release();
    let report_a = generate_certification_report(&input, &policy)
        .expect("first certification report should generate");
    let report_b = generate_certification_report(&input, &policy)
        .expect("second certification report should generate");

    assert_eq!(report_a, report_b);
    assert_eq!(report_a.report_checksum, report_b.report_checksum);
    assert_eq!(
        serde_json::to_string(&report_a).expect("report should serialize"),
        serde_json::to_string(&report_b).expect("report should serialize")
    );
}

#[test]
fn invalid_proof_or_compliance_forces_non_passing_certification() {
    let mut invalid_proof = passing_input();
    invalid_proof.semantic_proof.machine_verifiable = false;
    invalid_proof.semantic_proof.certification_passed = false;
    invalid_proof
        .semantic_proof
        .invalid_obligation_ids
        .push("semantic-proof-obligation:ST-001:covered".to_string());

    let proof_report = generate_certification_report(
        &invalid_proof,
        &CertificationPolicyProfile::strict_release(),
    )
    .expect("invalid proof report should generate");
    assert_eq!(proof_report.final_verdict, VerdictOutcome::Reject);
    assert!(!proof_report.certification_passed);

    let mut blocked_compliance = passing_input();
    blocked_compliance.provenance = provenance(IpArtifactStatus::Blocked);
    let compliance_report = generate_certification_report(
        &blocked_compliance,
        &CertificationPolicyProfile::strict_release(),
    )
    .expect("blocked compliance report should generate");
    assert_eq!(compliance_report.final_verdict, VerdictOutcome::Reject);
    assert!(compliance_report.next_steps.iter().any(|step| {
        step.domain == doctor_frankentui::certification_report::CertificationDomain::Compliance
            && step.reason.contains("Blocked")
    }));
    assert_ranked(&compliance_report.remediation_plan.actions);
    assert!(
        compliance_report
            .remediation_plan
            .issue_exports
            .iter()
            .any(|export| {
                export
                    .labels
                    .iter()
                    .any(|label| label == "certification-compliance")
                    && export.description.contains("component.tsx")
            })
    );
}

fn assert_ranked(actions: &[CertificationRemediationAction]) {
    for (index, action) in actions.iter().enumerate() {
        assert_eq!(
            action.rank,
            u32::try_from(index + 1).expect("test action count should fit u32")
        );
    }
    for pair in actions.windows(2) {
        let [left, right] = pair else {
            continue;
        };
        assert!(
            left.expected_value_score > right.expected_value_score
                || (left.expected_value_score == right.expected_value_score
                    && left.effort <= right.effort),
            "actions should be ranked by expected value, then effort: {left:?} vs {right:?}"
        );
    }
}

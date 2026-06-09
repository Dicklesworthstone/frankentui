//! Final migration certification report generation.
//!
//! This module combines the comparator reports produced by the certification
//! stack into one deterministic artifact. The output is data-only so CI, IDEs,
//! and downstream report renderers can consume it without depending on terminal
//! formatting behavior.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::accessibility_diff::{AccessibilityDiffReport, AccessibilityDiffVerdict};
use crate::performance_diff::{PerformanceDiffReport, PerformanceDiffVerdict};
use crate::proof_artifacts::SemanticProofValidationReport;
use crate::semantic_contract::{
    BayesianPosterior, ExpectedLossResult, IpArtifactStatus, MigrationDecision, ProvenanceReport,
    TransformationRiskLevel, VerdictOutcome,
};
use crate::semantic_diff::{SemanticDiffReport, SemanticDiffVerdict};
use crate::visual_diff::{VisualDiffReport, VisualDiffVerdict};

pub const CERTIFICATION_REPORT_SCHEMA_VERSION: &str =
    "doctor_frankentui.migration_certification_report.v2";

pub const CERTIFICATION_REMEDIATION_PLAN_SCHEMA_VERSION: &str =
    "doctor_frankentui.certification_remediation_plan.v1";

#[derive(Debug, Error)]
pub enum CertificationReportError {
    #[error("certification report serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CertificationReportError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CertificationReportInput {
    pub report_id: String,
    pub migration_id: String,
    pub semantic: SemanticDiffReport,
    pub semantic_proof: SemanticProofValidationReport,
    pub visual: VisualDiffReport,
    pub performance: PerformanceDiffReport,
    pub accessibility: AccessibilityDiffReport,
    pub confidence: ExpectedLossResult,
    pub provenance: ProvenanceReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CertificationPolicyProfile {
    pub profile_id: String,
    pub min_confidence_mean: f64,
    pub max_confidence_interval_width: f64,
    pub allow_visual_tolerance: bool,
    pub allow_performance_regression_within_policy: bool,
    pub require_machine_verifiable_proof: bool,
    pub require_compliance_clear: bool,
    pub warning_verdict: VerdictOutcome,
}

impl CertificationPolicyProfile {
    #[must_use]
    pub fn strict_release() -> Self {
        Self {
            profile_id: "strict_release".to_string(),
            min_confidence_mean: 0.90,
            max_confidence_interval_width: 0.20,
            allow_visual_tolerance: true,
            allow_performance_regression_within_policy: false,
            require_machine_verifiable_proof: true,
            require_compliance_clear: true,
            warning_verdict: VerdictOutcome::Hold,
        }
    }

    #[must_use]
    pub fn review_profile() -> Self {
        Self {
            profile_id: "human_review".to_string(),
            min_confidence_mean: 0.75,
            max_confidence_interval_width: 0.35,
            allow_visual_tolerance: true,
            allow_performance_regression_within_policy: true,
            require_machine_verifiable_proof: true,
            require_compliance_clear: false,
            warning_verdict: VerdictOutcome::Hold,
        }
    }
}

impl Default for CertificationPolicyProfile {
    fn default() -> Self {
        Self::strict_release()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MigrationCertificationReport {
    pub schema_version: String,
    pub report_id: String,
    pub migration_id: String,
    pub profile_id: String,
    pub source_run_id: String,
    pub translated_run_id: String,
    pub final_verdict: VerdictOutcome,
    pub certification_passed: bool,
    pub stage_results: Vec<CertificationStageResult>,
    pub clause_matrix: Vec<CertificationClauseRow>,
    pub confidence_intervals: Vec<CertificationConfidenceInterval>,
    pub next_steps: Vec<CertificationNextStep>,
    pub remediation_plan: CertificationRemediationPlan,
    pub report_checksum: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CertificationDomain {
    Semantic,
    SemanticProof,
    Visual,
    Performance,
    Accessibility,
    Confidence,
    Compliance,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CertificationStageStatus {
    Pass,
    Warning,
    Fail,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CertificationClauseStatus {
    Passed,
    Warning,
    Failed,
    MissingEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CertificationStageResult {
    pub domain: CertificationDomain,
    pub status: CertificationStageStatus,
    pub observed_verdict: String,
    pub risk_level: TransformationRiskLevel,
    pub evidence_refs: Vec<String>,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CertificationClauseRow {
    pub clause_id: String,
    pub domains: Vec<CertificationDomain>,
    pub status: CertificationClauseStatus,
    pub risk_level: TransformationRiskLevel,
    pub evidence_refs: Vec<String>,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CertificationConfidenceInterval {
    pub source_id: String,
    pub decision: MigrationDecision,
    pub mean: f64,
    pub credible_lower: f64,
    pub credible_upper: f64,
    pub interval_width: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CertificationNextStep {
    pub priority: u32,
    pub domain: CertificationDomain,
    pub target: String,
    pub action: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CertificationRemediationPlan {
    pub schema_version: String,
    pub migration_id: String,
    pub generated_for_verdict: VerdictOutcome,
    pub actions: Vec<CertificationRemediationAction>,
    pub issue_exports: Vec<CertificationIssueExport>,
}

impl CertificationRemediationPlan {
    #[must_use]
    pub fn empty(migration_id: &str, verdict: VerdictOutcome) -> Self {
        Self {
            schema_version: CERTIFICATION_REMEDIATION_PLAN_SCHEMA_VERSION.to_string(),
            migration_id: migration_id.to_string(),
            generated_for_verdict: verdict,
            actions: Vec::new(),
            issue_exports: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CertificationRemediationEffort {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CertificationRemediationAction {
    pub rank: u32,
    pub action_id: String,
    pub domain: CertificationDomain,
    pub target: String,
    pub title: String,
    pub action: String,
    pub effort: CertificationRemediationEffort,
    pub expected_confidence_impact: f64,
    pub expected_value_score: f64,
    pub failed_clause_ids: Vec<String>,
    pub artifact_refs: Vec<String>,
    pub evidence_messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CertificationIssueExport {
    pub export_id: String,
    pub action_id: String,
    pub title: String,
    pub issue_type: String,
    pub priority: u8,
    pub labels: Vec<String>,
    pub description: String,
}

#[derive(Debug)]
struct ClauseRowBuilder {
    domains: BTreeSet<CertificationDomain>,
    status: Option<CertificationClauseStatus>,
    risk_level: TransformationRiskLevel,
    evidence_refs: BTreeSet<String>,
    messages: BTreeSet<String>,
}

impl Default for ClauseRowBuilder {
    fn default() -> Self {
        Self {
            domains: BTreeSet::new(),
            status: None,
            risk_level: TransformationRiskLevel::Low,
            evidence_refs: BTreeSet::new(),
            messages: BTreeSet::new(),
        }
    }
}

pub fn generate_certification_report(
    input: &CertificationReportInput,
    policy: &CertificationPolicyProfile,
) -> Result<MigrationCertificationReport> {
    let mut stage_results = vec![
        semantic_stage_result(&input.semantic),
        semantic_proof_stage_result(&input.semantic_proof, policy),
        visual_stage_result(&input.visual, policy),
        performance_stage_result(&input.performance, policy),
        accessibility_stage_result(&input.accessibility),
        confidence_stage_result(&input.confidence, policy),
        compliance_stage_result(&input.provenance, policy),
    ];
    stage_results.sort_by_key(|stage| stage.domain);

    let mut clause_rows = BTreeMap::new();
    add_semantic_rows(&mut clause_rows, &input.semantic);
    add_semantic_proof_rows(&mut clause_rows, &input.semantic_proof, policy);
    add_visual_rows(&mut clause_rows, &input.visual, policy);
    add_performance_rows(&mut clause_rows, &input.performance, policy);
    add_accessibility_rows(&mut clause_rows, &input.accessibility);
    add_confidence_rows(&mut clause_rows, &input.confidence, policy);
    add_compliance_rows(&mut clause_rows, &input.provenance, policy);

    let clause_matrix = finalize_clause_rows(clause_rows);
    let confidence_intervals = confidence_intervals(input);
    let mut next_steps = next_steps(input, policy, &stage_results, &clause_matrix);
    next_steps.sort_by(|left, right| {
        (
            left.priority,
            left.domain,
            left.target.as_str(),
            left.action.as_str(),
        )
            .cmp(&(
                right.priority,
                right.domain,
                right.target.as_str(),
                right.action.as_str(),
            ))
    });

    let final_verdict = final_verdict(&stage_results, &clause_matrix, policy);
    let remediation_plan = remediation_plan(
        input,
        final_verdict,
        &stage_results,
        &clause_matrix,
        &next_steps,
    );
    let mut report = MigrationCertificationReport {
        schema_version: CERTIFICATION_REPORT_SCHEMA_VERSION.to_string(),
        report_id: report_id(input, policy),
        migration_id: input.migration_id.clone(),
        profile_id: policy.profile_id.clone(),
        source_run_id: input.semantic.source_run_id.clone(),
        translated_run_id: input.semantic.translated_run_id.clone(),
        final_verdict,
        certification_passed: final_verdict == VerdictOutcome::Accept,
        stage_results,
        clause_matrix,
        confidence_intervals,
        next_steps,
        remediation_plan,
        report_checksum: String::new(),
    };
    report.report_checksum = compute_certification_report_checksum(&report)?;
    Ok(report)
}

pub fn compute_certification_report_checksum(
    report: &MigrationCertificationReport,
) -> Result<String> {
    let mut canonical = report.clone();
    canonical.report_checksum.clear();
    hash_serializable(&canonical)
}

pub fn verify_certification_report_checksum(report: &MigrationCertificationReport) -> Result<bool> {
    Ok(report.report_checksum == compute_certification_report_checksum(report)?)
}

fn report_id(input: &CertificationReportInput, policy: &CertificationPolicyProfile) -> String {
    if input.report_id.trim().is_empty() {
        format!(
            "migration-certification:{}:{}:{}:{}",
            input.migration_id,
            policy.profile_id,
            input.semantic.source_run_id,
            input.semantic.translated_run_id
        )
    } else {
        input.report_id.clone()
    }
}

fn semantic_stage_result(report: &SemanticDiffReport) -> CertificationStageResult {
    let status = match report.verdict {
        SemanticDiffVerdict::Equivalent | SemanticDiffVerdict::AcceptableImprovement => {
            CertificationStageStatus::Pass
        }
        SemanticDiffVerdict::Violation => CertificationStageStatus::Fail,
    };
    CertificationStageResult {
        domain: CertificationDomain::Semantic,
        status,
        observed_verdict: format!("{:?}", report.verdict),
        risk_level: report.risk_level,
        evidence_refs: sorted_vec(
            report
                .covered_clause_ids
                .iter()
                .chain(report.violated_clause_ids.iter())
                .cloned(),
        ),
        messages: report
            .differences
            .iter()
            .map(|difference| difference.message.clone())
            .collect(),
    }
}

fn semantic_proof_stage_result(
    report: &SemanticProofValidationReport,
    policy: &CertificationPolicyProfile,
) -> CertificationStageResult {
    let status = if policy.require_machine_verifiable_proof && !report.machine_verifiable {
        CertificationStageStatus::Fail
    } else if report.certification_passed {
        CertificationStageStatus::Pass
    } else {
        CertificationStageStatus::Fail
    };
    CertificationStageResult {
        domain: CertificationDomain::SemanticProof,
        status,
        observed_verdict: if report.certification_passed {
            "certified".to_string()
        } else {
            "not_certified".to_string()
        },
        risk_level: if report.machine_verifiable {
            TransformationRiskLevel::Low
        } else {
            TransformationRiskLevel::Critical
        },
        evidence_refs: sorted_vec(
            report
                .expected_clause_ids
                .iter()
                .chain(report.missing_clause_ids.iter())
                .chain(report.invalid_obligation_ids.iter())
                .cloned(),
        ),
        messages: report
            .issues
            .iter()
            .map(|issue| format!("{}:{}:{}", issue.code, issue.target, issue.message))
            .collect(),
    }
}

fn visual_stage_result(
    report: &VisualDiffReport,
    policy: &CertificationPolicyProfile,
) -> CertificationStageResult {
    let status = match report.verdict {
        VisualDiffVerdict::Equivalent => CertificationStageStatus::Pass,
        VisualDiffVerdict::WithinTolerance => {
            if policy.allow_visual_tolerance {
                CertificationStageStatus::Pass
            } else {
                CertificationStageStatus::Fail
            }
        }
        VisualDiffVerdict::Violation => CertificationStageStatus::Fail,
    };
    CertificationStageResult {
        domain: CertificationDomain::Visual,
        status,
        observed_verdict: format!("{:?}", report.verdict),
        risk_level: report.risk_level,
        evidence_refs: sorted_vec(
            report
                .covered_clause_ids
                .iter()
                .chain(report.violated_clause_ids.iter())
                .cloned(),
        ),
        messages: report
            .differences
            .iter()
            .map(|difference| difference.message.clone())
            .collect(),
    }
}

fn performance_stage_result(
    report: &PerformanceDiffReport,
    policy: &CertificationPolicyProfile,
) -> CertificationStageResult {
    let status = match report.verdict {
        PerformanceDiffVerdict::Equivalent | PerformanceDiffVerdict::Improvement => {
            CertificationStageStatus::Pass
        }
        PerformanceDiffVerdict::RegressionWithinPolicy => {
            if policy.allow_performance_regression_within_policy {
                CertificationStageStatus::Warning
            } else {
                CertificationStageStatus::Fail
            }
        }
        PerformanceDiffVerdict::NeedsMoreEvidence => CertificationStageStatus::Warning,
        PerformanceDiffVerdict::PolicyRegression => CertificationStageStatus::Fail,
    };
    CertificationStageResult {
        domain: CertificationDomain::Performance,
        status,
        observed_verdict: format!("{:?}", report.verdict),
        risk_level: report.risk_level,
        evidence_refs: sorted_vec(
            report
                .covered_policy_ids
                .iter()
                .chain(report.violated_policy_ids.iter())
                .cloned(),
        ),
        messages: report
            .differences
            .iter()
            .map(|difference| difference.message.clone())
            .collect(),
    }
}

fn accessibility_stage_result(report: &AccessibilityDiffReport) -> CertificationStageResult {
    let status = match report.verdict {
        AccessibilityDiffVerdict::Equivalent | AccessibilityDiffVerdict::Improved => {
            CertificationStageStatus::Pass
        }
        AccessibilityDiffVerdict::Violation => CertificationStageStatus::Fail,
    };
    CertificationStageResult {
        domain: CertificationDomain::Accessibility,
        status,
        observed_verdict: format!("{:?}", report.verdict),
        risk_level: report.risk_level,
        evidence_refs: sorted_vec(
            report
                .covered_policy_ids
                .iter()
                .chain(report.violated_policy_ids.iter())
                .cloned(),
        ),
        messages: report
            .violations
            .iter()
            .map(|violation| violation.message.clone())
            .collect(),
    }
}

fn confidence_stage_result(
    expected_loss: &ExpectedLossResult,
    policy: &CertificationPolicyProfile,
) -> CertificationStageResult {
    let interval_width = interval_width(&expected_loss.posterior);
    let mut status = match expected_loss.decision {
        MigrationDecision::AutoApprove => CertificationStageStatus::Pass,
        MigrationDecision::HumanReview => CertificationStageStatus::Warning,
        MigrationDecision::Reject
        | MigrationDecision::HardReject
        | MigrationDecision::Rollback
        | MigrationDecision::ConservativeFallback => CertificationStageStatus::Fail,
    };
    if expected_loss.posterior.mean < policy.min_confidence_mean {
        status = CertificationStageStatus::Fail;
    } else if interval_width > policy.max_confidence_interval_width {
        status = worst_stage_status(status, CertificationStageStatus::Warning);
    }

    let mut messages = vec![expected_loss.rationale.clone()];
    if expected_loss.posterior.mean < policy.min_confidence_mean {
        messages.push(format!(
            "posterior mean {:.3} is below profile threshold {:.3}",
            expected_loss.posterior.mean, policy.min_confidence_mean
        ));
    }
    if interval_width > policy.max_confidence_interval_width {
        messages.push(format!(
            "credible interval width {:.3} exceeds profile threshold {:.3}",
            interval_width, policy.max_confidence_interval_width
        ));
    }

    CertificationStageResult {
        domain: CertificationDomain::Confidence,
        status,
        observed_verdict: format!("{:?}", expected_loss.decision),
        risk_level: if status == CertificationStageStatus::Fail {
            TransformationRiskLevel::High
        } else {
            TransformationRiskLevel::Low
        },
        evidence_refs: expected_loss
            .policy_id
            .iter()
            .chain(expected_loss.claim_id.iter())
            .cloned()
            .collect(),
        messages,
    }
}

fn compliance_stage_result(
    report: &ProvenanceReport,
    policy: &CertificationPolicyProfile,
) -> CertificationStageResult {
    let status = compliance_status(report.overall_status, policy);
    let mut evidence_refs = report
        .chain
        .iter()
        .map(|record| record.stage_id.clone())
        .chain(
            report
                .ip_artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.clone()),
        )
        .collect::<Vec<_>>();
    evidence_refs.sort();
    evidence_refs.dedup();

    let mut messages = report
        .unresolved_risk_flags
        .iter()
        .map(|flag| format!("unresolved provenance risk flag: {flag}"))
        .collect::<Vec<_>>();
    if report.overall_status != IpArtifactStatus::Clear {
        messages.push(format!("provenance status is {:?}", report.overall_status));
    }

    CertificationStageResult {
        domain: CertificationDomain::Compliance,
        status,
        observed_verdict: format!("{:?}", report.overall_status),
        risk_level: risk_for_ip_status(report.overall_status),
        evidence_refs,
        messages,
    }
}

fn add_semantic_rows(rows: &mut BTreeMap<String, ClauseRowBuilder>, report: &SemanticDiffReport) {
    for clause_id in &report.covered_clause_ids {
        add_clause_row(
            rows,
            clause_id,
            ClauseEvidence {
                domain: CertificationDomain::Semantic,
                status: CertificationClauseStatus::Passed,
                risk_level: TransformationRiskLevel::Low,
                evidence_ref: "semantic:covered".to_string(),
                message: "semantic clause covered by equivalent observations".to_string(),
            },
        );
    }
    for difference in &report.differences {
        for clause_id in &difference.clause_ids {
            add_clause_row(
                rows,
                clause_id,
                ClauseEvidence {
                    domain: CertificationDomain::Semantic,
                    status: CertificationClauseStatus::Failed,
                    risk_level: difference.risk_level,
                    evidence_ref: format!("semantic:{}", difference.key),
                    message: difference.message.clone(),
                },
            );
        }
    }
}

fn add_semantic_proof_rows(
    rows: &mut BTreeMap<String, ClauseRowBuilder>,
    report: &SemanticProofValidationReport,
    policy: &CertificationPolicyProfile,
) {
    for clause_id in &report.expected_clause_ids {
        let missing = report.missing_clause_ids.iter().any(|id| id == clause_id);
        let status = if missing || !report.certification_passed {
            CertificationClauseStatus::MissingEvidence
        } else {
            CertificationClauseStatus::Passed
        };
        let risk_level = if status == CertificationClauseStatus::Passed {
            TransformationRiskLevel::Low
        } else if policy.require_machine_verifiable_proof {
            TransformationRiskLevel::Critical
        } else {
            TransformationRiskLevel::Medium
        };
        add_clause_row(
            rows,
            clause_id,
            ClauseEvidence {
                domain: CertificationDomain::SemanticProof,
                status,
                risk_level,
                evidence_ref: format!("semantic-proof:{}", report.artifact_id),
                message: if status == CertificationClauseStatus::Passed {
                    "semantic proof obligation verified".to_string()
                } else {
                    "semantic proof obligation is missing or non-passing".to_string()
                },
            },
        );
    }

    for obligation_id in &report.invalid_obligation_ids {
        add_clause_row(
            rows,
            obligation_id,
            ClauseEvidence {
                domain: CertificationDomain::SemanticProof,
                status: CertificationClauseStatus::Failed,
                risk_level: TransformationRiskLevel::Critical,
                evidence_ref: format!("semantic-proof-invalid:{}", report.artifact_id),
                message: "semantic proof obligation failed integrity validation".to_string(),
            },
        );
    }
}

fn add_visual_rows(
    rows: &mut BTreeMap<String, ClauseRowBuilder>,
    report: &VisualDiffReport,
    policy: &CertificationPolicyProfile,
) {
    let passing_status = match report.verdict {
        VisualDiffVerdict::WithinTolerance if !policy.allow_visual_tolerance => {
            CertificationClauseStatus::Failed
        }
        VisualDiffVerdict::WithinTolerance => CertificationClauseStatus::Warning,
        VisualDiffVerdict::Equivalent => CertificationClauseStatus::Passed,
        VisualDiffVerdict::Violation => CertificationClauseStatus::Failed,
    };
    for clause_id in &report.covered_clause_ids {
        add_clause_row(
            rows,
            clause_id,
            ClauseEvidence {
                domain: CertificationDomain::Visual,
                status: passing_status,
                risk_level: report.risk_level,
                evidence_ref: "visual:covered".to_string(),
                message: "visual clause covered by terminal output comparison".to_string(),
            },
        );
    }
    for difference in &report.differences {
        for clause_id in &difference.clause_ids {
            add_clause_row(
                rows,
                clause_id,
                ClauseEvidence {
                    domain: CertificationDomain::Visual,
                    status: CertificationClauseStatus::Failed,
                    risk_level: difference.risk_level,
                    evidence_ref: format!("visual:frame:{}", difference.frame_index),
                    message: difference.message.clone(),
                },
            );
        }
    }
}

fn add_performance_rows(
    rows: &mut BTreeMap<String, ClauseRowBuilder>,
    report: &PerformanceDiffReport,
    policy: &CertificationPolicyProfile,
) {
    let covered_status = match report.verdict {
        PerformanceDiffVerdict::RegressionWithinPolicy => {
            if policy.allow_performance_regression_within_policy {
                CertificationClauseStatus::Warning
            } else {
                CertificationClauseStatus::Failed
            }
        }
        PerformanceDiffVerdict::NeedsMoreEvidence => CertificationClauseStatus::Warning,
        PerformanceDiffVerdict::Equivalent | PerformanceDiffVerdict::Improvement => {
            CertificationClauseStatus::Passed
        }
        PerformanceDiffVerdict::PolicyRegression => CertificationClauseStatus::Failed,
    };
    for policy_id in &report.covered_policy_ids {
        add_clause_row(
            rows,
            policy_id,
            ClauseEvidence {
                domain: CertificationDomain::Performance,
                status: covered_status,
                risk_level: report.risk_level,
                evidence_ref: "performance:covered".to_string(),
                message: "performance policy covered by deterministic benchmark comparison"
                    .to_string(),
            },
        );
    }
    for difference in &report.differences {
        add_clause_row(
            rows,
            &difference.policy_id,
            ClauseEvidence {
                domain: CertificationDomain::Performance,
                status: CertificationClauseStatus::Failed,
                risk_level: difference.risk_level,
                evidence_ref: format!("performance:{}", difference.scenario_id),
                message: difference.message.clone(),
            },
        );
    }
}

fn add_accessibility_rows(
    rows: &mut BTreeMap<String, ClauseRowBuilder>,
    report: &AccessibilityDiffReport,
) {
    for policy_id in &report.covered_policy_ids {
        add_clause_row(
            rows,
            policy_id,
            ClauseEvidence {
                domain: CertificationDomain::Accessibility,
                status: CertificationClauseStatus::Passed,
                risk_level: TransformationRiskLevel::Low,
                evidence_ref: "accessibility:covered".to_string(),
                message: "accessibility policy covered by parity comparison".to_string(),
            },
        );
    }
    for violation in &report.violations {
        add_clause_row(
            rows,
            &violation.policy_id,
            ClauseEvidence {
                domain: CertificationDomain::Accessibility,
                status: CertificationClauseStatus::Failed,
                risk_level: violation.risk_level,
                evidence_ref: violation.node_id.as_ref().map_or_else(
                    || "accessibility:global".to_string(),
                    |node_id| format!("accessibility:node:{node_id}"),
                ),
                message: violation.message.clone(),
            },
        );
    }
}

fn add_confidence_rows(
    rows: &mut BTreeMap<String, ClauseRowBuilder>,
    expected_loss: &ExpectedLossResult,
    policy: &CertificationPolicyProfile,
) {
    let status = if expected_loss.posterior.mean < policy.min_confidence_mean {
        CertificationClauseStatus::Failed
    } else if interval_width(&expected_loss.posterior) > policy.max_confidence_interval_width
        || expected_loss.decision == MigrationDecision::HumanReview
    {
        CertificationClauseStatus::Warning
    } else if expected_loss.decision == MigrationDecision::AutoApprove {
        CertificationClauseStatus::Passed
    } else {
        CertificationClauseStatus::Failed
    };
    add_clause_row(
        rows,
        "confidence:posterior",
        ClauseEvidence {
            domain: CertificationDomain::Confidence,
            status,
            risk_level: if status == CertificationClauseStatus::Failed {
                TransformationRiskLevel::High
            } else {
                TransformationRiskLevel::Low
            },
            evidence_ref: expected_loss
                .policy_id
                .clone()
                .unwrap_or_else(|| "confidence:expected-loss".to_string()),
            message: format!(
                "posterior mean {:.3}, interval [{:.3}, {:.3}], decision {:?}",
                expected_loss.posterior.mean,
                expected_loss.posterior.credible_lower,
                expected_loss.posterior.credible_upper,
                expected_loss.decision
            ),
        },
    );
}

fn add_compliance_rows(
    rows: &mut BTreeMap<String, ClauseRowBuilder>,
    report: &ProvenanceReport,
    policy: &CertificationPolicyProfile,
) {
    if report.ip_artifacts.is_empty() {
        add_clause_row(
            rows,
            "compliance:provenance-chain",
            ClauseEvidence {
                domain: CertificationDomain::Compliance,
                status: clause_status_from_stage(compliance_status(report.overall_status, policy)),
                risk_level: risk_for_ip_status(report.overall_status),
                evidence_ref: report.run_id.clone(),
                message: "provenance chain evaluated without IP artifact exceptions".to_string(),
            },
        );
    }

    for artifact in &report.ip_artifacts {
        add_clause_row(
            rows,
            &format!("compliance:{}", artifact.artifact_id),
            ClauseEvidence {
                domain: CertificationDomain::Compliance,
                status: clause_status_from_stage(compliance_status(artifact.status, policy)),
                risk_level: risk_for_ip_status(artifact.status),
                evidence_ref: artifact
                    .license_spdx
                    .clone()
                    .unwrap_or_else(|| artifact.license_class.clone()),
                message: format!(
                    "IP artifact status {:?} with license class {}",
                    artifact.status, artifact.license_class
                ),
            },
        );
    }

    for flag in &report.unresolved_risk_flags {
        add_clause_row(
            rows,
            &format!("compliance:risk:{flag}"),
            ClauseEvidence {
                domain: CertificationDomain::Compliance,
                status: if policy.require_compliance_clear {
                    CertificationClauseStatus::Failed
                } else {
                    CertificationClauseStatus::Warning
                },
                risk_level: TransformationRiskLevel::High,
                evidence_ref: report.run_id.clone(),
                message: format!("unresolved provenance risk flag: {flag}"),
            },
        );
    }
}

#[derive(Debug)]
struct ClauseEvidence {
    domain: CertificationDomain,
    status: CertificationClauseStatus,
    risk_level: TransformationRiskLevel,
    evidence_ref: String,
    message: String,
}

fn add_clause_row(
    rows: &mut BTreeMap<String, ClauseRowBuilder>,
    clause_id: &str,
    evidence: ClauseEvidence,
) {
    let entry = rows
        .entry(clause_id.to_string())
        .or_insert_with(|| ClauseRowBuilder {
            risk_level: evidence.risk_level,
            ..ClauseRowBuilder::default()
        });
    entry.domains.insert(evidence.domain);
    entry.status = Some(merge_clause_status(
        entry.status.unwrap_or(CertificationClauseStatus::Passed),
        evidence.status,
    ));
    entry.risk_level = entry.risk_level.max(evidence.risk_level);
    if !evidence.evidence_ref.trim().is_empty() {
        entry.evidence_refs.insert(evidence.evidence_ref);
    }
    if !evidence.message.trim().is_empty() {
        entry.messages.insert(evidence.message);
    }
}

fn finalize_clause_rows(rows: BTreeMap<String, ClauseRowBuilder>) -> Vec<CertificationClauseRow> {
    rows.into_iter()
        .map(|(clause_id, row)| CertificationClauseRow {
            clause_id,
            domains: row.domains.into_iter().collect(),
            status: row
                .status
                .unwrap_or(CertificationClauseStatus::MissingEvidence),
            risk_level: row.risk_level,
            evidence_refs: row.evidence_refs.into_iter().collect(),
            messages: row.messages.into_iter().collect(),
        })
        .collect()
}

fn confidence_intervals(input: &CertificationReportInput) -> Vec<CertificationConfidenceInterval> {
    let mut intervals = vec![
        confidence_interval("semantic", &input.semantic.expected_loss),
        confidence_interval("visual", &input.visual.expected_loss),
        confidence_interval("performance", &input.performance.expected_loss),
        confidence_interval("accessibility", &input.accessibility.expected_loss),
        confidence_interval("overall", &input.confidence),
    ];
    intervals.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    intervals
}

fn confidence_interval(
    source_id: &str,
    expected_loss: &ExpectedLossResult,
) -> CertificationConfidenceInterval {
    CertificationConfidenceInterval {
        source_id: source_id.to_string(),
        decision: expected_loss.decision,
        mean: expected_loss.posterior.mean,
        credible_lower: expected_loss.posterior.credible_lower,
        credible_upper: expected_loss.posterior.credible_upper,
        interval_width: interval_width(&expected_loss.posterior),
    }
}

fn next_steps(
    input: &CertificationReportInput,
    policy: &CertificationPolicyProfile,
    stage_results: &[CertificationStageResult],
    clause_matrix: &[CertificationClauseRow],
) -> Vec<CertificationNextStep> {
    let mut steps = Vec::new();
    for stage in stage_results {
        if stage.status == CertificationStageStatus::Pass {
            continue;
        }
        steps.push(CertificationNextStep {
            priority: priority_for_stage(stage.status, stage.risk_level),
            domain: stage.domain,
            target: stage.observed_verdict.clone(),
            action: stage_action(stage),
            reason: stage
                .messages
                .first()
                .cloned()
                .unwrap_or_else(|| "stage did not satisfy certification policy".to_string()),
        });
    }

    for row in clause_matrix {
        if matches!(row.status, CertificationClauseStatus::Passed) {
            continue;
        }
        steps.push(CertificationNextStep {
            priority: priority_for_clause(row.status, row.risk_level),
            domain: row
                .domains
                .first()
                .copied()
                .unwrap_or(CertificationDomain::Semantic),
            target: row.clause_id.clone(),
            action: clause_action(row),
            reason: row.messages.first().cloned().unwrap_or_else(|| {
                "clause lacks enough passing evidence for certification".to_string()
            }),
        });
    }

    if input.confidence.posterior.mean < policy.min_confidence_mean {
        steps.push(CertificationNextStep {
            priority: 1,
            domain: CertificationDomain::Confidence,
            target: "confidence:posterior".to_string(),
            action: "add evidence or rerun certification until posterior mean meets profile"
                .to_string(),
            reason: format!(
                "posterior mean {:.3} below {:.3}",
                input.confidence.posterior.mean, policy.min_confidence_mean
            ),
        });
    }

    steps
}

fn remediation_plan(
    input: &CertificationReportInput,
    final_verdict: VerdictOutcome,
    stage_results: &[CertificationStageResult],
    clause_matrix: &[CertificationClauseRow],
    next_steps: &[CertificationNextStep],
) -> CertificationRemediationPlan {
    let mut plan = CertificationRemediationPlan::empty(&input.migration_id, final_verdict);
    if final_verdict == VerdictOutcome::Accept {
        return plan;
    }

    let mut actions = Vec::new();
    for row in clause_matrix {
        if row.status == CertificationClauseStatus::Passed {
            continue;
        }
        actions.push(remediation_action_from_clause(
            row,
            stage_results,
            next_steps,
        ));
    }

    for stage in stage_results {
        if stage.status == CertificationStageStatus::Pass
            || actions.iter().any(|action| action.domain == stage.domain)
        {
            continue;
        }
        actions.push(remediation_action_from_stage(
            stage,
            clause_matrix,
            next_steps,
        ));
    }

    actions.sort_by(|left, right| {
        right
            .expected_value_score
            .partial_cmp(&left.expected_value_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.effort.cmp(&right.effort))
            .then_with(|| left.domain.cmp(&right.domain))
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| left.action.cmp(&right.action))
    });

    for (index, action) in actions.iter_mut().enumerate() {
        action.rank = u32::try_from(index + 1).unwrap_or(u32::MAX);
    }

    let issue_exports = actions
        .iter()
        .map(remediation_issue_export)
        .collect::<Vec<_>>();
    plan.actions = actions;
    plan.issue_exports = issue_exports;
    plan
}

fn remediation_action_from_clause(
    row: &CertificationClauseRow,
    stage_results: &[CertificationStageResult],
    next_steps: &[CertificationNextStep],
) -> CertificationRemediationAction {
    let domain = row
        .domains
        .first()
        .copied()
        .unwrap_or(CertificationDomain::Semantic);
    let step = next_steps
        .iter()
        .find(|step| step.domain == domain && step.target == row.clause_id)
        .or_else(|| next_steps.iter().find(|step| step.domain == domain));
    let stage = stage_results.iter().find(|stage| stage.domain == domain);
    let effort = effort_for_clause(row);
    let expected_confidence_impact = confidence_impact_for_clause(row, domain);
    let expected_value_score = expected_value_score(expected_confidence_impact, effort);
    let action = step
        .map(|step| step.action.clone())
        .unwrap_or_else(|| clause_action(row));
    let mut evidence_messages = row.messages.iter().cloned().collect::<BTreeSet<_>>();
    if let Some(step) = step {
        evidence_messages.insert(step.reason.clone());
    }
    if let Some(stage) = stage {
        evidence_messages.extend(stage.messages.iter().cloned());
    }

    CertificationRemediationAction {
        rank: 0,
        action_id: action_id(domain, &row.clause_id),
        domain,
        target: row.clause_id.clone(),
        title: format!(
            "Remediate {} certification target {}",
            domain_label(domain),
            row.clause_id
        ),
        action,
        effort,
        expected_confidence_impact,
        expected_value_score,
        failed_clause_ids: vec![row.clause_id.clone()],
        artifact_refs: artifact_refs_for_clause(row, stage),
        evidence_messages: evidence_messages.into_iter().collect(),
    }
}

fn remediation_action_from_stage(
    stage: &CertificationStageResult,
    clause_matrix: &[CertificationClauseRow],
    next_steps: &[CertificationNextStep],
) -> CertificationRemediationAction {
    let related_rows = clause_matrix
        .iter()
        .filter(|row| row.status != CertificationClauseStatus::Passed)
        .filter(|row| row.domains.contains(&stage.domain))
        .collect::<Vec<_>>();
    let target = if related_rows.is_empty() {
        format!("stage:{}", domain_label(stage.domain))
    } else {
        related_rows
            .iter()
            .map(|row| row.clause_id.as_str())
            .collect::<Vec<_>>()
            .join("+")
    };
    let step = next_steps
        .iter()
        .find(|step| step.domain == stage.domain && step.target == stage.observed_verdict)
        .or_else(|| next_steps.iter().find(|step| step.domain == stage.domain));
    let effort = effort_for_stage(stage);
    let expected_confidence_impact = confidence_impact_for_stage(stage);
    let expected_value_score = expected_value_score(expected_confidence_impact, effort);
    let mut failed_clause_ids = related_rows
        .iter()
        .map(|row| row.clause_id.clone())
        .collect::<BTreeSet<_>>();
    if failed_clause_ids.is_empty() {
        failed_clause_ids.insert(target.clone());
    }
    let mut artifact_refs = stage.evidence_refs.iter().cloned().collect::<BTreeSet<_>>();
    for row in &related_rows {
        artifact_refs.extend(row.evidence_refs.iter().cloned());
    }
    let mut evidence_messages = stage.messages.iter().cloned().collect::<BTreeSet<_>>();
    for row in &related_rows {
        evidence_messages.extend(row.messages.iter().cloned());
    }
    if let Some(step) = step {
        evidence_messages.insert(step.reason.clone());
    }

    CertificationRemediationAction {
        rank: 0,
        action_id: action_id(stage.domain, &target),
        domain: stage.domain,
        target,
        title: format!(
            "Remediate {} certification stage",
            domain_label(stage.domain)
        ),
        action: step
            .map(|step| step.action.clone())
            .unwrap_or_else(|| stage_action(stage)),
        effort,
        expected_confidence_impact,
        expected_value_score,
        failed_clause_ids: failed_clause_ids.into_iter().collect(),
        artifact_refs: artifact_refs.into_iter().collect(),
        evidence_messages: evidence_messages.into_iter().collect(),
    }
}

fn artifact_refs_for_clause(
    row: &CertificationClauseRow,
    stage: Option<&CertificationStageResult>,
) -> Vec<String> {
    let mut refs = row.evidence_refs.iter().cloned().collect::<BTreeSet<_>>();
    if let Some(stage) = stage {
        refs.extend(stage.evidence_refs.iter().cloned());
    }
    refs.into_iter().collect()
}

fn remediation_issue_export(action: &CertificationRemediationAction) -> CertificationIssueExport {
    let clauses = display_list(&action.failed_clause_ids);
    let artifacts = display_list(&action.artifact_refs);
    let evidence = display_list(&action.evidence_messages);
    CertificationIssueExport {
        export_id: format!("issue:{}", action.action_id),
        action_id: action.action_id.clone(),
        title: action.title.clone(),
        issue_type: "task".to_string(),
        priority: issue_priority(action),
        labels: vec![
            "opentui-import".to_string(),
            "remediation".to_string(),
            format!("certification-{}", domain_label(action.domain)),
        ],
        description: format!(
            "Action: {}\n\nFailed clauses: {}\nArtifact refs: {}\nExpected confidence impact: {:.3}\nExpected value score: {:.3}\nEffort: {}\nEvidence: {}",
            action.action,
            clauses,
            artifacts,
            action.expected_confidence_impact,
            action.expected_value_score,
            effort_label(action.effort),
            evidence
        ),
    }
}

fn final_verdict(
    stages: &[CertificationStageResult],
    clause_matrix: &[CertificationClauseRow],
    policy: &CertificationPolicyProfile,
) -> VerdictOutcome {
    let has_failure = stages
        .iter()
        .any(|stage| stage.status == CertificationStageStatus::Fail)
        || clause_matrix.iter().any(|row| {
            matches!(
                row.status,
                CertificationClauseStatus::Failed | CertificationClauseStatus::MissingEvidence
            )
        });
    if has_failure {
        return if stages.iter().any(|stage| {
            stage.domain == CertificationDomain::Confidence
                && stage.observed_verdict == format!("{:?}", MigrationDecision::Rollback)
        }) {
            VerdictOutcome::Rollback
        } else {
            VerdictOutcome::Reject
        };
    }

    let has_warning = stages
        .iter()
        .any(|stage| stage.status == CertificationStageStatus::Warning)
        || clause_matrix
            .iter()
            .any(|row| row.status == CertificationClauseStatus::Warning);
    if has_warning {
        policy.warning_verdict
    } else {
        VerdictOutcome::Accept
    }
}

fn compliance_status(
    status: IpArtifactStatus,
    policy: &CertificationPolicyProfile,
) -> CertificationStageStatus {
    match status {
        IpArtifactStatus::Clear => CertificationStageStatus::Pass,
        IpArtifactStatus::Blocked => CertificationStageStatus::Fail,
        IpArtifactStatus::Expired | IpArtifactStatus::Unknown | IpArtifactStatus::NeedsCounsel => {
            if policy.require_compliance_clear {
                CertificationStageStatus::Fail
            } else {
                CertificationStageStatus::Warning
            }
        }
    }
}

fn risk_for_ip_status(status: IpArtifactStatus) -> TransformationRiskLevel {
    match status {
        IpArtifactStatus::Clear => TransformationRiskLevel::Low,
        IpArtifactStatus::Expired => TransformationRiskLevel::Medium,
        IpArtifactStatus::Unknown | IpArtifactStatus::NeedsCounsel => TransformationRiskLevel::High,
        IpArtifactStatus::Blocked => TransformationRiskLevel::Critical,
    }
}

fn clause_status_from_stage(status: CertificationStageStatus) -> CertificationClauseStatus {
    match status {
        CertificationStageStatus::Pass => CertificationClauseStatus::Passed,
        CertificationStageStatus::Warning => CertificationClauseStatus::Warning,
        CertificationStageStatus::Fail => CertificationClauseStatus::Failed,
    }
}

fn merge_clause_status(
    left: CertificationClauseStatus,
    right: CertificationClauseStatus,
) -> CertificationClauseStatus {
    if clause_status_rank(left) >= clause_status_rank(right) {
        left
    } else {
        right
    }
}

fn worst_stage_status(
    left: CertificationStageStatus,
    right: CertificationStageStatus,
) -> CertificationStageStatus {
    if stage_status_rank(left) >= stage_status_rank(right) {
        left
    } else {
        right
    }
}

fn clause_status_rank(status: CertificationClauseStatus) -> u8 {
    match status {
        CertificationClauseStatus::Passed => 0,
        CertificationClauseStatus::Warning => 1,
        CertificationClauseStatus::MissingEvidence => 2,
        CertificationClauseStatus::Failed => 3,
    }
}

fn stage_status_rank(status: CertificationStageStatus) -> u8 {
    match status {
        CertificationStageStatus::Pass => 0,
        CertificationStageStatus::Warning => 1,
        CertificationStageStatus::Fail => 2,
    }
}

fn interval_width(posterior: &BayesianPosterior) -> f64 {
    posterior.credible_upper - posterior.credible_lower
}

fn priority_for_stage(status: CertificationStageStatus, risk: TransformationRiskLevel) -> u32 {
    match (status, risk) {
        (CertificationStageStatus::Fail, TransformationRiskLevel::Critical) => 0,
        (CertificationStageStatus::Fail, TransformationRiskLevel::High) => 1,
        (CertificationStageStatus::Fail, _) => 2,
        (
            CertificationStageStatus::Warning,
            TransformationRiskLevel::Critical | TransformationRiskLevel::High,
        ) => 3,
        (CertificationStageStatus::Warning, _) => 4,
        (CertificationStageStatus::Pass, _) => 5,
    }
}

fn priority_for_clause(status: CertificationClauseStatus, risk: TransformationRiskLevel) -> u32 {
    match (status, risk) {
        (
            CertificationClauseStatus::Failed | CertificationClauseStatus::MissingEvidence,
            TransformationRiskLevel::Critical,
        ) => 0,
        (
            CertificationClauseStatus::Failed | CertificationClauseStatus::MissingEvidence,
            TransformationRiskLevel::High,
        ) => 1,
        (CertificationClauseStatus::Failed | CertificationClauseStatus::MissingEvidence, _) => 2,
        (
            CertificationClauseStatus::Warning,
            TransformationRiskLevel::Critical | TransformationRiskLevel::High,
        ) => 3,
        (CertificationClauseStatus::Warning, _) => 4,
        (CertificationClauseStatus::Passed, _) => 5,
    }
}

fn stage_action(stage: &CertificationStageResult) -> String {
    match stage.domain {
        CertificationDomain::Semantic => {
            "fix semantic divergence or mark a documented acceptable improvement".to_string()
        }
        CertificationDomain::SemanticProof => {
            "regenerate proof artifacts with complete validated obligations".to_string()
        }
        CertificationDomain::Visual => {
            "repair visual diff or adjust the certification profile tolerance explicitly"
                .to_string()
        }
        CertificationDomain::Performance => {
            "rerun controlled benchmarks and remove policy regressions".to_string()
        }
        CertificationDomain::Accessibility => {
            "restore accessibility parity or document allowed improvements".to_string()
        }
        CertificationDomain::Confidence => {
            "add evidence until expected-loss policy reaches a passing decision".to_string()
        }
        CertificationDomain::Compliance => {
            "resolve provenance or licensing risk before release certification".to_string()
        }
    }
}

fn clause_action(row: &CertificationClauseRow) -> String {
    match row.status {
        CertificationClauseStatus::Passed => "no action required".to_string(),
        CertificationClauseStatus::Warning => {
            "review warning and decide whether the active profile permits it".to_string()
        }
        CertificationClauseStatus::Failed => {
            "fix the failing comparator, policy, or compliance evidence".to_string()
        }
        CertificationClauseStatus::MissingEvidence => {
            "add the missing witness, obligation, or evidence record".to_string()
        }
    }
}

fn effort_for_clause(row: &CertificationClauseRow) -> CertificationRemediationEffort {
    match (row.status, row.risk_level) {
        (
            CertificationClauseStatus::Failed,
            TransformationRiskLevel::Critical | TransformationRiskLevel::High,
        ) => CertificationRemediationEffort::High,
        (CertificationClauseStatus::Failed, _) => CertificationRemediationEffort::Medium,
        (
            CertificationClauseStatus::MissingEvidence,
            TransformationRiskLevel::Critical | TransformationRiskLevel::High,
        ) => CertificationRemediationEffort::Medium,
        (CertificationClauseStatus::MissingEvidence, _) => CertificationRemediationEffort::Low,
        (
            CertificationClauseStatus::Warning,
            TransformationRiskLevel::Critical | TransformationRiskLevel::High,
        ) => CertificationRemediationEffort::Medium,
        (CertificationClauseStatus::Warning, _) => CertificationRemediationEffort::Low,
        (CertificationClauseStatus::Passed, _) => CertificationRemediationEffort::Low,
    }
}

fn effort_for_stage(stage: &CertificationStageResult) -> CertificationRemediationEffort {
    match (stage.status, stage.risk_level) {
        (
            CertificationStageStatus::Fail,
            TransformationRiskLevel::Critical | TransformationRiskLevel::High,
        ) => CertificationRemediationEffort::High,
        (CertificationStageStatus::Fail, _) => CertificationRemediationEffort::Medium,
        (
            CertificationStageStatus::Warning,
            TransformationRiskLevel::Critical | TransformationRiskLevel::High,
        ) => CertificationRemediationEffort::Medium,
        (CertificationStageStatus::Warning, _) => CertificationRemediationEffort::Low,
        (CertificationStageStatus::Pass, _) => CertificationRemediationEffort::Low,
    }
}

fn confidence_impact_for_clause(row: &CertificationClauseRow, domain: CertificationDomain) -> f64 {
    let status_score = match row.status {
        CertificationClauseStatus::Failed => 0.18,
        CertificationClauseStatus::MissingEvidence => 0.14,
        CertificationClauseStatus::Warning => 0.06,
        CertificationClauseStatus::Passed => 0.0,
    };
    rounded_score(
        status_score + risk_confidence_bonus(row.risk_level) + domain_confidence_bonus(domain),
    )
}

fn confidence_impact_for_stage(stage: &CertificationStageResult) -> f64 {
    let status_score = match stage.status {
        CertificationStageStatus::Fail => 0.16,
        CertificationStageStatus::Warning => 0.06,
        CertificationStageStatus::Pass => 0.0,
    };
    rounded_score(
        status_score
            + risk_confidence_bonus(stage.risk_level)
            + domain_confidence_bonus(stage.domain),
    )
}

fn risk_confidence_bonus(risk_level: TransformationRiskLevel) -> f64 {
    match risk_level {
        TransformationRiskLevel::Low => 0.02,
        TransformationRiskLevel::Medium => 0.04,
        TransformationRiskLevel::High => 0.08,
        TransformationRiskLevel::Critical => 0.12,
    }
}

fn domain_confidence_bonus(domain: CertificationDomain) -> f64 {
    match domain {
        CertificationDomain::Semantic
        | CertificationDomain::SemanticProof
        | CertificationDomain::Confidence
        | CertificationDomain::Compliance => 0.08,
        CertificationDomain::Visual
        | CertificationDomain::Performance
        | CertificationDomain::Accessibility => 0.05,
    }
}

fn expected_value_score(confidence_impact: f64, effort: CertificationRemediationEffort) -> f64 {
    rounded_score(confidence_impact / effort_cost(effort))
}

fn effort_cost(effort: CertificationRemediationEffort) -> f64 {
    match effort {
        CertificationRemediationEffort::Low => 1.0,
        CertificationRemediationEffort::Medium => 2.0,
        CertificationRemediationEffort::High => 3.0,
    }
}

fn issue_priority(action: &CertificationRemediationAction) -> u8 {
    if action.expected_confidence_impact >= 0.30
        || action.effort == CertificationRemediationEffort::High
    {
        1
    } else if action.expected_confidence_impact >= 0.18 {
        2
    } else {
        3
    }
}

fn rounded_score(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn action_id(domain: CertificationDomain, target: &str) -> String {
    format!("remediate-{}-{}", domain_label(domain), slug(target))
}

fn domain_label(domain: CertificationDomain) -> &'static str {
    match domain {
        CertificationDomain::Semantic => "semantic",
        CertificationDomain::SemanticProof => "semantic-proof",
        CertificationDomain::Visual => "visual",
        CertificationDomain::Performance => "performance",
        CertificationDomain::Accessibility => "accessibility",
        CertificationDomain::Confidence => "confidence",
        CertificationDomain::Compliance => "compliance",
    }
}

fn effort_label(effort: CertificationRemediationEffort) -> &'static str {
    match effort {
        CertificationRemediationEffort::Low => "low",
        CertificationRemediationEffort::Medium => "medium",
        CertificationRemediationEffort::High => "high",
    }
}

fn display_list(values: &[String]) -> String {
    if values.is_empty() {
        "n/a".to_string()
    } else {
        values.join(", ")
    }
}

fn slug(value: &str) -> String {
    let mut out = String::new();
    let mut last_was_separator = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator && !out.is_empty() {
            out.push('-');
            last_was_separator = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "target".to_string()
    } else {
        out
    }
}

fn sorted_vec(values: impl Iterator<Item = String>) -> Vec<String> {
    values.collect::<BTreeSet<_>>().into_iter().collect()
}

fn hash_serializable<T: Serialize>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!(
        "sha256:{}",
        crate::util::hex_encode(&hasher.finalize())
    ))
}

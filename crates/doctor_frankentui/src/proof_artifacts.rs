//! Machine-checkable semantic proof artifacts for certification verdicts.
//!
//! The artifact is intentionally built from comparator reports plus the source
//! and translated semantic runs. Reports carry verdict summaries; runs carry
//! the concrete observations needed for replayable witnesses.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::semantic_diff::{
    CounterexampleTrace, SemanticDiffReport, SemanticDiffVerdict, SemanticDifference,
    SemanticObservation, SemanticObservationKind, SemanticRun,
};

pub const SEMANTIC_PROOF_ARTIFACT_SCHEMA_VERSION: &str =
    "doctor_frankentui.semantic_proof_artifact.v1";

#[derive(Debug, Error)]
pub enum ProofArtifactError {
    #[error("proof artifact serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ProofArtifactError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticProofArtifact {
    pub schema_version: String,
    pub artifact_id: String,
    pub comparator_id: String,
    pub contract_id: String,
    pub source_run_id: String,
    pub translated_run_id: String,
    pub verdict: SemanticDiffVerdict,
    pub certification_passed: bool,
    pub obligations: Vec<SemanticProofObligation>,
    pub obligation_graph_checksum: String,
    pub artifact_checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticProofObligation {
    pub obligation_id: String,
    pub clause_id: String,
    pub verdict_clause: VerdictClause,
    pub status: ProofObligationStatus,
    pub witnesses: Vec<SemanticProofWitness>,
    pub witness_checksum: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum VerdictClause {
    Covered,
    Violated,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProofObligationStatus {
    Proven,
    Refuted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProofWitnessKind {
    MatchedObservation,
    ImprovementObservation,
    CounterexampleDifference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticProofWitness {
    pub witness_id: String,
    pub witness_kind: ProofWitnessKind,
    pub clause_id: String,
    pub source_sequence: Option<u32>,
    pub translated_sequence: Option<u32>,
    pub source_signature: Option<String>,
    pub translated_signature: Option<String>,
    pub difference_kind: Option<String>,
    pub message: String,
    pub replay_command: Option<String>,
    pub evidence_checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticProofValidationReport {
    pub artifact_id: String,
    pub machine_verifiable: bool,
    pub certification_passed: bool,
    pub expected_clause_ids: Vec<String>,
    pub missing_clause_ids: Vec<String>,
    pub invalid_obligation_ids: Vec<String>,
    pub issues: Vec<ProofValidationIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProofValidationIssue {
    pub code: String,
    pub target: String,
    pub message: String,
}

#[must_use]
pub fn semantic_proof_artifact_id(report: &SemanticDiffReport) -> String {
    format!(
        "semantic-proof:{}:{}:{}",
        report.contract_id, report.source_run_id, report.translated_run_id
    )
}

pub fn build_semantic_proof_artifact(
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
    report: &SemanticDiffReport,
) -> Result<SemanticProofArtifact> {
    let mut obligations = Vec::new();

    for clause_id in &report.covered_clause_ids {
        obligations.push(build_obligation(
            clause_id,
            VerdictClause::Covered,
            ProofObligationStatus::Proven,
            covered_witnesses_for_clause(source_run, translated_run, clause_id)?,
        )?);
    }

    for clause_id in &report.violated_clause_ids {
        obligations.push(build_obligation(
            clause_id,
            VerdictClause::Violated,
            ProofObligationStatus::Refuted,
            violated_witnesses_for_clause(report, clause_id)?,
        )?);
    }

    obligations.sort_by(|left, right| {
        (
            left.clause_id.as_str(),
            left.verdict_clause,
            left.obligation_id.as_str(),
        )
            .cmp(&(
                right.clause_id.as_str(),
                right.verdict_clause,
                right.obligation_id.as_str(),
            ))
    });

    let certification_passed =
        is_passing_semantic_verdict(report.verdict) && obligations.iter().all(is_proven);
    let obligation_graph_checksum = hash_serializable(&obligations)?;
    let mut artifact = SemanticProofArtifact {
        schema_version: SEMANTIC_PROOF_ARTIFACT_SCHEMA_VERSION.to_string(),
        artifact_id: semantic_proof_artifact_id(report),
        comparator_id: report.validator_id.clone(),
        contract_id: report.contract_id.clone(),
        source_run_id: report.source_run_id.clone(),
        translated_run_id: report.translated_run_id.clone(),
        verdict: report.verdict,
        certification_passed,
        obligations,
        obligation_graph_checksum,
        artifact_checksum: String::new(),
    };
    artifact.artifact_checksum = compute_semantic_proof_artifact_checksum(&artifact)?;
    Ok(artifact)
}

pub fn parse_semantic_proof_artifact(raw_json: &str) -> Result<SemanticProofArtifact> {
    Ok(serde_json::from_str(raw_json)?)
}

pub fn parse_and_verify_semantic_proof_artifact(
    raw_json: &str,
    report: &SemanticDiffReport,
) -> Result<SemanticProofValidationReport> {
    let artifact = parse_semantic_proof_artifact(raw_json)?;
    verify_semantic_proof_artifact(&artifact, report)
}

pub fn verify_semantic_proof_artifact(
    artifact: &SemanticProofArtifact,
    report: &SemanticDiffReport,
) -> Result<SemanticProofValidationReport> {
    let mut issues = Vec::new();
    let mut invalid_obligation_ids = BTreeSet::new();

    check_artifact_header(artifact, report, &mut issues)?;
    check_obligations(artifact, report, &mut issues, &mut invalid_obligation_ids)?;

    let expected_clause_ids = expected_clause_ids(report);
    let actual_clause_ids = artifact
        .obligations
        .iter()
        .map(|obligation| obligation.clause_id.clone())
        .collect::<BTreeSet<_>>();
    let missing_clause_ids = expected_clause_ids
        .difference(&actual_clause_ids)
        .cloned()
        .collect::<Vec<_>>();

    for clause_id in &missing_clause_ids {
        push_issue(
            &mut issues,
            "missing_obligation",
            clause_id,
            "verdict clause has no proof obligation",
        );
    }

    if expected_clause_ids.is_empty() {
        push_issue(
            &mut issues,
            "empty_expected_clause_set",
            &artifact.artifact_id,
            "semantic proof artifacts require at least one covered or violated clause",
        );
    }

    let machine_verifiable = issues.is_empty();
    let certification_passed = machine_verifiable
        && artifact.certification_passed
        && is_passing_semantic_verdict(report.verdict);

    Ok(SemanticProofValidationReport {
        artifact_id: artifact.artifact_id.clone(),
        machine_verifiable,
        certification_passed,
        expected_clause_ids: expected_clause_ids.into_iter().collect(),
        missing_clause_ids,
        invalid_obligation_ids: invalid_obligation_ids.into_iter().collect(),
        issues,
    })
}

pub fn compute_semantic_proof_artifact_checksum(
    artifact: &SemanticProofArtifact,
) -> Result<String> {
    let mut canonical = artifact.clone();
    canonical.artifact_checksum.clear();
    hash_serializable(&canonical)
}

fn build_obligation(
    clause_id: &str,
    verdict_clause: VerdictClause,
    status: ProofObligationStatus,
    mut witnesses: Vec<SemanticProofWitness>,
) -> Result<SemanticProofObligation> {
    witnesses.sort_by(|left, right| left.witness_id.cmp(&right.witness_id));
    let mut obligation = SemanticProofObligation {
        obligation_id: format!(
            "semantic-proof-obligation:{}:{}",
            clause_id,
            verdict_clause_label(verdict_clause)
        ),
        clause_id: clause_id.to_string(),
        verdict_clause,
        status,
        witnesses,
        witness_checksum: String::new(),
    };
    obligation.witness_checksum = hash_serializable(&obligation.witnesses)?;
    Ok(obligation)
}

fn covered_witnesses_for_clause(
    source_run: &SemanticRun,
    translated_run: &SemanticRun,
    clause_id: &str,
) -> Result<Vec<SemanticProofWitness>> {
    let mut witnesses = Vec::new();

    for source in source_run
        .observations
        .iter()
        .filter(|observation| observation.kind != SemanticObservationKind::Improvement)
        .filter(|observation| {
            observation
                .contract_clause_ids
                .iter()
                .any(|id| id == clause_id)
        })
    {
        for translated in translated_run
            .observations
            .iter()
            .filter(|observation| observation.kind != SemanticObservationKind::Improvement)
            .filter(|observation| {
                observation
                    .contract_clause_ids
                    .iter()
                    .any(|id| id == clause_id)
            })
            .filter(|observation| source.core_eq(observation))
        {
            witnesses.push(new_matched_observation_witness(
                clause_id,
                source,
                translated,
                translated_run
                    .replay_command
                    .as_ref()
                    .or(source_run.replay_command.as_ref()),
            )?);
        }
    }

    for translated in translated_run
        .observations
        .iter()
        .filter(|observation| observation.kind == SemanticObservationKind::Improvement)
        .filter(|observation| {
            observation
                .contract_clause_ids
                .iter()
                .any(|id| id == clause_id)
        })
    {
        witnesses.push(new_improvement_witness(
            clause_id,
            translated,
            translated_run.replay_command.as_ref(),
        )?);
    }

    Ok(witnesses)
}

fn violated_witnesses_for_clause(
    report: &SemanticDiffReport,
    clause_id: &str,
) -> Result<Vec<SemanticProofWitness>> {
    let mut witnesses = Vec::new();
    for (index, difference) in report
        .differences
        .iter()
        .enumerate()
        .filter(|(_, difference)| difference.clause_ids.iter().any(|id| id == clause_id))
    {
        witnesses.push(new_counterexample_witness(
            clause_id,
            index,
            difference,
            report.counterexample.as_ref(),
        )?);
    }
    Ok(witnesses)
}

fn new_matched_observation_witness(
    clause_id: &str,
    source: &SemanticObservation,
    translated: &SemanticObservation,
    replay_command: Option<&String>,
) -> Result<SemanticProofWitness> {
    let mut witness = SemanticProofWitness {
        witness_id: format!(
            "semantic-witness:{}:match:{}:{}:{}",
            clause_id, source.sequence, translated.sequence, source.key
        ),
        witness_kind: ProofWitnessKind::MatchedObservation,
        clause_id: clause_id.to_string(),
        source_sequence: Some(source.sequence),
        translated_sequence: Some(translated.sequence),
        source_signature: Some(source.comparable_signature()),
        translated_signature: Some(translated.comparable_signature()),
        difference_kind: None,
        message: "source and translated semantic observations match".to_string(),
        replay_command: replay_command.cloned(),
        evidence_checksum: String::new(),
    };
    witness.evidence_checksum = compute_witness_checksum(&witness)?;
    Ok(witness)
}

fn new_improvement_witness(
    clause_id: &str,
    translated: &SemanticObservation,
    replay_command: Option<&String>,
) -> Result<SemanticProofWitness> {
    let mut witness = SemanticProofWitness {
        witness_id: format!(
            "semantic-witness:{}:improvement:{}:{}",
            clause_id, translated.sequence, translated.key
        ),
        witness_kind: ProofWitnessKind::ImprovementObservation,
        clause_id: clause_id.to_string(),
        source_sequence: None,
        translated_sequence: Some(translated.sequence),
        source_signature: None,
        translated_signature: Some(translated.comparable_signature()),
        difference_kind: None,
        message: "translated run declares an allowed improvement dimension".to_string(),
        replay_command: replay_command.cloned(),
        evidence_checksum: String::new(),
    };
    witness.evidence_checksum = compute_witness_checksum(&witness)?;
    Ok(witness)
}

fn new_counterexample_witness(
    clause_id: &str,
    index: usize,
    difference: &SemanticDifference,
    counterexample: Option<&CounterexampleTrace>,
) -> Result<SemanticProofWitness> {
    let mut witness = SemanticProofWitness {
        witness_id: format!("semantic-witness:{clause_id}:counterexample:{index}"),
        witness_kind: ProofWitnessKind::CounterexampleDifference,
        clause_id: clause_id.to_string(),
        source_sequence: counterexample
            .and_then(|trace| trace.source_observations.first())
            .map(|observation| observation.sequence),
        translated_sequence: counterexample
            .and_then(|trace| trace.translated_observations.first())
            .map(|observation| observation.sequence),
        source_signature: difference.source_value.clone(),
        translated_signature: difference.translated_value.clone(),
        difference_kind: Some(format!("{:?}", difference.difference_kind)),
        message: difference.message.clone(),
        replay_command: counterexample.map(|trace| trace.replay_command.clone()),
        evidence_checksum: String::new(),
    };
    witness.evidence_checksum = compute_witness_checksum(&witness)?;
    Ok(witness)
}

fn check_artifact_header(
    artifact: &SemanticProofArtifact,
    report: &SemanticDiffReport,
    issues: &mut Vec<ProofValidationIssue>,
) -> Result<()> {
    if artifact.schema_version != SEMANTIC_PROOF_ARTIFACT_SCHEMA_VERSION {
        push_issue(
            issues,
            "schema_version_mismatch",
            &artifact.artifact_id,
            "proof artifact schema version is unsupported",
        );
    }
    if artifact.artifact_id.trim().is_empty() {
        push_issue(
            issues,
            "empty_artifact_id",
            "artifact_id",
            "proof artifact id must not be empty",
        );
    }
    if artifact.artifact_id != semantic_proof_artifact_id(report) {
        push_issue(
            issues,
            "artifact_id_mismatch",
            &artifact.artifact_id,
            "proof artifact id does not match semantic report identity",
        );
    }
    if artifact.comparator_id != report.validator_id {
        push_issue(
            issues,
            "comparator_id_mismatch",
            &artifact.comparator_id,
            "comparator id does not match semantic report validator",
        );
    }
    if artifact.contract_id != report.contract_id {
        push_issue(
            issues,
            "contract_id_mismatch",
            &artifact.contract_id,
            "contract id does not match semantic report",
        );
    }
    if artifact.source_run_id != report.source_run_id {
        push_issue(
            issues,
            "source_run_id_mismatch",
            &artifact.source_run_id,
            "source run id does not match semantic report",
        );
    }
    if artifact.translated_run_id != report.translated_run_id {
        push_issue(
            issues,
            "translated_run_id_mismatch",
            &artifact.translated_run_id,
            "translated run id does not match semantic report",
        );
    }
    if artifact.verdict != report.verdict {
        push_issue(
            issues,
            "verdict_mismatch",
            &artifact.artifact_id,
            "proof artifact verdict does not match semantic report",
        );
    }

    let expected_graph_checksum = hash_serializable(&artifact.obligations)?;
    if artifact.obligation_graph_checksum != expected_graph_checksum {
        push_issue(
            issues,
            "obligation_graph_checksum_mismatch",
            &artifact.artifact_id,
            "obligation graph checksum does not match obligations",
        );
    }

    let expected_artifact_checksum = compute_semantic_proof_artifact_checksum(artifact)?;
    if artifact.artifact_checksum != expected_artifact_checksum {
        push_issue(
            issues,
            "artifact_checksum_mismatch",
            &artifact.artifact_id,
            "artifact checksum does not match canonical artifact payload",
        );
    }

    if artifact.certification_passed && !is_passing_semantic_verdict(report.verdict) {
        push_issue(
            issues,
            "certification_pass_mismatch",
            &artifact.artifact_id,
            "non-passing semantic verdict cannot carry a passing proof artifact",
        );
    }

    Ok(())
}

fn check_obligations(
    artifact: &SemanticProofArtifact,
    report: &SemanticDiffReport,
    issues: &mut Vec<ProofValidationIssue>,
    invalid_obligation_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let expected_clause_ids = expected_clause_ids(report);
    let expected_status_by_clause = expected_status_by_clause(report);
    let mut seen_obligation_ids = BTreeSet::new();
    let mut seen_clause_sides = BTreeSet::new();

    for obligation in &artifact.obligations {
        let mut invalid = false;
        if obligation.obligation_id.trim().is_empty() {
            push_issue(
                issues,
                "empty_obligation_id",
                &obligation.clause_id,
                "proof obligation id must not be empty",
            );
            invalid = true;
        }
        if !seen_obligation_ids.insert(obligation.obligation_id.clone()) {
            push_issue(
                issues,
                "duplicate_obligation_id",
                &obligation.obligation_id,
                "proof obligation ids must be unique",
            );
            invalid = true;
        }
        if !seen_clause_sides.insert((obligation.clause_id.clone(), obligation.verdict_clause)) {
            push_issue(
                issues,
                "duplicate_clause_obligation",
                &obligation.obligation_id,
                "each clause/verdict side pair may appear once",
            );
            invalid = true;
        }
        if obligation.clause_id.trim().is_empty() {
            push_issue(
                issues,
                "empty_clause_id",
                &obligation.obligation_id,
                "proof obligation clause id must not be empty",
            );
            invalid = true;
        }
        if !expected_clause_ids.contains(&obligation.clause_id) {
            push_issue(
                issues,
                "unexpected_clause_id",
                &obligation.obligation_id,
                "proof obligation clause is not present in semantic verdict",
            );
            invalid = true;
        }
        if obligation.witnesses.is_empty() {
            push_issue(
                issues,
                "missing_witness",
                &obligation.obligation_id,
                "proof obligation must link to at least one concrete witness",
            );
            invalid = true;
        }
        if let Some(expected_status) = expected_status_by_clause.get(&obligation.clause_id)
            && obligation.status != *expected_status
        {
            push_issue(
                issues,
                "obligation_status_mismatch",
                &obligation.obligation_id,
                "proof obligation status does not match semantic verdict clause side",
            );
            invalid = true;
        }
        if !verdict_clause_matches_status(obligation.verdict_clause, obligation.status) {
            push_issue(
                issues,
                "verdict_clause_status_mismatch",
                &obligation.obligation_id,
                "covered obligations must be proven and violated obligations must be refuted",
            );
            invalid = true;
        }
        let expected_witness_checksum = hash_serializable(&obligation.witnesses)?;
        if obligation.witness_checksum != expected_witness_checksum {
            push_issue(
                issues,
                "witness_checksum_mismatch",
                &obligation.obligation_id,
                "witness checksum does not match witness payloads",
            );
            invalid = true;
        }
        invalid |= check_witnesses(obligation, issues)?;

        if invalid {
            invalid_obligation_ids.insert(obligation.obligation_id.clone());
        }
    }

    Ok(())
}

fn check_witnesses(
    obligation: &SemanticProofObligation,
    issues: &mut Vec<ProofValidationIssue>,
) -> Result<bool> {
    let mut invalid = false;
    let mut seen_witness_ids = BTreeSet::new();

    for witness in &obligation.witnesses {
        if witness.witness_id.trim().is_empty() {
            push_issue(
                issues,
                "empty_witness_id",
                &obligation.obligation_id,
                "proof witness id must not be empty",
            );
            invalid = true;
        }
        if !seen_witness_ids.insert(witness.witness_id.clone()) {
            push_issue(
                issues,
                "duplicate_witness_id",
                &witness.witness_id,
                "proof witness ids must be unique within an obligation",
            );
            invalid = true;
        }
        if witness.clause_id != obligation.clause_id {
            push_issue(
                issues,
                "witness_clause_mismatch",
                &witness.witness_id,
                "witness clause id must match its proof obligation",
            );
            invalid = true;
        }
        if witness.message.trim().is_empty() {
            push_issue(
                issues,
                "empty_witness_message",
                &witness.witness_id,
                "proof witness message must not be empty",
            );
            invalid = true;
        }
        if witness.source_signature.is_none() && witness.translated_signature.is_none() {
            push_issue(
                issues,
                "missing_witness_signature",
                &witness.witness_id,
                "proof witness must include source or translated evidence",
            );
            invalid = true;
        }
        let expected_evidence_checksum = compute_witness_checksum(witness)?;
        if witness.evidence_checksum != expected_evidence_checksum {
            push_issue(
                issues,
                "witness_evidence_checksum_mismatch",
                &witness.witness_id,
                "witness evidence checksum does not match witness payload",
            );
            invalid = true;
        }
    }

    Ok(invalid)
}

fn compute_witness_checksum(witness: &SemanticProofWitness) -> Result<String> {
    let mut canonical = witness.clone();
    canonical.evidence_checksum.clear();
    hash_serializable(&canonical)
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

fn expected_clause_ids(report: &SemanticDiffReport) -> BTreeSet<String> {
    report
        .covered_clause_ids
        .iter()
        .chain(report.violated_clause_ids.iter())
        .cloned()
        .collect()
}

fn expected_status_by_clause(
    report: &SemanticDiffReport,
) -> BTreeMap<String, ProofObligationStatus> {
    let mut statuses = BTreeMap::new();
    for clause_id in &report.covered_clause_ids {
        statuses.insert(clause_id.clone(), ProofObligationStatus::Proven);
    }
    for clause_id in &report.violated_clause_ids {
        statuses.insert(clause_id.clone(), ProofObligationStatus::Refuted);
    }
    statuses
}

fn is_passing_semantic_verdict(verdict: SemanticDiffVerdict) -> bool {
    matches!(
        verdict,
        SemanticDiffVerdict::Equivalent | SemanticDiffVerdict::AcceptableImprovement
    )
}

fn is_proven(obligation: &SemanticProofObligation) -> bool {
    obligation.status == ProofObligationStatus::Proven && !obligation.witnesses.is_empty()
}

fn verdict_clause_matches_status(
    verdict_clause: VerdictClause,
    status: ProofObligationStatus,
) -> bool {
    matches!(
        (verdict_clause, status),
        (VerdictClause::Covered, ProofObligationStatus::Proven)
            | (VerdictClause::Violated, ProofObligationStatus::Refuted)
    )
}

fn verdict_clause_label(verdict_clause: VerdictClause) -> &'static str {
    match verdict_clause {
        VerdictClause::Covered => "covered",
        VerdictClause::Violated => "violated",
    }
}

fn push_issue(issues: &mut Vec<ProofValidationIssue>, code: &str, target: &str, message: &str) {
    issues.push(ProofValidationIssue {
        code: code.to_string(),
        target: target.to_string(),
        message: message.to_string(),
    });
}

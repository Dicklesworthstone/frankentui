//! Cross-validator unit/property test-evidence harness for the alien-governance
//! validators (bd-3bxhj.10.15).
//!
//! Three modules form the *quality firewall* for the alien-graveyard governance
//! artifacts:
//!
//! - the recommendation-contract compiler + linter
//!   ([`crate::recommendation_contract`]) — EV/relevance/tier rules,
//!   risk-countermeasure completeness, comparator/rollback clauses, demo linkage,
//!   and graveyard-verify status;
//! - the ecosystem-scan schema gate ([`crate::ecosystem_scan`]) — adopt-vs-build
//!   coverage, crate-verdict consistency, constraint-axis rationale, and
//!   adoption-constraint enforcement (unsafe / determinism / license /
//!   portability);
//! - the primary-paper checklist validator ([`crate::paper_verification`]) —
//!   claim/threat completeness, citation/evidence linkage, and legal/IP rollout
//!   gating.
//!
//! Each module already ships its own inline unit tests. This module adds the
//! *cross-validator* contract the parent bead asks for: a single, host-agnostic
//! [`GovernanceDiagnostic`] envelope that normalizes every validator's findings
//! into one structured schema, plus a deterministic [`GovernanceValidationReport`]
//! that the downstream E2E gauntlets (bd-3bxhj.10.16 / .10.36 / .10.37 / .10.44)
//! can consume without adapter drift.
//!
//! Every diagnostic carries the exact fields the bead's acceptance criteria
//! mandate for failure logs:
//!
//! - `contract_id` (the recommendation-contract card id, or `n/a`),
//! - `clause_id` (the lint clause / scan-violation code / checklist field),
//! - `checklist_id` (the paper-verification primitive, or `n/a`),
//! - `severity` (block / warn),
//! - `artifact_path` (a deterministic pointer to the originating evidence
//!   artifact),
//! - `remediation_hint`, and
//! - `replay_cmd` (a deterministic single-command replay reference).
//!
//! The harness is pure and deterministic — it owns no I/O — so the same fixture
//! set always yields the same `report_id` and `evidence_checksum`. Equivalent
//! inputs produce byte-identical diagnostics, ordering, and hashes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ecosystem_scan::{
    EcosystemScanConfig, EcosystemScanGate, EcosystemScanRecord, EcosystemScanReport, ScanSeverity,
    canonical_ledger,
};
use crate::paper_verification::{
    ExtractedClaim, Primitive, VERIFICATION_SCHEMA_VERSION, VerificationBundle,
    VerificationRegistry, VerificationStatus, build_default_registry, validate_registry,
};
use crate::recommendation_contract::{
    ContractLintConfig, ContractLintReport, ContractSeverity, GraveyardVerifyStatus,
    RecommendationContract, RecommendationContractLinter, example_complete_contract,
};
use crate::semantic_contract::IpArtifactStatus;

/// Schema version for the cross-validator governance diagnostic contract.
pub const GOVERNANCE_VALIDATOR_SCHEMA_VERSION: &str = "governance-validator-tests-v1";

// ── Validator identity ──────────────────────────────────────────────────────

/// Which governance validator produced a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidatorClass {
    /// Recommendation-contract compiler + linter (`recommendation_contract`).
    ContractLinter,
    /// Ecosystem-scan adopt-vs-build schema gate (`ecosystem_scan`).
    EcosystemScan,
    /// Primary-paper checklist validator (`paper_verification`).
    PaperChecklist,
}

impl ValidatorClass {
    /// Every validator class, in stable order.
    pub const ALL: &'static [ValidatorClass] = &[
        ValidatorClass::ContractLinter,
        ValidatorClass::EcosystemScan,
        ValidatorClass::PaperChecklist,
    ];

    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContractLinter => "contract_linter",
            Self::EcosystemScan => "ecosystem_scan",
            Self::PaperChecklist => "paper_checklist",
        }
    }
}

/// Normalized severity across all validator classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceSeverity {
    /// Blocks the governance gate.
    Block,
    /// Advisory only.
    Warn,
}

impl GovernanceSeverity {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Warn => "warn",
        }
    }
}

impl From<ContractSeverity> for GovernanceSeverity {
    fn from(severity: ContractSeverity) -> Self {
        match severity {
            ContractSeverity::Block => Self::Block,
            ContractSeverity::Warn => Self::Warn,
        }
    }
}

impl From<ScanSeverity> for GovernanceSeverity {
    fn from(severity: ScanSeverity) -> Self {
        match severity {
            ScanSeverity::Block => Self::Block,
            ScanSeverity::Warn => Self::Warn,
        }
    }
}

// ── Unified diagnostic envelope ─────────────────────────────────────────────

/// A single normalized governance diagnostic.
///
/// This is the structured schema contract consumed by the downstream E2E
/// gauntlets. Every field is always populated (`n/a` where a slot does not apply
/// to the originating validator class), so failure logs are forensically rich
/// regardless of which validator produced them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceDiagnostic {
    /// Which validator produced this diagnostic.
    pub validator_class: ValidatorClass,
    /// The universal subject handle (card id / candidate id / primitive id).
    pub subject_id: String,
    /// The recommendation-contract card id (or `n/a`).
    pub contract_id: String,
    /// The lint clause / scan-violation code / checklist field path.
    pub clause_id: String,
    /// The paper-verification primitive id (or `n/a`).
    pub checklist_id: String,
    /// The stable machine code for this diagnostic.
    pub code: String,
    /// Normalized severity.
    pub severity: GovernanceSeverity,
    /// Deterministic pointer to the originating evidence artifact.
    pub artifact_path: String,
    /// Human-readable detail.
    pub detail: String,
    /// Remediation hint.
    pub remediation_hint: String,
    /// Upstream dependency hint (or `n/a`).
    pub dependency_hint: String,
    /// Deterministic single-command replay reference.
    pub replay_cmd: String,
}

impl GovernanceDiagnostic {
    /// Whether this diagnostic blocks the governance gate.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.severity == GovernanceSeverity::Block
    }

    /// Whether every required failure-log field is populated and non-empty.
    ///
    /// Mirrors the bead's acceptance criterion that failure logs always emit
    /// `contract_id`, `clause_id`, `checklist_id`, `severity`, `artifact_path`,
    /// `remediation_hint`, and `replay_cmd`.
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.subject_id.is_empty()
            && !self.contract_id.is_empty()
            && !self.clause_id.is_empty()
            && !self.checklist_id.is_empty()
            && !self.code.is_empty()
            && !self.artifact_path.is_empty()
            && !self.detail.is_empty()
            && !self.remediation_hint.is_empty()
            && !self.dependency_hint.is_empty()
            && !self.replay_cmd.is_empty()
    }
}

// ── Normalization (validator report → unified diagnostics) ──────────────────

/// Normalize a recommendation-contract lint report into unified diagnostics.
#[must_use]
pub fn normalize_contract_report(report: &ContractLintReport) -> Vec<GovernanceDiagnostic> {
    let artifact_path = report.exported_json_stats.path.clone();
    report
        .findings
        .iter()
        .map(|finding| {
            let contract_id = nonempty_or_na(&finding.card_id);
            GovernanceDiagnostic {
                validator_class: ValidatorClass::ContractLinter,
                subject_id: contract_id.clone(),
                contract_id,
                clause_id: nonempty_or_na(&finding.clause_id),
                checklist_id: na(),
                code: finding.code.as_str().to_string(),
                severity: GovernanceSeverity::from(finding.severity),
                artifact_path: artifact_path.clone(),
                detail: nonempty_or_na(&finding.detail),
                remediation_hint: nonempty_or_na(&finding.remediation),
                dependency_hint: nonempty_or_na(&finding.dependency_hint),
                replay_cmd: report.replay_command.clone(),
            }
        })
        .collect()
}

/// Normalize an ecosystem-scan report into unified diagnostics.
#[must_use]
pub fn normalize_scan_report(report: &EcosystemScanReport) -> Vec<GovernanceDiagnostic> {
    let artifact_path = report.exported_json_stats.path.clone();
    report
        .violations
        .iter()
        .map(|violation| {
            let code = violation.code.as_str().to_string();
            GovernanceDiagnostic {
                validator_class: ValidatorClass::EcosystemScan,
                subject_id: nonempty_or_na(&violation.candidate_id),
                contract_id: na(),
                clause_id: code.clone(),
                checklist_id: na(),
                code,
                severity: GovernanceSeverity::from(violation.severity),
                artifact_path: artifact_path.clone(),
                detail: nonempty_or_na(&violation.detail),
                remediation_hint: nonempty_or_na(&violation.remediation),
                dependency_hint: na(),
                replay_cmd: report.replay_command.clone(),
            }
        })
        .collect()
}

/// Normalize a paper-verification registry's validation errors into unified
/// diagnostics.
///
/// Paper-checklist validation errors are correctness violations, so they all
/// normalize to [`GovernanceSeverity::Block`]. The `artifact_path` points at the
/// most specific reproduction artifact available for the offending primitive,
/// and the `replay_cmd` is a deterministic per-primitive validate command.
#[must_use]
pub fn normalize_paper_registry(registry: &VerificationRegistry) -> Vec<GovernanceDiagnostic> {
    validate_registry(registry)
        .iter()
        .map(|error| {
            let primitive = primitive_id(error.primitive);
            let artifact_path = registry
                .bundles
                .get(&error.primitive)
                .map_or_else(na, primary_repro_pointer);
            let replay_cmd = format!(
                "doctor_frankentui paper-verification-validate \
                 --primitive {primitive} --schema {VERIFICATION_SCHEMA_VERSION}"
            );
            GovernanceDiagnostic {
                validator_class: ValidatorClass::PaperChecklist,
                subject_id: primitive.clone(),
                contract_id: na(),
                clause_id: nonempty_or_na(&error.field),
                checklist_id: primitive,
                code: format!("PV-{}", error.field.replace('.', "-")),
                severity: GovernanceSeverity::Block,
                artifact_path,
                detail: nonempty_or_na(&error.message),
                remediation_hint: paper_remediation_for(&error.field),
                dependency_hint: na(),
                replay_cmd,
            }
        })
        .collect()
}

// ── Fixture set + configuration ─────────────────────────────────────────────

/// A green-path or red-path fixture set spanning all three validator classes.
#[derive(Debug, Clone)]
pub struct GovernanceFixtureSet {
    /// Scenario label (becomes the report's `scenario_label`).
    pub label: String,
    /// Recommendation contracts to lint.
    pub contracts: Vec<RecommendationContract>,
    /// Ecosystem-scan records to gate.
    pub scan_records: Vec<EcosystemScanRecord>,
    /// Paper-verification registry to validate.
    pub paper_registry: VerificationRegistry,
}

/// Configuration for the unified governance validation harness.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GovernanceValidationConfig {
    /// Contract-linter configuration.
    pub lint_config: ContractLintConfig,
    /// Ecosystem-scan gate configuration.
    pub scan_config: EcosystemScanConfig,
}

// ── Summary + artifact + report ─────────────────────────────────────────────

/// Aggregate counts over the unified diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceValidationSummary {
    /// Total diagnostics emitted.
    pub total_diagnostics: usize,
    /// Diagnostics that block the gate.
    pub blocking_diagnostics: usize,
    /// Advisory diagnostics.
    pub advisory_diagnostics: usize,
    /// Diagnostics from the contract linter.
    pub contract_diagnostics: usize,
    /// Diagnostics from the ecosystem-scan gate.
    pub scan_diagnostics: usize,
    /// Diagnostics from the paper-checklist validator.
    pub paper_diagnostics: usize,
    /// Whether the contract-lint gate passed.
    pub lint_gate_passes: bool,
    /// Whether the ecosystem-scan gate passed.
    pub scan_gate_passes: bool,
    /// Whether the paper-verification registry validated clean.
    pub paper_validates_clean: bool,
    /// Whether the combined governance gate passed.
    pub governance_gate_passes: bool,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// The full cross-validator governance validation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GovernanceValidationReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report identifier (derived from the diagnostics).
    pub report_id: String,
    /// Scenario label.
    pub scenario_label: String,
    /// The configuration that produced this report.
    pub config: GovernanceValidationConfig,
    /// The full diagnostic ledger.
    pub diagnostics: Vec<GovernanceDiagnostic>,
    /// Aggregate summary.
    pub summary: GovernanceValidationSummary,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: GovernanceJsonStatsArtifact,
    /// Replay command for the whole report.
    pub replay_command: String,
    /// SHA-256 fingerprint of the diagnostics (output checksum).
    pub evidence_checksum: String,
}

impl GovernanceValidationReport {
    /// All diagnostics from a given validator class, in ledger order.
    #[must_use]
    pub fn diagnostics_for(&self, class: ValidatorClass) -> Vec<&GovernanceDiagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.validator_class == class)
            .collect()
    }

    /// All blocking diagnostics, in ledger order.
    #[must_use]
    pub fn blocking(&self) -> Vec<&GovernanceDiagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.is_blocking())
            .collect()
    }
}

/// Run all three validators over a fixture set and assemble a deterministic,
/// normalized governance report.
#[must_use]
pub fn run_governance_validation(
    fixtures: &GovernanceFixtureSet,
    config: &GovernanceValidationConfig,
) -> GovernanceValidationReport {
    let contract_report = RecommendationContractLinter::new(config.lint_config.clone())
        .evaluate(fixtures.contracts.clone());
    let scan_report =
        EcosystemScanGate::new(config.scan_config.clone()).evaluate(fixtures.scan_records.clone());
    let paper_validates_clean = validate_registry(&fixtures.paper_registry).is_empty();

    let mut diagnostics = Vec::new();
    diagnostics.extend(normalize_contract_report(&contract_report));
    diagnostics.extend(normalize_scan_report(&scan_report));
    diagnostics.extend(normalize_paper_registry(&fixtures.paper_registry));

    let summary = summarize(
        &diagnostics,
        contract_report.lint_gate_passes,
        scan_report.scan_gate_passes,
        paper_validates_clean,
    );
    let evidence_checksum = stable_hash(&diagnostics);
    let report_id = format!(
        "governance-validator-tests-{}",
        short_hash(&stable_hash(&ReportIdInput {
            schema_version: GOVERNANCE_VALIDATOR_SCHEMA_VERSION,
            scenario_label: &fixtures.label,
            evidence_checksum: &evidence_checksum,
        })),
    );
    let replay_command = format!(
        "doctor_frankentui governance-validate --report-id {report_id} --scenario {}",
        fixtures.label
    );
    let exported_json_stats = export_json_stats(
        &report_id,
        &fixtures.label,
        &summary,
        &diagnostics,
        &evidence_checksum,
    );

    GovernanceValidationReport {
        schema_version: GOVERNANCE_VALIDATOR_SCHEMA_VERSION.to_string(),
        report_id,
        scenario_label: fixtures.label.clone(),
        config: config.clone(),
        diagnostics,
        summary,
        exported_json_stats,
        replay_command,
        evidence_checksum,
    }
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    schema_version: &'a str,
    scenario_label: &'a str,
    evidence_checksum: &'a str,
}

fn summarize(
    diagnostics: &[GovernanceDiagnostic],
    lint_gate_passes: bool,
    scan_gate_passes: bool,
    paper_validates_clean: bool,
) -> GovernanceValidationSummary {
    let mut blocking = 0;
    let mut contract = 0;
    let mut scan = 0;
    let mut paper = 0;
    for diagnostic in diagnostics {
        if diagnostic.is_blocking() {
            blocking += 1;
        }
        match diagnostic.validator_class {
            ValidatorClass::ContractLinter => contract += 1,
            ValidatorClass::EcosystemScan => scan += 1,
            ValidatorClass::PaperChecklist => paper += 1,
        }
    }
    let total = diagnostics.len();
    let governance_gate_passes =
        lint_gate_passes && scan_gate_passes && paper_validates_clean && blocking == 0;
    GovernanceValidationSummary {
        total_diagnostics: total,
        blocking_diagnostics: blocking,
        advisory_diagnostics: total - blocking,
        contract_diagnostics: contract,
        scan_diagnostics: scan,
        paper_diagnostics: paper,
        lint_gate_passes,
        scan_gate_passes,
        paper_validates_clean,
        governance_gate_passes,
    }
}

fn export_json_stats(
    report_id: &str,
    scenario_label: &str,
    summary: &GovernanceValidationSummary,
    diagnostics: &[GovernanceDiagnostic],
    evidence_checksum: &str,
) -> GovernanceJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        scenario_label: &'a str,
        summary: &'a GovernanceValidationSummary,
        evidence_checksum: &'a str,
        diagnostics: &'a [GovernanceDiagnostic],
    }
    let payload = Export {
        schema_version: GOVERNANCE_VALIDATOR_SCHEMA_VERSION,
        report_id,
        scenario_label,
        summary,
        evidence_checksum,
        diagnostics,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    GovernanceJsonStatsArtifact {
        path: format!("{report_id}/governance_validator_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Green-path fixtures ─────────────────────────────────────────────────────

/// A fully-populated contract that lints clean under the default linter.
#[must_use]
pub fn contract_clean() -> RecommendationContract {
    example_complete_contract()
}

/// The canonical ecosystem-scan ledger that passes the default gate.
#[must_use]
pub fn scan_ledger_clean() -> Vec<EcosystemScanRecord> {
    canonical_ledger()
}

/// The canonical paper-verification registry that validates clean.
#[must_use]
pub fn paper_registry_clean() -> VerificationRegistry {
    build_default_registry()
}

/// A green-path fixture set: every validator passes its gate.
#[must_use]
pub fn green_fixture_set() -> GovernanceFixtureSet {
    GovernanceFixtureSet {
        label: "green".to_string(),
        contracts: vec![contract_clean()],
        scan_records: scan_ledger_clean(),
        paper_registry: paper_registry_clean(),
    }
}

// ── Red-path contract fixtures ──────────────────────────────────────────────

/// A contract with an empty card id (malformed identity).
#[must_use]
pub fn contract_missing_card_id() -> RecommendationContract {
    let mut contract = example_complete_contract();
    contract.card_id = String::new();
    contract
}

/// A contract declaring a legally-blocked artifact (contradictory legal policy).
#[must_use]
pub fn contract_legal_blocked() -> RecommendationContract {
    let mut contract = example_complete_contract();
    contract.card_id = "rc-legal-blocked".to_string();
    contract.failure_mode.legal_status = IpArtifactStatus::Blocked;
    contract
}

/// A contract missing its reproduction and provenance artifacts
/// (missing-artifact mode).
#[must_use]
pub fn contract_missing_artifacts() -> RecommendationContract {
    let mut contract = example_complete_contract();
    contract.card_id = "rc-missing-artifacts".to_string();
    contract.failure_mode.repro_artifact = None;
    contract.failure_mode.provenance_artifact = None;
    contract
}

/// A contract whose graveyard-verify status is still unverified
/// (incomplete graduation evidence).
#[must_use]
pub fn contract_graveyard_unverified() -> RecommendationContract {
    let mut contract = example_complete_contract();
    contract.card_id = "rc-graveyard-unverified".to_string();
    contract.graduation.graveyard_verify_status = GraveyardVerifyStatus::NotVerified;
    contract
}

// ── Red-path ecosystem-scan fixtures ────────────────────────────────────────

/// A scan ledger missing one candidate (coverage gap).
#[must_use]
pub fn scan_ledger_missing_candidate() -> Vec<EcosystemScanRecord> {
    let mut ledger = canonical_ledger();
    ledger.pop();
    ledger
}

/// A scan ledger with a duplicated candidate record (contradictory declarations).
#[must_use]
pub fn scan_ledger_duplicate() -> Vec<EcosystemScanRecord> {
    let mut ledger = canonical_ledger();
    if let Some(first) = ledger.first().cloned() {
        ledger.push(first);
    }
    ledger
}

// ── Red-path paper-verification fixtures ────────────────────────────────────

/// A registry whose CEGIS checklist claims `Read` status with no extracted
/// claims (incomplete checklist).
#[must_use]
pub fn paper_registry_read_without_claims() -> VerificationRegistry {
    let mut registry = build_default_registry();
    mutate_bundle(&mut registry, Primitive::Cegis, |bundle| {
        bundle.checklist.status = VerificationStatus::Read;
        bundle.checklist.claims.clear();
    });
    registry
}

/// A registry whose E-Graphs primitive is marked production-cleared despite a
/// blocked patent status (contradictory legal/IP declaration).
#[must_use]
pub fn paper_registry_blocked_cleared() -> VerificationRegistry {
    let mut registry = build_default_registry();
    mutate_bundle(&mut registry, Primitive::EGraphs, |bundle| {
        bundle.legal_ip.patent_status = IpArtifactStatus::Blocked;
        bundle.legal_ip.cleared_for_production = true;
    });
    registry
}

/// A registry whose Concolic/DSE checklist has duplicate claim ids.
#[must_use]
pub fn paper_registry_duplicate_claims() -> VerificationRegistry {
    let mut registry = build_default_registry();
    mutate_bundle(&mut registry, Primitive::ConcolicDse, |bundle| {
        bundle.checklist.claims = vec![
            make_claim("dse-claim-001", false, None),
            make_claim("dse-claim-001", false, None),
        ];
    });
    registry
}

/// A registry whose Abstract-Interpretation checklist marks a claim verified
/// with no evidence path (missing-artifact mode).
#[must_use]
pub fn paper_registry_verified_without_evidence() -> VerificationRegistry {
    let mut registry = build_default_registry();
    mutate_bundle(&mut registry, Primitive::AbstractInterpretation, |bundle| {
        bundle.checklist.claims = vec![make_claim("ai-claim-001", true, None)];
    });
    registry
}

/// A registry whose Metamorphic-Relations repro pack references a claim id that
/// is not present in the checklist (stale reference).
#[must_use]
pub fn paper_registry_orphan_claim_ref() -> VerificationRegistry {
    let mut registry = build_default_registry();
    mutate_bundle(&mut registry, Primitive::MetamorphicRelations, |bundle| {
        bundle.repro_pack.referenced_by_claims = vec!["ghost-claim".to_string()];
    });
    registry
}

/// A registry whose Shadow-Run repro pack references an on-disk artifact path
/// that does not exist (used with [`crate::paper_verification::verify_artifact_paths`]).
#[must_use]
pub fn paper_registry_missing_artifact() -> VerificationRegistry {
    let mut registry = build_default_registry();
    mutate_bundle(&mut registry, Primitive::ShadowRunGovernance, |bundle| {
        bundle.repro_pack.artifact_paths = vec![PathBuf::from("governance/missing_artifact.json")];
    });
    registry
}

/// A registry that trips several distinct paper-verification invariants at once.
#[must_use]
pub fn paper_registry_broken() -> VerificationRegistry {
    let mut registry = build_default_registry();
    mutate_bundle(&mut registry, Primitive::Cegis, |bundle| {
        bundle.checklist.status = VerificationStatus::Read;
        bundle.checklist.claims.clear();
    });
    mutate_bundle(&mut registry, Primitive::EGraphs, |bundle| {
        bundle.legal_ip.patent_status = IpArtifactStatus::Blocked;
        bundle.legal_ip.cleared_for_production = true;
    });
    mutate_bundle(&mut registry, Primitive::ConcolicDse, |bundle| {
        bundle.checklist.claims = vec![
            make_claim("dse-claim-001", false, None),
            make_claim("dse-claim-001", false, None),
        ];
    });
    registry
}

/// A red-path fixture set: every validator surfaces blocking diagnostics.
#[must_use]
pub fn red_fixture_set() -> GovernanceFixtureSet {
    GovernanceFixtureSet {
        label: "red".to_string(),
        contracts: vec![contract_legal_blocked(), contract_missing_artifacts()],
        scan_records: scan_ledger_missing_candidate(),
        paper_registry: paper_registry_broken(),
    }
}

// ── Fixture helpers ─────────────────────────────────────────────────────────

fn mutate_bundle(
    registry: &mut VerificationRegistry,
    primitive: Primitive,
    apply: impl FnOnce(&mut VerificationBundle),
) {
    if let Some(bundle) = registry.bundles.get_mut(&primitive) {
        apply(bundle);
    }
}

fn make_claim(id: &str, verified: bool, evidence: Option<PathBuf>) -> ExtractedClaim {
    ExtractedClaim {
        claim_id: id.to_string(),
        claim: format!("claim {id}"),
        source_ref: "§1".to_string(),
        relevant: true,
        verified,
        evidence_path: evidence,
    }
}

// ── Normalization helpers ───────────────────────────────────────────────────

fn primary_repro_pointer(bundle: &VerificationBundle) -> String {
    let pack = &bundle.repro_pack;
    pack.repro_lock
        .as_ref()
        .or(pack.manifest_json.as_ref())
        .or(pack.env_json.as_ref())
        .or(pack.corpus_manifest.as_ref())
        .or_else(|| pack.artifact_paths.first())
        .map_or_else(na, |path| path.display().to_string())
}

fn primitive_id(primitive: Primitive) -> String {
    serde_json::to_value(primitive)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{primitive:?}"))
}

fn paper_remediation_for(field: &str) -> String {
    match field {
        "checklist.claims" => "extract paper claims before advancing past not-started",
        "checklist.claims.claim_id" => "give every claim a unique, non-empty id",
        "checklist.claims.evidence_path" => "attach an evidence artifact path to verified claims",
        "checklist.threats.threat_id" => "give every threat a non-empty id",
        "legal_ip.patent_status" => "reconcile patent status with the production-clearance flag",
        "legal_ip.rollout_risk_gate" => "set the rollout-risk gate consistent with patent status",
        "repro_pack.referenced_by_claims" => "reference only claim ids present in the checklist",
        "repro_pack.artifact_paths" => "ensure every referenced artifact exists on disk",
        "schema_version" => "regenerate the registry with the current schema version",
        _ => "review the paper-verification invariant for this field",
    }
    .to_string()
}

fn nonempty_or_na(value: &str) -> String {
    if value.trim().is_empty() {
        na()
    } else {
        value.to_string()
    }
}

// ── Hashing helpers (mirrors the crate's deterministic-stack idiom) ─────────

fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

fn na() -> String {
    "n/a".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paper_verification::{validate_bundle, verify_artifact_paths};
    use proptest::prelude::*;

    fn config() -> GovernanceValidationConfig {
        GovernanceValidationConfig::default()
    }

    // ── Green path ───────────────────────────────────────────────────────

    #[test]
    fn green_fixture_passes_every_gate() {
        let report = run_governance_validation(&green_fixture_set(), &config());
        assert!(report.summary.lint_gate_passes);
        assert!(report.summary.scan_gate_passes);
        assert!(report.summary.paper_validates_clean);
        assert!(report.summary.governance_gate_passes);
        assert_eq!(report.summary.blocking_diagnostics, 0);
        assert!(report.blocking().is_empty());
    }

    #[test]
    fn green_fixture_has_no_paper_diagnostics() {
        let report = run_governance_validation(&green_fixture_set(), &config());
        assert_eq!(report.summary.paper_diagnostics, 0);
        assert!(
            report
                .diagnostics_for(ValidatorClass::PaperChecklist)
                .is_empty()
        );
    }

    // ── Red path: contract linter ────────────────────────────────────────

    #[test]
    fn contract_missing_card_id_blocks_gate() {
        let report = RecommendationContractLinter::new(ContractLintConfig::default())
            .evaluate(vec![contract_missing_card_id()]);
        let diagnostics = normalize_contract_report(&report);
        assert!(!report.lint_gate_passes);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "missing_card_id" && d.is_blocking())
        );
    }

    #[test]
    fn contract_legal_blocked_is_flagged() {
        let report = RecommendationContractLinter::new(ContractLintConfig::default())
            .evaluate(vec![contract_legal_blocked()]);
        let diagnostics = normalize_contract_report(&report);
        assert!(!report.lint_gate_passes);
        let finding = diagnostics
            .iter()
            .find(|d| d.code == "legal_status_blocked")
            .expect("legal_status_blocked diagnostic");
        assert!(finding.is_blocking());
        assert_eq!(finding.contract_id, "rc-legal-blocked");
        assert_eq!(finding.checklist_id, "n/a");
    }

    #[test]
    fn contract_missing_artifacts_flags_both_artifacts() {
        let report = RecommendationContractLinter::new(ContractLintConfig::default())
            .evaluate(vec![contract_missing_artifacts()]);
        let diagnostics = normalize_contract_report(&report);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "missing_repro_artifact")
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "missing_provenance_artifact")
        );
    }

    #[test]
    fn contract_graveyard_unverified_severity_follows_policy() {
        // Strict policy: an unverified graveyard entry blocks promotion.
        let strict = ContractLintConfig::default().with_graveyard_unverified_blocks(true);
        let strict_report = RecommendationContractLinter::new(strict)
            .evaluate(vec![contract_graveyard_unverified()]);
        assert!(
            normalize_contract_report(&strict_report)
                .iter()
                .any(|d| d.code == "graveyard_not_verified" && d.is_blocking())
        );
        // Default policy: the same condition is advisory, not blocking.
        let default_report = RecommendationContractLinter::new(ContractLintConfig::default())
            .evaluate(vec![contract_graveyard_unverified()]);
        assert!(
            normalize_contract_report(&default_report)
                .iter()
                .any(|d| d.code == "graveyard_not_verified" && !d.is_blocking())
        );
    }

    #[test]
    fn duplicate_contracts_are_flagged_once() {
        let report = RecommendationContractLinter::new(ContractLintConfig::default())
            .evaluate(vec![contract_clean(), contract_clean()]);
        let diagnostics = normalize_contract_report(&report);
        let dups = diagnostics
            .iter()
            .filter(|d| d.code == "duplicate_contract")
            .count();
        assert_eq!(dups, 1, "duplicate should be reported exactly once");
    }

    // ── Red path: ecosystem scan ─────────────────────────────────────────

    #[test]
    fn scan_missing_candidate_blocks_coverage() {
        let report = EcosystemScanGate::default().evaluate(scan_ledger_missing_candidate());
        let diagnostics = normalize_scan_report(&report);
        assert!(!report.coverage_complete);
        assert!(!report.scan_gate_passes);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "missing_candidate_coverage" && d.is_blocking())
        );
    }

    #[test]
    fn scan_duplicate_candidate_is_flagged() {
        let report = EcosystemScanGate::default().evaluate(scan_ledger_duplicate());
        let diagnostics = normalize_scan_report(&report);
        assert!(!report.scan_gate_passes);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "duplicate_candidate" && d.is_blocking())
        );
    }

    #[test]
    fn clean_scan_ledger_passes() {
        let report = EcosystemScanGate::default().evaluate(scan_ledger_clean());
        assert!(report.scan_gate_passes);
        assert!(
            normalize_scan_report(&report)
                .iter()
                .all(|d| !d.is_blocking())
        );
    }

    // ── Red path: paper checklist ────────────────────────────────────────

    #[test]
    fn paper_read_without_claims_is_flagged() {
        let registry = paper_registry_read_without_claims();
        let diagnostics = normalize_paper_registry(&registry);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.clause_id == "checklist.claims" && d.is_blocking())
        );
        assert_eq!(diagnostics[0].checklist_id, "cegis");
    }

    #[test]
    fn paper_blocked_cleared_is_flagged() {
        let registry = paper_registry_blocked_cleared();
        let diagnostics = normalize_paper_registry(&registry);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.clause_id == "legal_ip.patent_status")
        );
    }

    #[test]
    fn paper_duplicate_claims_is_flagged() {
        let registry = paper_registry_duplicate_claims();
        let diagnostics = normalize_paper_registry(&registry);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.clause_id == "checklist.claims.claim_id")
        );
    }

    #[test]
    fn paper_verified_without_evidence_is_flagged() {
        let registry = paper_registry_verified_without_evidence();
        let diagnostics = normalize_paper_registry(&registry);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.clause_id == "checklist.claims.evidence_path")
        );
    }

    #[test]
    fn paper_orphan_claim_ref_is_flagged() {
        let registry = paper_registry_orphan_claim_ref();
        let diagnostics = normalize_paper_registry(&registry);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.clause_id == "repro_pack.referenced_by_claims")
        );
    }

    #[test]
    fn paper_missing_artifact_path_is_detected_on_disk() {
        // missing-artifact mode: validate_registry ignores disk, but
        // verify_artifact_paths against a tempdir root must catch it.
        let registry = paper_registry_missing_artifact();
        let bundle = registry
            .bundles
            .get(&Primitive::ShadowRunGovernance)
            .expect("shadow-run bundle");
        assert!(
            validate_bundle(bundle).is_empty(),
            "schema-only validation should pass; the failure is on-disk only"
        );
        let root = tempfile::tempdir().expect("tempdir");
        let errors = verify_artifact_paths(bundle, root.path());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "repro_pack.artifact_paths");
    }

    #[test]
    fn broken_paper_registry_trips_multiple_primitives() {
        let diagnostics = normalize_paper_registry(&paper_registry_broken());
        let subjects: std::collections::BTreeSet<&str> = diagnostics
            .iter()
            .map(|d| d.checklist_id.as_str())
            .collect();
        assert!(subjects.contains("cegis"));
        assert!(subjects.contains("e_graphs"));
        assert!(subjects.contains("concolic_dse"));
    }

    // ── Acceptance criterion: required failure-log fields ────────────────

    #[test]
    fn every_red_diagnostic_carries_required_fields() {
        let report = run_governance_validation(&red_fixture_set(), &config());
        assert!(report.summary.blocking_diagnostics > 0);
        for diagnostic in &report.diagnostics {
            assert!(
                diagnostic.has_required_fields(),
                "diagnostic missing a required field: {diagnostic:?}"
            );
            assert!(diagnostic.replay_cmd.contains("doctor_frankentui"));
        }
    }

    #[test]
    fn red_fixture_spans_all_three_validator_classes() {
        let report = run_governance_validation(&red_fixture_set(), &config());
        for class in ValidatorClass::ALL {
            assert!(
                !report.diagnostics_for(*class).is_empty(),
                "no diagnostics from {}",
                class.as_str()
            );
        }
    }

    #[test]
    fn each_class_uses_its_own_subject_slot() {
        let report = run_governance_validation(&red_fixture_set(), &config());
        for diagnostic in &report.diagnostics {
            match diagnostic.validator_class {
                ValidatorClass::ContractLinter => {
                    assert_ne!(diagnostic.contract_id, "n/a");
                    assert_eq!(diagnostic.checklist_id, "n/a");
                }
                ValidatorClass::EcosystemScan => {
                    assert_eq!(diagnostic.contract_id, "n/a");
                    assert_eq!(diagnostic.checklist_id, "n/a");
                    assert_ne!(diagnostic.subject_id, "n/a");
                }
                ValidatorClass::PaperChecklist => {
                    assert_eq!(diagnostic.contract_id, "n/a");
                    assert_ne!(diagnostic.checklist_id, "n/a");
                }
            }
        }
    }

    // ── Acceptance criterion: byte-stable outputs ────────────────────────

    #[test]
    fn report_is_deterministic() {
        let fixtures = red_fixture_set();
        let first = run_governance_validation(&fixtures, &config());
        let second = run_governance_validation(&fixtures, &config());
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(first.evidence_checksum, second.evidence_checksum);
        assert_eq!(first.diagnostics, second.diagnostics);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let report = run_governance_validation(&red_fixture_set(), &config());
        let json = serde_json::to_string(&report).expect("serialize");
        let restored: GovernanceValidationReport =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.diagnostics, report.diagnostics);
        assert_eq!(restored.summary, report.summary);
        assert_eq!(restored.evidence_checksum, report.evidence_checksum);
    }

    #[test]
    fn json_stats_checksum_is_self_consistent() {
        let report = run_governance_validation(&red_fixture_set(), &config());
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert_eq!(report.evidence_checksum, stable_hash(&report.diagnostics));
    }

    #[test]
    fn replay_command_references_report_id() {
        let report = run_governance_validation(&green_fixture_set(), &config());
        assert!(report.replay_command.contains(&report.report_id));
        assert!(report.replay_command.contains("governance-validate"));
    }

    #[test]
    fn distinct_scenario_label_changes_report_id() {
        let green = run_governance_validation(&green_fixture_set(), &config());
        let red = run_governance_validation(&red_fixture_set(), &config());
        assert_ne!(green.report_id, red.report_id);
        assert_ne!(green.evidence_checksum, red.evidence_checksum);
    }

    // ── Property tests ───────────────────────────────────────────────────

    /// Build a fixture set deterministically from a 4-bit selection mask. Bits
    /// 0..=1 pick contracts, bit 2 picks the scan ledger, bit 3 picks the paper
    /// registry. The label is content-stable for a given mask.
    fn fixtures_from_mask(label: &str, mask: u8) -> GovernanceFixtureSet {
        let mut contracts = vec![contract_clean()];
        if mask & 0b0001 != 0 {
            contracts.push(contract_legal_blocked());
        }
        if mask & 0b0010 != 0 {
            contracts.push(contract_missing_artifacts());
        }
        let scan_records = if mask & 0b0100 != 0 {
            scan_ledger_missing_candidate()
        } else {
            scan_ledger_clean()
        };
        let paper_registry = if mask & 0b1000 != 0 {
            paper_registry_broken()
        } else {
            paper_registry_clean()
        };
        GovernanceFixtureSet {
            label: format!("{label}-{mask}"),
            contracts,
            scan_records,
            paper_registry,
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Equivalent inputs produce byte-identical reports — including diagnostic
        /// ordering, the report id, and every hash. This proves idempotence,
        /// stable ordering, and stable hash generation in one shot.
        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}", mask in 0u8..16) {
            let fixtures = fixtures_from_mask(&label, mask);
            let first = run_governance_validation(&fixtures, &config());
            let second = run_governance_validation(&fixtures, &config());
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(
                &first.exported_json_stats.sha256,
                &second.exported_json_stats.sha256
            );
        }

        /// Normalizing the same validator report twice is idempotent.
        #[test]
        fn prop_normalization_is_idempotent(mask in 0u8..16) {
            let fixtures = fixtures_from_mask("norm", mask);
            let report = RecommendationContractLinter::new(ContractLintConfig::default())
                .evaluate(fixtures.contracts.clone());
            prop_assert_eq!(
                normalize_contract_report(&report),
                normalize_contract_report(&report)
            );
            let paper = normalize_paper_registry(&fixtures.paper_registry);
            prop_assert_eq!(paper.clone(), normalize_paper_registry(&fixtures.paper_registry));
        }

        /// Every emitted diagnostic always carries the full required field set
        /// and a `doctor_frankentui` replay command — regardless of which red
        /// fixtures are present.
        #[test]
        fn prop_every_diagnostic_has_required_fields(mask in 1u8..16) {
            let report = run_governance_validation(&fixtures_from_mask("fields", mask), &config());
            for diagnostic in &report.diagnostics {
                prop_assert!(diagnostic.has_required_fields());
                prop_assert!(diagnostic.replay_cmd.contains("doctor_frankentui"));
            }
        }

        /// No cross-validator state leakage: changing the contract list never
        /// changes the scan or paper diagnostics. The validators are independent.
        #[test]
        fn prop_validators_do_not_leak_state(extra in 0u8..4) {
            let mut a = green_fixture_set();
            a.label = "leak-a".to_string();
            let mut b = green_fixture_set();
            b.label = "leak-b".to_string();
            for _ in 0..extra {
                b.contracts.push(contract_missing_artifacts());
            }
            let report_a = run_governance_validation(&a, &config());
            let report_b = run_governance_validation(&b, &config());
            let scan_a: Vec<_> = report_a.diagnostics_for(ValidatorClass::EcosystemScan);
            let scan_b: Vec<_> = report_b.diagnostics_for(ValidatorClass::EcosystemScan);
            let paper_a: Vec<_> = report_a.diagnostics_for(ValidatorClass::PaperChecklist);
            let paper_b: Vec<_> = report_b.diagnostics_for(ValidatorClass::PaperChecklist);
            prop_assert_eq!(scan_a, scan_b);
            prop_assert_eq!(paper_a, paper_b);
        }

        /// Linting the same green contract any number of times yields zero
        /// findings — there is no hidden accumulating state.
        #[test]
        fn prop_clean_contract_lint_is_stateless(repeats in 1u8..6) {
            let linter = RecommendationContractLinter::new(ContractLintConfig::default());
            for _ in 0..repeats {
                let findings = linter.lint(&contract_clean());
                prop_assert!(findings.is_empty());
            }
        }
    }
}

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const BUILTIN_CONTRACT_JSON: &str =
    include_str!("../contracts/opentui_semantic_equivalence_v1.json");
const SUPPORTED_SCHEMA_VERSION: &str = "sem-eq-contract-v1";
const BUILTIN_TRANSFORMATION_POLICY_JSON: &str =
    include_str!("../contracts/opentui_transformation_policy_v1.json");
const SUPPORTED_TRANSFORMATION_POLICY_SCHEMA_VERSION: &str = "transform-policy-v1";
const BUILTIN_EVIDENCE_MANIFEST_JSON: &str =
    include_str!("../contracts/opentui_evidence_manifest_v1.json");
const SUPPORTED_EVIDENCE_MANIFEST_SCHEMA_VERSION: &str = "evidence-manifest-v1";
const BUILTIN_CONFIDENCE_MODEL_JSON: &str =
    include_str!("../contracts/opentui_confidence_model_v1.json");
const SUPPORTED_CONFIDENCE_MODEL_SCHEMA_VERSION: &str = "confidence-model-v1";
const BUILTIN_LICENSING_PROVENANCE_JSON: &str =
    include_str!("../contracts/opentui_licensing_provenance_v1.json");
const SUPPORTED_LICENSING_PROVENANCE_SCHEMA_VERSION: &str = "licensing-provenance-v1";
/// The licensing contract's risk flag for "one or more pipeline stages lack
/// provenance records"; raised by `assess_ip_artifacts` for an empty or
/// invalid chain.
const PROVENANCE_GAP_FLAG: &str = "lp-provenance-gap";
const REQUIRED_POLICY_CATEGORIES: [&str; 6] = [
    "state",
    "layout",
    "style",
    "effects",
    "accessibility",
    "terminal_capability",
];

#[derive(Debug, Error)]
pub enum SemanticContractError {
    #[error("failed to parse semantic contract JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("semantic contract validation failed: {0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, SemanticContractError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SemanticEquivalenceContract {
    pub contract_id: String,
    pub schema_version: String,
    pub contract_version: String,
    pub equivalence_axes: EquivalenceAxes,
    pub visual_tolerance_policy: VisualTolerancePolicy,
    pub improvement_envelope: ImprovementEnvelope,
    pub deterministic_tie_breakers: Vec<TieBreakerRule>,
    pub clauses: Vec<ContractClause>,
    pub validator_clause_map: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceAxes {
    pub state_transition: String,
    pub event_ordering: String,
    pub side_effect_observability: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VisualTolerancePolicy {
    pub strict_classes: Vec<String>,
    pub strict_policy: String,
    pub perceptual_classes: Vec<String>,
    pub perceptual_policy: String,
    pub max_perceptual_delta: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImprovementEnvelope {
    pub allowed_dimensions: Vec<String>,
    pub forbidden_rewrites: Vec<String>,
    pub required_safeguards: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TieBreakerRule {
    pub priority: u16,
    pub rule_id: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContractClause {
    pub clause_id: String,
    pub title: String,
    pub category: String,
    pub requirement: String,
    pub severity: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransformationHandlingClass {
    Exact,
    Approximate,
    ExtendFtui,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TransformationRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyCategoryDefinition {
    pub category_id: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyConstructCatalogEntry {
    pub construct_signature: String,
    pub category_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransformationPolicyCell {
    pub construct_signature: String,
    pub handling_class: TransformationHandlingClass,
    pub rationale: String,
    pub risk_level: TransformationRiskLevel,
    pub fallback_behavior: String,
    pub user_messaging: String,
    pub planner_strategy: String,
    pub semantic_clause_links: Vec<String>,
    pub certification_evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlannerConsumptionProjection {
    pub required_fields: Vec<String>,
    pub deterministic_sort_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CertificationConsumptionProjection {
    pub required_fields: Vec<String>,
    pub required_risk_levels: Vec<TransformationRiskLevel>,
    pub requires_clause_traceability: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransformationPolicyMatrix {
    pub policy_id: String,
    pub schema_version: String,
    pub policy_version: String,
    pub categories: Vec<PolicyCategoryDefinition>,
    pub construct_catalog: Vec<PolicyConstructCatalogEntry>,
    pub policy_cells: Vec<TransformationPolicyCell>,
    pub planner_projection: PlannerConsumptionProjection,
    pub certification_projection: CertificationConsumptionProjection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannerPolicyRow {
    pub construct_signature: String,
    pub category_id: String,
    pub handling_class: TransformationHandlingClass,
    pub planner_strategy: String,
    pub fallback_behavior: String,
    pub risk_level: TransformationRiskLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CertificationPolicyRow {
    pub construct_signature: String,
    pub handling_class: TransformationHandlingClass,
    pub risk_level: TransformationRiskLevel,
    pub semantic_clause_links: Vec<String>,
    pub certification_evidence: Vec<String>,
    pub user_messaging: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledClauseValidator {
    pub validator_id: String,
    pub clause_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractValidatorExecution {
    pub validator_id: String,
    pub passed: bool,
    pub missing_claim_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledContractGateReport {
    pub passed: bool,
    pub validator_results: Vec<ContractValidatorExecution>,
    pub orphan_claim_ids: Vec<String>,
}

impl CompiledClauseValidator {
    #[must_use]
    pub fn execute(&self, observed_claim_ids: &BTreeSet<String>) -> ContractValidatorExecution {
        let missing_claim_ids = self
            .clause_ids
            .iter()
            .filter(|claim_id| !observed_claim_ids.contains(*claim_id))
            .cloned()
            .collect::<Vec<_>>();
        ContractValidatorExecution {
            validator_id: self.validator_id.clone(),
            passed: missing_claim_ids.is_empty(),
            missing_claim_ids,
        }
    }
}

impl SemanticEquivalenceContract {
    pub fn parse_and_validate(raw_json: &str) -> Result<Self> {
        let parsed: Self = serde_json::from_str(raw_json)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(SemanticContractError::Validation(format!(
                "unsupported schema_version '{}' (expected '{}')",
                self.schema_version, SUPPORTED_SCHEMA_VERSION
            )));
        }
        if self.contract_id.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "contract_id must not be empty".to_string(),
            ));
        }
        if self.clauses.is_empty() {
            return Err(SemanticContractError::Validation(
                "clauses must not be empty".to_string(),
            ));
        }
        if self.deterministic_tie_breakers.is_empty() {
            return Err(SemanticContractError::Validation(
                "deterministic_tie_breakers must not be empty".to_string(),
            ));
        }
        if self.validator_clause_map.is_empty() {
            return Err(SemanticContractError::Validation(
                "validator_clause_map must not be empty".to_string(),
            ));
        }

        let mut clause_ids = BTreeSet::new();
        for clause in &self.clauses {
            if clause.clause_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "clause_id must not be empty".to_string(),
                ));
            }
            if !clause_ids.insert(clause.clause_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate clause_id '{}'",
                    clause.clause_id
                )));
            }
        }

        let mut priorities = BTreeSet::new();
        for rule in &self.deterministic_tie_breakers {
            if rule.rule_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "tie-break rule_id must not be empty".to_string(),
                ));
            }
            if !priorities.insert(rule.priority) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate tie-break priority '{}'",
                    rule.priority
                )));
            }
        }

        for (validator_id, clauses) in &self.validator_clause_map {
            if validator_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "validator id must not be empty".to_string(),
                ));
            }
            if clauses.is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "validator '{}' has no clause mappings",
                    validator_id
                )));
            }
            for clause_id in clauses {
                if !clause_ids.contains(clause_id) {
                    return Err(SemanticContractError::Validation(format!(
                        "validator '{}' references unknown clause '{}'",
                        validator_id, clause_id
                    )));
                }
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn clause(&self, clause_id: &str) -> Option<&ContractClause> {
        self.clauses
            .iter()
            .find(|clause| clause.clause_id == clause_id)
    }

    #[must_use]
    pub fn clauses_for_validator(&self, validator_id: &str) -> Vec<&ContractClause> {
        let Some(ids) = self.validator_clause_map.get(validator_id) else {
            return Vec::new();
        };
        ids.iter().filter_map(|id| self.clause(id)).collect()
    }

    #[must_use]
    pub fn compile_validators(&self) -> Vec<CompiledClauseValidator> {
        self.validator_clause_map
            .iter()
            .map(|(validator_id, clause_ids)| CompiledClauseValidator {
                validator_id: validator_id.clone(),
                clause_ids: clause_ids.clone(),
            })
            .collect()
    }

    #[must_use]
    pub fn execute_compiled_validators(
        &self,
        observed_claim_ids: &BTreeSet<String>,
    ) -> CompiledContractGateReport {
        let known_claim_ids = self
            .clauses
            .iter()
            .map(|clause| clause.clause_id.clone())
            .collect::<BTreeSet<_>>();
        let orphan_claim_ids = observed_claim_ids
            .iter()
            .filter(|claim_id| !known_claim_ids.contains(*claim_id))
            .cloned()
            .collect::<Vec<_>>();
        let validator_results = self
            .compile_validators()
            .into_iter()
            .map(|validator| validator.execute(observed_claim_ids))
            .collect::<Vec<_>>();
        let passed =
            orphan_claim_ids.is_empty() && validator_results.iter().all(|result| result.passed);

        CompiledContractGateReport {
            passed,
            validator_results,
            orphan_claim_ids,
        }
    }
}

impl TransformationPolicyMatrix {
    pub fn parse_and_validate(raw_json: &str) -> Result<Self> {
        let parsed: Self = serde_json::from_str(raw_json)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SUPPORTED_TRANSFORMATION_POLICY_SCHEMA_VERSION {
            return Err(SemanticContractError::Validation(format!(
                "unsupported transformation policy schema_version '{}' (expected '{}')",
                self.schema_version, SUPPORTED_TRANSFORMATION_POLICY_SCHEMA_VERSION
            )));
        }
        if self.policy_id.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "policy_id must not be empty".to_string(),
            ));
        }
        if self.categories.is_empty() {
            return Err(SemanticContractError::Validation(
                "categories must not be empty".to_string(),
            ));
        }
        if self.construct_catalog.is_empty() {
            return Err(SemanticContractError::Validation(
                "construct_catalog must not be empty".to_string(),
            ));
        }
        if self.policy_cells.is_empty() {
            return Err(SemanticContractError::Validation(
                "policy_cells must not be empty".to_string(),
            ));
        }
        if self.planner_projection.required_fields.is_empty() {
            return Err(SemanticContractError::Validation(
                "planner_projection.required_fields must not be empty".to_string(),
            ));
        }
        if self
            .planner_projection
            .deterministic_sort_key
            .trim()
            .is_empty()
        {
            return Err(SemanticContractError::Validation(
                "planner_projection.deterministic_sort_key must not be empty".to_string(),
            ));
        }
        if self.certification_projection.required_fields.is_empty() {
            return Err(SemanticContractError::Validation(
                "certification_projection.required_fields must not be empty".to_string(),
            ));
        }
        if self
            .certification_projection
            .required_risk_levels
            .is_empty()
        {
            return Err(SemanticContractError::Validation(
                "certification_projection.required_risk_levels must not be empty".to_string(),
            ));
        }

        let mut category_ids = BTreeSet::new();
        for category in &self.categories {
            if category.category_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "category_id must not be empty".to_string(),
                ));
            }
            if category.description.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "category '{}' has empty description",
                    category.category_id
                )));
            }
            if !category_ids.insert(category.category_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate category '{}'",
                    category.category_id
                )));
            }
        }
        for required in REQUIRED_POLICY_CATEGORIES {
            if !category_ids.contains(required) {
                return Err(SemanticContractError::Validation(format!(
                    "required policy category '{}' is missing",
                    required
                )));
            }
        }

        let mut catalog_signature_to_category = BTreeMap::new();
        for entry in &self.construct_catalog {
            if entry.construct_signature.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "construct_signature must not be empty".to_string(),
                ));
            }
            if entry.summary.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "construct '{}' has empty summary",
                    entry.construct_signature
                )));
            }
            if !category_ids.contains(entry.category_id.as_str()) {
                return Err(SemanticContractError::Validation(format!(
                    "construct '{}' references unknown category '{}'",
                    entry.construct_signature, entry.category_id
                )));
            }
            if catalog_signature_to_category
                .insert(entry.construct_signature.clone(), entry.category_id.clone())
                .is_some()
            {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate construct_signature '{}' in construct_catalog",
                    entry.construct_signature
                )));
            }
        }

        let semantic_contract = load_builtin_semantic_contract()?;
        let semantic_clause_ids = semantic_contract
            .clauses
            .iter()
            .map(|clause| clause.clause_id.as_str())
            .collect::<BTreeSet<_>>();

        let mut seen_policy_signatures = BTreeSet::new();
        let mut previous_signature: Option<&str> = None;
        for cell in &self.policy_cells {
            if cell.construct_signature.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "policy cell construct_signature must not be empty".to_string(),
                ));
            }
            if !catalog_signature_to_category.contains_key(cell.construct_signature.as_str()) {
                return Err(SemanticContractError::Validation(format!(
                    "policy cell '{}' not present in construct_catalog",
                    cell.construct_signature
                )));
            }
            if !seen_policy_signatures.insert(cell.construct_signature.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate policy cell for construct '{}'",
                    cell.construct_signature
                )));
            }
            if let Some(previous) = previous_signature
                && cell.construct_signature.as_str() < previous
            {
                return Err(SemanticContractError::Validation(
                    "policy_cells must be sorted by construct_signature for deterministic consumption".to_string(),
                ));
            }
            previous_signature = Some(cell.construct_signature.as_str());
            if cell.rationale.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "construct '{}' has empty rationale",
                    cell.construct_signature
                )));
            }
            if cell.fallback_behavior.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "construct '{}' has empty fallback_behavior",
                    cell.construct_signature
                )));
            }
            if cell.user_messaging.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "construct '{}' has empty user_messaging",
                    cell.construct_signature
                )));
            }
            if cell.planner_strategy.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "construct '{}' has empty planner_strategy",
                    cell.construct_signature
                )));
            }
            if cell.semantic_clause_links.is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "construct '{}' must map to at least one semantic clause",
                    cell.construct_signature
                )));
            }
            for clause_id in &cell.semantic_clause_links {
                if !semantic_clause_ids.contains(clause_id.as_str()) {
                    return Err(SemanticContractError::Validation(format!(
                        "construct '{}' references unknown semantic clause '{}'",
                        cell.construct_signature, clause_id
                    )));
                }
            }
            if cell.certification_evidence.is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "construct '{}' must define certification_evidence",
                    cell.construct_signature
                )));
            }
            for evidence in &cell.certification_evidence {
                if evidence.trim().is_empty() {
                    return Err(SemanticContractError::Validation(format!(
                        "construct '{}' contains empty certification evidence entry",
                        cell.construct_signature
                    )));
                }
            }
        }

        let catalog_signatures = catalog_signature_to_category
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        if seen_policy_signatures != catalog_signatures {
            let missing = catalog_signatures
                .difference(&seen_policy_signatures)
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "construct(s) without explicit handling class: {}",
                    missing.join(", ")
                )));
            }
        }

        let risk_levels = self
            .certification_projection
            .required_risk_levels
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let required_levels = [
            TransformationRiskLevel::Low,
            TransformationRiskLevel::Medium,
            TransformationRiskLevel::High,
            TransformationRiskLevel::Critical,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        if risk_levels != required_levels {
            return Err(SemanticContractError::Validation(
                "certification_projection.required_risk_levels must include low, medium, high, critical".to_string(),
            ));
        }

        Ok(())
    }

    #[must_use]
    pub fn policy_for_construct(
        &self,
        construct_signature: &str,
    ) -> Option<&TransformationPolicyCell> {
        self.policy_cells
            .iter()
            .find(|cell| cell.construct_signature == construct_signature)
    }

    #[must_use]
    pub fn planner_rows(&self) -> Vec<PlannerPolicyRow> {
        let category_by_signature = self
            .construct_catalog
            .iter()
            .map(|entry| (&entry.construct_signature, &entry.category_id))
            .collect::<BTreeMap<_, _>>();
        let mut rows = self
            .policy_cells
            .iter()
            .map(|cell| PlannerPolicyRow {
                construct_signature: cell.construct_signature.clone(),
                category_id: category_by_signature
                    .get(&cell.construct_signature)
                    .map_or_else(String::new, |category| (*category).clone()),
                handling_class: cell.handling_class,
                planner_strategy: cell.planner_strategy.clone(),
                fallback_behavior: cell.fallback_behavior.clone(),
                risk_level: cell.risk_level,
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| a.construct_signature.cmp(&b.construct_signature));
        rows
    }

    #[must_use]
    pub fn certification_rows(&self) -> Vec<CertificationPolicyRow> {
        let mut rows = self
            .policy_cells
            .iter()
            .map(|cell| CertificationPolicyRow {
                construct_signature: cell.construct_signature.clone(),
                handling_class: cell.handling_class,
                risk_level: cell.risk_level,
                semantic_clause_links: cell.semantic_clause_links.clone(),
                certification_evidence: cell.certification_evidence.clone(),
                user_messaging: cell.user_messaging.clone(),
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| a.construct_signature.cmp(&b.construct_signature));
        rows
    }
}

// ---------------------------------------------------------------------------
// Evidence Manifest — deterministic artifact manifest for migration runs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceManifest {
    pub manifest_id: String,
    pub schema_version: String,
    pub manifest_version: String,
    pub run_id: String,
    pub source_fingerprint: SourceFingerprint,
    pub stages: Vec<StageRecord>,
    pub generated_code_fingerprint: GeneratedCodeFingerprint,
    pub certification_verdict: CertificationVerdict,
    pub determinism_attestation: DeterminismAttestation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceFingerprint {
    pub repo_url: Option<String>,
    pub repo_commit: Option<String>,
    pub local_path: Option<String>,
    pub source_hash: String,
    pub lockfiles: Vec<LockfileEntry>,
    pub parser_versions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LockfileEntry {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StageRecord {
    pub stage_id: String,
    pub stage_index: u32,
    pub correlation_id: String,
    pub claim_id: String,
    pub evidence_id: String,
    pub policy_id: String,
    pub trace_id: String,
    pub started_at: String,
    pub finished_at: String,
    pub status: StageStatus,
    pub input_hash: String,
    pub output_hash: String,
    pub artifact_paths: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Ok,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GeneratedCodeFingerprint {
    pub code_hash: String,
    pub formatter_version: String,
    pub linter_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CertificationVerdict {
    pub verdict: VerdictOutcome,
    pub confidence: f64,
    pub test_pass_count: u32,
    pub test_fail_count: u32,
    pub test_skip_count: u32,
    pub semantic_clause_coverage: SemanticClauseCoverage,
    pub benchmark_summary: BenchmarkSummary,
    pub risk_flags: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictOutcome {
    Accept,
    Hold,
    Reject,
    Rollback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticClauseCoverage {
    pub covered: Vec<String>,
    pub uncovered: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSummary {
    pub latency_p50_ms: f64,
    pub latency_p99_ms: f64,
    pub throughput_ops_per_sec: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeterminismAttestation {
    pub identical_runs_count: u32,
    pub manifest_hash_stable: bool,
    pub divergence_detected: bool,
}

// ---------------------------------------------------------------------------
// JSONL evidence record — one structured line per stage event
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StageEvidenceRecord {
    pub event: String,
    pub run_id: String,
    pub correlation_id: String,
    pub stage_id: String,
    pub stage_index: u32,
    pub claim_id: String,
    pub evidence_id: String,
    pub policy_id: String,
    pub trace_id: String,
    pub timestamp: String,
    pub status: StageStatus,
    pub input_hash: String,
    pub output_hash: String,
    pub artifact_count: u32,
    pub error: Option<String>,
}

impl EvidenceManifest {
    pub fn parse_and_validate(raw_json: &str) -> Result<Self> {
        let parsed: Self = serde_json::from_str(raw_json)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SUPPORTED_EVIDENCE_MANIFEST_SCHEMA_VERSION {
            return Err(SemanticContractError::Validation(format!(
                "unsupported evidence manifest schema_version '{}' (expected '{}')",
                self.schema_version, SUPPORTED_EVIDENCE_MANIFEST_SCHEMA_VERSION
            )));
        }
        if self.manifest_id.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "manifest_id must not be empty".to_string(),
            ));
        }
        if self.run_id.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "run_id must not be empty".to_string(),
            ));
        }

        // Source fingerprint validation.
        self.validate_source_fingerprint()?;

        // Stage validation.
        self.validate_stages()?;

        // Generated code fingerprint validation.
        self.validate_generated_code_fingerprint()?;

        // Certification verdict validation.
        self.validate_certification_verdict()?;

        // Determinism attestation validation.
        self.validate_determinism_attestation()?;

        Ok(())
    }

    fn validate_source_fingerprint(&self) -> Result<()> {
        let fp = &self.source_fingerprint;
        if fp.source_hash.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "source_fingerprint.source_hash must not be empty".to_string(),
            ));
        }
        if fp.repo_url.is_none() && fp.local_path.is_none() {
            return Err(SemanticContractError::Validation(
                "source_fingerprint must specify repo_url or local_path".to_string(),
            ));
        }
        if fp.parser_versions.is_empty() {
            return Err(SemanticContractError::Validation(
                "source_fingerprint.parser_versions must not be empty".to_string(),
            ));
        }
        for lockfile in &fp.lockfiles {
            if lockfile.path.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "lockfile path must not be empty".to_string(),
                ));
            }
            if lockfile.sha256.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "lockfile '{}' sha256 must not be empty",
                    lockfile.path
                )));
            }
        }
        Ok(())
    }

    fn validate_stages(&self) -> Result<()> {
        if self.stages.is_empty() {
            return Err(SemanticContractError::Validation(
                "stages must not be empty".to_string(),
            ));
        }

        let mut seen_ids = BTreeSet::new();
        let mut seen_evidence_ids = BTreeSet::new();
        let covered_claim_ids = self
            .certification_verdict
            .semantic_clause_coverage
            .covered
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut declared_claim_ids = covered_claim_ids.clone();
        declared_claim_ids.extend(
            self.certification_verdict
                .semantic_clause_coverage
                .uncovered
                .iter()
                .cloned(),
        );
        let mut linked_claim_ids = BTreeSet::new();
        let mut prev_index: Option<u32> = None;
        let mut expected_policy_id: Option<String> = None;
        let mut expected_trace_id: Option<String> = None;
        for stage in &self.stages {
            if stage.stage_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "stage_id must not be empty".to_string(),
                ));
            }
            if !seen_ids.insert(stage.stage_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate stage_id '{}'",
                    stage.stage_id
                )));
            }
            if let Some(prev) = prev_index {
                if stage.stage_index != prev + 1 {
                    return Err(SemanticContractError::Validation(format!(
                        "stage_index for '{}' is {} but expected {} (must be consecutive)",
                        stage.stage_id,
                        stage.stage_index,
                        prev + 1
                    )));
                }
            } else if stage.stage_index != 0 {
                return Err(SemanticContractError::Validation(format!(
                    "first stage '{}' must have stage_index 0",
                    stage.stage_id
                )));
            }
            prev_index = Some(stage.stage_index);
            if stage.correlation_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' correlation_id must not be empty",
                    stage.stage_id
                )));
            }
            if stage.claim_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' claim_id must not be empty",
                    stage.stage_id
                )));
            }
            if !covered_claim_ids.contains(&stage.claim_id) {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' claim_id '{}' must be present in semantic_clause_coverage.covered",
                    stage.stage_id, stage.claim_id
                )));
            }
            linked_claim_ids.insert(stage.claim_id.clone());
            if stage.evidence_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' evidence_id must not be empty",
                    stage.stage_id
                )));
            }
            if !seen_evidence_ids.insert(stage.evidence_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate evidence_id '{}'",
                    stage.evidence_id
                )));
            }
            if stage.policy_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' policy_id must not be empty",
                    stage.stage_id
                )));
            }
            if stage.trace_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' trace_id must not be empty",
                    stage.stage_id
                )));
            }
            if let Some(policy_id) = &expected_policy_id {
                if stage.policy_id != *policy_id {
                    return Err(SemanticContractError::Validation(format!(
                        "stage '{}' policy_id '{}' does not match run policy_id '{}'",
                        stage.stage_id, stage.policy_id, policy_id
                    )));
                }
            } else {
                expected_policy_id = Some(stage.policy_id.clone());
            }
            if let Some(trace_id) = &expected_trace_id {
                if stage.trace_id != *trace_id {
                    return Err(SemanticContractError::Validation(format!(
                        "stage '{}' trace_id '{}' does not match run trace_id '{}'",
                        stage.stage_id, stage.trace_id, trace_id
                    )));
                }
            } else {
                expected_trace_id = Some(stage.trace_id.clone());
            }
            if stage.started_at.trim().is_empty() || stage.finished_at.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' must have non-empty started_at and finished_at",
                    stage.stage_id
                )));
            }
            if stage.input_hash.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' input_hash must not be empty",
                    stage.stage_id
                )));
            }
            if stage.status != StageStatus::Failed && stage.output_hash.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' output_hash must not be empty for non-failed stages",
                    stage.stage_id
                )));
            }
            if stage.status == StageStatus::Failed && stage.error.is_none() {
                return Err(SemanticContractError::Validation(format!(
                    "stage '{}' has status 'failed' but no error message",
                    stage.stage_id
                )));
            }
        }

        // Verify hash chain: each stage's input_hash must equal the previous
        // stage's output_hash (except the first stage).
        for window in self.stages.windows(2) {
            let prev_stage = &window[0];
            let curr_stage = &window[1];
            if prev_stage.status == StageStatus::Ok
                && curr_stage.input_hash != prev_stage.output_hash
            {
                return Err(SemanticContractError::Validation(format!(
                    "hash chain broken: stage '{}' output_hash ({}) != stage '{}' input_hash ({})",
                    prev_stage.stage_id,
                    prev_stage.output_hash,
                    curr_stage.stage_id,
                    curr_stage.input_hash
                )));
            }
        }

        let unlinked_covered_claims = covered_claim_ids
            .difference(&linked_claim_ids)
            .cloned()
            .collect::<Vec<_>>();
        if !unlinked_covered_claims.is_empty() {
            return Err(SemanticContractError::Validation(format!(
                "covered claims missing stage linkage: {}",
                unlinked_covered_claims.join(", ")
            )));
        }

        Ok(())
    }

    fn validate_generated_code_fingerprint(&self) -> Result<()> {
        let gcf = &self.generated_code_fingerprint;
        if gcf.code_hash.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "generated_code_fingerprint.code_hash must not be empty".to_string(),
            ));
        }
        if gcf.formatter_version.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "generated_code_fingerprint.formatter_version must not be empty".to_string(),
            ));
        }
        if gcf.linter_version.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "generated_code_fingerprint.linter_version must not be empty".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_certification_verdict(&self) -> Result<()> {
        let cv = &self.certification_verdict;
        if !(0.0..=1.0).contains(&cv.confidence) {
            return Err(SemanticContractError::Validation(format!(
                "certification_verdict.confidence must be in [0.0, 1.0], got {}",
                cv.confidence
            )));
        }
        if cv.test_fail_count > 0 && cv.verdict == VerdictOutcome::Accept {
            return Err(SemanticContractError::Validation(
                "certification_verdict cannot be 'accept' with failing tests".to_string(),
            ));
        }

        let mut seen_covered = BTreeSet::new();
        let mut seen_uncovered = BTreeSet::new();
        for claim_id in &cv.semantic_clause_coverage.covered {
            if claim_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "semantic_clause_coverage.covered must not contain empty claim IDs".to_string(),
                ));
            }
            if !seen_covered.insert(claim_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "semantic_clause_coverage.covered contains duplicate claim_id '{}'",
                    claim_id
                )));
            }
        }
        for claim_id in &cv.semantic_clause_coverage.uncovered {
            if claim_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "semantic_clause_coverage.uncovered must not contain empty claim IDs"
                        .to_string(),
                ));
            }
            if seen_covered.contains(claim_id) {
                return Err(SemanticContractError::Validation(format!(
                    "semantic clause '{}' cannot be both covered and uncovered",
                    claim_id
                )));
            }
            if !seen_uncovered.insert(claim_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "semantic_clause_coverage.uncovered contains duplicate claim_id '{}'",
                    claim_id
                )));
            }
        }
        if seen_covered.is_empty() && seen_uncovered.is_empty() {
            return Err(SemanticContractError::Validation(
                "semantic_clause_coverage must include at least one claim ID".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_determinism_attestation(&self) -> Result<()> {
        let da = &self.determinism_attestation;
        if da.identical_runs_count == 0 {
            return Err(SemanticContractError::Validation(
                "determinism_attestation.identical_runs_count must be > 0".to_string(),
            ));
        }
        if da.divergence_detected && da.manifest_hash_stable {
            return Err(SemanticContractError::Validation(
                "determinism_attestation: divergence_detected=true is inconsistent with manifest_hash_stable=true".to_string(),
            ));
        }
        Ok(())
    }

    /// Reconstruct the stage lineage (ordered stage_id chain with hash links)
    /// from the manifest alone, suitable for deterministic replay.
    #[must_use]
    pub fn stage_lineage(&self) -> Vec<StageLineageEntry> {
        self.stages
            .iter()
            .map(|stage| StageLineageEntry {
                stage_id: stage.stage_id.clone(),
                stage_index: stage.stage_index,
                correlation_id: stage.correlation_id.clone(),
                claim_id: stage.claim_id.clone(),
                evidence_id: stage.evidence_id.clone(),
                policy_id: stage.policy_id.clone(),
                trace_id: stage.trace_id.clone(),
                input_hash: stage.input_hash.clone(),
                output_hash: stage.output_hash.clone(),
                status: stage.status,
            })
            .collect()
    }

    /// Convert all stage records to JSONL evidence records for structured logging.
    #[must_use]
    pub fn to_evidence_records(&self) -> Vec<StageEvidenceRecord> {
        self.stages
            .iter()
            .map(|stage| StageEvidenceRecord {
                event: "stage_completed".to_string(),
                run_id: self.run_id.clone(),
                correlation_id: stage.correlation_id.clone(),
                stage_id: stage.stage_id.clone(),
                stage_index: stage.stage_index,
                claim_id: stage.claim_id.clone(),
                evidence_id: stage.evidence_id.clone(),
                policy_id: stage.policy_id.clone(),
                trace_id: stage.trace_id.clone(),
                timestamp: stage.finished_at.clone(),
                status: stage.status,
                input_hash: stage.input_hash.clone(),
                output_hash: stage.output_hash.clone(),
                artifact_count: stage.artifact_paths.len() as u32,
                error: stage.error.clone(),
            })
            .collect()
    }

    /// Serialize evidence records as JSONL (one JSON object per line).
    #[must_use]
    pub fn evidence_jsonl(&self) -> String {
        self.to_evidence_records()
            .iter()
            .filter_map(|record| serde_json::to_string(record).ok())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Look up a stage by its correlation ID.
    #[must_use]
    pub fn stage_by_correlation_id(&self, correlation_id: &str) -> Option<&StageRecord> {
        self.stages
            .iter()
            .find(|stage| stage.correlation_id == correlation_id)
    }

    #[must_use]
    pub fn observed_claim_ids(&self) -> BTreeSet<String> {
        let mut observed = self
            .certification_verdict
            .semantic_clause_coverage
            .covered
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        observed.extend(
            self.certification_verdict
                .semantic_clause_coverage
                .uncovered
                .iter()
                .cloned(),
        );
        observed.extend(self.stages.iter().map(|stage| stage.claim_id.clone()));
        observed
    }

    pub fn execute_runtime_contract_gate(
        &self,
        contract: &SemanticEquivalenceContract,
    ) -> Result<CompiledContractGateReport> {
        let observed_claim_ids = self.observed_claim_ids();
        let report = contract.execute_compiled_validators(&observed_claim_ids);
        if !report.orphan_claim_ids.is_empty() {
            return Err(SemanticContractError::Validation(format!(
                "contract gate orphan claims: {}",
                report.orphan_claim_ids.join(", ")
            )));
        }
        let failed_validators = report
            .validator_results
            .iter()
            .filter(|result| !result.passed)
            .map(|result| {
                format!(
                    "{}({})",
                    result.validator_id,
                    result.missing_claim_ids.join(",")
                )
            })
            .collect::<Vec<_>>();
        if !failed_validators.is_empty() {
            return Err(SemanticContractError::Validation(format!(
                "contract gate validator failures: {}",
                failed_validators.join("; ")
            )));
        }
        Ok(report)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageLineageEntry {
    pub stage_id: String,
    pub stage_index: u32,
    pub correlation_id: String,
    pub claim_id: String,
    pub evidence_id: String,
    pub policy_id: String,
    pub trace_id: String,
    pub input_hash: String,
    pub output_hash: String,
    pub status: StageStatus,
}

// ---------------------------------------------------------------------------
// Confidence Model — Bayesian decision model for migration verdicts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfidenceModel {
    pub model_id: String,
    pub schema_version: String,
    pub model_version: String,
    pub decision_space: DecisionSpace,
    pub loss_matrix: LossMatrix,
    pub prior_config: PriorConfig,
    pub likelihood_sources: Vec<LikelihoodSource>,
    pub decision_boundaries: DecisionBoundaries,
    pub calibration: CalibrationConfig,
    pub fallback_triggers: Vec<FallbackTrigger>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionSpace {
    pub actions: Vec<String>,
    pub states: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LossMatrix {
    pub accept_correct: f64,
    pub accept_incorrect: f64,
    pub hold_correct: f64,
    pub hold_incorrect: f64,
    pub reject_correct: f64,
    pub reject_incorrect: f64,
    pub rollback_cost: f64,
    pub conservative_fallback_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PriorConfig {
    pub semantic_pass_rate_alpha: f64,
    pub semantic_pass_rate_beta: f64,
    pub performance_mean_prior: f64,
    pub performance_variance_prior: f64,
    pub unsupported_feature_penalty_per_item: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LikelihoodSource {
    pub source_id: String,
    pub weight: f64,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DecisionBoundaries {
    pub auto_approve_threshold: f64,
    pub human_review_lower: f64,
    pub human_review_upper: f64,
    pub reject_threshold: f64,
    pub hard_reject_threshold: f64,
    pub rollback_trigger: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CalibrationConfig {
    pub credible_interval_width: f64,
    pub conformal_coverage_target: f64,
    pub min_calibration_samples: u32,
    pub recalibration_trigger_drift: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FallbackTrigger {
    pub trigger_id: String,
    pub condition: String,
    pub action: String,
}

// ---------------------------------------------------------------------------
// Bayesian posterior computation and decision engine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BayesianPosterior {
    pub alpha: f64,
    pub beta: f64,
    pub mean: f64,
    pub variance: f64,
    pub credible_lower: f64,
    pub credible_upper: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationDecision {
    AutoApprove,
    HumanReview,
    Reject,
    HardReject,
    Rollback,
    ConservativeFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpectedLossResult {
    pub decision: MigrationDecision,
    pub posterior: BayesianPosterior,
    pub expected_loss_accept: f64,
    pub expected_loss_reject: f64,
    pub expected_loss_hold: f64,
    pub rationale: String,
    pub claim_id: Option<String>,
    pub policy_id: Option<String>,
}

impl ConfidenceModel {
    pub fn parse_and_validate(raw_json: &str) -> Result<Self> {
        let parsed: Self = serde_json::from_str(raw_json)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SUPPORTED_CONFIDENCE_MODEL_SCHEMA_VERSION {
            return Err(SemanticContractError::Validation(format!(
                "unsupported confidence model schema_version '{}' (expected '{}')",
                self.schema_version, SUPPORTED_CONFIDENCE_MODEL_SCHEMA_VERSION
            )));
        }
        if self.model_id.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "model_id must not be empty".to_string(),
            ));
        }

        self.validate_decision_space()?;
        self.validate_loss_matrix()?;
        self.validate_prior_config()?;
        self.validate_likelihood_sources()?;
        self.validate_decision_boundaries()?;
        self.validate_calibration()?;
        self.validate_fallback_triggers()?;

        Ok(())
    }

    fn validate_decision_space(&self) -> Result<()> {
        let ds = &self.decision_space;
        if ds.actions.is_empty() {
            return Err(SemanticContractError::Validation(
                "decision_space.actions must not be empty".to_string(),
            ));
        }
        if ds.states.is_empty() {
            return Err(SemanticContractError::Validation(
                "decision_space.states must not be empty".to_string(),
            ));
        }
        let required_actions = ["accept", "hold", "reject", "rollback"];
        for required in required_actions {
            if !ds.actions.iter().any(|a| a == required) {
                return Err(SemanticContractError::Validation(format!(
                    "decision_space.actions must include '{required}'"
                )));
            }
        }
        Ok(())
    }

    fn validate_loss_matrix(&self) -> Result<()> {
        let lm = &self.loss_matrix;
        if lm.accept_correct < 0.0 {
            return Err(SemanticContractError::Validation(
                "loss_matrix.accept_correct must be >= 0".to_string(),
            ));
        }
        if lm.accept_incorrect <= 0.0 {
            return Err(SemanticContractError::Validation(
                "loss_matrix.accept_incorrect must be > 0 (accepting wrong is costly)".to_string(),
            ));
        }
        if lm.accept_incorrect <= lm.reject_correct {
            return Err(SemanticContractError::Validation(
                "loss_matrix: accept_incorrect must exceed reject_correct (wrong acceptance is worse than correct rejection)".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_prior_config(&self) -> Result<()> {
        let pc = &self.prior_config;
        if pc.semantic_pass_rate_alpha <= 0.0 || pc.semantic_pass_rate_beta <= 0.0 {
            return Err(SemanticContractError::Validation(
                "prior_config: alpha and beta must be > 0 for valid Beta distribution".to_string(),
            ));
        }
        if pc.performance_variance_prior <= 0.0 {
            return Err(SemanticContractError::Validation(
                "prior_config.performance_variance_prior must be > 0".to_string(),
            ));
        }
        if pc.unsupported_feature_penalty_per_item < 0.0 {
            return Err(SemanticContractError::Validation(
                "prior_config.unsupported_feature_penalty_per_item must be >= 0".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_likelihood_sources(&self) -> Result<()> {
        if self.likelihood_sources.is_empty() {
            return Err(SemanticContractError::Validation(
                "likelihood_sources must not be empty".to_string(),
            ));
        }
        let mut total_weight = 0.0_f64;
        let mut seen_ids = BTreeSet::new();
        for source in &self.likelihood_sources {
            if source.source_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "likelihood source_id must not be empty".to_string(),
                ));
            }
            if !seen_ids.insert(source.source_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate likelihood source_id '{}'",
                    source.source_id
                )));
            }
            if !(0.0..=1.0).contains(&source.weight) {
                return Err(SemanticContractError::Validation(format!(
                    "likelihood source '{}' weight must be in [0.0, 1.0]",
                    source.source_id
                )));
            }
            total_weight += source.weight;
        }
        if (total_weight - 1.0).abs() > 0.01 {
            return Err(SemanticContractError::Validation(format!(
                "likelihood source weights must sum to ~1.0, got {total_weight:.4}"
            )));
        }
        Ok(())
    }

    fn validate_decision_boundaries(&self) -> Result<()> {
        let db = &self.decision_boundaries;
        let boundaries = [
            ("rollback_trigger", db.rollback_trigger),
            ("hard_reject_threshold", db.hard_reject_threshold),
            ("reject_threshold", db.reject_threshold),
            ("human_review_lower", db.human_review_lower),
            ("human_review_upper", db.human_review_upper),
            ("auto_approve_threshold", db.auto_approve_threshold),
        ];
        for (name, value) in &boundaries {
            if !(0.0..=1.0).contains(value) {
                return Err(SemanticContractError::Validation(format!(
                    "decision_boundary '{name}' must be in [0.0, 1.0], got {value}"
                )));
            }
        }
        // Boundaries must be monotonically ordered.
        if db.rollback_trigger > db.hard_reject_threshold {
            return Err(SemanticContractError::Validation(
                "rollback_trigger must be <= hard_reject_threshold".to_string(),
            ));
        }
        if db.hard_reject_threshold > db.reject_threshold {
            return Err(SemanticContractError::Validation(
                "hard_reject_threshold must be <= reject_threshold".to_string(),
            ));
        }
        if db.reject_threshold > db.human_review_lower {
            return Err(SemanticContractError::Validation(
                "reject_threshold must be <= human_review_lower".to_string(),
            ));
        }
        if db.human_review_lower > db.human_review_upper {
            return Err(SemanticContractError::Validation(
                "human_review_lower must be <= human_review_upper".to_string(),
            ));
        }
        if db.human_review_upper > db.auto_approve_threshold {
            return Err(SemanticContractError::Validation(
                "human_review_upper must be <= auto_approve_threshold".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_calibration(&self) -> Result<()> {
        let cal = &self.calibration;
        if !(0.0..=1.0).contains(&cal.credible_interval_width) {
            return Err(SemanticContractError::Validation(
                "calibration.credible_interval_width must be in [0.0, 1.0]".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&cal.conformal_coverage_target) {
            return Err(SemanticContractError::Validation(
                "calibration.conformal_coverage_target must be in [0.0, 1.0]".to_string(),
            ));
        }
        if cal.min_calibration_samples == 0 {
            return Err(SemanticContractError::Validation(
                "calibration.min_calibration_samples must be > 0".to_string(),
            ));
        }
        if cal.recalibration_trigger_drift <= 0.0 {
            return Err(SemanticContractError::Validation(
                "calibration.recalibration_trigger_drift must be > 0".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_fallback_triggers(&self) -> Result<()> {
        let mut seen_ids = BTreeSet::new();
        for trigger in &self.fallback_triggers {
            if trigger.trigger_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "fallback trigger_id must not be empty".to_string(),
                ));
            }
            if !seen_ids.insert(trigger.trigger_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate fallback trigger_id '{}'",
                    trigger.trigger_id
                )));
            }
            if trigger.condition.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "fallback trigger '{}' condition must not be empty",
                    trigger.trigger_id
                )));
            }
            let valid_actions = [
                "accept",
                "hold",
                "reject",
                "rollback",
                "conservative_fallback",
            ];
            if !valid_actions.contains(&trigger.action.as_str()) {
                return Err(SemanticContractError::Validation(format!(
                    "fallback trigger '{}' action '{}' not in decision space",
                    trigger.trigger_id, trigger.action
                )));
            }
        }
        Ok(())
    }

    /// Compute Beta-distribution posterior from prior config and observed evidence.
    #[must_use]
    pub fn compute_posterior(&self, successes: u32, failures: u32) -> BayesianPosterior {
        let alpha = self.prior_config.semantic_pass_rate_alpha + f64::from(successes);
        let beta = self.prior_config.semantic_pass_rate_beta + f64::from(failures);
        let mean = alpha / (alpha + beta);
        let variance = (alpha * beta) / ((alpha + beta).powi(2) * (alpha + beta + 1.0));

        // Approximate credible interval using Normal approximation of Beta distribution.
        let half_width = self.calibration.credible_interval_width / 2.0;
        let z = 1.645; // ~90% CI z-score
        let std_dev = variance.sqrt();
        let credible_lower = (mean - z * std_dev * (1.0 + half_width)).max(0.0);
        let credible_upper = (mean + z * std_dev * (1.0 + half_width)).min(1.0);

        BayesianPosterior {
            alpha,
            beta,
            mean,
            variance,
            credible_lower,
            credible_upper,
        }
    }

    /// Make a migration decision based on posterior mean and decision boundaries.
    #[must_use]
    pub fn decide(&self, posterior: &BayesianPosterior) -> MigrationDecision {
        let db = &self.decision_boundaries;
        let p = posterior.mean;

        if p >= db.auto_approve_threshold {
            MigrationDecision::AutoApprove
        } else if p >= db.human_review_lower {
            MigrationDecision::HumanReview
        } else if p >= db.reject_threshold {
            MigrationDecision::Reject
        } else if p >= db.hard_reject_threshold {
            MigrationDecision::HardReject
        } else if p >= db.rollback_trigger {
            MigrationDecision::Rollback
        } else {
            MigrationDecision::ConservativeFallback
        }
    }

    /// Compute expected loss for each action and select the minimum-loss decision.
    #[must_use]
    pub fn expected_loss_decision(
        &self,
        posterior: &BayesianPosterior,
        claim_id: Option<String>,
        policy_id: Option<String>,
    ) -> ExpectedLossResult {
        let p = posterior.mean;
        let lm = &self.loss_matrix;

        // Expected loss for each action.
        let el_accept = p * lm.accept_correct + (1.0 - p) * lm.accept_incorrect;
        let el_reject = p * lm.reject_incorrect + (1.0 - p) * lm.reject_correct;
        let el_hold = p * lm.hold_correct + (1.0 - p) * lm.hold_incorrect;

        // Select minimum expected loss action, with boundary override.
        let boundary_decision = self.decide(posterior);
        let el_decision = if el_accept <= el_reject && el_accept <= el_hold {
            MigrationDecision::AutoApprove
        } else if el_hold <= el_reject {
            MigrationDecision::HumanReview
        } else {
            MigrationDecision::Reject
        };

        // Use boundary decision as override when it is more conservative.
        let decision = match (boundary_decision, el_decision) {
            (MigrationDecision::ConservativeFallback, _)
            | (MigrationDecision::Rollback, _)
            | (MigrationDecision::HardReject, _) => boundary_decision,
            (MigrationDecision::Reject, MigrationDecision::AutoApprove) => boundary_decision,
            _ => el_decision,
        };

        let rationale = format!(
            "posterior_mean={:.4}, ci=[{:.4},{:.4}], EL(accept)={:.2}, EL(reject)={:.2}, EL(hold)={:.2}, boundary={}",
            p,
            posterior.credible_lower,
            posterior.credible_upper,
            el_accept,
            el_reject,
            el_hold,
            match boundary_decision {
                MigrationDecision::AutoApprove => "auto_approve",
                MigrationDecision::HumanReview => "human_review",
                MigrationDecision::Reject => "reject",
                MigrationDecision::HardReject => "hard_reject",
                MigrationDecision::Rollback => "rollback",
                MigrationDecision::ConservativeFallback => "conservative_fallback",
            }
        );

        ExpectedLossResult {
            decision,
            posterior: posterior.clone(),
            expected_loss_accept: el_accept,
            expected_loss_reject: el_reject,
            expected_loss_hold: el_hold,
            rationale,
            claim_id,
            policy_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Licensing & Provenance Guardrails — IP-risk and attribution enforcement
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LicensingProvenanceContract {
    pub contract_id: String,
    pub schema_version: String,
    pub contract_version: String,
    pub licensing_policy: LicensingPolicy,
    pub provenance_chain_policy: ProvenanceChainPolicy,
    pub ip_artifact_statuses: Vec<String>,
    pub fail_safe_defaults: FailSafeDefaults,
    pub attribution_template: AttributionTemplate,
    pub risk_flags: Vec<LicensingRiskFlag>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LicensingPolicy {
    pub allowed_license_classes: Vec<String>,
    pub blocked_license_classes: Vec<String>,
    pub copyleft_boundary_action: String,
    pub missing_license_action: String,
    pub ambiguous_license_action: String,
    pub license_class_definitions: Vec<LicenseClassDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LicenseClassDefinition {
    pub class_id: String,
    pub description: String,
    pub risk_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceChainPolicy {
    pub required_stages: Vec<String>,
    pub hash_algorithm: String,
    pub chain_must_be_unbroken: bool,
    pub each_stage_must_record: Vec<String>,
    pub attribution_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FailSafeDefaults {
    pub on_missing_provenance: String,
    pub on_broken_chain: String,
    pub on_blocked_license: String,
    pub on_unknown_license: String,
    pub on_expired_attribution: String,
    pub on_needs_counsel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttributionTemplate {
    pub format: String,
    pub required_fields: Vec<String>,
    pub optional_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LicensingRiskFlag {
    pub flag_id: String,
    pub severity: String,
    pub description: String,
}

// ---------------------------------------------------------------------------
// Runtime types for provenance tracking and IP artifact assessment
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IpArtifactStatus {
    Clear,
    Expired,
    Unknown,
    NeedsCounsel,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpArtifactRecord {
    pub artifact_id: String,
    pub license_spdx: Option<String>,
    pub license_class: String,
    pub status: IpArtifactStatus,
    pub risk_flags: Vec<String>,
    pub design_around_notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvenanceChainRecord {
    pub stage_id: String,
    pub input_hash: String,
    pub output_hash: String,
    pub tool_version: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvenanceReport {
    pub run_id: String,
    pub chain: Vec<ProvenanceChainRecord>,
    pub ip_artifacts: Vec<IpArtifactRecord>,
    pub attribution_notice: String,
    pub unresolved_risk_flags: Vec<String>,
    pub overall_status: IpArtifactStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceAction {
    Accept,
    Hold,
    Reject,
}

impl LicensingProvenanceContract {
    pub fn parse_and_validate(raw_json: &str) -> Result<Self> {
        let parsed: Self = serde_json::from_str(raw_json)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SUPPORTED_LICENSING_PROVENANCE_SCHEMA_VERSION {
            return Err(SemanticContractError::Validation(format!(
                "unsupported licensing provenance schema_version '{}' (expected '{}')",
                self.schema_version, SUPPORTED_LICENSING_PROVENANCE_SCHEMA_VERSION
            )));
        }
        if self.contract_id.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "contract_id must not be empty".to_string(),
            ));
        }

        self.validate_licensing_policy()?;
        self.validate_provenance_chain_policy()?;
        self.validate_ip_artifact_statuses()?;
        self.validate_fail_safe_defaults()?;
        self.validate_attribution_template()?;
        self.validate_risk_flags()?;

        Ok(())
    }

    fn validate_licensing_policy(&self) -> Result<()> {
        let lp = &self.licensing_policy;
        if lp.allowed_license_classes.is_empty() {
            return Err(SemanticContractError::Validation(
                "licensing_policy.allowed_license_classes must not be empty".to_string(),
            ));
        }
        if lp.blocked_license_classes.is_empty() {
            return Err(SemanticContractError::Validation(
                "licensing_policy.blocked_license_classes must not be empty".to_string(),
            ));
        }
        if lp.license_class_definitions.is_empty() {
            return Err(SemanticContractError::Validation(
                "licensing_policy.license_class_definitions must not be empty".to_string(),
            ));
        }

        // Every referenced class must have a definition.
        let defined_classes: BTreeSet<_> = lp
            .license_class_definitions
            .iter()
            .map(|d| d.class_id.as_str())
            .collect();
        for class in lp
            .allowed_license_classes
            .iter()
            .chain(lp.blocked_license_classes.iter())
        {
            if !defined_classes.contains(class.as_str()) {
                return Err(SemanticContractError::Validation(format!(
                    "license class '{}' referenced but not defined in license_class_definitions",
                    class
                )));
            }
        }

        // Allowed and blocked must be disjoint.
        let allowed: BTreeSet<_> = lp.allowed_license_classes.iter().collect();
        let blocked: BTreeSet<_> = lp.blocked_license_classes.iter().collect();
        let overlap: Vec<_> = allowed.intersection(&blocked).collect();
        if !overlap.is_empty() {
            return Err(SemanticContractError::Validation(format!(
                "license classes cannot be both allowed and blocked: {:?}",
                overlap
            )));
        }

        // Validate class definitions have non-empty fields.
        let mut seen_ids = BTreeSet::new();
        let valid_risk_levels = ["low", "medium", "high", "critical"];
        for def in &lp.license_class_definitions {
            if def.class_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "license class_id must not be empty".to_string(),
                ));
            }
            if !seen_ids.insert(def.class_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate license class_id '{}'",
                    def.class_id
                )));
            }
            if def.description.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "license class '{}' has empty description",
                    def.class_id
                )));
            }
            if !valid_risk_levels.contains(&def.risk_level.as_str()) {
                return Err(SemanticContractError::Validation(format!(
                    "license class '{}' has invalid risk_level '{}' (expected: low, medium, high, critical)",
                    def.class_id, def.risk_level
                )));
            }
        }

        // Validate action strings.
        let valid_actions = ["fail_safe", "reject", "hold", "needs_counsel", "warn"];
        for (name, action) in [
            ("copyleft_boundary_action", &lp.copyleft_boundary_action),
            ("missing_license_action", &lp.missing_license_action),
            ("ambiguous_license_action", &lp.ambiguous_license_action),
        ] {
            if !valid_actions.contains(&action.as_str()) {
                return Err(SemanticContractError::Validation(format!(
                    "licensing_policy.{name} '{}' not a valid action",
                    action
                )));
            }
        }

        Ok(())
    }

    fn validate_provenance_chain_policy(&self) -> Result<()> {
        let pcp = &self.provenance_chain_policy;
        if pcp.required_stages.is_empty() {
            return Err(SemanticContractError::Validation(
                "provenance_chain_policy.required_stages must not be empty".to_string(),
            ));
        }
        if pcp.hash_algorithm.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "provenance_chain_policy.hash_algorithm must not be empty".to_string(),
            ));
        }
        if pcp.each_stage_must_record.is_empty() {
            return Err(SemanticContractError::Validation(
                "provenance_chain_policy.each_stage_must_record must not be empty".to_string(),
            ));
        }
        // Validate required recording fields.
        let required_recording = ["input_hash", "output_hash", "tool_version", "timestamp"];
        for required in required_recording {
            if !pcp.each_stage_must_record.iter().any(|f| f == required) {
                return Err(SemanticContractError::Validation(format!(
                    "provenance_chain_policy.each_stage_must_record missing required field '{}'",
                    required
                )));
            }
        }
        Ok(())
    }

    fn validate_ip_artifact_statuses(&self) -> Result<()> {
        if self.ip_artifact_statuses.is_empty() {
            return Err(SemanticContractError::Validation(
                "ip_artifact_statuses must not be empty".to_string(),
            ));
        }
        let required = ["clear", "unknown", "blocked"];
        for r in required {
            if !self.ip_artifact_statuses.iter().any(|s| s == r) {
                return Err(SemanticContractError::Validation(format!(
                    "ip_artifact_statuses must include '{r}'"
                )));
            }
        }
        Ok(())
    }

    fn validate_fail_safe_defaults(&self) -> Result<()> {
        let fsd = &self.fail_safe_defaults;
        let valid_actions = ["accept", "hold", "reject"];
        for (name, action) in [
            ("on_missing_provenance", &fsd.on_missing_provenance),
            ("on_broken_chain", &fsd.on_broken_chain),
            ("on_blocked_license", &fsd.on_blocked_license),
            ("on_unknown_license", &fsd.on_unknown_license),
            ("on_expired_attribution", &fsd.on_expired_attribution),
            ("on_needs_counsel", &fsd.on_needs_counsel),
        ] {
            if !valid_actions.contains(&action.as_str()) {
                return Err(SemanticContractError::Validation(format!(
                    "fail_safe_defaults.{name} '{}' not a valid action (accept, hold, reject)",
                    action
                )));
            }
        }
        // Critical paths must default to reject (fail-safe requirement).
        if fsd.on_missing_provenance != "reject" {
            return Err(SemanticContractError::Validation(
                "fail_safe_defaults.on_missing_provenance must be 'reject' for safety".to_string(),
            ));
        }
        if fsd.on_broken_chain != "reject" {
            return Err(SemanticContractError::Validation(
                "fail_safe_defaults.on_broken_chain must be 'reject' for safety".to_string(),
            ));
        }
        if fsd.on_blocked_license != "reject" {
            return Err(SemanticContractError::Validation(
                "fail_safe_defaults.on_blocked_license must be 'reject' for safety".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_attribution_template(&self) -> Result<()> {
        let at = &self.attribution_template;
        if at.format.trim().is_empty() {
            return Err(SemanticContractError::Validation(
                "attribution_template.format must not be empty".to_string(),
            ));
        }
        if at.required_fields.is_empty() {
            return Err(SemanticContractError::Validation(
                "attribution_template.required_fields must not be empty".to_string(),
            ));
        }
        for field in &at.required_fields {
            if field.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "attribution_template contains empty required field".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn validate_risk_flags(&self) -> Result<()> {
        let mut seen_ids = BTreeSet::new();
        let valid_severities = ["low", "medium", "high", "critical"];
        for flag in &self.risk_flags {
            if flag.flag_id.trim().is_empty() {
                return Err(SemanticContractError::Validation(
                    "risk flag_id must not be empty".to_string(),
                ));
            }
            if !seen_ids.insert(flag.flag_id.clone()) {
                return Err(SemanticContractError::Validation(format!(
                    "duplicate risk flag_id '{}'",
                    flag.flag_id
                )));
            }
            if !valid_severities.contains(&flag.severity.as_str()) {
                return Err(SemanticContractError::Validation(format!(
                    "risk flag '{}' has invalid severity '{}' (expected: low, medium, high, critical)",
                    flag.flag_id, flag.severity
                )));
            }
            if flag.description.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "risk flag '{}' has empty description",
                    flag.flag_id
                )));
            }
        }
        Ok(())
    }

    /// Look up a license class definition by class_id.
    #[must_use]
    pub fn license_class(&self, class_id: &str) -> Option<&LicenseClassDefinition> {
        self.licensing_policy
            .license_class_definitions
            .iter()
            .find(|def| def.class_id == class_id)
    }

    /// Check if a given license class is allowed.
    #[must_use]
    pub fn is_license_allowed(&self, class_id: &str) -> bool {
        self.licensing_policy
            .allowed_license_classes
            .iter()
            .any(|c| c == class_id)
    }

    /// Check if a given license class is blocked.
    #[must_use]
    pub fn is_license_blocked(&self, class_id: &str) -> bool {
        self.licensing_policy
            .blocked_license_classes
            .iter()
            .any(|c| c == class_id)
    }

    /// Determine the fail-safe action for a given IP artifact status.
    #[must_use]
    pub fn fail_safe_action(&self, status: IpArtifactStatus) -> ProvenanceAction {
        let action_str = match status {
            IpArtifactStatus::Clear => return ProvenanceAction::Accept,
            IpArtifactStatus::Blocked => &self.fail_safe_defaults.on_blocked_license,
            IpArtifactStatus::Unknown => &self.fail_safe_defaults.on_unknown_license,
            IpArtifactStatus::NeedsCounsel => &self.fail_safe_defaults.on_needs_counsel,
            IpArtifactStatus::Expired => &self.fail_safe_defaults.on_expired_attribution,
        };
        match action_str.as_str() {
            "accept" => ProvenanceAction::Accept,
            "hold" => ProvenanceAction::Hold,
            _ => ProvenanceAction::Reject,
        }
    }

    /// Validate a provenance chain: check it covers all required stages
    /// and has an unbroken hash chain.
    pub fn validate_provenance_chain(&self, chain: &[ProvenanceChainRecord]) -> Result<()> {
        let required = &self.provenance_chain_policy.required_stages;
        let chain_stage_ids: BTreeSet<_> = chain.iter().map(|r| r.stage_id.as_str()).collect();

        for stage in required {
            if !chain_stage_ids.contains(stage.as_str()) {
                return Err(SemanticContractError::Validation(format!(
                    "provenance chain missing required stage '{stage}'"
                )));
            }
        }

        if self.provenance_chain_policy.chain_must_be_unbroken {
            for window in chain.windows(2) {
                if window[0].output_hash != window[1].input_hash {
                    return Err(SemanticContractError::Validation(format!(
                        "provenance chain broken: stage '{}' output_hash ({}) != stage '{}' input_hash ({})",
                        window[0].stage_id,
                        window[0].output_hash,
                        window[1].stage_id,
                        window[1].input_hash
                    )));
                }
            }
        }

        for record in chain {
            if record.tool_version.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "provenance record for stage '{}' missing tool_version",
                    record.stage_id
                )));
            }
            if record.timestamp.trim().is_empty() {
                return Err(SemanticContractError::Validation(format!(
                    "provenance record for stage '{}' missing timestamp",
                    record.stage_id
                )));
            }
        }

        Ok(())
    }

    /// Assess a set of IP artifacts and produce a provenance report.
    ///
    /// The provenance chain is judged too, not just copied in. An empty chain
    /// or one that fails [`Self::validate_provenance_chain`] - a required stage
    /// missing, a hash link broken, a record without a tool version or
    /// timestamp - makes the report `Blocked` and raises `lp-provenance-gap`.
    #[must_use]
    pub fn assess_ip_artifacts(
        &self,
        run_id: &str,
        chain: &[ProvenanceChainRecord],
        artifacts: &[IpArtifactRecord],
    ) -> ProvenanceReport {
        let mut unresolved_flags = Vec::new();
        let mut worst_status = IpArtifactStatus::Clear;

        // The contract already says what to do here, and nothing did it.
        // `fail_safe_defaults.on_missing_provenance` and `on_broken_chain`
        // must both be "reject" - `validate_fail_safe_defaults` refuses any
        // other value "for safety" - and `lp-provenance-gap` is the risk flag
        // defined for it. But the chain was copied into the report unchecked
        // and the status came from the artifacts alone, so an empty or
        // hash-broken chain with no blocked artifact assessed as `Clear`, and
        // `certification_report` passed its compliance stage. `Blocked` is the
        // status that carries a rejection: `compliance_status` fails on it
        // under every profile, and the artifact loop below never lowers it.
        if chain.is_empty() || self.validate_provenance_chain(chain).is_err() {
            worst_status = IpArtifactStatus::Blocked;
            unresolved_flags.push(PROVENANCE_GAP_FLAG.to_string());
        }

        for artifact in artifacts {
            if self.is_license_blocked(&artifact.license_class) {
                worst_status = IpArtifactStatus::Blocked;
            }
            match artifact.status {
                IpArtifactStatus::Blocked => worst_status = IpArtifactStatus::Blocked,
                IpArtifactStatus::NeedsCounsel if worst_status != IpArtifactStatus::Blocked => {
                    worst_status = IpArtifactStatus::NeedsCounsel;
                }
                IpArtifactStatus::Unknown
                    if worst_status != IpArtifactStatus::Blocked
                        && worst_status != IpArtifactStatus::NeedsCounsel =>
                {
                    worst_status = IpArtifactStatus::Unknown;
                }
                IpArtifactStatus::Expired if worst_status == IpArtifactStatus::Clear => {
                    worst_status = IpArtifactStatus::Expired;
                }
                _ => {}
            }
            for flag in &artifact.risk_flags {
                if !unresolved_flags.contains(flag) {
                    unresolved_flags.push(flag.clone());
                }
            }
        }

        ProvenanceReport {
            run_id: run_id.to_string(),
            chain: chain.to_vec(),
            ip_artifacts: artifacts.to_vec(),
            attribution_notice: String::new(), // Populated by caller
            unresolved_risk_flags: unresolved_flags,
            overall_status: worst_status,
        }
    }
}

pub fn load_builtin_semantic_contract() -> Result<SemanticEquivalenceContract> {
    SemanticEquivalenceContract::parse_and_validate(BUILTIN_CONTRACT_JSON)
}

pub fn load_builtin_transformation_policy_matrix() -> Result<TransformationPolicyMatrix> {
    TransformationPolicyMatrix::parse_and_validate(BUILTIN_TRANSFORMATION_POLICY_JSON)
}

pub fn load_builtin_evidence_manifest() -> Result<EvidenceManifest> {
    EvidenceManifest::parse_and_validate(BUILTIN_EVIDENCE_MANIFEST_JSON)
}

pub fn load_builtin_confidence_model() -> Result<ConfidenceModel> {
    ConfidenceModel::parse_and_validate(BUILTIN_CONFIDENCE_MODEL_JSON)
}

pub fn load_builtin_licensing_provenance() -> Result<LicensingProvenanceContract> {
    LicensingProvenanceContract::parse_and_validate(BUILTIN_LICENSING_PROVENANCE_JSON)
}

#[cfg(test)]
mod tests {
    use super::{
        REQUIRED_POLICY_CATEGORIES, Result, SUPPORTED_SCHEMA_VERSION,
        SUPPORTED_TRANSFORMATION_POLICY_SCHEMA_VERSION, SemanticContractError,
        SemanticEquivalenceContract, TransformationPolicyMatrix, load_builtin_semantic_contract,
        load_builtin_transformation_policy_matrix,
    };

    fn load() -> Result<SemanticEquivalenceContract> {
        load_builtin_semantic_contract()
    }

    fn load_policy() -> Result<TransformationPolicyMatrix> {
        load_builtin_transformation_policy_matrix()
    }

    #[test]
    fn builtin_contract_parses_and_validates() {
        let contract = load().expect("builtin contract should parse");
        assert_eq!(contract.schema_version, SUPPORTED_SCHEMA_VERSION);
        assert!(!contract.contract_id.is_empty());
    }

    #[test]
    fn validators_map_to_known_clauses() {
        let contract = load().expect("builtin contract should parse");
        for (validator_id, clause_ids) in &contract.validator_clause_map {
            assert!(
                !clause_ids.is_empty(),
                "validator {} should not have empty clause map",
                validator_id
            );
            for clause_id in clause_ids {
                assert!(
                    contract.clause(clause_id).is_some(),
                    "clause {} should exist",
                    clause_id
                );
            }
        }
    }

    #[test]
    fn tie_break_priorities_are_unique_and_sorted() {
        let contract = load().expect("builtin contract should parse");
        let mut priorities = contract
            .deterministic_tie_breakers
            .iter()
            .map(|rule| rule.priority)
            .collect::<Vec<_>>();
        let mut sorted = priorities.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            priorities.len(),
            sorted.len(),
            "tie-break priorities must be unique"
        );
        priorities.sort_unstable();
        assert_eq!(priorities, sorted);
    }

    #[test]
    fn unknown_validator_has_no_mapped_clauses() {
        let contract = load().expect("builtin contract should parse");
        assert!(
            contract
                .clauses_for_validator("validator-that-does-not-exist")
                .is_empty()
        );
    }

    #[test]
    fn compiled_validators_cover_validator_clause_map() {
        let contract = load().expect("builtin contract should parse");
        let compiled = contract.compile_validators();
        assert_eq!(
            compiled.len(),
            contract.validator_clause_map.len(),
            "compiled validator count should match map cardinality"
        );
        for validator in &compiled {
            let mapped = contract
                .validator_clause_map
                .get(&validator.validator_id)
                .expect("compiled validator should exist in map");
            assert_eq!(
                &validator.clause_ids, mapped,
                "compiled clause IDs should match source map"
            );
        }
    }

    #[test]
    fn invalid_contract_rejects_unknown_clauses() {
        let invalid = r#"
        {
          "contract_id": "bad",
          "schema_version": "sem-eq-contract-v1",
          "contract_version": "2026-02-25",
          "equivalence_axes": {
            "state_transition": "x",
            "event_ordering": "x",
            "side_effect_observability": "x"
          },
          "visual_tolerance_policy": {
            "strict_classes": ["a"],
            "strict_policy": "x",
            "perceptual_classes": ["b"],
            "perceptual_policy": "x",
            "max_perceptual_delta": 0.1
          },
          "improvement_envelope": {
            "allowed_dimensions": ["x"],
            "forbidden_rewrites": ["x"],
            "required_safeguards": ["x"]
          },
          "deterministic_tie_breakers": [
            {"priority": 1, "rule_id": "tb", "description": "x"}
          ],
          "clauses": [
            {
              "clause_id": "CL-1",
              "title": "t",
              "category": "c",
              "requirement": "r",
              "severity": "high"
            }
          ],
          "validator_clause_map": {
            "v": ["DOES-NOT-EXIST"]
          }
        }
        "#;

        let error =
            SemanticEquivalenceContract::parse_and_validate(invalid).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref message) if message.contains("unknown clause")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn builtin_transformation_policy_matrix_parses_and_validates() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        assert_eq!(
            matrix.schema_version,
            SUPPORTED_TRANSFORMATION_POLICY_SCHEMA_VERSION
        );
        assert!(!matrix.policy_id.is_empty());
    }

    #[test]
    fn transformation_policy_covers_required_categories() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        let categories = matrix
            .categories
            .iter()
            .map(|category| category.category_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for required in REQUIRED_POLICY_CATEGORIES {
            assert!(
                categories.contains(required),
                "required category {} should exist",
                required
            );
        }
    }

    #[test]
    fn transformation_policy_explicitly_classifies_every_construct() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        let catalog_signatures = matrix
            .construct_catalog
            .iter()
            .map(|entry| entry.construct_signature.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let policy_signatures = matrix
            .policy_cells
            .iter()
            .map(|cell| cell.construct_signature.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            catalog_signatures, policy_signatures,
            "every construct must have exactly one policy class"
        );
    }

    #[test]
    fn planner_and_certification_rows_are_deterministic_and_complete() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        let planner_rows = matrix.planner_rows();
        let certification_rows = matrix.certification_rows();
        assert_eq!(planner_rows.len(), matrix.construct_catalog.len());
        assert_eq!(certification_rows.len(), matrix.construct_catalog.len());
        assert!(
            planner_rows
                .windows(2)
                .all(|window| window[0].construct_signature <= window[1].construct_signature)
        );
        assert!(
            certification_rows
                .iter()
                .all(|row| !row.semantic_clause_links.is_empty()
                    && !row.certification_evidence.is_empty())
        );
    }

    #[test]
    fn invalid_transformation_policy_rejects_missing_construct_classification() {
        let mut matrix = load_policy().expect("builtin policy matrix should parse");
        matrix
            .policy_cells
            .pop()
            .expect("policy cells should not be empty");
        let raw = serde_json::to_string(&matrix).expect("policy matrix should serialize");
        let error = TransformationPolicyMatrix::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref message) if message.contains("without explicit handling class")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_transformation_policy_rejects_unknown_semantic_clause() {
        let mut matrix = load_policy().expect("builtin policy matrix should parse");
        matrix.policy_cells[0].semantic_clause_links = vec!["DOES-NOT-EXIST".to_string()];
        let raw = serde_json::to_string(&matrix).expect("policy matrix should serialize");
        let error = TransformationPolicyMatrix::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref message) if message.contains("unknown semantic clause")),
            "unexpected error: {error}"
        );
    }

    // -----------------------------------------------------------------------
    // Evidence Manifest tests
    // -----------------------------------------------------------------------

    fn load_manifest() -> Result<super::EvidenceManifest> {
        super::load_builtin_evidence_manifest()
    }

    #[test]
    fn builtin_evidence_manifest_parses_and_validates() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        assert_eq!(
            manifest.schema_version,
            super::SUPPORTED_EVIDENCE_MANIFEST_SCHEMA_VERSION
        );
        assert!(!manifest.manifest_id.is_empty());
        assert!(!manifest.run_id.is_empty());
    }

    #[test]
    fn evidence_manifest_stages_are_consecutively_indexed() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        for (i, stage) in manifest.stages.iter().enumerate() {
            assert_eq!(
                stage.stage_index, i as u32,
                "stage '{}' should have index {}",
                stage.stage_id, i
            );
        }
    }

    #[test]
    fn evidence_manifest_hash_chain_is_valid() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        for window in manifest.stages.windows(2) {
            let prev = &window[0];
            let curr = &window[1];
            assert_eq!(
                prev.output_hash, curr.input_hash,
                "hash chain: stage '{}' output should equal stage '{}' input",
                prev.stage_id, curr.stage_id
            );
        }
    }

    #[test]
    fn evidence_manifest_correlation_ids_are_unique() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        let mut seen = std::collections::BTreeSet::new();
        for stage in &manifest.stages {
            assert!(
                seen.insert(&stage.correlation_id),
                "duplicate correlation_id '{}'",
                stage.correlation_id
            );
        }
    }

    #[test]
    fn evidence_manifest_source_fingerprint_has_hash() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        assert!(
            !manifest.source_fingerprint.source_hash.is_empty(),
            "source_hash must not be empty"
        );
        assert!(
            manifest.source_fingerprint.repo_url.is_some()
                || manifest.source_fingerprint.local_path.is_some(),
            "must have repo_url or local_path"
        );
    }

    #[test]
    fn evidence_manifest_certification_verdict_is_consistent() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        let cv = &manifest.certification_verdict;
        assert!(
            (0.0..=1.0).contains(&cv.confidence),
            "confidence must be in [0.0, 1.0]"
        );
        if cv.verdict == super::VerdictOutcome::Accept {
            assert_eq!(
                cv.test_fail_count, 0,
                "accept verdict must have zero failures"
            );
        }
    }

    #[test]
    fn evidence_manifest_determinism_attestation_is_consistent() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        let da = &manifest.determinism_attestation;
        assert!(
            da.identical_runs_count > 0,
            "identical_runs_count must be > 0"
        );
        if da.divergence_detected {
            assert!(
                !da.manifest_hash_stable,
                "divergence detected but manifest_hash_stable is true"
            );
        }
    }

    #[test]
    fn evidence_manifest_stage_lineage_reconstructs_full_chain() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        let lineage = manifest.stage_lineage();
        assert_eq!(lineage.len(), manifest.stages.len());
        for (entry, stage) in lineage.iter().zip(manifest.stages.iter()) {
            assert_eq!(entry.stage_id, stage.stage_id);
            assert_eq!(entry.correlation_id, stage.correlation_id);
            assert_eq!(entry.claim_id, stage.claim_id);
            assert_eq!(entry.evidence_id, stage.evidence_id);
            assert_eq!(entry.policy_id, stage.policy_id);
            assert_eq!(entry.trace_id, stage.trace_id);
            assert_eq!(entry.input_hash, stage.input_hash);
            assert_eq!(entry.output_hash, stage.output_hash);
        }
    }

    #[test]
    fn evidence_manifest_to_jsonl_produces_valid_json_lines() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        let jsonl = manifest.evidence_jsonl();
        let lines = jsonl.lines().collect::<Vec<_>>();
        assert_eq!(
            lines.len(),
            manifest.stages.len(),
            "one JSONL line per stage"
        );
        for line in &lines {
            let parsed: serde_json::Value =
                serde_json::from_str(line).expect("each JSONL line must be valid JSON");
            assert_eq!(
                parsed["event"], "stage_completed",
                "event field must be stage_completed"
            );
            assert!(
                !parsed["correlation_id"].as_str().unwrap_or("").is_empty(),
                "correlation_id must not be empty"
            );
            assert!(
                !parsed["run_id"].as_str().unwrap_or("").is_empty(),
                "run_id must not be empty"
            );
            assert!(
                !parsed["claim_id"].as_str().unwrap_or("").is_empty(),
                "claim_id must not be empty"
            );
            assert!(
                !parsed["evidence_id"].as_str().unwrap_or("").is_empty(),
                "evidence_id must not be empty"
            );
            assert!(
                !parsed["policy_id"].as_str().unwrap_or("").is_empty(),
                "policy_id must not be empty"
            );
            assert!(
                !parsed["trace_id"].as_str().unwrap_or("").is_empty(),
                "trace_id must not be empty"
            );
        }
    }

    #[test]
    fn evidence_manifest_stage_lookup_by_correlation_id() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        for stage in &manifest.stages {
            let found = manifest
                .stage_by_correlation_id(&stage.correlation_id)
                .expect("should find stage by correlation_id");
            assert_eq!(found.stage_id, stage.stage_id);
        }
        assert!(
            manifest.stage_by_correlation_id("nonexistent-id").is_none(),
            "nonexistent correlation_id should return None"
        );
    }

    #[test]
    fn evidence_manifest_round_trip_serialization_is_stable() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        let serialized =
            serde_json::to_string_pretty(&manifest).expect("evidence manifest should serialize");
        let deserialized: super::EvidenceManifest =
            serde_json::from_str(&serialized).expect("should deserialize back");
        assert_eq!(manifest, deserialized, "round-trip must be stable");
    }

    #[test]
    fn invalid_evidence_manifest_rejects_empty_run_id() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.run_id = String::new();
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("run_id")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_broken_hash_chain() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        if manifest.stages.len() >= 2 {
            manifest.stages[1].input_hash = "sha256:BROKEN".to_string();
            let raw = serde_json::to_string(&manifest).expect("should serialize");
            let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("hash chain broken")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn invalid_evidence_manifest_rejects_non_consecutive_indices() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        if manifest.stages.len() >= 2 {
            manifest.stages[1].stage_index = 5;
            let raw = serde_json::to_string(&manifest).expect("should serialize");
            let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("consecutive")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn invalid_evidence_manifest_rejects_accept_with_failures() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.certification_verdict.verdict = super::VerdictOutcome::Accept;
        manifest.certification_verdict.test_fail_count = 3;
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("failing tests")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_divergence_with_stable_hash() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.determinism_attestation.divergence_detected = true;
        manifest.determinism_attestation.manifest_hash_stable = true;
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("divergence")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_confidence_out_of_range() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.certification_verdict.confidence = 1.5;
        manifest.certification_verdict.verdict = super::VerdictOutcome::Hold;
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("confidence")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_empty_source_hash() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.source_fingerprint.source_hash = String::new();
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("source_hash")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_failed_stage_without_error() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        if !manifest.stages.is_empty() {
            let last_idx = manifest.stages.len() - 1;
            manifest.stages[last_idx].status = super::StageStatus::Failed;
            manifest.stages[last_idx].error = None;
            let raw = serde_json::to_string(&manifest).expect("should serialize");
            let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("failed")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn invalid_evidence_manifest_rejects_zero_identical_runs() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.determinism_attestation.identical_runs_count = 0;
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("identical_runs_count")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_empty_claim_id() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.stages[0].claim_id.clear();
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("claim_id")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_duplicate_evidence_id() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        if manifest.stages.len() >= 2 {
            manifest.stages[1].evidence_id = manifest.stages[0].evidence_id.clone();
        }
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("duplicate evidence_id")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_orphan_stage_claim() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.stages[0].claim_id = "UNKNOWN-CLAIM".to_string();
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("semantic_clause_coverage.covered")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_unlinked_covered_claim() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest
            .certification_verdict
            .semantic_clause_coverage
            .covered
            .push("TB-001".to_string());
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("missing stage linkage")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_evidence_manifest_rejects_overlap_between_covered_and_uncovered() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest
            .certification_verdict
            .semantic_clause_coverage
            .uncovered
            .push(
                manifest
                    .certification_verdict
                    .semantic_clause_coverage
                    .covered[0]
                    .clone(),
            );
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("both covered and uncovered")),
            "unexpected error: {error}"
        );
    }

    // -----------------------------------------------------------------------
    // Confidence Model tests
    // -----------------------------------------------------------------------

    fn load_confidence() -> Result<super::ConfidenceModel> {
        super::load_builtin_confidence_model()
    }

    #[test]
    fn builtin_confidence_model_parses_and_validates() {
        let model = load_confidence().expect("builtin confidence model should parse");
        assert_eq!(
            model.schema_version,
            super::SUPPORTED_CONFIDENCE_MODEL_SCHEMA_VERSION
        );
        assert!(!model.model_id.is_empty());
    }

    #[test]
    fn confidence_model_likelihood_weights_sum_to_one() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let total: f64 = model.likelihood_sources.iter().map(|s| s.weight).sum();
        assert!(
            (total - 1.0).abs() < 0.01,
            "likelihood weights must sum to ~1.0, got {total}"
        );
    }

    #[test]
    fn confidence_model_decision_boundaries_are_monotonic() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let db = &model.decision_boundaries;
        assert!(db.rollback_trigger <= db.hard_reject_threshold);
        assert!(db.hard_reject_threshold <= db.reject_threshold);
        assert!(db.reject_threshold <= db.human_review_lower);
        assert!(db.human_review_lower <= db.human_review_upper);
        assert!(db.human_review_upper <= db.auto_approve_threshold);
    }

    #[test]
    fn confidence_model_decision_space_has_required_actions() {
        let model = load_confidence().expect("builtin confidence model should parse");
        for required in ["accept", "hold", "reject", "rollback"] {
            assert!(
                model.decision_space.actions.iter().any(|a| a == required),
                "decision_space.actions must include '{required}'"
            );
        }
    }

    #[test]
    fn confidence_model_loss_matrix_is_consistent() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let lm = &model.loss_matrix;
        assert!(
            lm.accept_incorrect > lm.reject_correct,
            "wrong acceptance must be costlier than correct rejection"
        );
        assert!(lm.accept_correct >= 0.0, "correct acceptance cost >= 0");
    }

    #[test]
    fn confidence_model_fallback_triggers_have_valid_actions() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let valid = [
            "accept",
            "hold",
            "reject",
            "rollback",
            "conservative_fallback",
        ];
        for trigger in &model.fallback_triggers {
            assert!(
                valid.contains(&trigger.action.as_str()),
                "trigger '{}' has invalid action '{}'",
                trigger.trigger_id,
                trigger.action
            );
        }
    }

    #[test]
    fn posterior_computation_with_no_evidence_returns_prior() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let posterior = model.compute_posterior(0, 0);
        let expected_mean = model.prior_config.semantic_pass_rate_alpha
            / (model.prior_config.semantic_pass_rate_alpha
                + model.prior_config.semantic_pass_rate_beta);
        assert!(
            (posterior.mean - expected_mean).abs() < 1e-10,
            "posterior mean with no evidence should equal prior mean"
        );
        assert_eq!(posterior.alpha, model.prior_config.semantic_pass_rate_alpha);
        assert_eq!(posterior.beta, model.prior_config.semantic_pass_rate_beta);
    }

    #[test]
    fn posterior_mean_increases_with_successes() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let p0 = model.compute_posterior(0, 0);
        let p10 = model.compute_posterior(10, 0);
        let p100 = model.compute_posterior(100, 0);
        assert!(p10.mean > p0.mean, "more successes should increase mean");
        assert!(p100.mean > p10.mean, "more successes should increase mean");
    }

    #[test]
    fn posterior_mean_decreases_with_failures() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let p0 = model.compute_posterior(0, 0);
        let p_fail = model.compute_posterior(0, 10);
        assert!(p_fail.mean < p0.mean, "more failures should decrease mean");
    }

    #[test]
    fn posterior_variance_decreases_with_more_data() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let p_small = model.compute_posterior(5, 1);
        let p_large = model.compute_posterior(50, 10);
        assert!(
            p_large.variance < p_small.variance,
            "more data should decrease variance"
        );
    }

    #[test]
    fn posterior_credible_interval_is_within_bounds() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let posterior = model.compute_posterior(20, 3);
        assert!(
            posterior.credible_lower >= 0.0,
            "credible lower must be >= 0"
        );
        assert!(
            posterior.credible_upper <= 1.0,
            "credible upper must be <= 1"
        );
        assert!(
            posterior.credible_lower <= posterior.mean,
            "lower bound must be <= mean"
        );
        assert!(
            posterior.credible_upper >= posterior.mean,
            "upper bound must be >= mean"
        );
    }

    #[test]
    fn decision_high_confidence_auto_approves() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let posterior = model.compute_posterior(200, 1);
        let decision = model.decide(&posterior);
        assert_eq!(
            decision,
            super::MigrationDecision::AutoApprove,
            "very high success rate should auto-approve"
        );
    }

    #[test]
    fn decision_low_confidence_rejects() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let posterior = model.compute_posterior(1, 20);
        let decision = model.decide(&posterior);
        assert!(
            matches!(
                decision,
                super::MigrationDecision::Reject
                    | super::MigrationDecision::HardReject
                    | super::MigrationDecision::Rollback
                    | super::MigrationDecision::ConservativeFallback
            ),
            "low confidence should reject, got {:?}",
            decision
        );
    }

    #[test]
    fn expected_loss_decision_includes_rationale() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let posterior = model.compute_posterior(30, 5);
        let result =
            model.expected_loss_decision(&posterior, Some("claim-1".into()), Some("pol-1".into()));
        assert!(!result.rationale.is_empty(), "rationale must not be empty");
        assert!(
            result.rationale.contains("posterior_mean"),
            "rationale must include posterior_mean"
        );
        assert!(
            result.rationale.contains("EL(accept)"),
            "rationale must include expected loss"
        );
        assert_eq!(result.claim_id.as_deref(), Some("claim-1"));
        assert_eq!(result.policy_id.as_deref(), Some("pol-1"));
    }

    #[test]
    fn expected_loss_decision_round_trip_serialization() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let posterior = model.compute_posterior(15, 3);
        let result = model.expected_loss_decision(&posterior, None, None);
        let serialized = serde_json::to_string_pretty(&result).expect("result should serialize");
        let deserialized: super::ExpectedLossResult =
            serde_json::from_str(&serialized).expect("should deserialize back");
        assert_eq!(result.decision, deserialized.decision);
        assert_eq!(result.rationale, deserialized.rationale);
    }

    #[test]
    fn invalid_confidence_model_rejects_empty_model_id() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.model_id = String::new();
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("model_id")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_unbalanced_weights() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        for source in &mut model.likelihood_sources {
            source.weight = 0.5;
        }
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("sum to")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_inverted_boundaries() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.decision_boundaries.auto_approve_threshold = 0.1;
        model.decision_boundaries.rollback_trigger = 0.9;
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(_)),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_negative_alpha() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.prior_config.semantic_pass_rate_alpha = -1.0;
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("alpha")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn confidence_model_round_trip_serialization_is_stable() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let serialized =
            serde_json::to_string_pretty(&model).expect("confidence model should serialize");
        let deserialized: super::ConfidenceModel =
            serde_json::from_str(&serialized).expect("should deserialize back");
        assert_eq!(model, deserialized, "round-trip must be stable");
    }

    // -----------------------------------------------------------------------
    // bd-3bxhj.1.6: Additional comprehensive tests
    // -----------------------------------------------------------------------

    // --- Malformed JSON rejection ---

    #[test]
    fn malformed_json_fails_gracefully_for_semantic_contract() {
        let result = super::SemanticEquivalenceContract::parse_and_validate("not json at all");
        assert!(
            matches!(result, Err(SemanticContractError::Parse(_))),
            "malformed JSON must produce Parse error, got: {:?}",
            result
        );
    }

    #[test]
    fn malformed_json_fails_gracefully_for_policy_matrix() {
        let result = super::TransformationPolicyMatrix::parse_and_validate("{truncated");
        assert!(
            matches!(result, Err(SemanticContractError::Parse(_))),
            "malformed JSON must produce Parse error"
        );
    }

    #[test]
    fn malformed_json_fails_gracefully_for_evidence_manifest() {
        let result = super::EvidenceManifest::parse_and_validate("[1,2,3]");
        assert!(
            matches!(result, Err(SemanticContractError::Parse(_))),
            "wrong JSON shape must produce Parse error"
        );
    }

    #[test]
    fn malformed_json_fails_gracefully_for_confidence_model() {
        let result = super::ConfidenceModel::parse_and_validate("");
        assert!(
            matches!(result, Err(SemanticContractError::Parse(_))),
            "empty string must produce Parse error"
        );
    }

    // --- Schema version rejection ---

    #[test]
    fn semantic_contract_rejects_wrong_schema_version() {
        let mut contract = load().expect("builtin contract should parse");
        contract.schema_version = "wrong-version-v99".to_string();
        let raw = serde_json::to_string(&contract).expect("should serialize");
        let error =
            super::SemanticEquivalenceContract::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("unsupported schema_version")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn policy_matrix_rejects_wrong_schema_version() {
        let mut matrix = load_policy().expect("builtin policy matrix should parse");
        matrix.schema_version = "wrong-version-v99".to_string();
        let raw = serde_json::to_string(&matrix).expect("should serialize");
        let error =
            super::TransformationPolicyMatrix::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("unsupported")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn evidence_manifest_rejects_wrong_schema_version() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.schema_version = "wrong-version-v99".to_string();
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("unsupported")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn confidence_model_rejects_wrong_schema_version() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.schema_version = "wrong-version-v99".to_string();
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("unsupported")),
            "unexpected error: {error}"
        );
    }

    // --- Semantic contract edge cases ---

    #[test]
    fn semantic_contract_rejects_empty_clauses() {
        let mut contract = load().expect("builtin contract should parse");
        contract.clauses.clear();
        let raw = serde_json::to_string(&contract).expect("should serialize");
        let error =
            super::SemanticEquivalenceContract::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("clauses must not be empty")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn semantic_contract_rejects_duplicate_clause_ids() {
        let mut contract = load().expect("builtin contract should parse");
        if contract.clauses.len() >= 2 {
            contract.clauses[1].clause_id = contract.clauses[0].clause_id.clone();
            let raw = serde_json::to_string(&contract).expect("should serialize");
            let error = super::SemanticEquivalenceContract::parse_and_validate(&raw)
                .expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("duplicate clause_id")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn semantic_contract_rejects_empty_tie_breakers() {
        let mut contract = load().expect("builtin contract should parse");
        contract.deterministic_tie_breakers.clear();
        let raw = serde_json::to_string(&contract).expect("should serialize");
        let error =
            super::SemanticEquivalenceContract::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("deterministic_tie_breakers")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn semantic_contract_rejects_duplicate_tie_break_priority() {
        let mut contract = load().expect("builtin contract should parse");
        if contract.deterministic_tie_breakers.len() >= 2 {
            contract.deterministic_tie_breakers[1].priority =
                contract.deterministic_tie_breakers[0].priority;
            let raw = serde_json::to_string(&contract).expect("should serialize");
            let error = super::SemanticEquivalenceContract::parse_and_validate(&raw)
                .expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("duplicate tie-break priority")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn semantic_contract_rejects_empty_contract_id() {
        let mut contract = load().expect("builtin contract should parse");
        contract.contract_id = String::new();
        let raw = serde_json::to_string(&contract).expect("should serialize");
        let error =
            super::SemanticEquivalenceContract::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("contract_id")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn semantic_contract_clause_lookup_returns_correct_clause() {
        let contract = load().expect("builtin contract should parse");
        for clause in &contract.clauses {
            let found = contract
                .clause(&clause.clause_id)
                .expect("clause must exist");
            assert_eq!(found.title, clause.title);
            assert_eq!(found.severity, clause.severity);
        }
    }

    #[test]
    fn semantic_contract_clauses_for_validator_returns_correct_count() {
        let contract = load().expect("builtin contract should parse");
        for (validator_id, clause_ids) in &contract.validator_clause_map {
            let clauses = contract.clauses_for_validator(validator_id);
            assert_eq!(
                clauses.len(),
                clause_ids.len(),
                "validator '{}' should map to {} clauses",
                validator_id,
                clause_ids.len()
            );
        }
    }

    // --- Policy matrix edge cases ---

    #[test]
    fn policy_matrix_rejects_empty_policy_id() {
        let mut matrix = load_policy().expect("builtin policy matrix should parse");
        matrix.policy_id = String::new();
        let raw = serde_json::to_string(&matrix).expect("should serialize");
        let error =
            super::TransformationPolicyMatrix::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("policy_id")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn policy_matrix_rejects_missing_required_category() {
        let mut matrix = load_policy().expect("builtin policy matrix should parse");
        matrix.categories.retain(|c| c.category_id != "state");
        // Also remove catalog entries and policy cells referencing "state"
        matrix
            .construct_catalog
            .retain(|e| e.category_id != "state");
        let state_sigs: std::collections::BTreeSet<_> = matrix
            .policy_cells
            .iter()
            .filter(|c| {
                matrix
                    .construct_catalog
                    .iter()
                    .all(|e| e.construct_signature != c.construct_signature)
            })
            .map(|c| c.construct_signature.clone())
            .collect();
        matrix
            .policy_cells
            .retain(|c| !state_sigs.contains(&c.construct_signature));
        let raw = serde_json::to_string(&matrix).expect("should serialize");
        let error =
            super::TransformationPolicyMatrix::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("required policy category")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn policy_for_construct_returns_correct_cell() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        for cell in &matrix.policy_cells {
            let found = matrix
                .policy_for_construct(&cell.construct_signature)
                .expect("construct must be found");
            assert_eq!(found.handling_class, cell.handling_class);
            assert_eq!(found.rationale, cell.rationale);
        }
    }

    #[test]
    fn policy_for_construct_returns_none_for_unknown() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        assert!(
            matrix
                .policy_for_construct("nonexistent_construct")
                .is_none(),
            "unknown construct should return None"
        );
    }

    #[test]
    fn planner_rows_are_sorted_alphabetically() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        let rows = matrix.planner_rows();
        for window in rows.windows(2) {
            assert!(
                window[0].construct_signature <= window[1].construct_signature,
                "planner rows must be sorted: '{}' > '{}'",
                window[0].construct_signature,
                window[1].construct_signature
            );
        }
    }

    #[test]
    fn certification_rows_have_clause_links_and_evidence() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        let rows = matrix.certification_rows();
        for row in &rows {
            assert!(
                !row.semantic_clause_links.is_empty(),
                "construct '{}' must have semantic clause links",
                row.construct_signature
            );
            assert!(
                !row.certification_evidence.is_empty(),
                "construct '{}' must have certification evidence",
                row.construct_signature
            );
        }
    }

    // --- Evidence manifest: no repo_url or local_path ---

    #[test]
    fn evidence_manifest_rejects_missing_source_location() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.source_fingerprint.repo_url = None;
        manifest.source_fingerprint.local_path = None;
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("repo_url or local_path")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn evidence_manifest_rejects_empty_parser_versions() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.source_fingerprint.parser_versions.clear();
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("parser_versions")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn evidence_manifest_rejects_duplicate_stage_ids() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        if manifest.stages.len() >= 2 {
            manifest.stages[1].stage_id = manifest.stages[0].stage_id.clone();
            manifest.stages[1].stage_index = 1; // Keep consecutive
            let raw = serde_json::to_string(&manifest).expect("should serialize");
            let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("duplicate stage_id")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn evidence_manifest_rejects_empty_code_hash() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.generated_code_fingerprint.code_hash = String::new();
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("code_hash")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn evidence_manifest_rejects_empty_stages() {
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest.stages.clear();
        let raw = serde_json::to_string(&manifest).expect("should serialize");
        let error = super::EvidenceManifest::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("stages must not be empty")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn runtime_contract_gate_executes_compiled_validators() {
        let contract = load().expect("builtin contract should parse");
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        let report = manifest
            .execute_runtime_contract_gate(&contract)
            .expect("runtime contract gate should pass");
        assert!(report.passed, "report should be fully passing");
        assert!(
            report.validator_results.iter().all(|result| result.passed),
            "all compiled validators should pass"
        );
        assert!(
            report.orphan_claim_ids.is_empty(),
            "no orphan claims should be present"
        );
    }

    #[test]
    fn runtime_contract_gate_rejects_orphan_coverage_claim() {
        let contract = load().expect("builtin contract should parse");
        let mut manifest = load_manifest().expect("builtin evidence manifest should parse");
        manifest
            .certification_verdict
            .semantic_clause_coverage
            .covered
            .push("UNKNOWN-CLAIM".to_string());

        let error = manifest
            .execute_runtime_contract_gate(&contract)
            .expect_err("orphan claim should fail runtime gate");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("orphan claims")),
            "unexpected error: {error}"
        );
    }

    // --- Confidence model boundary threshold tests ---

    #[test]
    fn decision_at_exact_auto_approve_boundary() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let threshold = model.decision_boundaries.auto_approve_threshold;
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 1.0,
            mean: threshold,
            variance: 0.01,
            credible_lower: threshold - 0.05,
            credible_upper: threshold + 0.05,
        };
        let decision = model.decide(&posterior);
        assert_eq!(
            decision,
            super::MigrationDecision::AutoApprove,
            "mean exactly at auto_approve threshold ({threshold}) should auto-approve"
        );
    }

    #[test]
    fn decision_just_below_auto_approve_boundary() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let threshold = model.decision_boundaries.auto_approve_threshold;
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 1.0,
            mean: threshold - 0.001,
            variance: 0.01,
            credible_lower: threshold - 0.1,
            credible_upper: threshold - 0.001,
        };
        let decision = model.decide(&posterior);
        assert_eq!(
            decision,
            super::MigrationDecision::HumanReview,
            "mean just below auto_approve should be human_review"
        );
    }

    #[test]
    fn decision_at_exact_human_review_lower_boundary() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let threshold = model.decision_boundaries.human_review_lower;
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 1.0,
            mean: threshold,
            variance: 0.01,
            credible_lower: threshold - 0.05,
            credible_upper: threshold + 0.05,
        };
        let decision = model.decide(&posterior);
        assert_eq!(
            decision,
            super::MigrationDecision::HumanReview,
            "mean at human_review_lower ({threshold}) should be human_review"
        );
    }

    #[test]
    fn decision_just_below_human_review_lower_boundary() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let threshold = model.decision_boundaries.human_review_lower;
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 1.0,
            mean: threshold - 0.001,
            variance: 0.01,
            credible_lower: threshold - 0.1,
            credible_upper: threshold - 0.001,
        };
        let decision = model.decide(&posterior);
        assert_eq!(
            decision,
            super::MigrationDecision::Reject,
            "mean just below human_review_lower should be reject"
        );
    }

    #[test]
    fn decision_at_exact_reject_threshold() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let threshold = model.decision_boundaries.reject_threshold;
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 1.0,
            mean: threshold,
            variance: 0.01,
            credible_lower: threshold - 0.05,
            credible_upper: threshold + 0.05,
        };
        let decision = model.decide(&posterior);
        assert_eq!(
            decision,
            super::MigrationDecision::Reject,
            "mean at reject_threshold ({threshold}) should be reject"
        );
    }

    #[test]
    fn decision_at_exact_hard_reject_threshold() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let threshold = model.decision_boundaries.hard_reject_threshold;
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 1.0,
            mean: threshold,
            variance: 0.01,
            credible_lower: threshold - 0.05,
            credible_upper: threshold + 0.05,
        };
        let decision = model.decide(&posterior);
        assert_eq!(
            decision,
            super::MigrationDecision::HardReject,
            "mean at hard_reject ({threshold}) should be hard_reject"
        );
    }

    #[test]
    fn decision_at_exact_rollback_trigger() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let threshold = model.decision_boundaries.rollback_trigger;
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 1.0,
            mean: threshold,
            variance: 0.01,
            credible_lower: 0.0,
            credible_upper: threshold + 0.05,
        };
        let decision = model.decide(&posterior);
        assert_eq!(
            decision,
            super::MigrationDecision::Rollback,
            "mean at rollback_trigger ({threshold}) should be rollback"
        );
    }

    #[test]
    fn decision_below_rollback_trigger_is_conservative_fallback() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 1.0,
            mean: 0.001,
            variance: 0.0001,
            credible_lower: 0.0,
            credible_upper: 0.01,
        };
        let decision = model.decide(&posterior);
        assert_eq!(
            decision,
            super::MigrationDecision::ConservativeFallback,
            "mean far below rollback_trigger should be conservative_fallback"
        );
    }

    // --- Confidence model: expected loss boundary override ---

    #[test]
    fn expected_loss_respects_conservative_fallback_override() {
        let model = load_confidence().expect("builtin confidence model should parse");
        // Very low posterior mean => boundary says ConservativeFallback, EL might prefer something else
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 100.0,
            mean: 0.01,
            variance: 0.0001,
            credible_lower: 0.0,
            credible_upper: 0.03,
        };
        let result = model.expected_loss_decision(&posterior, None, None);
        assert_eq!(
            result.decision,
            super::MigrationDecision::ConservativeFallback,
            "conservative_fallback boundary override must take precedence"
        );
    }

    #[test]
    fn expected_loss_respects_hard_reject_override() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let db = &model.decision_boundaries;
        // Mean in hard_reject zone
        let mean = (db.hard_reject_threshold + db.reject_threshold) / 2.0;
        let posterior = super::BayesianPosterior {
            alpha: 1.0,
            beta: 1.0,
            mean,
            variance: 0.01,
            credible_lower: mean - 0.05,
            credible_upper: mean + 0.05,
        };
        let result = model.expected_loss_decision(&posterior, None, None);
        assert_eq!(
            result.decision,
            super::MigrationDecision::HardReject,
            "hard_reject boundary override must take precedence"
        );
    }

    // --- Determinism checks ---

    #[test]
    fn posterior_computation_is_deterministic_across_calls() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let results: Vec<_> = (0..10).map(|_| model.compute_posterior(42, 8)).collect();
        for (i, result) in results.iter().enumerate().skip(1) {
            assert_eq!(
                results[0], *result,
                "posterior computation must be deterministic (mismatch at iteration {i})"
            );
        }
    }

    #[test]
    fn decision_is_deterministic_across_calls() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let posterior = model.compute_posterior(42, 8);
        let decisions: Vec<_> = (0..10).map(|_| model.decide(&posterior)).collect();
        for (i, decision) in decisions.iter().enumerate().skip(1) {
            assert_eq!(
                decisions[0], *decision,
                "decision must be deterministic (mismatch at iteration {i})"
            );
        }
    }

    #[test]
    fn expected_loss_decision_is_deterministic_across_calls() {
        let model = load_confidence().expect("builtin confidence model should parse");
        let posterior = model.compute_posterior(42, 8);
        let results: Vec<_> = (0..10)
            .map(|_| model.expected_loss_decision(&posterior, None, None))
            .collect();
        for (i, result) in results.iter().enumerate().skip(1) {
            assert_eq!(
                results[0].decision, result.decision,
                "expected_loss_decision must be deterministic (mismatch at iteration {i})"
            );
            assert_eq!(results[0].rationale, result.rationale);
        }
    }

    #[test]
    fn evidence_manifest_lineage_is_deterministic() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        let lineage_a = manifest.stage_lineage();
        let lineage_b = manifest.stage_lineage();
        assert_eq!(lineage_a, lineage_b, "lineage must be deterministic");
    }

    #[test]
    fn evidence_manifest_jsonl_is_deterministic() {
        let manifest = load_manifest().expect("builtin evidence manifest should parse");
        let jsonl_a = manifest.evidence_jsonl();
        let jsonl_b = manifest.evidence_jsonl();
        assert_eq!(jsonl_a, jsonl_b, "JSONL output must be deterministic");
    }

    #[test]
    fn planner_rows_are_deterministic() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        let rows_a = matrix.planner_rows();
        let rows_b = matrix.planner_rows();
        assert_eq!(rows_a, rows_b, "planner rows must be deterministic");
    }

    #[test]
    fn certification_rows_are_deterministic() {
        let matrix = load_policy().expect("builtin policy matrix should parse");
        let rows_a = matrix.certification_rows();
        let rows_b = matrix.certification_rows();
        assert_eq!(rows_a, rows_b, "certification rows must be deterministic");
    }

    // --- Confidence model: additional validation edge cases ---

    #[test]
    fn invalid_confidence_model_rejects_negative_beta() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.prior_config.semantic_pass_rate_beta = -1.0;
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("alpha and beta must be > 0")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_zero_performance_variance() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.prior_config.performance_variance_prior = 0.0;
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("performance_variance_prior")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_negative_penalty() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.prior_config.unsupported_feature_penalty_per_item = -1.0;
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("unsupported_feature_penalty_per_item")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_empty_likelihood_sources() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.likelihood_sources.clear();
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("likelihood_sources must not be empty")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_duplicate_source_id() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        if model.likelihood_sources.len() >= 2 {
            model.likelihood_sources[1].source_id = model.likelihood_sources[0].source_id.clone();
            let raw = serde_json::to_string(&model).expect("should serialize");
            let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("duplicate likelihood source_id")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn invalid_confidence_model_rejects_weight_out_of_range() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        if !model.likelihood_sources.is_empty() {
            model.likelihood_sources[0].weight = 1.5;
            let raw = serde_json::to_string(&model).expect("should serialize");
            let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("weight must be in")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn invalid_confidence_model_rejects_boundary_out_of_range() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.decision_boundaries.auto_approve_threshold = 1.5;
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("must be in [0.0, 1.0]")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_empty_decision_actions() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.decision_space.actions.clear();
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("actions must not be empty")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_missing_required_action() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.decision_space.actions.retain(|a| a != "reject");
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("reject")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_zero_calibration_samples() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.calibration.min_calibration_samples = 0;
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("min_calibration_samples")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_zero_recalibration_drift() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.calibration.recalibration_trigger_drift = 0.0;
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("recalibration_trigger_drift")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_confidence_model_rejects_duplicate_fallback_trigger() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        if model.fallback_triggers.len() >= 2 {
            model.fallback_triggers[1].trigger_id = model.fallback_triggers[0].trigger_id.clone();
            let raw = serde_json::to_string(&model).expect("should serialize");
            let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("duplicate fallback trigger_id")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn invalid_confidence_model_rejects_invalid_fallback_action() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        if !model.fallback_triggers.is_empty() {
            model.fallback_triggers[0].action = "invalid_action".to_string();
            let raw = serde_json::to_string(&model).expect("should serialize");
            let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("not in decision space")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn invalid_confidence_model_rejects_accept_incorrect_zero() {
        let mut model = load_confidence().expect("builtin confidence model should parse");
        model.loss_matrix.accept_incorrect = 0.0;
        let raw = serde_json::to_string(&model).expect("should serialize");
        let error = super::ConfidenceModel::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("accept_incorrect must be > 0")),
            "unexpected error: {error}"
        );
    }

    // --- Cross-contract consistency ---

    #[test]
    fn all_builtin_contracts_parse_and_validate_consistently() {
        let contract = load().expect("semantic contract should parse");
        let matrix = load_policy().expect("policy matrix should parse");
        let manifest = load_manifest().expect("evidence manifest should parse");
        let model = load_confidence().expect("confidence model should parse");

        // Verify they all have non-empty identifiers
        assert!(!contract.contract_id.is_empty());
        assert!(!matrix.policy_id.is_empty());
        assert!(!manifest.manifest_id.is_empty());
        assert!(!model.model_id.is_empty());

        // Policy matrix clause links must reference valid semantic contract clauses
        let clause_ids: std::collections::BTreeSet<_> = contract
            .clauses
            .iter()
            .map(|c| c.clause_id.as_str())
            .collect();
        for cell in &matrix.policy_cells {
            for clause_ref in &cell.semantic_clause_links {
                assert!(
                    clause_ids.contains(clause_ref.as_str()),
                    "policy cell '{}' references clause '{}' which doesn't exist in semantic contract",
                    cell.construct_signature,
                    clause_ref
                );
            }
        }
    }

    #[test]
    fn evidence_manifest_covered_clauses_match_semantic_contract() {
        let contract = load().expect("semantic contract should parse");
        let manifest = load_manifest().expect("evidence manifest should parse");

        let clause_ids: std::collections::BTreeSet<_> = contract
            .clauses
            .iter()
            .map(|c| c.clause_id.as_str())
            .collect();
        let coverage = &manifest.certification_verdict.semantic_clause_coverage;
        for covered in &coverage.covered {
            assert!(
                clause_ids.contains(covered.as_str()),
                "covered clause '{}' must exist in semantic contract",
                covered
            );
        }
        for uncovered in &coverage.uncovered {
            assert!(
                clause_ids.contains(uncovered.as_str()),
                "uncovered clause '{}' must exist in semantic contract",
                uncovered
            );
        }
    }

    // -----------------------------------------------------------------------
    // Licensing & Provenance Guardrails tests
    // -----------------------------------------------------------------------

    fn load_licensing() -> Result<super::LicensingProvenanceContract> {
        super::load_builtin_licensing_provenance()
    }

    /// A chain covering every required stage with unbroken hash links, so
    /// artifact-status tests exercise the artifacts rather than the chain.
    fn valid_chain(
        contract: &super::LicensingProvenanceContract,
    ) -> Vec<super::ProvenanceChainRecord> {
        contract
            .provenance_chain_policy
            .required_stages
            .iter()
            .enumerate()
            .map(|(i, stage_id)| super::ProvenanceChainRecord {
                stage_id: stage_id.clone(),
                input_hash: format!("sha256:hash_{i}"),
                output_hash: format!("sha256:hash_{}", i + 1),
                tool_version: "1.0.0".to_string(),
                timestamp: "2026-02-25T12:00:00Z".to_string(),
            })
            .collect()
    }

    #[test]
    fn builtin_licensing_provenance_parses_and_validates() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        assert_eq!(
            contract.schema_version,
            super::SUPPORTED_LICENSING_PROVENANCE_SCHEMA_VERSION
        );
        assert!(!contract.contract_id.is_empty());
    }

    #[test]
    fn licensing_policy_has_allowed_and_blocked_classes() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        assert!(
            !contract.licensing_policy.allowed_license_classes.is_empty(),
            "must have allowed license classes"
        );
        assert!(
            !contract.licensing_policy.blocked_license_classes.is_empty(),
            "must have blocked license classes"
        );
    }

    #[test]
    fn licensing_allowed_and_blocked_are_disjoint() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let allowed: std::collections::BTreeSet<_> = contract
            .licensing_policy
            .allowed_license_classes
            .iter()
            .collect();
        let blocked: std::collections::BTreeSet<_> = contract
            .licensing_policy
            .blocked_license_classes
            .iter()
            .collect();
        let overlap: Vec<_> = allowed.intersection(&blocked).collect();
        assert!(
            overlap.is_empty(),
            "allowed and blocked classes must be disjoint, overlap: {:?}",
            overlap
        );
    }

    #[test]
    fn licensing_class_definitions_cover_all_references() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let defined: std::collections::BTreeSet<_> = contract
            .licensing_policy
            .license_class_definitions
            .iter()
            .map(|d| d.class_id.as_str())
            .collect();
        for class in contract
            .licensing_policy
            .allowed_license_classes
            .iter()
            .chain(contract.licensing_policy.blocked_license_classes.iter())
        {
            assert!(
                defined.contains(class.as_str()),
                "class '{}' must have a definition",
                class
            );
        }
    }

    #[test]
    fn licensing_class_lookup_works() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        for def in &contract.licensing_policy.license_class_definitions {
            let found = contract
                .license_class(&def.class_id)
                .expect("class must be found");
            assert_eq!(found.description, def.description);
        }
        assert!(
            contract.license_class("nonexistent-class").is_none(),
            "unknown class must return None"
        );
    }

    #[test]
    fn licensing_is_allowed_and_blocked_are_correct() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        for class in &contract.licensing_policy.allowed_license_classes {
            assert!(
                contract.is_license_allowed(class),
                "class '{}' should be allowed",
                class
            );
            assert!(
                !contract.is_license_blocked(class),
                "class '{}' should not be blocked",
                class
            );
        }
        for class in &contract.licensing_policy.blocked_license_classes {
            assert!(
                contract.is_license_blocked(class),
                "class '{}' should be blocked",
                class
            );
            assert!(
                !contract.is_license_allowed(class),
                "class '{}' should not be allowed",
                class
            );
        }
    }

    #[test]
    fn provenance_chain_policy_has_required_stages() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        assert!(
            !contract.provenance_chain_policy.required_stages.is_empty(),
            "required_stages must not be empty"
        );
        assert!(
            contract.provenance_chain_policy.chain_must_be_unbroken,
            "chain_must_be_unbroken should be true for safety"
        );
    }

    #[test]
    fn provenance_chain_policy_requires_essential_recording_fields() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let fields = &contract.provenance_chain_policy.each_stage_must_record;
        for required in ["input_hash", "output_hash", "tool_version", "timestamp"] {
            assert!(
                fields.iter().any(|f| f == required),
                "each_stage_must_record missing '{}'",
                required
            );
        }
    }

    #[test]
    fn fail_safe_defaults_reject_critical_issues() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let fsd = &contract.fail_safe_defaults;
        assert_eq!(
            fsd.on_missing_provenance, "reject",
            "missing provenance must default to reject"
        );
        assert_eq!(
            fsd.on_broken_chain, "reject",
            "broken chain must default to reject"
        );
        assert_eq!(
            fsd.on_blocked_license, "reject",
            "blocked license must default to reject"
        );
    }

    #[test]
    fn fail_safe_action_returns_correct_action() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        assert_eq!(
            contract.fail_safe_action(super::IpArtifactStatus::Clear),
            super::ProvenanceAction::Accept
        );
        assert_eq!(
            contract.fail_safe_action(super::IpArtifactStatus::Blocked),
            super::ProvenanceAction::Reject
        );
        assert_eq!(
            contract.fail_safe_action(super::IpArtifactStatus::Unknown),
            super::ProvenanceAction::Hold
        );
        assert_eq!(
            contract.fail_safe_action(super::IpArtifactStatus::NeedsCounsel),
            super::ProvenanceAction::Hold
        );
    }

    #[test]
    fn attribution_template_has_required_fields() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        assert!(
            !contract.attribution_template.required_fields.is_empty(),
            "attribution template must have required fields"
        );
        assert!(
            !contract.attribution_template.format.is_empty(),
            "attribution template must have a format"
        );
    }

    #[test]
    fn risk_flags_have_unique_ids_and_valid_severities() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let mut seen = std::collections::BTreeSet::new();
        for flag in &contract.risk_flags {
            assert!(
                seen.insert(&flag.flag_id),
                "duplicate risk flag_id '{}'",
                flag.flag_id
            );
            assert!(
                ["low", "medium", "high", "critical"].contains(&flag.severity.as_str()),
                "flag '{}' has invalid severity '{}'",
                flag.flag_id,
                flag.severity
            );
        }
    }

    #[test]
    fn validate_provenance_chain_accepts_valid_chain() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let chain = contract
            .provenance_chain_policy
            .required_stages
            .iter()
            .enumerate()
            .map(|(i, stage_id)| super::ProvenanceChainRecord {
                stage_id: stage_id.clone(),
                input_hash: format!("sha256:hash_{i}"),
                output_hash: format!("sha256:hash_{}", i + 1),
                tool_version: "1.0.0".to_string(),
                timestamp: "2026-02-25T12:00:00Z".to_string(),
            })
            .collect::<Vec<_>>();
        contract
            .validate_provenance_chain(&chain)
            .expect("valid chain should pass");
    }

    #[test]
    fn validate_provenance_chain_rejects_missing_stage() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        // Chain missing the first required stage
        let chain = contract
            .provenance_chain_policy
            .required_stages
            .iter()
            .skip(1) // Skip first required stage
            .enumerate()
            .map(|(i, stage_id)| super::ProvenanceChainRecord {
                stage_id: stage_id.clone(),
                input_hash: format!("sha256:hash_{i}"),
                output_hash: format!("sha256:hash_{}", i + 1),
                tool_version: "1.0.0".to_string(),
                timestamp: "2026-02-25T12:00:00Z".to_string(),
            })
            .collect::<Vec<_>>();
        let error = contract
            .validate_provenance_chain(&chain)
            .expect_err("missing stage should fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("missing required stage")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn validate_provenance_chain_rejects_broken_hash_link() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let mut chain: Vec<super::ProvenanceChainRecord> = contract
            .provenance_chain_policy
            .required_stages
            .iter()
            .enumerate()
            .map(|(i, stage_id)| super::ProvenanceChainRecord {
                stage_id: stage_id.clone(),
                input_hash: format!("sha256:hash_{i}"),
                output_hash: format!("sha256:hash_{}", i + 1),
                tool_version: "1.0.0".to_string(),
                timestamp: "2026-02-25T12:00:00Z".to_string(),
            })
            .collect();
        if chain.len() >= 2 {
            chain[1].input_hash = "sha256:BROKEN".to_string();
            let error = contract
                .validate_provenance_chain(&chain)
                .expect_err("broken chain should fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("chain broken")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn assess_ip_artifacts_clear_status_for_all_clear() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let artifacts = vec![super::IpArtifactRecord {
            artifact_id: "dep-1".to_string(),
            license_spdx: Some("MIT".to_string()),
            license_class: "permissive".to_string(),
            status: super::IpArtifactStatus::Clear,
            risk_flags: vec![],
            design_around_notes: None,
        }];
        let report = contract.assess_ip_artifacts("run-1", &valid_chain(&contract), &artifacts);
        assert_eq!(report.overall_status, super::IpArtifactStatus::Clear);
        assert!(report.unresolved_risk_flags.is_empty());
    }

    #[test]
    fn assess_ip_artifacts_blocked_status_propagates() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let artifacts = vec![
            super::IpArtifactRecord {
                artifact_id: "dep-clear".to_string(),
                license_spdx: Some("MIT".to_string()),
                license_class: "permissive".to_string(),
                status: super::IpArtifactStatus::Clear,
                risk_flags: vec![],
                design_around_notes: None,
            },
            super::IpArtifactRecord {
                artifact_id: "dep-blocked".to_string(),
                license_spdx: Some("GPL-3.0".to_string()),
                license_class: "strong_copyleft".to_string(),
                status: super::IpArtifactStatus::Blocked,
                risk_flags: vec!["lp-copyleft-contamination".to_string()],
                design_around_notes: Some("Consider removing this dependency".to_string()),
            },
        ];
        let report = contract.assess_ip_artifacts("run-1", &valid_chain(&contract), &artifacts);
        assert_eq!(report.overall_status, super::IpArtifactStatus::Blocked);
        assert!(
            report
                .unresolved_risk_flags
                .contains(&"lp-copyleft-contamination".to_string())
        );
    }

    #[test]
    fn assess_ip_artifacts_unknown_status_propagates() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        // Use a non-blocked license class with Unknown status to test status propagation
        let artifacts = vec![super::IpArtifactRecord {
            artifact_id: "dep-unknown".to_string(),
            license_spdx: None,
            license_class: "permissive".to_string(),
            status: super::IpArtifactStatus::Unknown,
            risk_flags: vec!["lp-no-license-detected".to_string()],
            design_around_notes: None,
        }];
        let report = contract.assess_ip_artifacts("run-1", &valid_chain(&contract), &artifacts);
        assert_eq!(report.overall_status, super::IpArtifactStatus::Unknown);
    }

    #[test]
    fn assess_ip_artifacts_blocked_class_overrides_clear_status() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        // Even with Clear status, a blocked license class should propagate to Blocked
        let artifacts = vec![super::IpArtifactRecord {
            artifact_id: "dep-gpl".to_string(),
            license_spdx: Some("GPL-3.0".to_string()),
            license_class: "unknown".to_string(), // "unknown" is a blocked class
            status: super::IpArtifactStatus::Clear,
            risk_flags: vec![],
            design_around_notes: None,
        }];
        let report = contract.assess_ip_artifacts("run-1", &valid_chain(&contract), &artifacts);
        assert_eq!(report.overall_status, super::IpArtifactStatus::Blocked);
    }

    /// `fail_safe_defaults.on_missing_provenance` and `on_broken_chain` must
    /// be "reject" - `validate_fail_safe_defaults` refuses anything else - and
    /// `lp-provenance-gap` is the flag for it, yet `assess_ip_artifacts` never
    /// looked at the chain: an empty or broken one with clean artifacts came
    /// out `Clear`, which passes the certification compliance stage.
    #[test]
    fn assess_ip_artifacts_rejects_a_missing_or_invalid_chain() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        assert_eq!(contract.fail_safe_defaults.on_missing_provenance, "reject");
        assert_eq!(contract.fail_safe_defaults.on_broken_chain, "reject");
        let clean = vec![super::IpArtifactRecord {
            artifact_id: "dep-1".to_string(),
            license_spdx: Some("MIT".to_string()),
            license_class: "permissive".to_string(),
            status: super::IpArtifactStatus::Clear,
            risk_flags: vec![],
            design_around_notes: None,
        }];

        let mut broken_link = valid_chain(&contract);
        broken_link[2].input_hash = "sha256:not-the-previous-output".to_string();
        let mut missing_stage = valid_chain(&contract);
        missing_stage.remove(0);
        let mut unversioned = valid_chain(&contract);
        unversioned[1].tool_version = "  ".to_string();

        for (label, chain) in [
            ("empty chain", Vec::new()),
            ("broken hash link", broken_link),
            ("missing required stage", missing_stage),
            ("record without tool version", unversioned),
        ] {
            let report = contract.assess_ip_artifacts("run-1", &chain, &clean);
            assert_eq!(
                report.overall_status,
                super::IpArtifactStatus::Blocked,
                "{label}"
            );
            assert_eq!(
                report.unresolved_risk_flags,
                vec![super::PROVENANCE_GAP_FLAG.to_string()],
                "{label}"
            );
            assert_eq!(
                contract.fail_safe_action(report.overall_status),
                super::ProvenanceAction::Reject,
                "{label}"
            );
        }
        assert!(
            contract
                .risk_flags
                .iter()
                .any(|flag| flag.flag_id == super::PROVENANCE_GAP_FLAG),
            "the flag raised must be one the contract defines"
        );

        // And a valid chain is not flagged.
        let report = contract.assess_ip_artifacts("run-1", &valid_chain(&contract), &clean);
        assert_eq!(report.overall_status, super::IpArtifactStatus::Clear);
        assert!(report.unresolved_risk_flags.is_empty());
    }

    #[test]
    fn licensing_provenance_round_trip_serialization_is_stable() {
        let contract = load_licensing().expect("builtin licensing provenance should parse");
        let serialized = serde_json::to_string_pretty(&contract).expect("should serialize");
        let deserialized: super::LicensingProvenanceContract =
            serde_json::from_str(&serialized).expect("should deserialize");
        assert_eq!(contract, deserialized, "round-trip must be stable");
    }

    #[test]
    fn invalid_licensing_rejects_wrong_schema_version() {
        let mut contract = load_licensing().expect("builtin licensing provenance should parse");
        contract.schema_version = "wrong-version".to_string();
        let raw = serde_json::to_string(&contract).expect("should serialize");
        let error =
            super::LicensingProvenanceContract::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("unsupported")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_licensing_rejects_empty_contract_id() {
        let mut contract = load_licensing().expect("builtin licensing provenance should parse");
        contract.contract_id = String::new();
        let raw = serde_json::to_string(&contract).expect("should serialize");
        let error =
            super::LicensingProvenanceContract::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("contract_id")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_licensing_rejects_overlapping_allowed_blocked() {
        let mut contract = load_licensing().expect("builtin licensing provenance should parse");
        contract
            .licensing_policy
            .allowed_license_classes
            .push("strong_copyleft".to_string());
        let raw = serde_json::to_string(&contract).expect("should serialize");
        let error =
            super::LicensingProvenanceContract::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("both allowed and blocked")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_licensing_rejects_non_reject_for_blocked_license() {
        let mut contract = load_licensing().expect("builtin licensing provenance should parse");
        contract.fail_safe_defaults.on_blocked_license = "accept".to_string();
        let raw = serde_json::to_string(&contract).expect("should serialize");
        let error =
            super::LicensingProvenanceContract::parse_and_validate(&raw).expect_err("must fail");
        assert!(
            matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("on_blocked_license")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_licensing_rejects_duplicate_risk_flag_id() {
        let mut contract = load_licensing().expect("builtin licensing provenance should parse");
        if contract.risk_flags.len() >= 2 {
            contract.risk_flags[1].flag_id = contract.risk_flags[0].flag_id.clone();
            let raw = serde_json::to_string(&contract).expect("should serialize");
            let error = super::LicensingProvenanceContract::parse_and_validate(&raw)
                .expect_err("must fail");
            assert!(
                matches!(error, SemanticContractError::Validation(ref msg) if msg.contains("duplicate risk flag_id")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn licensing_provenance_determinism_check() {
        let c1 = load_licensing().expect("should parse");
        let c2 = load_licensing().expect("should parse");
        assert_eq!(c1, c2, "repeated parsing must be deterministic");
    }
}

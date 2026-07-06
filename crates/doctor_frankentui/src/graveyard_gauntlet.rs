//! E2E graveyard-executable gauntlet (bd-3bxhj.10.37): the full
//! route -> rank -> contract -> verify -> demo -> release chain driven across
//! green and red campaigns with machine-checkable failure clauses.
//!
//! Eleven scenarios span the five mandated families:
//!
//! - green-path promotion: valid routing, an accepted composition, complete
//!   metadata, a passing verify gate, a reproducible demo, and a clean
//!   release decision;
//! - metadata/contract faults: a missing mandatory header field, an
//!   artifact-incomplete contract (linter block), and a lint-passing but
//!   risk-inconsistent contract (verify block);
//! - composition risk failures: an unmitigated dangerous combination and a
//!   multi-controller plan without interference evidence;
//! - demo/execution failures: golden-output checksum drift and a
//!   demo/claim-linkage break between the recommendation contract and the
//!   demo contract;
//! - optimization policy failures: a multi-lever change without override +
//!   waiver, an incomplete rollback plan, and a release-policy emergency
//!   hold.
//!
//! Every ledger line always carries the AC3-mandated fields: `stage_id`,
//! `route_id`, `ranking_hash`, `contract_id`, `verify_verdict`, `demo_id`,
//! `release_policy_verdict`, and `reproduction_command` (`n/a` sentinels
//! before a stage computes a value — never empty). Red paths terminate at
//! their expected stage with an explicit `violated_clause` plus a
//! failure-signature triage mapping (`triage_cluster` + `triage_hint`)
//! naming the likely root-cause cluster (AC2 + triage deliverable). The
//! ledger is float-free, derives `Eq`, and replays byte-identically (AC1;
//! proven by proptest). The pipeline materializes the JSONL evidence bundle
//! with per-artifact SHA-256 integrity, and the CLI supports one-command
//! replay per scenario.
//!
//! Precedents: `formal_assurance_gauntlet` (bd-3bxhj.10.28) and
//! `optimization_gauntlet` (bd-3bxhj.8.21). Kernels driven for real:
//! `symptom_router`, `entry_header_compiler`, `recommendation_contract`
//! linter, `composition_matrix`, `graveyardctl`, `killer_demo` contract
//! layer, `one_lever_policy`, and `rollout_gate_tests` release evaluation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adversarial_fixtures::RiskClass;
use crate::composition_matrix::{
    CompositionGate, CompositionPlan, ControllerRole, HazardSeverity, canonical_matrix,
    canonical_registry,
};
use crate::decision_loss_policy::RiskTier;
use crate::ecosystem_scan::UpliftCandidate;
use crate::entry_header_compiler::{
    CriterionStatus, EntryHeaderCompiler, EntryHeaderDraft, GraduationCriteriaTemplate,
    HeaderFieldId, SchemaSeverity, SizeEstimate, generate_graduation_template,
};
use crate::graveyardctl::{ActiveEntry, GraveyardctlEngine, GraveyardctlStage, VerifyResult};
use crate::killer_demo::{
    DemoContract, ExpectedOutput, KILLER_DEMO_SCHEMA_VERSION, ReplayScenario, parse_demo_yaml,
    render_demo_yaml,
};
use crate::milestone_policy::QualityBar;
use crate::one_lever_policy::{OptimizationChange, RollbackPlan, run_one_lever_policy};
use crate::recommendation_contract::{
    ContractLintConfig, ContractSeverity, EffortSize, RecommendationContract,
    RecommendationContractLinter, example_complete_contract,
};
use crate::rollout_gate_tests::{
    FeedbackCorpus, ReadinessSpec, ReleaseSpec, ReleaseTrafficLight, RolloutGateFixture,
    evaluate_fixture,
};
use crate::semantic_contract::IpArtifactStatus;
use crate::symptom_router::{
    MigrationHotspot, PolicyException, SymptomClass, SymptomRouter, canonical_corpus,
};

/// Schema version for the graveyard-gauntlet report/ledger.
pub const GRAVEYARD_GAUNTLET_SCHEMA_VERSION: &str = "graveyard-gauntlet-v1";

/// Schema version for the materialized pipeline manifest.
pub const GRAVEYARD_GAUNTLET_PIPELINE_SCHEMA_VERSION: &str = "graveyard-gauntlet-pipeline-v1";

// ── Local helpers ────────────────────────────────────────────────────────────

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

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// The six stages of the graveyard-executable chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainStage {
    /// Symptom routing over the canonical corpus.
    Route,
    /// Fast-Start ranking / candidate-pool hashing.
    Rank,
    /// Entry-header compile + contract lint + composition gate.
    Contract,
    /// graveyardctl verify classification.
    Verify,
    /// Demo-contract round-trip, claim linkage, and output reproducibility.
    Demo,
    /// One-lever policy + release-gate decision.
    Release,
}

impl ChainStage {
    /// All stages, in chain order.
    pub const ALL: [ChainStage; 6] = [
        ChainStage::Route,
        ChainStage::Rank,
        ChainStage::Contract,
        ChainStage::Verify,
        ChainStage::Demo,
        ChainStage::Release,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ChainStage::Route => "route",
            ChainStage::Rank => "rank",
            ChainStage::Contract => "contract",
            ChainStage::Verify => "verify",
            ChainStage::Demo => "demo",
            ChainStage::Release => "release",
        }
    }

    /// Position in the chain.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            ChainStage::Route => 0,
            ChainStage::Rank => 1,
            ChainStage::Contract => 2,
            ChainStage::Verify => 3,
            ChainStage::Demo => 4,
            ChainStage::Release => 5,
        }
    }
}

/// The five mandated scenario families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GauntletFamily {
    /// Green-path promotion through all six stages.
    GreenPromotion,
    /// Missing/malformed metadata and contract clauses.
    MetadataContractFault,
    /// Unsafe primitive combinations and missing countermeasures.
    CompositionRisk,
    /// Demo mismatch, claim-linkage breaks, non-reproducible outputs.
    DemoExecutionFault,
    /// Unsafe lever combinations, rollback-clause violations, release-policy
    /// holds.
    OptimizationPolicyFault,
}

impl GauntletFamily {
    /// All families.
    pub const ALL: [GauntletFamily; 5] = [
        GauntletFamily::GreenPromotion,
        GauntletFamily::MetadataContractFault,
        GauntletFamily::CompositionRisk,
        GauntletFamily::DemoExecutionFault,
        GauntletFamily::OptimizationPolicyFault,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            GauntletFamily::GreenPromotion => "green_promotion",
            GauntletFamily::MetadataContractFault => "metadata_contract_fault",
            GauntletFamily::CompositionRisk => "composition_risk",
            GauntletFamily::DemoExecutionFault => "demo_execution_fault",
            GauntletFamily::OptimizationPolicyFault => "optimization_policy_fault",
        }
    }
}

/// Root-cause clusters for the failure-signature triage map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageCluster {
    /// No failure.
    None,
    /// Entry-header schema gaps (missing/blank/unreasoned fields).
    MetadataGap,
    /// Missing contract artifacts (repro, provenance, budgets, safe-mode).
    ContractArtifactGap,
    /// High-severity risk posture (legal/IP + primary failure mode).
    RiskPosture,
    /// Dangerous combination / controller-interference hazards.
    CompositionHazard,
    /// Non-reproducible demo outputs (checksum drift, yaml drift).
    DemoReproducibility,
    /// Contract-to-demo linkage drift (stale demo/claim references).
    LinkageDrift,
    /// One-lever / rollback governance violations.
    LeverGovernance,
    /// Administrative release-policy hold.
    ReleasePolicyHold,
}

impl TriageCluster {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TriageCluster::None => "none",
            TriageCluster::MetadataGap => "metadata_gap",
            TriageCluster::ContractArtifactGap => "contract_artifact_gap",
            TriageCluster::RiskPosture => "risk_posture",
            TriageCluster::CompositionHazard => "composition_hazard",
            TriageCluster::DemoReproducibility => "demo_reproducibility",
            TriageCluster::LinkageDrift => "linkage_drift",
            TriageCluster::LeverGovernance => "lever_governance",
            TriageCluster::ReleasePolicyHold => "release_policy_hold",
        }
    }
}

/// Map a failure signature (stage + violated clause) to its likely
/// root-cause cluster and a remediation hint. Green (no clause) maps to
/// `None` with an empty hint.
#[must_use]
pub fn triage_for(stage: ChainStage, violated_clause: &str) -> (TriageCluster, String) {
    if violated_clause == "none" {
        return (TriageCluster::None, String::new());
    }
    match stage {
        ChainStage::Contract => {
            if violated_clause.starts_with("RC-") {
                (
                    TriageCluster::ContractArtifactGap,
                    "attach the missing contract artifacts (repro pack, provenance, budget \
                     semantics, metadata) and re-run the linter"
                        .to_string(),
                )
            } else if violated_clause.contains("combination")
                || violated_clause.contains("controller")
                || violated_clause.contains("composition")
                || violated_clause.contains("conflict")
                || violated_clause.contains("primitive")
            {
                (
                    TriageCluster::CompositionHazard,
                    "apply the registry-required mitigations verbatim or split controllers \
                     into fast/slow roles with interference artifacts"
                        .to_string(),
                )
            } else {
                (
                    TriageCluster::MetadataGap,
                    "complete the entry-header schema (provide every mandatory field with a \
                     non-blank value or a reasoned Tbd) before resubmitting"
                        .to_string(),
                )
            }
        }
        ChainStage::Verify => (
            TriageCluster::RiskPosture,
            "resolve the high-severity legal/IP posture (or downgrade the primary failure \
             mode) before re-running graveyard verify"
                .to_string(),
        ),
        ChainStage::Demo => {
            if violated_clause == "claim-linkage-mismatch" {
                (
                    TriageCluster::LinkageDrift,
                    "realign the recommendation contract's demo_linkage {demo_id, claim_id} \
                     with the demo contract"
                        .to_string(),
                )
            } else {
                (
                    TriageCluster::DemoReproducibility,
                    "re-materialize the golden pack and diff the drifted artifacts; demo \
                     outputs must replay checksum-identical"
                        .to_string(),
                )
            }
        }
        ChainStage::Release => {
            if violated_clause == "release-policy-hold" || violated_clause.contains("hold") {
                (
                    TriageCluster::ReleasePolicyHold,
                    "release is administratively held; clear the emergency hold and re-run \
                     the release gate"
                        .to_string(),
                )
            } else {
                (
                    TriageCluster::LeverGovernance,
                    "split the change to a single lever (or attach a valid policy override + \
                     risk waiver) and complete the rollback plan (revert command + \
                     post-rollback validations)"
                        .to_string(),
                )
            }
        }
        ChainStage::Route | ChainStage::Rank => (
            TriageCluster::MetadataGap,
            "inspect the canonical corpus metadata feeding the router".to_string(),
        ),
    }
}

/// The eleven gauntlet scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GauntletScenario {
    /// Full green chain: promoted through all six stages.
    GreenPromotion,
    /// Entry header missing a mandatory field.
    MalformedHeader,
    /// Artifact-incomplete contract (linter block).
    IncompleteContract,
    /// Lint-passing contract whose risk posture trips graveyard verify.
    InconsistentContract,
    /// Unmitigated high-severity dangerous combination.
    UnsafeCombination,
    /// Multi-controller plan without interference evidence.
    MissingInterferenceEvidence,
    /// Golden demo outputs drift between runs.
    DemoDivergence,
    /// Contract demo_linkage points at a stale claim id.
    ClaimLinkageBreak,
    /// Multi-lever change without override + waiver.
    MultiLeverViolation,
    /// Rollback plan without post-rollback validations.
    RollbackClauseViolation,
    /// Release gate held by an emergency hold.
    ReleasePolicyHold,
}

impl GauntletScenario {
    /// All scenarios, in canonical order.
    pub const ALL: [GauntletScenario; 11] = [
        GauntletScenario::GreenPromotion,
        GauntletScenario::MalformedHeader,
        GauntletScenario::IncompleteContract,
        GauntletScenario::InconsistentContract,
        GauntletScenario::UnsafeCombination,
        GauntletScenario::MissingInterferenceEvidence,
        GauntletScenario::DemoDivergence,
        GauntletScenario::ClaimLinkageBreak,
        GauntletScenario::MultiLeverViolation,
        GauntletScenario::RollbackClauseViolation,
        GauntletScenario::ReleasePolicyHold,
    ];

    /// Stable scenario id.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            GauntletScenario::GreenPromotion => "green_promotion",
            GauntletScenario::MalformedHeader => "malformed_header",
            GauntletScenario::IncompleteContract => "incomplete_contract",
            GauntletScenario::InconsistentContract => "inconsistent_contract",
            GauntletScenario::UnsafeCombination => "unsafe_combination",
            GauntletScenario::MissingInterferenceEvidence => "missing_interference_evidence",
            GauntletScenario::DemoDivergence => "demo_divergence",
            GauntletScenario::ClaimLinkageBreak => "claim_linkage_break",
            GauntletScenario::MultiLeverViolation => "multi_lever_violation",
            GauntletScenario::RollbackClauseViolation => "rollback_clause_violation",
            GauntletScenario::ReleasePolicyHold => "release_policy_hold",
        }
    }

    /// The scenario family.
    #[must_use]
    pub fn family(self) -> GauntletFamily {
        match self {
            GauntletScenario::GreenPromotion => GauntletFamily::GreenPromotion,
            GauntletScenario::MalformedHeader
            | GauntletScenario::IncompleteContract
            | GauntletScenario::InconsistentContract => GauntletFamily::MetadataContractFault,
            GauntletScenario::UnsafeCombination | GauntletScenario::MissingInterferenceEvidence => {
                GauntletFamily::CompositionRisk
            }
            GauntletScenario::DemoDivergence | GauntletScenario::ClaimLinkageBreak => {
                GauntletFamily::DemoExecutionFault
            }
            GauntletScenario::MultiLeverViolation
            | GauntletScenario::RollbackClauseViolation
            | GauntletScenario::ReleasePolicyHold => GauntletFamily::OptimizationPolicyFault,
        }
    }

    /// The stage at which the scenario must terminate.
    #[must_use]
    pub fn expected_terminal_stage(self) -> ChainStage {
        match self {
            GauntletScenario::GreenPromotion
            | GauntletScenario::MultiLeverViolation
            | GauntletScenario::RollbackClauseViolation
            | GauntletScenario::ReleasePolicyHold => ChainStage::Release,
            GauntletScenario::MalformedHeader
            | GauntletScenario::IncompleteContract
            | GauntletScenario::UnsafeCombination
            | GauntletScenario::MissingInterferenceEvidence => ChainStage::Contract,
            GauntletScenario::InconsistentContract => ChainStage::Verify,
            GauntletScenario::DemoDivergence | GauntletScenario::ClaimLinkageBreak => {
                ChainStage::Demo
            }
        }
    }

    /// The terminal stage outcome the scenario must report.
    #[must_use]
    pub fn expected_outcome(self) -> &'static str {
        match self {
            GauntletScenario::GreenPromotion => "release_pass",
            GauntletScenario::MalformedHeader | GauntletScenario::IncompleteContract => {
                "contract_blocked"
            }
            GauntletScenario::InconsistentContract => "verify_inconsistent",
            GauntletScenario::UnsafeCombination | GauntletScenario::MissingInterferenceEvidence => {
                "composition_blocked"
            }
            GauntletScenario::DemoDivergence => "demo_divergent",
            GauntletScenario::ClaimLinkageBreak => "linkage_broken",
            GauntletScenario::MultiLeverViolation => "release_rejected_lever",
            GauntletScenario::RollbackClauseViolation => "release_rejected_rollback",
            GauntletScenario::ReleasePolicyHold => "release_hold",
        }
    }

    /// The triage cluster the terminal failure must map to.
    #[must_use]
    pub fn expected_cluster(self) -> TriageCluster {
        match self {
            GauntletScenario::GreenPromotion => TriageCluster::None,
            GauntletScenario::MalformedHeader => TriageCluster::MetadataGap,
            GauntletScenario::IncompleteContract => TriageCluster::ContractArtifactGap,
            GauntletScenario::InconsistentContract => TriageCluster::RiskPosture,
            GauntletScenario::UnsafeCombination | GauntletScenario::MissingInterferenceEvidence => {
                TriageCluster::CompositionHazard
            }
            GauntletScenario::DemoDivergence => TriageCluster::DemoReproducibility,
            GauntletScenario::ClaimLinkageBreak => TriageCluster::LinkageDrift,
            GauntletScenario::MultiLeverViolation | GauntletScenario::RollbackClauseViolation => {
                TriageCluster::LeverGovernance
            }
            GauntletScenario::ReleasePolicyHold => TriageCluster::ReleasePolicyHold,
        }
    }

    /// Whether the scenario is an adversarial red path.
    #[must_use]
    pub fn is_red_path(self) -> bool {
        !matches!(self, GauntletScenario::GreenPromotion)
    }
}

// ── Ledger + verdicts ────────────────────────────────────────────────────────

/// One graveyard-gauntlet ledger line (float-free; derives `Eq`). Every line
/// carries the AC3-mandated fields with `n/a` sentinels before a stage has
/// computed a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardGauntletLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Scenario id.
    pub scenario_id: String,
    /// Scenario family.
    pub family: GauntletFamily,
    /// Chain stage.
    pub stage: ChainStage,
    /// Unique stage id (`<scenario>:<stage>`; AC3 `stage_id`).
    pub stage_id: String,
    /// Stage position in the chain.
    pub stage_index: usize,
    /// Routed symptom id (AC3 `route_id`).
    pub route_id: String,
    /// Candidate-pool hash from ranking (AC3 `ranking_hash`).
    pub ranking_hash: String,
    /// Recommendation-contract card id (AC3 `contract_id`).
    pub contract_id: String,
    /// graveyardctl verify verdict (AC3 `verify_verdict`).
    pub verify_verdict: String,
    /// Demo contract id (AC3 `demo_id`).
    pub demo_id: String,
    /// Release decision (AC3 `release_policy_verdict`).
    pub release_policy_verdict: String,
    /// Stage outcome tag.
    pub stage_outcome: String,
    /// Whether the stage gate passed.
    pub gate_passed: bool,
    /// Whether this is the scenario's terminal line.
    pub is_terminal_stage: bool,
    /// Machine-checkable violated clause (`none` on pass; AC2).
    pub violated_clause: String,
    /// Failure-signature root-cause cluster.
    pub triage_cluster: String,
    /// Remediation hint for the cluster.
    pub triage_hint: String,
    /// Human-readable detail.
    pub detail: String,
    /// One-command replay handle (AC3 `replay_cmd`).
    pub reproduction_command: String,
}

impl GraveyardGauntletLedgerEntry {
    /// Whether every mandated field is populated (sentinels count; empty
    /// strings do not).
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.run_id.is_empty()
            && !self.scenario_id.is_empty()
            && !self.stage_id.is_empty()
            && !self.route_id.is_empty()
            && !self.ranking_hash.is_empty()
            && !self.contract_id.is_empty()
            && !self.verify_verdict.is_empty()
            && !self.demo_id.is_empty()
            && !self.release_policy_verdict.is_empty()
            && !self.stage_outcome.is_empty()
            && !self.violated_clause.is_empty()
            && !self.triage_cluster.is_empty()
            && !self.detail.is_empty()
            && !self.reproduction_command.is_empty()
    }
}

/// Render the ledger as one JSON object per line.
#[must_use]
pub fn render_ledger_jsonl(ledger: &[GraveyardGauntletLedgerEntry]) -> String {
    let mut out = String::new();
    for entry in ledger {
        match serde_json::to_string(entry) {
            Ok(line) => out.push_str(&line),
            Err(error) => out.push_str(&error.to_string()),
        }
        out.push('\n');
    }
    out
}

/// Expected-vs-observed verdict for one scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GauntletScenarioVerdict {
    /// Scenario id.
    pub scenario_id: String,
    /// Scenario family.
    pub family: GauntletFamily,
    /// Expected terminal stage.
    pub expected_stage: String,
    /// Observed terminal stage.
    pub observed_stage: String,
    /// Expected terminal outcome.
    pub expected_outcome: String,
    /// Observed terminal outcome.
    pub observed_outcome: String,
    /// Expected triage cluster.
    pub expected_cluster: String,
    /// Observed triage cluster.
    pub observed_cluster: String,
    /// Whether the scenario met its oracle.
    pub expectation_met: bool,
}

// ── Scenario inputs ──────────────────────────────────────────────────────────

struct ScenarioInputs {
    header_draft: EntryHeaderDraft,
    graduation: GraduationCriteriaTemplate,
    size: SizeEstimate,
    contract: RecommendationContract,
    plan: CompositionPlan,
    demo: DemoContract,
    observed_outputs: Vec<ExpectedOutput>,
    change: OptimizationChange,
    release_fixture: RolloutGateFixture,
}

fn complete_draft(entry_id: &str) -> EntryHeaderDraft {
    let mut draft = EntryHeaderDraft::new(entry_id);
    for field in HeaderFieldId::ALL {
        draft = draft.provide(*field, format!("{} evidence", field.as_str()));
    }
    draft
}

fn passing_template(bar: QualityBar) -> GraduationCriteriaTemplate {
    let mut template = generate_graduation_template(bar);
    template.criteria = template
        .criteria
        .into_iter()
        .map(|criterion| {
            let link = format!("ci://artifacts/{}.json", criterion.artifact.as_str());
            criterion.with_ci(link, CriterionStatus::Passed)
        })
        .collect();
    template
}

fn green_contract(scenario: GauntletScenario) -> RecommendationContract {
    let mut contract = example_complete_contract();
    contract.card_id = format!("rc-gg-{}", scenario.as_str());
    contract.title = format!("graveyard-gauntlet contract for {}", scenario.as_str());
    contract.source_canonical_entry_id = "gv-erasure-coding".to_string();
    contract.demo_linkage.demo_id = "demo-graveyard-flagship".to_string();
    contract.demo_linkage.claim_id = "claim-graveyard-reproducible".to_string();
    contract
}

fn green_plan() -> CompositionPlan {
    CompositionPlan::new(
        "comp-gg-green",
        [
            UpliftCandidate::ContractsAsCode,
            UpliftCandidate::EGraphOptimizer,
            UpliftCandidate::MetamorphicOracle,
            UpliftCandidate::ShadowRun,
        ],
    )
}

fn green_demo() -> DemoContract {
    DemoContract {
        schema_version: KILLER_DEMO_SCHEMA_VERSION.to_string(),
        demo_id: "demo-graveyard-flagship".to_string(),
        category: "migration".to_string(),
        claim_id: "claim-graveyard-reproducible".to_string(),
        evidence_id: "ev-graveyard-pack-manifest".to_string(),
        policy_id: "pol-rollout-canary".to_string(),
        contract_ref: "clause-migration-traceability".to_string(),
        release_gate_ref: "release-gate/graveyard-chain".to_string(),
        max_duration_seconds: 60,
        commands: vec!["doctor_frankentui graveyard-gauntlet # demo replay".to_string()],
        replay_scenario: ReplayScenario {
            scenario_id: "replay-graveyard-pack".to_string(),
            steps: vec![
                "render golden pack".to_string(),
                "verify checksums".to_string(),
                "replay scenario".to_string(),
            ],
        },
        expected_outputs: vec![
            ExpectedOutput {
                artifact: "pack/manifest.json".to_string(),
                sha256: sha256_hex(b"graveyard-pack-manifest-v1"),
            },
            ExpectedOutput {
                artifact: "pack/evidence_ledger.jsonl".to_string(),
                sha256: sha256_hex(b"graveyard-pack-ledger-v1"),
            },
        ],
    }
}

fn green_change() -> OptimizationChange {
    OptimizationChange::new("chg.gg.single", ["lever.render_diff_simd".to_string()]).with_rollback(
        RollbackPlan::new(
            "rbk.gg.single",
            "git revert <optimization-sha>",
            RiskTier::Low,
            [
                "cargo test -p ftui-render --lib".to_string(),
                "./scripts/perf_gate.sh".to_string(),
            ],
        ),
    )
}

fn all_artifact_kinds() -> Vec<String> {
    vec![
        "certification".to_string(),
        "critical-gaps".to_string(),
        "determinism".to_string(),
        "performance".to_string(),
        "readiness".to_string(),
    ]
}

fn green_release_fixture() -> RolloutGateFixture {
    RolloutGateFixture {
        fixture_id: "gg-alpha-green".to_string(),
        description: "alpha promotion with boundary-clearing evidence".to_string(),
        expected_class: ReleaseTrafficLight::Green,
        target_stage: "alpha".to_string(),
        gate_mode: "enforce".to_string(),
        readiness: ReadinessSpec {
            certification_bps: 9000,
            corpus_coverage_bps: 2500,
            reliability_bps: 9500,
            deterministic_artifacts: 3,
            benchmark_gate_passed: true,
            open_blockers: 2,
            authority: "release-owner".to_string(),
            emergency_hold: None,
        },
        release: ReleaseSpec {
            determinism_bps: 9900,
            performance_budget_passed: true,
            unresolved_critical_gaps: 2,
            artifact_kinds: all_artifact_kinds(),
        },
        feedback_corpus: FeedbackCorpus::Default,
    }
}

fn scenario_inputs(scenario: GauntletScenario) -> ScenarioInputs {
    let mut inputs = ScenarioInputs {
        header_draft: complete_draft("gv-erasure-coding"),
        graduation: passing_template(QualityBar::Gold),
        size: SizeEstimate::new(450, EffortSize::Medium),
        contract: green_contract(scenario),
        plan: green_plan(),
        demo: green_demo(),
        observed_outputs: green_demo().expected_outputs,
        change: green_change(),
        release_fixture: green_release_fixture(),
    };
    match scenario {
        GauntletScenario::GreenPromotion => {}
        GauntletScenario::MalformedHeader => {
            let mut draft = EntryHeaderDraft::new("gv-erasure-coding");
            for field in HeaderFieldId::ALL {
                if *field != HeaderFieldId::Papers {
                    draft = draft.provide(*field, format!("{} evidence", field.as_str()));
                }
            }
            inputs.header_draft = draft;
        }
        GauntletScenario::IncompleteContract => {
            inputs.contract.failure_mode.repro_artifact = None;
            inputs.contract.failure_mode.provenance_artifact = None;
            inputs.contract.budgeted_mode.exhaustion_behavior.clear();
            inputs
                .contract
                .budgeted_mode
                .conservative_fallback_trigger
                .clear();
            inputs.contract.entry_header.tags.clear();
            inputs.contract.entry_header.archetype.clear();
        }
        GauntletScenario::InconsistentContract => {
            inputs.contract.failure_mode.risk_class = RiskClass::High;
            inputs.contract.failure_mode.legal_status = IpArtifactStatus::NeedsCounsel;
        }
        GauntletScenario::UnsafeCombination => {
            inputs.plan = CompositionPlan::new(
                "comp-gg-unsafe",
                [
                    UpliftCandidate::CegisSynthesis,
                    UpliftCandidate::EGraphOptimizer,
                    UpliftCandidate::MetamorphicOracle,
                ],
            );
        }
        GauntletScenario::MissingInterferenceEvidence => {
            inputs.plan = CompositionPlan::new(
                "comp-gg-interference",
                [
                    UpliftCandidate::ShadowRun,
                    UpliftCandidate::ConcolicDifferential,
                ],
            )
            .with_controller(UpliftCandidate::ShadowRun, ControllerRole::Fast)
            .with_controller(UpliftCandidate::ConcolicDifferential, ControllerRole::Fast);
        }
        GauntletScenario::DemoDivergence => {
            inputs.observed_outputs = vec![
                ExpectedOutput {
                    artifact: "pack/manifest.json".to_string(),
                    sha256: sha256_hex(b"graveyard-pack-manifest-DRIFTED"),
                },
                ExpectedOutput {
                    artifact: "pack/evidence_ledger.jsonl".to_string(),
                    sha256: sha256_hex(b"graveyard-pack-ledger-v1"),
                },
            ];
        }
        GauntletScenario::ClaimLinkageBreak => {
            inputs.contract.demo_linkage.claim_id = "claim-stale-reference".to_string();
        }
        GauntletScenario::MultiLeverViolation => {
            inputs.change = OptimizationChange::new(
                "chg.gg.multi",
                [
                    "lever.simd_diff".to_string(),
                    "lever.arena_alloc".to_string(),
                    "lever.presenter_batch".to_string(),
                ],
            )
            .with_rollback(RollbackPlan::new(
                "rbk.gg.multi",
                "git revert <optimization-sha>",
                RiskTier::Medium,
                ["cargo test -p ftui-render --lib".to_string()],
            ));
        }
        GauntletScenario::RollbackClauseViolation => {
            inputs.change = OptimizationChange::new(
                "chg.gg.norollback",
                ["lever.render_diff_simd".to_string()],
            )
            .with_rollback(RollbackPlan::new(
                "rbk.gg.bad",
                "git revert <optimization-sha>",
                RiskTier::Medium,
                Vec::<String>::new(),
            ));
        }
        GauntletScenario::ReleasePolicyHold => {
            inputs.release_fixture.fixture_id = "gg-release-hold".to_string();
            inputs.release_fixture.description =
                "emergency hold blocks an otherwise-clean release".to_string();
            inputs.release_fixture.expected_class = ReleaseTrafficLight::Red;
            inputs.release_fixture.readiness.emergency_hold = Some("security-incident".to_string());
        }
    }
    inputs
}

// ── Engine ───────────────────────────────────────────────────────────────────

struct StageContext {
    route_id: String,
    ranking_hash: String,
    contract_id: String,
    demo_id: String,
    verify_verdict: String,
    release_policy_verdict: String,
}

struct LineParts {
    stage: ChainStage,
    outcome: String,
    gate_passed: bool,
    terminal: bool,
    violated_clause: String,
    detail: String,
}

/// Compare expected demo outputs against an observed run.
fn output_gaps(expected: &[ExpectedOutput], observed: &[ExpectedOutput]) -> Vec<String> {
    let mut gaps = Vec::new();
    for want in expected {
        match observed.iter().find(|o| o.artifact == want.artifact) {
            None => gaps.push(format!("artifact '{}' missing from run", want.artifact)),
            Some(got) if got.sha256 != want.sha256 => gaps.push(format!(
                "artifact '{}' checksum drift: expected {} got {}",
                want.artifact,
                short_hash(&want.sha256),
                short_hash(&got.sha256)
            )),
            Some(_) => {}
        }
    }
    for got in observed {
        if !expected.iter().any(|w| w.artifact == got.artifact) {
            gaps.push(format!("unexpected artifact '{}' in run", got.artifact));
        }
    }
    gaps
}

/// The graveyard-executable E2E gauntlet engine.
#[derive(Debug, Clone)]
pub struct GraveyardGauntlet {
    label: String,
    run_id: String,
}

impl GraveyardGauntlet {
    /// Build an engine with a deterministic run id derived from the label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "graveyard-gauntlet-{}",
            short_hash(&stable_hash(&format!(
                "{GRAVEYARD_GAUNTLET_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { label, run_id }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn reproduction_command(&self, scenario: GauntletScenario) -> String {
        format!(
            "cargo run -p doctor_frankentui -- graveyard-gauntlet --label '{}' # run {} scenario {}",
            self.label,
            self.run_id,
            scenario.as_str()
        )
    }

    fn make_entry(
        &self,
        scenario: GauntletScenario,
        ctx: &StageContext,
        parts: LineParts,
    ) -> GraveyardGauntletLedgerEntry {
        let (cluster, hint) = if parts.gate_passed {
            (TriageCluster::None, String::new())
        } else {
            triage_for(parts.stage, &parts.violated_clause)
        };
        GraveyardGauntletLedgerEntry {
            schema_version: GRAVEYARD_GAUNTLET_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            scenario_id: scenario.as_str().to_string(),
            family: scenario.family(),
            stage: parts.stage,
            stage_id: format!("{}:{}", scenario.as_str(), parts.stage.as_str()),
            stage_index: parts.stage.index(),
            route_id: ctx.route_id.clone(),
            ranking_hash: ctx.ranking_hash.clone(),
            contract_id: ctx.contract_id.clone(),
            verify_verdict: ctx.verify_verdict.clone(),
            demo_id: ctx.demo_id.clone(),
            release_policy_verdict: ctx.release_policy_verdict.clone(),
            stage_outcome: parts.outcome,
            gate_passed: parts.gate_passed,
            is_terminal_stage: parts.terminal,
            violated_clause: parts.violated_clause,
            triage_cluster: cluster.as_str().to_string(),
            triage_hint: hint,
            detail: parts.detail,
            reproduction_command: self.reproduction_command(scenario),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run_scenario(&self, scenario: GauntletScenario) -> Vec<GraveyardGauntletLedgerEntry> {
        let inputs = scenario_inputs(scenario);
        let mut entries = Vec::new();
        let mut ctx = StageContext {
            route_id: na(),
            ranking_hash: na(),
            contract_id: inputs.contract.card_id.clone(),
            demo_id: inputs.demo.demo_id.clone(),
            verify_verdict: na(),
            release_policy_verdict: na(),
        };

        // Stage 1: route.
        let router = SymptomRouter::default();
        let hotspots: Vec<MigrationHotspot> = Vec::new();
        let exceptions: Vec<PolicyException> = Vec::new();
        let rec = router.route(
            SymptomClass::TailLatency,
            &canonical_corpus(),
            &hotspots,
            &exceptions,
        );
        ctx.route_id = rec.symptom_id.clone();
        ctx.ranking_hash = rec.candidate_pool_hash.clone();
        let routed = rec.selected_entry_id.is_some();
        entries.push(self.make_entry(
            scenario,
            &ctx,
            LineParts {
                stage: ChainStage::Route,
                outcome: "routed".to_string(),
                gate_passed: routed,
                terminal: !routed,
                violated_clause: if routed {
                    "none".to_string()
                } else {
                    "empty-candidate-pool".to_string()
                },
                detail: format!(
                    "symptom {} routed to {}",
                    rec.symptom_id,
                    rec.selected_entry_id.clone().unwrap_or_else(|| "none".to_string())
                ),
            },
        ));
        if !routed {
            return entries;
        }

        // Stage 2: rank.
        entries.push(self.make_entry(
            scenario,
            &ctx,
            LineParts {
                stage: ChainStage::Rank,
                outcome: "ranked".to_string(),
                gate_passed: rec.fast_start_applied,
                terminal: !rec.fast_start_applied,
                violated_clause: if rec.fast_start_applied {
                    "none".to_string()
                } else {
                    "fast-start-threshold".to_string()
                },
                detail: format!(
                    "candidate pool of {} hashed to {}",
                    rec.candidate_pool_ids.len(),
                    rec.candidate_pool_hash
                ),
            },
        ));
        if !rec.fast_start_applied {
            return entries;
        }

        // Stage 3: contract (header schema + lint + composition).
        let header_report = EntryHeaderCompiler::default().compile(
            &inputs.header_draft,
            &inputs.graduation,
            Some(&inputs.size),
        );
        if !header_report.can_progress {
            let clause = header_report
                .findings
                .iter()
                .find(|f| f.severity == SchemaSeverity::Block)
                .map_or_else(
                    || "header-blocked".to_string(),
                    |f| f.code.as_str().to_string(),
                );
            entries.push(self.make_entry(
                scenario,
                &ctx,
                LineParts {
                    stage: ChainStage::Contract,
                    outcome: "contract_blocked".to_string(),
                    gate_passed: false,
                    terminal: true,
                    violated_clause: clause,
                    detail: format!(
                        "entry-header compile blocked progress ({} blocking findings)",
                        header_report.summary.blocking_finding_count
                    ),
                },
            ));
            return entries;
        }
        let lint = RecommendationContractLinter::new(ContractLintConfig::default())
            .evaluate(vec![inputs.contract.clone()]);
        if !lint.lint_gate_passes {
            let clause = lint
                .findings
                .iter()
                .find(|f| f.severity == ContractSeverity::Block)
                .map_or_else(|| "lint-blocked".to_string(), |f| f.clause_id.clone());
            entries.push(self.make_entry(
                scenario,
                &ctx,
                LineParts {
                    stage: ChainStage::Contract,
                    outcome: "contract_blocked".to_string(),
                    gate_passed: false,
                    terminal: true,
                    violated_clause: clause,
                    detail: format!(
                        "contract linter blocked {} card(s)",
                        lint.blocked_card_ids.len()
                    ),
                },
            ));
            return entries;
        }
        let composition =
            CompositionGate.evaluate(&inputs.plan, &canonical_matrix(), &canonical_registry());
        if !composition.gate_passes {
            let clause = composition
                .hazards
                .iter()
                .find(|h| h.severity == HazardSeverity::Block)
                .map_or_else(
                    || "composition-blocked".to_string(),
                    |h| h.code.as_str().to_string(),
                );
            entries.push(self.make_entry(
                scenario,
                &ctx,
                LineParts {
                    stage: ChainStage::Contract,
                    outcome: "composition_blocked".to_string(),
                    gate_passed: false,
                    terminal: true,
                    violated_clause: clause,
                    detail: format!(
                        "composition gate blocked plan {} ({} blocking hazards)",
                        composition.composition_id, composition.summary.blocking_hazard_count
                    ),
                },
            ));
            return entries;
        }
        entries.push(self.make_entry(
            scenario,
            &ctx,
            LineParts {
                stage: ChainStage::Contract,
                outcome: "contract_cleared".to_string(),
                gate_passed: true,
                terminal: false,
                violated_clause: "none".to_string(),
                detail: format!(
                    "header progresses, lint passes, composition {} accepted",
                    composition.composition_id
                ),
            },
        ));

        // Stage 4: verify.
        let engine = GraveyardctlEngine::new(
            format!("{}/{}", self.label, scenario.as_str()),
            vec![ActiveEntry {
                entry_id: inputs.contract.source_canonical_entry_id.clone(),
                contract: inputs.contract.clone(),
            }],
        );
        let ctl = engine.run(Some(GraveyardctlStage::Verify));
        let verify_line = ctl
            .ledger
            .iter()
            .find(|line| line.stage == GraveyardctlStage::Verify)
            .cloned();
        let verdict = verify_line
            .as_ref()
            .map_or(VerifyResult::NotApplicable, |line| line.verify_result);
        ctx.verify_verdict = verdict.as_str().to_string();
        if !ctl.gate_passes {
            let (outcome, clause) = match verdict {
                VerifyResult::Inconsistent => (
                    "verify_inconsistent",
                    "dangerous-combination-escalation".to_string(),
                ),
                VerifyResult::Incomplete => (
                    "verify_incomplete",
                    verify_line
                        .as_ref()
                        .and_then(|line| line.missing_artifacts.first().cloned())
                        .unwrap_or_else(|| "missing-artifacts".to_string()),
                ),
                _ => ("verify_failed", "verify-gate".to_string()),
            };
            entries.push(self.make_entry(
                scenario,
                &ctx,
                LineParts {
                    stage: ChainStage::Verify,
                    outcome: outcome.to_string(),
                    gate_passed: false,
                    terminal: true,
                    violated_clause: clause,
                    detail: format!(
                        "graveyardctl verify classified {} as {}",
                        ctx.contract_id,
                        verdict.as_str()
                    ),
                },
            ));
            return entries;
        }
        entries.push(self.make_entry(
            scenario,
            &ctx,
            LineParts {
                stage: ChainStage::Verify,
                outcome: "verified".to_string(),
                gate_passed: true,
                terminal: false,
                violated_clause: "none".to_string(),
                detail: format!("graveyardctl verify passed for {}", ctx.contract_id),
            },
        ));

        // Stage 5: demo.
        let rendered = render_demo_yaml(&inputs.demo);
        let roundtrip_ok =
            matches!(parse_demo_yaml(&rendered), Ok(parsed) if parsed == inputs.demo);
        let linkage_ok = inputs.contract.demo_linkage.demo_id == inputs.demo.demo_id
            && inputs.contract.demo_linkage.claim_id == inputs.demo.claim_id;
        let gaps = output_gaps(&inputs.demo.expected_outputs, &inputs.observed_outputs);
        if !linkage_ok {
            entries.push(self.make_entry(
                scenario,
                &ctx,
                LineParts {
                    stage: ChainStage::Demo,
                    outcome: "linkage_broken".to_string(),
                    gate_passed: false,
                    terminal: true,
                    violated_clause: "claim-linkage-mismatch".to_string(),
                    detail: format!(
                        "contract demo_linkage ({}, {}) does not match demo contract ({}, {})",
                        inputs.contract.demo_linkage.demo_id,
                        inputs.contract.demo_linkage.claim_id,
                        inputs.demo.demo_id,
                        inputs.demo.claim_id
                    ),
                },
            ));
            return entries;
        }
        if !roundtrip_ok || !gaps.is_empty() {
            let clause = if roundtrip_ok {
                "demo-checksum-drift".to_string()
            } else {
                "demo-yaml-roundtrip".to_string()
            };
            entries.push(self.make_entry(
                scenario,
                &ctx,
                LineParts {
                    stage: ChainStage::Demo,
                    outcome: "demo_divergent".to_string(),
                    gate_passed: false,
                    terminal: true,
                    violated_clause: clause,
                    detail: gaps.first().cloned().unwrap_or_else(|| {
                        "demo.yaml did not round-trip byte-identically".to_string()
                    }),
                },
            ));
            return entries;
        }
        entries.push(self.make_entry(
            scenario,
            &ctx,
            LineParts {
                stage: ChainStage::Demo,
                outcome: "demo_reproduced".to_string(),
                gate_passed: true,
                terminal: false,
                violated_clause: "none".to_string(),
                detail: format!(
                    "demo {} round-tripped and {} outputs matched",
                    ctx.demo_id,
                    inputs.demo.expected_outputs.len()
                ),
            },
        ));

        // Stage 6: release (one-lever policy, then the release gate).
        let lever_report = run_one_lever_policy(
            &format!("{}/{}", self.label, scenario.as_str()),
            std::slice::from_ref(&inputs.change),
        );
        let card = lever_report.card(&inputs.change.change_id).cloned();
        let accepted = card.as_ref().is_some_and(|c| c.accepted);
        if !accepted {
            let multi_lever = inputs.change.levers.len() > 1;
            let (outcome, verdict_tag, clause) = if multi_lever {
                (
                    "release_rejected_lever",
                    "reject_multi_lever",
                    "one-lever-policy".to_string(),
                )
            } else {
                (
                    "release_rejected_rollback",
                    "reject_rollback",
                    "rollback-plan-incomplete".to_string(),
                )
            };
            ctx.release_policy_verdict = verdict_tag.to_string();
            entries.push(self.make_entry(
                scenario,
                &ctx,
                LineParts {
                    stage: ChainStage::Release,
                    outcome: outcome.to_string(),
                    gate_passed: false,
                    terminal: true,
                    violated_clause: clause,
                    detail: card.as_ref().map_or_else(
                        || "change card missing".to_string(),
                        |c| format!("change {} rejected: {}", c.change_id, c.rejection_reason),
                    ),
                },
            ));
            return entries;
        }
        let release = evaluate_fixture(&inputs.release_fixture);
        let diagnostic = &release.diagnostic;
        if diagnostic.blocks_release || diagnostic.release_verdict != "pass" {
            ctx.release_policy_verdict = diagnostic.release_verdict.clone();
            let clause = if diagnostic.violated_rule.is_empty() {
                "release-policy-hold".to_string()
            } else {
                diagnostic.violated_rule.clone()
            };
            entries.push(self.make_entry(
                scenario,
                &ctx,
                LineParts {
                    stage: ChainStage::Release,
                    outcome: "release_hold".to_string(),
                    gate_passed: false,
                    terminal: true,
                    violated_clause: clause,
                    detail: format!(
                        "release gate held promotion (light {}, verdict {})",
                        diagnostic.decision_output, diagnostic.release_verdict
                    ),
                },
            ));
            return entries;
        }
        ctx.release_policy_verdict = diagnostic.release_verdict.clone();
        entries.push(self.make_entry(
            scenario,
            &ctx,
            LineParts {
                stage: ChainStage::Release,
                outcome: "release_pass".to_string(),
                gate_passed: true,
                terminal: true,
                violated_clause: "none".to_string(),
                detail: format!(
                    "one-lever change accepted and release gate green (light {})",
                    diagnostic.decision_output
                ),
            },
        ));
        entries
    }

    /// Run the full eleven-scenario gauntlet.
    #[must_use]
    pub fn run(&self) -> GraveyardGauntletReport {
        let mut ledger: Vec<GraveyardGauntletLedgerEntry> = Vec::new();
        let mut verdicts: Vec<GauntletScenarioVerdict> = Vec::new();

        for scenario in GauntletScenario::ALL {
            let entries = self.run_scenario(scenario);
            let terminal = entries
                .iter()
                .rev()
                .find(|entry| entry.is_terminal_stage)
                .cloned();
            let (observed_stage, observed_outcome, observed_cluster, gate_passed) =
                terminal.as_ref().map_or_else(
                    || (na(), na(), na(), false),
                    |entry| {
                        (
                            entry.stage.as_str().to_string(),
                            entry.stage_outcome.clone(),
                            entry.triage_cluster.clone(),
                            entry.gate_passed,
                        )
                    },
                );
            let expectation_met = observed_stage == scenario.expected_terminal_stage().as_str()
                && observed_outcome == scenario.expected_outcome()
                && observed_cluster == scenario.expected_cluster().as_str()
                && if scenario.is_red_path() {
                    !gate_passed
                } else {
                    gate_passed
                };
            verdicts.push(GauntletScenarioVerdict {
                scenario_id: scenario.as_str().to_string(),
                family: scenario.family(),
                expected_stage: scenario.expected_terminal_stage().as_str().to_string(),
                observed_stage,
                expected_outcome: scenario.expected_outcome().to_string(),
                observed_outcome,
                expected_cluster: scenario.expected_cluster().as_str().to_string(),
                observed_cluster,
                expectation_met,
            });
            ledger.extend(entries);
        }

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "graveyard-gauntlet-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );

        let required_fields_complete = ledger
            .iter()
            .all(GraveyardGauntletLedgerEntry::has_required_fields);
        let scenarios_meeting_expectation = verdicts.iter().filter(|v| v.expectation_met).count();
        let all_expectations_met = scenarios_meeting_expectation == verdicts.len();
        let families_covered = {
            let mut families: Vec<GauntletFamily> = verdicts.iter().map(|v| v.family).collect();
            families.sort();
            families.dedup();
            families.len()
        };
        let red_paths_covered = GauntletScenario::ALL
            .iter()
            .filter(|s| s.is_red_path())
            .all(|s| {
                verdicts
                    .iter()
                    .any(|v| v.scenario_id == s.as_str() && v.expectation_met)
            });
        let green_anchor_promoted = verdicts.iter().any(|v| {
            v.scenario_id == GauntletScenario::GreenPromotion.as_str() && v.expectation_met
        }) && ledger
            .iter()
            .filter(|e| e.scenario_id == GauntletScenario::GreenPromotion.as_str())
            .count()
            == ChainStage::ALL.len();
        let triage_actionable = ledger
            .iter()
            .filter(|e| e.is_terminal_stage && !e.gate_passed)
            .all(|e| {
                e.triage_cluster != TriageCluster::None.as_str()
                    && !e.triage_hint.is_empty()
                    && !e.reproduction_command.is_empty()
            });

        let gate_passes = required_fields_complete
            && all_expectations_met
            && families_covered == GauntletFamily::ALL.len()
            && red_paths_covered
            && green_anchor_promoted
            && triage_actionable;

        let replay_command = format!(
            "cargo run -p doctor_frankentui -- graveyard-gauntlet --label '{}' # report {report_id}",
            self.label
        );

        let summary = GraveyardGauntletSummary {
            schema_version: GRAVEYARD_GAUNTLET_SCHEMA_VERSION.to_string(),
            report_id: report_id.clone(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.clone(),
            total_scenarios: verdicts.len(),
            total_ledger_lines: ledger.len(),
            families_covered,
            scenarios_meeting_expectation,
            required_fields_complete,
            all_expectations_met,
            red_paths_covered,
            green_anchor_promoted,
            triage_actionable,
            gate_passes,
            replay_command: replay_command.clone(),
        };

        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a GraveyardGauntletSummary,
            verdicts: &'a [GauntletScenarioVerdict],
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: GRAVEYARD_GAUNTLET_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
            verdicts: &verdicts,
        })
        .unwrap_or_default();
        let exported_json_stats = GraveyardGauntletStatsArtifact {
            path: format!("graveyard_gauntlet/{report_id}.json"),
            sha256: sha256_hex(content.as_bytes()),
            content,
        };

        GraveyardGauntletReport {
            schema_version: GRAVEYARD_GAUNTLET_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            ledger,
            verdicts,
            summary,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }
}

/// Run the gauntlet under a label.
#[must_use]
pub fn run_graveyard_gauntlet(label: &str) -> GraveyardGauntletReport {
    GraveyardGauntlet::new(label).run()
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Aggregate summary + gate booleans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardGauntletSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// SHA-256 of the exact ledger JSONL bytes.
    pub evidence_checksum: String,
    /// Scenarios evaluated.
    pub total_scenarios: usize,
    /// Ledger lines emitted.
    pub total_ledger_lines: usize,
    /// Distinct families exercised.
    pub families_covered: usize,
    /// Scenarios that met their oracle.
    pub scenarios_meeting_expectation: usize,
    /// Every ledger line carries all mandated fields.
    pub required_fields_complete: bool,
    /// Every scenario met its oracle.
    pub all_expectations_met: bool,
    /// Every red scenario terminated at its expected stage/outcome/cluster.
    pub red_paths_covered: bool,
    /// The green scenario promoted through all six stages.
    pub green_anchor_promoted: bool,
    /// Every failure carries a cluster + hint + replay handle.
    pub triage_actionable: bool,
    /// Fail-closed gate verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
}

/// Deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardGauntletStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory gauntlet report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardGauntletReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// SHA-256 of the exact ledger JSONL bytes.
    pub evidence_checksum: String,
    /// The evidence ledger (one line per executed stage).
    pub ledger: Vec<GraveyardGauntletLedgerEntry>,
    /// Per-scenario verdicts.
    pub verdicts: Vec<GauntletScenarioVerdict>,
    /// Aggregate summary.
    pub summary: GraveyardGauntletSummary,
    /// Fail-closed gate verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: GraveyardGauntletStatsArtifact,
}

impl GraveyardGauntletReport {
    /// Render the ledger as JSONL.
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }

    /// The terminal ledger line for a scenario.
    #[must_use]
    pub fn terminal_for(
        &self,
        scenario: GauntletScenario,
    ) -> Option<&GraveyardGauntletLedgerEntry> {
        self.ledger
            .iter()
            .find(|e| e.scenario_id == scenario.as_str() && e.is_terminal_stage)
    }
}

// ── Pipeline materializer ────────────────────────────────────────────────────

/// Pipeline configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct GraveyardGauntletConfig {
    /// Run directory name under the run root.
    pub run_name: String,
    /// Run label.
    pub label: String,
}

impl Default for GraveyardGauntletConfig {
    fn default() -> Self {
        Self {
            run_name: "graveyard_gauntlet".to_string(),
            label: "graveyard-gauntlet/e2e".to_string(),
        }
    }
}

/// A materialized artifact with integrity metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardGauntletArtifact {
    /// Artifact name (extension trimmed).
    pub name: String,
    /// File name.
    pub file: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Content size in bytes.
    pub bytes: u64,
}

/// The materialized pipeline outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraveyardGauntletPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Evidence-ledger path.
    pub ledger_path: String,
    /// Pipeline-summary path.
    pub summary_path: String,
    /// Artifact-manifest path.
    pub manifest_path: String,
    /// JSON-stats path.
    pub stats_path: String,
    /// The run summary.
    pub summary: GraveyardGauntletSummary,
    /// Tracked artifacts (the manifest does not track itself).
    pub artifacts: Vec<GraveyardGauntletArtifact>,
}

fn artifact_of(file: &str, content: &str) -> GraveyardGauntletArtifact {
    GraveyardGauntletArtifact {
        name: file.replace(['.', '/'], "-"),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Materialize the gauntlet evidence bundle under `run_root/<run_name>/`.
pub fn run_graveyard_gauntlet_pipeline(
    run_root: &Path,
    config: &GraveyardGauntletConfig,
) -> crate::error::Result<GraveyardGauntletPipelineOutcome> {
    let report = run_graveyard_gauntlet(&config.label);
    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary).unwrap_or_default();

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "graveyard_gauntlet_stats.json";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&run_dir.join(stats_file), &stats_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    let artifacts = vec![
        artifact_of(ledger_file, &ledger_content),
        artifact_of(stats_file, &stats_content),
        artifact_of(summary_file, &summary_content),
    ];

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        gate_passes: bool,
        artifacts: &'a [GraveyardGauntletArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: GRAVEYARD_GAUNTLET_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })
    .unwrap_or_default();
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(GraveyardGauntletPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for the graveyard-gauntlet subcommand.
#[derive(Debug, clap::Args)]
pub struct GraveyardGauntletArgs {
    /// Root directory for materialized evidence.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/graveyard_gauntlet"
    )]
    pub run_root: PathBuf,

    /// Run directory name under the run root.
    #[arg(long = "run-name", default_value = "graveyard_gauntlet")]
    pub run_name: String,

    /// Run label folded into run/report ids.
    #[arg(long = "label", default_value = "graveyard-gauntlet/e2e")]
    pub label: String,
}

/// Run the graveyard-gauntlet subcommand (fail-closed).
pub fn run_graveyard_gauntlet_command(args: GraveyardGauntletArgs) -> crate::error::Result<()> {
    let config = GraveyardGauntletConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_graveyard_gauntlet_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("graveyard-executable gauntlet"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "scenarios: {} (met expectation: {}), families: {}, ledger lines: {}",
            summary.total_scenarios,
            summary.scenarios_meeting_expectation,
            summary.families_covered,
            summary.total_ledger_lines
        ));
        ui.info(&format!(
            "red paths covered: {}, green anchor promoted: {}, triage actionable: {}",
            summary.red_paths_covered, summary.green_anchor_promoted, summary.triage_actionable
        ));
        if summary.gate_passes {
            ui.success("graveyard-gauntlet gate PASSED");
        } else {
            ui.error("graveyard-gauntlet gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "graveyard-gauntlet gate failed: all_expectations_met={}, families_covered={}, \
                 red_paths_covered={}, green_anchor_promoted={}, triage_actionable={}",
                summary.all_expectations_met,
                summary.families_covered,
                summary.red_paths_covered,
                summary.green_anchor_promoted,
                summary.triage_actionable
            ),
        ))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn report() -> GraveyardGauntletReport {
        run_graveyard_gauntlet("test")
    }

    #[test]
    fn gate_passes_and_covers_all_families() {
        let report = report();
        assert!(report.gate_passes, "verdicts: {:?}", report.verdicts);
        assert_eq!(report.summary.total_scenarios, 11);
        assert_eq!(report.summary.families_covered, 5);
        assert_eq!(report.summary.scenarios_meeting_expectation, 11);
        assert!(report.summary.required_fields_complete);
        assert!(report.summary.all_expectations_met);
        assert!(report.summary.red_paths_covered);
        assert!(report.summary.green_anchor_promoted);
        assert!(report.summary.triage_actionable);
    }

    #[test]
    fn green_promotion_walks_all_six_stages() {
        let report = report();
        let green: Vec<_> = report
            .ledger
            .iter()
            .filter(|e| e.scenario_id == "green_promotion")
            .collect();
        assert_eq!(green.len(), 6);
        for (i, entry) in green.iter().enumerate() {
            assert_eq!(entry.stage_index, i);
            assert!(entry.gate_passed, "stage {} failed", entry.stage.as_str());
        }
        let terminal = report
            .terminal_for(GauntletScenario::GreenPromotion)
            .expect("terminal");
        assert_eq!(terminal.stage_outcome, "release_pass");
        assert_eq!(terminal.release_policy_verdict, "pass");
        assert_eq!(terminal.violated_clause, "none");
    }

    #[test]
    fn every_red_scenario_terminates_at_its_expected_stage() {
        let report = report();
        for scenario in GauntletScenario::ALL {
            if !scenario.is_red_path() {
                continue;
            }
            let terminal = report.terminal_for(scenario).expect("terminal line");
            assert_eq!(
                terminal.stage,
                scenario.expected_terminal_stage(),
                "{}",
                scenario.as_str()
            );
            assert_eq!(terminal.stage_outcome, scenario.expected_outcome());
            assert!(!terminal.gate_passed);
            assert_ne!(terminal.violated_clause, "none");
        }
    }

    #[test]
    fn failure_signatures_map_to_expected_clusters() {
        let report = report();
        for scenario in GauntletScenario::ALL {
            let terminal = report.terminal_for(scenario).expect("terminal line");
            assert_eq!(
                terminal.triage_cluster,
                scenario.expected_cluster().as_str(),
                "{}",
                scenario.as_str()
            );
            if scenario.is_red_path() {
                assert!(!terminal.triage_hint.is_empty(), "{}", scenario.as_str());
            }
        }
    }

    #[test]
    fn metadata_faults_carry_machine_clauses() {
        let report = report();
        let malformed = report
            .terminal_for(GauntletScenario::MalformedHeader)
            .expect("terminal");
        assert_eq!(malformed.violated_clause, "missing_mandatory_header_field");
        let incomplete = report
            .terminal_for(GauntletScenario::IncompleteContract)
            .expect("terminal");
        assert!(
            incomplete.violated_clause.starts_with("RC-"),
            "lint clause expected, got {}",
            incomplete.violated_clause
        );
        let inconsistent = report
            .terminal_for(GauntletScenario::InconsistentContract)
            .expect("terminal");
        assert_eq!(inconsistent.verify_verdict, "inconsistent");
    }

    #[test]
    fn composition_and_demo_faults_are_differentiated() {
        let report = report();
        let unsafe_combo = report
            .terminal_for(GauntletScenario::UnsafeCombination)
            .expect("terminal");
        assert_eq!(
            unsafe_combo.violated_clause,
            "unmitigated_dangerous_combination"
        );
        let interference = report
            .terminal_for(GauntletScenario::MissingInterferenceEvidence)
            .expect("terminal");
        assert!(interference.violated_clause.contains("controller"));
        let divergent = report
            .terminal_for(GauntletScenario::DemoDivergence)
            .expect("terminal");
        assert_eq!(divergent.violated_clause, "demo-checksum-drift");
        assert!(divergent.detail.contains("checksum drift"));
        let linkage = report
            .terminal_for(GauntletScenario::ClaimLinkageBreak)
            .expect("terminal");
        assert_eq!(linkage.violated_clause, "claim-linkage-mismatch");
    }

    #[test]
    fn release_faults_carry_policy_verdicts() {
        let report = report();
        let multi = report
            .terminal_for(GauntletScenario::MultiLeverViolation)
            .expect("terminal");
        assert_eq!(multi.release_policy_verdict, "reject_multi_lever");
        let rollback = report
            .terminal_for(GauntletScenario::RollbackClauseViolation)
            .expect("terminal");
        assert_eq!(rollback.release_policy_verdict, "reject_rollback");
        let hold = report
            .terminal_for(GauntletScenario::ReleasePolicyHold)
            .expect("terminal");
        assert_eq!(hold.release_policy_verdict, "fail");
        assert_eq!(hold.stage_outcome, "release_hold");
    }

    #[test]
    fn every_ledger_line_carries_ac3_fields() {
        let report = report();
        assert!(!report.ledger.is_empty());
        for entry in &report.ledger {
            assert!(entry.has_required_fields(), "incomplete: {entry:?}");
            assert!(entry.reproduction_command.contains("graveyard-gauntlet"));
            assert!(entry.stage_id.contains(':'));
        }
    }

    #[test]
    fn report_is_deterministic_and_replays_byte_identically() {
        let first = run_graveyard_gauntlet("determinism");
        let second = run_graveyard_gauntlet("determinism");
        assert_eq!(first, second);
        assert_eq!(first.render_ledger_jsonl(), second.render_ledger_jsonl());
    }

    #[test]
    fn ledger_jsonl_has_one_line_per_entry() {
        let report = report();
        let jsonl = report.render_ledger_jsonl();
        assert_eq!(jsonl.lines().count(), report.ledger.len());
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = report();
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert!(report.exported_json_stats.path.contains(&report.report_id));
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome =
            run_graveyard_gauntlet_pipeline(dir.path(), &GraveyardGauntletConfig::default())
                .expect("pipeline");
        assert!(outcome.summary.gate_passes);
        assert_eq!(outcome.artifacts.len(), 3);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let content = std::fs::read(&path).expect("artifact readable");
            assert_eq!(artifact.sha256, sha256_hex(&content), "{}", artifact.file);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_graveyard_gauntlet(&label);
            let second = run_graveyard_gauntlet(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_graveyard_gauntlet(&label);
            prop_assert!(report.gate_passes);
            prop_assert_eq!(report.summary.families_covered, 5);
        }
    }
}

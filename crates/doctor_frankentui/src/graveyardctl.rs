//! Executable alien-graveyard workflow + CI verify gate (bd-3bxhj.10.32).
//!
//! Makes alien-graveyard *planning* executable by wrapping the canonical
//! governance modules into a deterministic `route -> rank -> contract -> verify`
//! pipeline with five command wrappers:
//!
//! - **index**: enumerate the active recommendation entries (canonical-entry
//!   provenance joined with the recommendation-contract corpus).
//! - **score**: rank candidate primitives per symptom via the canonical
//!   symptom-router (`gv §0.x` Fast-Start S-tier harvesting).
//! - **pick**: select the actively-implemented entries (the router's S-tier
//!   fast-start selections that have a contract).
//! - **scaffold**: emit the entry-header draft, graduation template, and
//!   LOC/effort estimate for each active entry.
//! - **verify**: run the recommendation-contract linter and the failure-mode
//!   risk gate over each active entry, classifying it as `pass`, `incomplete`
//!   (missing artifact-contract fields: budgets, safe-mode, metadata, repro
//!   packs), or `inconsistent` (artifact-complete but the risk posture fails
//!   the dangerous-combination / proof gate).
//!
//! Acceptance criteria:
//!
//! - **AC1**: the verify gate fails closed (non-zero exit) if any active alien
//!   entry's verify check is incomplete or inconsistent. The default corpus is
//!   all-complete, so the production CI gate is green; the negative corpora used
//!   in tests drive `incomplete`/`inconsistent` entries through the same gate.
//! - **AC2**: pick/score outputs are deterministic for a fixed corpus, profile,
//!   and policy. The same inputs produce a byte-identical evidence ledger and a
//!   stable `report_id`/`evidence_checksum`.
//! - **AC3**: every JSONL ledger line carries `entry_id`, `verify_result`,
//!   `missing_artifacts`, a `remediation` command set, plus `run_id`, the
//!   `stage`, and a `reproduction_command`.
//!
//! The ledger is float-free (only `String`/enum/`Vec<String>`/`bool`), so it
//! derives `Eq` and replays byte-identically.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
use crate::adversarial_fixtures::RiskClass;
use crate::entry_header_compiler::{
    EntryHeaderCompiler, EntryHeaderCompilerConfig, EntryHeaderDraft, HeaderFieldId,
    HeaderFieldValue, SizeEstimate, estimate_effort_band, generate_graduation_template,
};
use crate::failure_mode_risk::{FailureModeClass, RiskGate, RiskMatrix, RolloutPhase};
use crate::milestone_policy::{PriorityTier, QualityBar};
use crate::recommendation_contract::{
    ContractLintConfig, ContractSeverity, RecommendationContract, RecommendationContractLinter,
    example_complete_contract,
};
#[cfg(test)]
use crate::semantic_contract::IpArtifactStatus;
use crate::symptom_router::{
    CanonicalEntry, MigrationHotspot, PolicyException, SymptomRouter, canonical_corpus,
};

/// Schema version for the in-memory graveyardctl report.
pub const GRAVEYARDCTL_SCHEMA_VERSION: &str = "graveyardctl-v1";

/// Schema version for the materialized graveyardctl pipeline artifacts.
pub const GRAVEYARDCTL_PIPELINE_SCHEMA_VERSION: &str = "graveyardctl-pipeline-v1";

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

fn tier_str(tier: PriorityTier) -> &'static str {
    match tier {
        PriorityTier::S => "S",
        PriorityTier::A => "A",
        PriorityTier::B => "B",
        PriorityTier::C => "C",
    }
}

/// Map a priority tier to the graduation quality bar it must clear.
fn quality_bar_for_tier(tier: PriorityTier) -> QualityBar {
    match tier {
        PriorityTier::S => QualityBar::Platinum,
        PriorityTier::A => QualityBar::Gold,
        PriorityTier::B => QualityBar::Silver,
        PriorityTier::C => QualityBar::Bronze,
    }
}

// ── Vocabulary ──────────────────────────────────────────────────────────────

/// A graveyardctl workflow stage (one per command wrapper).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraveyardctlStage {
    /// Enumerate the active recommendation entries.
    Index,
    /// Rank candidate primitives per symptom.
    Score,
    /// Select the actively-implemented entries.
    Pick,
    /// Emit entry-header draft + graduation template + LOC estimate.
    Scaffold,
    /// Run the contract linter + risk gate over each active entry.
    Verify,
}

impl GraveyardctlStage {
    /// All stages in canonical (pipeline) order.
    pub const ALL: [GraveyardctlStage; 5] = [
        GraveyardctlStage::Index,
        GraveyardctlStage::Score,
        GraveyardctlStage::Pick,
        GraveyardctlStage::Scaffold,
        GraveyardctlStage::Verify,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Score => "score",
            Self::Pick => "pick",
            Self::Scaffold => "scaffold",
            Self::Verify => "verify",
        }
    }
}

/// The verify verdict for an active entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyResult {
    /// Contract linter and risk gate both pass.
    Pass,
    /// Contract linter blocks: an artifact-contract field is missing (budgets,
    /// safe-mode semantics, metadata completeness, or a repro pack).
    Incomplete,
    /// Contract is artifact-complete, but the risk gate blocks (e.g. a
    /// dangerous combination of co-occurring high-severity failure modes).
    Inconsistent,
    /// Not a verify-stage line (index/score/pick/scaffold).
    NotApplicable,
}

impl VerifyResult {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Incomplete => "incomplete",
            Self::Inconsistent => "inconsistent",
            Self::NotApplicable => "n/a",
        }
    }

    /// Whether this verdict counts as an active-entry verify failure.
    #[must_use]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Incomplete | Self::Inconsistent)
    }
}

// ── Evidence ledger entry (AC3, float-free) ─────────────────────────────────

/// One line of the graveyardctl evidence ledger.
///
/// Float-free (only `String`/enum/`Vec<String>`/`bool`), so it derives `Eq` and
/// serializes byte-identically across runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardctlLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id (stable for fixed corpus/profile/policy/stage).
    pub run_id: String,
    /// Workflow stage this line belongs to.
    pub stage: GraveyardctlStage,
    /// Canonical graveyard entry id (or `n/a` for non-entry-scoped lines).
    pub entry_id: String,
    /// Recommendation card / contract id (or `n/a`).
    pub card_id: String,
    /// The action performed for this line.
    pub action: String,
    /// The verify verdict (`n/a` for non-verify stages).
    pub verify_result: VerifyResult,
    /// Missing artifact-contract fields surfaced by verify (empty otherwise).
    pub missing_artifacts: Vec<String>,
    /// Remediation command set for the surfaced defects (empty otherwise).
    pub remediation: Vec<String>,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic command to reproduce this stage.
    pub reproduction_command: String,
}

// ── Active recommendation entries ───────────────────────────────────────────

/// An actively-implemented alien entry: a canonical-entry id joined with its
/// recommendation contract.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveEntry {
    /// Canonical graveyard entry id (the router's selection key).
    pub entry_id: String,
    /// Recommendation contract for the entry.
    pub contract: RecommendationContract,
}

impl ActiveEntry {
    /// The recommendation card / contract id.
    #[must_use]
    pub fn card_id(&self) -> &str {
        &self.contract.card_id
    }
}

/// Build a complete (gate-passing) contract bound to a canonical entry id.
fn complete_contract_for(
    entry_id: &str,
    card_id: &str,
    title: &str,
    claim_id: &str,
) -> RecommendationContract {
    let mut contract = example_complete_contract();
    contract.card_id = card_id.to_string();
    contract.title = title.to_string();
    contract.source_canonical_entry_id = entry_id.to_string();
    contract.demo_linkage.demo_id = format!("demo-{card_id}");
    contract.demo_linkage.claim_id = claim_id.to_string();
    contract
}

/// Derive an incomplete contract that drops the artifact-contract fields the
/// verify gate must catch: budgets, safe-mode trigger, metadata, repro pack,
/// and provenance.
#[cfg(test)]
fn incomplete_contract_for(
    entry_id: &str,
    card_id: &str,
    title: &str,
    claim_id: &str,
) -> RecommendationContract {
    let mut contract = complete_contract_for(entry_id, card_id, title, claim_id);
    contract.failure_mode.repro_artifact = None;
    contract.failure_mode.provenance_artifact = None;
    contract.budgeted_mode.exhaustion_behavior.clear();
    contract.budgeted_mode.conservative_fallback_trigger.clear();
    contract.entry_header.tags.clear();
    contract.entry_header.archetype.clear();
    contract
}

/// Derive an inconsistent contract that is artifact-complete (passes the lint
/// gate) but whose risk posture trips the dangerous-combination gate: a
/// high-severity primary failure mode co-occurring with a high-severity
/// legal/IP status (advisory `needs_counsel` lint, but `High` risk severity).
#[cfg(test)]
fn inconsistent_contract_for(
    entry_id: &str,
    card_id: &str,
    title: &str,
    claim_id: &str,
) -> RecommendationContract {
    let mut contract = complete_contract_for(entry_id, card_id, title, claim_id);
    contract.failure_mode.risk_class = RiskClass::High;
    contract.failure_mode.legal_status = IpArtifactStatus::NeedsCounsel;
    contract
}

/// The default corpus of actively-implemented alien entries (all complete).
///
/// Entry ids reference the canonical corpus' S-tier primitives so the router's
/// fast-start picks line up with the active set (the `route -> pick` linkage).
#[must_use]
pub fn default_active_corpus() -> Vec<ActiveEntry> {
    vec![
        ActiveEntry {
            entry_id: "gv-erasure-coding".to_string(),
            contract: complete_contract_for(
                "gv-erasure-coding",
                "rc-erasure-coding",
                "Reed-Solomon tail-latency hedging",
                "claim-erasure-tail",
            ),
        },
        ActiveEntry {
            entry_id: "gv-metamorphic-equiv".to_string(),
            contract: complete_contract_for(
                "gv-metamorphic-equiv",
                "rc-metamorphic-equiv",
                "Metamorphic equivalence oracle",
                "claim-metamorphic-equiv",
            ),
        },
        ActiveEntry {
            entry_id: "gv-lockfree-queue".to_string(),
            contract: complete_contract_for(
                "gv-lockfree-queue",
                "rc-lockfree-queue",
                "Lock-free MPSC with hazard pointers",
                "claim-lockfree-queue",
            ),
        },
    ]
}

// ── Verify classification ───────────────────────────────────────────────────

/// Classify one active entry's contract: run the contract linter, then (if
/// artifact-complete) the failure-mode risk gate.
fn classify_verify(contract: &RecommendationContract) -> (VerifyResult, Vec<String>, Vec<String>) {
    let lint = RecommendationContractLinter::new(ContractLintConfig::default())
        .evaluate(vec![contract.clone()]);
    if !lint.lint_gate_passes {
        let missing: Vec<String> = lint
            .findings
            .iter()
            .filter(|f| f.severity == ContractSeverity::Block)
            .map(|f| format!("{}: {}", f.clause_id, f.detail))
            .collect();
        return (
            VerifyResult::Incomplete,
            missing,
            lint.blocking_remediations(),
        );
    }

    match RiskMatrix::from_contract(
        contract,
        FailureModeClass::WholeStackBarrier,
        RolloutPhase::Canary,
    ) {
        Ok(matrix) => {
            let risk = RiskGate::default().evaluate(&matrix);
            if risk.gate_passes {
                (VerifyResult::Pass, Vec::new(), Vec::new())
            } else {
                let missing: Vec<String> = risk
                    .findings
                    .iter()
                    .filter(|f| f.severity == ContractSeverity::Block)
                    .map(|f| format!("{}: {}", f.clause_id, f.detail))
                    .collect();
                (
                    VerifyResult::Inconsistent,
                    missing,
                    risk.blocking_remediations(),
                )
            }
        }
        Err(error) => (
            VerifyResult::Inconsistent,
            vec![format!("risk-matrix: {error}")],
            vec!["repair risk-matrix inputs before promotion".to_string()],
        ),
    }
}

// ── Engine ──────────────────────────────────────────────────────────────────

/// The graveyardctl workflow engine over an active-entry corpus.
#[derive(Debug, Clone)]
pub struct GraveyardctlEngine {
    label: String,
    run_id: String,
    corpus: Vec<ActiveEntry>,
    canonical: Vec<CanonicalEntry>,
}

impl GraveyardctlEngine {
    /// Build an engine for a label over the given active-entry corpus.
    #[must_use]
    pub fn new(label: impl Into<String>, corpus: Vec<ActiveEntry>) -> Self {
        let label = label.into();
        let canonical = canonical_corpus();
        let id_seed = stable_hash(&IdSeed {
            label: &label,
            card_ids: corpus.iter().map(ActiveEntry::card_id).collect(),
            entry_ids: corpus.iter().map(|e| e.entry_id.as_str()).collect(),
        });
        let run_id = format!("graveyardctl-{}", short_hash(&id_seed));
        Self {
            label,
            run_id,
            corpus,
            canonical,
        }
    }

    fn reproduction_command(&self, stage: GraveyardctlStage) -> String {
        format!(
            "doctor_frankentui --machine json graveyardctl --stage {} --label {}",
            stage.as_str(),
            self.label
        )
    }

    fn assemble(&self, parts: LineParts) -> GraveyardctlLedgerEntry {
        GraveyardctlLedgerEntry {
            schema_version: GRAVEYARDCTL_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            stage: parts.stage,
            entry_id: parts.entry_id,
            card_id: parts.card_id,
            action: parts.action.to_string(),
            verify_result: parts.verify_result,
            missing_artifacts: parts.missing_artifacts,
            remediation: parts.remediation,
            detail: parts.detail,
            reproduction_command: self.reproduction_command(parts.stage),
        }
    }

    /// Build a non-verify (informational) ledger line: `n/a` verdict, no defects.
    fn info_line(
        &self,
        stage: GraveyardctlStage,
        entry_id: impl Into<String>,
        card_id: impl Into<String>,
        action: &'static str,
        detail: impl Into<String>,
    ) -> GraveyardctlLedgerEntry {
        self.assemble(LineParts {
            stage,
            entry_id: entry_id.into(),
            card_id: card_id.into(),
            action,
            verify_result: VerifyResult::NotApplicable,
            missing_artifacts: Vec::new(),
            remediation: Vec::new(),
            detail: detail.into(),
        })
    }

    /// Build a verify-stage ledger line carrying the verdict, missing artifacts,
    /// and remediation command set.
    fn verify_line(
        &self,
        entry_id: impl Into<String>,
        card_id: impl Into<String>,
        verify_result: VerifyResult,
        missing_artifacts: Vec<String>,
        remediation: Vec<String>,
        detail: impl Into<String>,
    ) -> GraveyardctlLedgerEntry {
        self.assemble(LineParts {
            stage: GraveyardctlStage::Verify,
            entry_id: entry_id.into(),
            card_id: card_id.into(),
            action: "verify-active-entry",
            verify_result,
            missing_artifacts,
            remediation,
            detail: detail.into(),
        })
    }

    /// INDEX: enumerate the active entries with canonical-entry provenance.
    fn index_lines(&self) -> Vec<GraveyardctlLedgerEntry> {
        self.corpus
            .iter()
            .map(|entry| {
                let provenance = self
                    .canonical
                    .iter()
                    .find(|c| c.entry_id == entry.entry_id)
                    .map_or_else(
                        || "no-canonical-provenance".to_string(),
                        |c| {
                            format!(
                                "tier={} archetype={} source={}",
                                tier_str(c.tier),
                                c.archetype,
                                c.source_ref
                            )
                        },
                    );
                self.info_line(
                    GraveyardctlStage::Index,
                    entry.entry_id.clone(),
                    entry.card_id().to_string(),
                    "index-active-entry",
                    format!("{} ({provenance})", entry.contract.title),
                )
            })
            .collect()
    }

    /// SCORE: rank candidate primitives per symptom via the canonical router.
    fn score_lines(&self) -> Vec<GraveyardctlLedgerEntry> {
        let hotspots: Vec<MigrationHotspot> = Vec::new();
        let exceptions: Vec<PolicyException> = Vec::new();
        let report = SymptomRouter::default().evaluate(&self.canonical, &hotspots, &exceptions);
        report
            .recommendations
            .iter()
            .map(|rec| {
                let selected = rec
                    .selected_entry_id
                    .clone()
                    .unwrap_or_else(|| "n/a".to_string());
                let tier = rec.selected_tier.map_or("n/a", tier_str);
                let detail = format!(
                    "symptom={} ranked_pool=[{}] top={selected} tier={tier} fast_start={}",
                    rec.symptom.as_str(),
                    rec.candidate_pool_ids.join(","),
                    rec.fast_start_applied
                );
                self.info_line(
                    GraveyardctlStage::Score,
                    selected,
                    "n/a",
                    "score-symptom-candidates",
                    detail,
                )
            })
            .collect()
    }

    /// The set of canonical entry ids the router fast-start selects.
    fn router_picks(&self) -> Vec<String> {
        let hotspots: Vec<MigrationHotspot> = Vec::new();
        let exceptions: Vec<PolicyException> = Vec::new();
        let report = SymptomRouter::default().evaluate(&self.canonical, &hotspots, &exceptions);
        report
            .recommendations
            .iter()
            .filter_map(|rec| rec.selected_entry_id.clone())
            .collect()
    }

    /// PICK: select the actively-implemented entries (router picks ∩ corpus).
    fn pick_lines(&self) -> Vec<GraveyardctlLedgerEntry> {
        let picks = self.router_picks();
        self.corpus
            .iter()
            .map(|entry| {
                let picked = picks.contains(&entry.entry_id);
                let detail = if picked {
                    format!("{} selected via router fast-start", entry.entry_id)
                } else {
                    format!(
                        "{} active without a router fast-start pick (provenance gap)",
                        entry.entry_id
                    )
                };
                self.info_line(
                    GraveyardctlStage::Pick,
                    entry.entry_id.clone(),
                    entry.card_id().to_string(),
                    "pick-active-entry",
                    detail,
                )
            })
            .collect()
    }

    /// SCAFFOLD: emit the entry-header draft, graduation template, and LOC/effort
    /// estimate for each active entry.
    fn scaffold_lines(&self) -> Vec<GraveyardctlLedgerEntry> {
        self.corpus
            .iter()
            .map(|entry| {
                let draft = draft_from_contract(&entry.contract);
                let template = generate_graduation_template(quality_bar_for_tier(
                    entry.contract.priority_tier,
                ));
                let loc = entry.contract.graduation.size_estimate_loc.unwrap_or(0);
                let size = SizeEstimate::new(loc, estimate_effort_band(loc));
                let report = EntryHeaderCompiler::new(EntryHeaderCompilerConfig::default())
                    .compile(&draft, &template, Some(&size));
                let effort = report
                    .effort_band
                    .map_or("n/a", crate::recommendation_contract::EffortSize::as_str);
                let detail = format!(
                    "loc={loc} effort={effort} can_progress={} can_graduate={} criteria={}/{}",
                    report.can_progress,
                    report.can_graduate,
                    report.summary.criteria_passed,
                    report.summary.criteria_total
                );
                self.info_line(
                    GraveyardctlStage::Scaffold,
                    entry.entry_id.clone(),
                    entry.card_id().to_string(),
                    "scaffold-entry-header",
                    detail,
                )
            })
            .collect()
    }

    /// VERIFY: run the contract linter + risk gate over each active entry.
    fn verify_lines(&self) -> Vec<GraveyardctlLedgerEntry> {
        self.corpus
            .iter()
            .map(|entry| {
                let (result, missing, remediation) = classify_verify(&entry.contract);
                let detail = format!(
                    "verify={} missing={} remediations={}",
                    result.as_str(),
                    missing.len(),
                    remediation.len()
                );
                self.verify_line(
                    entry.entry_id.clone(),
                    entry.card_id().to_string(),
                    result,
                    missing,
                    remediation,
                    detail,
                )
            })
            .collect()
    }

    /// Compute the full ledger for every stage in canonical order.
    fn full_ledger(&self) -> Vec<GraveyardctlLedgerEntry> {
        let mut ledger = Vec::new();
        ledger.extend(self.index_lines());
        ledger.extend(self.score_lines());
        ledger.extend(self.pick_lines());
        ledger.extend(self.scaffold_lines());
        ledger.extend(self.verify_lines());
        ledger
    }

    /// Build a report for a stage view (`None` = all stages, gate applies).
    #[must_use]
    pub fn run(&self, stage: Option<GraveyardctlStage>) -> GraveyardctlReport {
        let full = self.full_ledger();
        let ledger: Vec<GraveyardctlLedgerEntry> = match stage {
            None => full,
            Some(stage) => full
                .into_iter()
                .filter(|line| line.stage == stage)
                .collect(),
        };

        let gate_applies = matches!(stage, None | Some(GraveyardctlStage::Verify));
        let verify: Vec<&GraveyardctlLedgerEntry> = ledger
            .iter()
            .filter(|line| line.stage == GraveyardctlStage::Verify)
            .collect();
        let verify_total = verify.len();
        let verify_pass = verify
            .iter()
            .filter(|l| l.verify_result == VerifyResult::Pass)
            .count();
        let verify_incomplete = verify
            .iter()
            .filter(|l| l.verify_result == VerifyResult::Incomplete)
            .count();
        let verify_inconsistent = verify
            .iter()
            .filter(|l| l.verify_result == VerifyResult::Inconsistent)
            .count();
        let entries_with_missing_artifacts = ledger
            .iter()
            .filter(|l| !l.missing_artifacts.is_empty())
            .count();

        let required_fields_complete = ledger.iter().all(required_fields_present);
        let all_verify_pass = verify_total > 0 && verify_pass == verify_total;
        let gate_passes = if gate_applies {
            all_verify_pass && required_fields_complete
        } else {
            true
        };

        let stage_label = stage.map_or("all", GraveyardctlStage::as_str).to_string();
        let stages_covered: std::collections::BTreeSet<GraveyardctlStage> =
            ledger.iter().map(|l| l.stage).collect();

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "graveyardctl-{}",
            short_hash(&stable_hash(&ReportIdInput {
                run_id: &self.run_id,
                stage: &stage_label,
                evidence_checksum: &evidence_checksum,
            }))
        );
        let replay_command = format!(
            "doctor_frankentui --machine json graveyardctl --stage {stage_label} --label {}",
            self.label
        );

        let summary = GraveyardctlSummary {
            schema_version: GRAVEYARDCTL_SCHEMA_VERSION.to_string(),
            report_id: report_id.clone(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            stage: stage_label.clone(),
            evidence_checksum: evidence_checksum.clone(),
            active_entries: self.corpus.len(),
            total_ledger_lines: ledger.len(),
            stages_covered: stages_covered.len(),
            verify_total,
            verify_pass,
            verify_incomplete,
            verify_inconsistent,
            entries_with_missing_artifacts,
            required_fields_complete,
            gate_applies,
            gate_passes,
            replay_command: replay_command.clone(),
        };

        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);

        GraveyardctlReport {
            schema_version: GRAVEYARDCTL_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            stage: stage_label,
            evidence_checksum,
            ledger,
            summary,
            gate_applies,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }
}

/// Whether a ledger line has every mandatory string field populated. The
/// `missing_artifacts`/`remediation` vectors are legitimately empty for clean
/// (passing) entries, so they are not required to be non-empty here.
fn required_fields_present(line: &GraveyardctlLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.entry_id.is_empty()
        && !line.card_id.is_empty()
        && !line.action.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

/// Build the entry-header draft from a contract (all nine header fields).
fn draft_from_contract(contract: &RecommendationContract) -> EntryHeaderDraft {
    let status = format!("{:?}", contract.entry_header.status).to_lowercase();
    let budgeted = if contract.budgeted_mode.budgeted {
        format!(
            "budgeted: exhaustion={} fallback={}",
            field_or_tbd(&contract.budgeted_mode.exhaustion_behavior),
            field_or_tbd(&contract.budgeted_mode.conservative_fallback_trigger)
        )
    } else {
        "not budgeted".to_string()
    };

    EntryHeaderDraft::new(contract.card_id.clone())
        .set(HeaderFieldId::Status, header_value(&status))
        .set(
            HeaderFieldId::Tier,
            header_value(tier_str(contract.priority_tier)),
        )
        .set(
            HeaderFieldId::AdoptionWedge,
            header_value(&contract.adoption_wedge),
        )
        .set(HeaderFieldId::BudgetedMode, header_value(&budgeted))
        .set(
            HeaderFieldId::Papers,
            header_value(&contract.inference_model.bayes_factor_refs.join(",")),
        )
        .set(
            HeaderFieldId::Repro,
            option_header_value(contract.failure_mode.repro_artifact.as_deref()),
        )
        .set(
            HeaderFieldId::RealityCheck,
            header_value(&contract.entry_header.reality_check),
        )
        .set(
            HeaderFieldId::Archetype,
            header_value(&contract.entry_header.archetype),
        )
        .set(
            HeaderFieldId::IntegrationTargets,
            header_value(&contract.entry_header.integration_targets.join(",")),
        )
}

fn field_or_tbd(value: &str) -> String {
    if value.trim().is_empty() {
        "tbd".to_string()
    } else {
        value.to_string()
    }
}

fn header_value(value: &str) -> HeaderFieldValue {
    if value.trim().is_empty() {
        HeaderFieldValue::tbd("derived value empty in contract")
    } else {
        HeaderFieldValue::provided(value)
    }
}

fn option_header_value(value: Option<&str>) -> HeaderFieldValue {
    match value {
        Some(value) if !value.trim().is_empty() => HeaderFieldValue::provided(value),
        _ => HeaderFieldValue::tbd("artifact pointer missing in contract"),
    }
}

fn render_ledger_jsonl(ledger: &[GraveyardctlLedgerEntry]) -> String {
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

/// Grouped fields for assembling one ledger line (keeps helper arity low).
struct LineParts {
    stage: GraveyardctlStage,
    entry_id: String,
    card_id: String,
    action: &'static str,
    verify_result: VerifyResult,
    missing_artifacts: Vec<String>,
    remediation: Vec<String>,
    detail: String,
}

#[derive(Serialize)]
struct IdSeed<'a> {
    label: &'a str,
    card_ids: Vec<&'a str>,
    entry_ids: Vec<&'a str>,
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    run_id: &'a str,
    stage: &'a str,
    evidence_checksum: &'a str,
}

// ── Report + summary + stats ────────────────────────────────────────────────

/// Machine-readable summary of one graveyardctl run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardctlSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// The stage view (`all` or a single stage).
    pub stage: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// Number of active entries.
    pub active_entries: usize,
    /// Total emitted ledger lines.
    pub total_ledger_lines: usize,
    /// Distinct stages covered by the emitted ledger.
    pub stages_covered: usize,
    /// Verify-stage entries evaluated.
    pub verify_total: usize,
    /// Verify-stage entries that pass.
    pub verify_pass: usize,
    /// Verify-stage entries blocked as incomplete.
    pub verify_incomplete: usize,
    /// Verify-stage entries blocked as inconsistent.
    pub verify_inconsistent: usize,
    /// Ledger lines that surfaced missing artifacts.
    pub entries_with_missing_artifacts: usize,
    /// Whether every ledger line has all mandatory fields populated.
    pub required_fields_complete: bool,
    /// Whether the verify gate applies to this stage view.
    pub gate_applies: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardctlStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory graveyardctl report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardctlReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// The stage view (`all` or a single stage).
    pub stage: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// The emitted evidence ledger.
    pub ledger: Vec<GraveyardctlLedgerEntry>,
    /// Aggregate summary.
    pub summary: GraveyardctlSummary,
    /// Whether the verify gate applies to this stage view.
    pub gate_applies: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: GraveyardctlStatsArtifact,
}

impl GraveyardctlReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &GraveyardctlSummary,
    ledger: &[GraveyardctlLedgerEntry],
) -> GraveyardctlStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a GraveyardctlSummary,
        ledger: &'a [GraveyardctlLedgerEntry],
    }

    let payload = Export {
        schema_version: GRAVEYARDCTL_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    GraveyardctlStatsArtifact {
        path: format!("{report_id}/graveyardctl_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Report builders ─────────────────────────────────────────────────────────

/// Run the full graveyardctl workflow over the default active corpus.
#[must_use]
pub fn run_graveyardctl_report(label: &str) -> GraveyardctlReport {
    GraveyardctlEngine::new(label, default_active_corpus()).run(None)
}

/// Run a single stage view over the default active corpus.
#[must_use]
pub fn run_graveyardctl_report_for_stage(
    label: &str,
    stage: Option<GraveyardctlStage>,
) -> GraveyardctlReport {
    GraveyardctlEngine::new(label, default_active_corpus()).run(stage)
}

/// Run a stage view over a custom active corpus (used for negative gates).
#[must_use]
pub fn run_graveyardctl_report_with_corpus(
    label: &str,
    stage: Option<GraveyardctlStage>,
    corpus: Vec<ActiveEntry>,
) -> GraveyardctlReport {
    GraveyardctlEngine::new(label, corpus).run(stage)
}

// ── Pipeline (materialized artifacts) ───────────────────────────────────────

/// Configuration for the materialized graveyardctl pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraveyardctlConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
    /// The stage view to materialize (`None` = all stages, gate applies).
    pub stage: Option<GraveyardctlStage>,
}

impl Default for GraveyardctlConfig {
    fn default() -> Self {
        Self {
            run_name: "graveyardctl".to_string(),
            label: "graveyardctl/e2e".to_string(),
            stage: None,
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardctlArtifact {
    /// Logical artifact name.
    pub name: String,
    /// Relative file name within the run directory.
    pub file: String,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// Byte length of the file content.
    pub bytes: u64,
}

/// The outcome of running and materializing the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardctlPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL evidence ledger.
    pub ledger_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the JSON-stats artifact.
    pub stats_path: String,
    /// The machine-readable summary.
    pub summary: GraveyardctlSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<GraveyardctlArtifact>,
}

fn artifact_of(file: &str, content: &str) -> GraveyardctlArtifact {
    GraveyardctlArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the workflow and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// Writes a JSONL evidence ledger (one [`GraveyardctlLedgerEntry`] per line), a
/// `pipeline_summary.json`, an `artifact_manifest.json`, and the JSON-stats
/// artifact. Artifacts are always written (so red-path triage is possible); the
/// returned summary's `gate_passes` reflects the verify gate.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_graveyardctl_pipeline(
    run_root: &Path,
    config: &GraveyardctlConfig,
) -> crate::error::Result<GraveyardctlPipelineOutcome> {
    let report = run_graveyardctl_report_for_stage(&config.label, config.stage);

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "graveyardctl_stats.json";
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
        stage: &'a str,
        gate_applies: bool,
        gate_passes: bool,
        artifacts: &'a [GraveyardctlArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: GRAVEYARDCTL_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        stage: &report.stage,
        gate_applies: report.gate_applies,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(GraveyardctlPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ─────────────────────────────────────────────────────────────────────

/// The stage selector for the `graveyardctl` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GraveyardctlStageArg {
    /// Run all stages and apply the verify gate (default; the CI gate).
    All,
    /// Enumerate the active recommendation entries.
    Index,
    /// Rank candidate primitives per symptom.
    Score,
    /// Select the actively-implemented entries.
    Pick,
    /// Emit entry-header draft + graduation template + LOC estimate.
    Scaffold,
    /// Run the contract linter + risk gate; apply the verify gate.
    Verify,
}

impl GraveyardctlStageArg {
    fn to_stage(self) -> Option<GraveyardctlStage> {
        match self {
            Self::All => None,
            Self::Index => Some(GraveyardctlStage::Index),
            Self::Score => Some(GraveyardctlStage::Score),
            Self::Pick => Some(GraveyardctlStage::Pick),
            Self::Scaffold => Some(GraveyardctlStage::Scaffold),
            Self::Verify => Some(GraveyardctlStage::Verify),
        }
    }
}

/// CLI arguments for the `graveyardctl` executable workflow command.
#[derive(Debug, clap::Args)]
pub struct GraveyardctlArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/graveyardctl"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "graveyardctl")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "graveyardctl/e2e")]
    pub label: String,

    /// Which stage(s) to run. `all` (default) applies the verify gate.
    #[arg(long = "stage", value_enum, default_value_t = GraveyardctlStageArg::All)]
    pub stage: GraveyardctlStageArg,
}

/// Run the `graveyardctl` command: materialize the pipeline and apply the verify
/// gate (when the stage view includes verify).
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the
/// verify gate fails (an active entry is incomplete or inconsistent), or an I/O
/// error if artifacts cannot be materialized.
pub fn run_graveyardctl(args: GraveyardctlArgs) -> crate::error::Result<()> {
    let config = GraveyardctlConfig {
        run_name: args.run_name,
        label: args.label,
        stage: args.stage.to_stage(),
    };
    let outcome = run_graveyardctl_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("graveyardctl workflow"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!("stage: {}", summary.stage));
        ui.info(&format!(
            "active entries: {} | ledger lines: {} | stages covered: {}",
            summary.active_entries, summary.total_ledger_lines, summary.stages_covered
        ));
        ui.info(&format!(
            "verify: {}/{} pass (incomplete={}, inconsistent={})",
            summary.verify_pass,
            summary.verify_total,
            summary.verify_incomplete,
            summary.verify_inconsistent
        ));
        if !summary.gate_applies {
            ui.info("verify gate not applicable to this stage view");
        } else if summary.gate_passes {
            ui.success("graveyardctl verify gate PASSED");
        } else {
            ui.error("graveyardctl verify gate FAILED");
        }
    }

    if summary.gate_applies && !summary.gate_passes {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "graveyardctl verify gate failed: {}/{} active entries passed verify (incomplete={}, inconsistent={}, required_fields_complete={})",
                summary.verify_pass,
                summary.verify_total,
                summary.verify_incomplete,
                summary.verify_inconsistent,
                summary.required_fields_complete
            ),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incomplete_corpus() -> Vec<ActiveEntry> {
        let mut corpus = default_active_corpus();
        corpus[2] = ActiveEntry {
            entry_id: "gv-lockfree-queue".to_string(),
            contract: incomplete_contract_for(
                "gv-lockfree-queue",
                "rc-lockfree-queue",
                "Lock-free MPSC with hazard pointers",
                "claim-lockfree-queue",
            ),
        };
        corpus
    }

    fn inconsistent_corpus() -> Vec<ActiveEntry> {
        let mut corpus = default_active_corpus();
        corpus[1] = ActiveEntry {
            entry_id: "gv-metamorphic-equiv".to_string(),
            contract: inconsistent_contract_for(
                "gv-metamorphic-equiv",
                "rc-metamorphic-equiv",
                "Metamorphic equivalence oracle",
                "claim-metamorphic-equiv",
            ),
        };
        corpus
    }

    #[test]
    fn default_corpus_passes_the_verify_gate() {
        let report = run_graveyardctl_report("graveyardctl/test");
        assert!(report.gate_applies);
        assert!(report.gate_passes, "default corpus must pass the gate");
        assert_eq!(report.summary.verify_total, 3);
        assert_eq!(report.summary.verify_pass, 3);
        assert_eq!(report.summary.verify_incomplete, 0);
        assert_eq!(report.summary.verify_inconsistent, 0);
        assert!(report.summary.required_fields_complete);
    }

    #[test]
    fn all_stages_emit_lines_and_cover_every_stage() {
        let report = run_graveyardctl_report("graveyardctl/test");
        let stages: std::collections::BTreeSet<GraveyardctlStage> =
            report.ledger.iter().map(|l| l.stage).collect();
        assert_eq!(stages.len(), GraveyardctlStage::ALL.len());
        // 3 active entries × {index, pick, scaffold, verify} + 5 symptom score lines.
        assert_eq!(report.summary.total_ledger_lines, 3 * 4 + 5);
    }

    #[test]
    fn every_ledger_line_carries_ac3_fields() {
        let report = run_graveyardctl_report("graveyardctl/test");
        for line in &report.ledger {
            assert!(!line.run_id.is_empty(), "run_id");
            assert!(!line.entry_id.is_empty(), "entry_id");
            assert!(!line.card_id.is_empty(), "card_id");
            assert!(!line.action.is_empty(), "action");
            assert!(!line.detail.is_empty(), "detail");
            assert!(
                !line.reproduction_command.is_empty(),
                "reproduction_command"
            );
        }
    }

    #[test]
    fn incomplete_active_entry_fails_the_gate() {
        let report =
            run_graveyardctl_report_with_corpus("graveyardctl/test", None, incomplete_corpus());
        assert!(!report.gate_passes, "incomplete entry must fail the gate");
        assert_eq!(report.summary.verify_incomplete, 1);
        let incomplete = report
            .ledger
            .iter()
            .find(|l| {
                l.stage == GraveyardctlStage::Verify && l.verify_result == VerifyResult::Incomplete
            })
            .expect("an incomplete verify line");
        assert!(
            !incomplete.missing_artifacts.is_empty(),
            "missing_artifacts must be populated"
        );
        assert!(
            !incomplete.remediation.is_empty(),
            "remediation must be populated"
        );
        // The artifact-contract dimensions (budgets, safe-mode, repro/provenance)
        // must be surfaced.
        let joined = incomplete.missing_artifacts.join(" ").to_lowercase();
        assert!(
            joined.contains("reproduction") || joined.contains("provenance"),
            "repro/provenance defect surfaced: {joined}"
        );
    }

    #[test]
    fn inconsistent_active_entry_fails_the_gate() {
        let report =
            run_graveyardctl_report_with_corpus("graveyardctl/test", None, inconsistent_corpus());
        assert!(!report.gate_passes, "inconsistent entry must fail the gate");
        assert_eq!(report.summary.verify_inconsistent, 1);
        let inconsistent = report
            .ledger
            .iter()
            .find(|l| {
                l.stage == GraveyardctlStage::Verify
                    && l.verify_result == VerifyResult::Inconsistent
            })
            .expect("an inconsistent verify line");
        assert!(
            !inconsistent.missing_artifacts.is_empty(),
            "risk findings surfaced"
        );
        assert!(!inconsistent.remediation.is_empty());
    }

    #[test]
    fn verify_stage_view_applies_the_gate_other_stages_do_not() {
        let verify =
            run_graveyardctl_report_for_stage("graveyardctl/test", Some(GraveyardctlStage::Verify));
        assert!(verify.gate_applies);
        assert!(
            verify
                .ledger
                .iter()
                .all(|l| l.stage == GraveyardctlStage::Verify)
        );

        for stage in [
            GraveyardctlStage::Index,
            GraveyardctlStage::Score,
            GraveyardctlStage::Pick,
            GraveyardctlStage::Scaffold,
        ] {
            let report = run_graveyardctl_report_for_stage("graveyardctl/test", Some(stage));
            assert!(!report.gate_applies, "{stage:?} must not apply the gate");
            assert!(report.gate_passes, "{stage:?} informational view passes");
            assert!(report.ledger.iter().all(|l| l.stage == stage));
        }
    }

    #[test]
    fn score_and_pick_are_deterministic() {
        let a = run_graveyardctl_report("graveyardctl/test");
        let b = run_graveyardctl_report("graveyardctl/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());

        // Pick lines reference the router's S-tier fast-start selections.
        let picks: Vec<&str> = a
            .ledger
            .iter()
            .filter(|l| l.stage == GraveyardctlStage::Pick)
            .map(|l| l.entry_id.as_str())
            .collect();
        assert!(picks.contains(&"gv-erasure-coding"));
        assert!(picks.contains(&"gv-metamorphic-equiv"));
        assert!(picks.contains(&"gv-lockfree-queue"));
    }

    #[test]
    fn pipeline_materializes_artifacts_and_manifest_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_graveyardctl_pipeline(dir.path(), &GraveyardctlConfig::default()).unwrap();
        assert!(std::path::Path::new(&outcome.ledger_path).exists());
        assert!(std::path::Path::new(&outcome.summary_path).exists());
        assert!(std::path::Path::new(&outcome.manifest_path).exists());
        assert!(std::path::Path::new(&outcome.stats_path).exists());
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(u64::try_from(bytes.len()).unwrap(), artifact.bytes);
            assert_eq!(sha256_hex(&bytes), artifact.sha256);
        }
        assert!(outcome.summary.gate_passes);
    }

    #[test]
    fn pipeline_is_deterministic_across_run_roots() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = run_graveyardctl_pipeline(dir_a.path(), &GraveyardctlConfig::default()).unwrap();
        let b = run_graveyardctl_pipeline(dir_b.path(), &GraveyardctlConfig::default()).unwrap();
        assert_eq!(a.summary.report_id, b.summary.report_id);
        assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
        let ledger_a = std::fs::read_to_string(&a.ledger_path).unwrap();
        let ledger_b = std::fs::read_to_string(&b.ledger_path).unwrap();
        assert_eq!(ledger_a, ledger_b);
    }

    #[test]
    fn index_provenance_links_to_canonical_corpus() {
        let report =
            run_graveyardctl_report_for_stage("graveyardctl/test", Some(GraveyardctlStage::Index));
        assert_eq!(report.ledger.len(), 3);
        for line in &report.ledger {
            assert!(
                line.detail.contains("tier=") && line.detail.contains("archetype="),
                "index line carries canonical provenance: {}",
                line.detail
            );
        }
    }
}

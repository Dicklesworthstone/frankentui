//! E2E release-candidate gate (bd-3bxhj.9.9): the RC-convergence capstone
//! that composes every operational gauntlet into one fail-closed go/no-go
//! decision for migration-service release candidates.
//!
//! Eight sections run end to end, each driving a real engine and folding
//! its verdict, evidence checksum, and replay command into the RC ledger:
//!
//! - operator certification: the six headless operator workflows,
//!   including certification accept, tamper refusal, and failure triage;
//! - formal assurance: optional stopping, conformal coverage, drift, and
//!   explainability replay;
//! - graveyard verify: contract completeness, reproducibility, and
//!   fallback integrity;
//! - graveyard chain: route -> rank -> contract -> verify -> demo ->
//!   release across green and red campaigns;
//! - deep assurance: streaming fusion, sequential FDR, counterfactuals,
//!   degradation/recovery, guarantee faults, and UX contracts;
//! - chaos drill: reverse-round perturbations with safe degradation;
//! - optimization gauntlet: the profile-first loop with one-lever,
//!   isomorphism, regression-rollback, and drift scenarios (the RC
//!   hotspot/score context carrier);
//! - multi-round drill: Round1 -> Round2 -> Round3 tier progression with
//!   the Round3 rollback rehearsal.
//!
//! The release fails closed (AC3) unless: rollback readiness is proven
//! (optimization rollback restored AND the multi-round rehearsal passed),
//! behavior-regression proofs held (red-path decisions safe AND graveyard
//! verify green), and drift signals resolved (the drift scenario reordered
//! the backlog). Every ledger line carries the section span, the
//! sub-report id + evidence checksum, the policy verdict, hotspot/score
//! context where the section produces it, and the sub-report's own replay
//! command (AC2). The ledger is float-free, derives `Eq`, and replays
//! byte-identically (AC per .9.9-1 determinism).
//!
//! Precedents: `formal_assurance_gauntlet` (bd-3bxhj.10.28) and
//! `deep_assurance_gauntlet` (bd-3bxhj.10.45).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::chaos_drill::run_chaos_drill_report;
use crate::deep_assurance_gauntlet::run_deep_assurance_gauntlet;
use crate::formal_assurance_gauntlet::run_formal_assurance_gauntlet;
use crate::graveyard_gauntlet::run_graveyard_gauntlet;
use crate::graveyard_verify::run_graveyard_verify_report;
use crate::multi_round_drill::run_multi_round_drill;
use crate::operator_workflows::run_operator_workflows;
use crate::optimization_gauntlet::run_optimization_gauntlet;

/// Schema version for the release-candidate gate report/ledger.
pub const RELEASE_CANDIDATE_GATE_SCHEMA_VERSION: &str = "release-candidate-gate-v1";

/// Schema version for the materialized pipeline manifest.
pub const RELEASE_CANDIDATE_GATE_PIPELINE_SCHEMA_VERSION: &str =
    "release-candidate-gate-pipeline-v1";

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

/// The eight RC-gate sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RcSection {
    /// Operator workflows incl. certification accept + tamper refusal.
    OperatorCertification,
    /// Formal-assurance gauntlet (optional stopping/coverage/drift/XAI).
    FormalAssurance,
    /// Graveyard-verify completeness/reproducibility/fallback gate.
    GraveyardVerify,
    /// Graveyard-executable chain gauntlet.
    GraveyardChain,
    /// Deep-assurance artifact-coding gauntlet.
    DeepAssurance,
    /// Reverse-round chaos drill.
    ChaosDrill,
    /// Profile-first optimization gauntlet (hotspot/score carrier).
    OptimizationGauntlet,
    /// Multi-round tier-progression drill with rollback rehearsal.
    MultiRoundDrill,
}

impl RcSection {
    /// All sections, in canonical order.
    pub const ALL: [RcSection; 8] = [
        RcSection::OperatorCertification,
        RcSection::FormalAssurance,
        RcSection::GraveyardVerify,
        RcSection::GraveyardChain,
        RcSection::DeepAssurance,
        RcSection::ChaosDrill,
        RcSection::OptimizationGauntlet,
        RcSection::MultiRoundDrill,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RcSection::OperatorCertification => "operator_certification",
            RcSection::FormalAssurance => "formal_assurance",
            RcSection::GraveyardVerify => "graveyard_verify",
            RcSection::GraveyardChain => "graveyard_chain",
            RcSection::DeepAssurance => "deep_assurance",
            RcSection::ChaosDrill => "chaos_drill",
            RcSection::OptimizationGauntlet => "optimization_gauntlet",
            RcSection::MultiRoundDrill => "multi_round_drill",
        }
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One RC-gate ledger line (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic RC run id.
    pub run_id: String,
    /// RC section.
    pub section: RcSection,
    /// Stage span descriptor (AC2).
    pub stage_span: String,
    /// The composed sub-report's deterministic id.
    pub sub_report_id: String,
    /// The composed sub-report's evidence checksum.
    pub sub_evidence_checksum: String,
    /// The section's policy verdict (AC2).
    pub policy_verdict: String,
    /// Hotspot context where the section produces it (AC2; `n/a` else).
    pub hotspot_context: String,
    /// Score context where the section produces it (AC2; `n/a` else).
    pub score_context: String,
    /// Whether the section's gate passed.
    pub gate_passed: bool,
    /// Human-readable detail.
    pub detail: String,
    /// The sub-report's own replay command (AC2).
    pub reproduction_command: String,
}

impl RcLedgerEntry {
    /// Whether every mandated field is populated (sentinels count; empty
    /// strings do not).
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.run_id.is_empty()
            && !self.stage_span.is_empty()
            && !self.sub_report_id.is_empty()
            && !self.sub_evidence_checksum.is_empty()
            && !self.policy_verdict.is_empty()
            && !self.hotspot_context.is_empty()
            && !self.score_context.is_empty()
            && !self.detail.is_empty()
            && !self.reproduction_command.is_empty()
    }
}

/// Render the ledger as one JSON object per line.
#[must_use]
pub fn render_ledger_jsonl(ledger: &[RcLedgerEntry]) -> String {
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

// ── Engine ───────────────────────────────────────────────────────────────────

/// The release-candidate gate engine.
#[derive(Debug, Clone)]
pub struct ReleaseCandidateGate {
    label: String,
    run_id: String,
}

struct SectionOutcome {
    stage_span: String,
    sub_report_id: String,
    sub_evidence_checksum: String,
    policy_verdict: String,
    hotspot_context: String,
    score_context: String,
    gate_passed: bool,
    detail: String,
    reproduction_command: String,
}

impl ReleaseCandidateGate {
    /// Build an engine with a deterministic run id derived from the label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "release-candidate-{}",
            short_hash(&stable_hash(&format!(
                "{RELEASE_CANDIDATE_GATE_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { label, run_id }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn entry(&self, section: RcSection, outcome: SectionOutcome) -> RcLedgerEntry {
        RcLedgerEntry {
            schema_version: RELEASE_CANDIDATE_GATE_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            section,
            stage_span: outcome.stage_span,
            sub_report_id: outcome.sub_report_id,
            sub_evidence_checksum: outcome.sub_evidence_checksum,
            policy_verdict: outcome.policy_verdict,
            hotspot_context: outcome.hotspot_context,
            score_context: outcome.score_context,
            gate_passed: outcome.gate_passed,
            detail: outcome.detail,
            reproduction_command: outcome.reproduction_command,
        }
    }

    /// Run the full release-candidate gate.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn run(&self) -> ReleaseCandidateReport {
        let sub = |suffix: &str| format!("{}/{suffix}", self.label);
        let mut ledger: Vec<RcLedgerEntry> = Vec::new();

        // 1. Operator certification (incl. certification accept + tamper refusal).
        let operator = run_operator_workflows(&sub("operator"));
        ledger.push(self.entry(
            RcSection::OperatorCertification,
            SectionOutcome {
                stage_span: "operator workflows + certification signoff".to_string(),
                sub_report_id: operator.report_id.clone(),
                sub_evidence_checksum: operator.evidence_checksum.clone(),
                policy_verdict: if operator.gate_passes { "pass" } else { "fail" }.to_string(),
                hotspot_context: na(),
                score_context: na(),
                gate_passed: operator.gate_passes,
                detail: format!(
                    "{} workflows, {} red paths surfaced with recovery guidance",
                    operator.summary.total_workflows, operator.summary.red_path_lines
                ),
                reproduction_command: operator.replay_command.clone(),
            },
        ));

        // 2. Formal assurance.
        let formal = run_formal_assurance_gauntlet(&sub("formal"));
        ledger.push(self.entry(
            RcSection::FormalAssurance,
            SectionOutcome {
                stage_span: "optional-stopping / coverage / drift / explainability".to_string(),
                sub_report_id: formal.report_id.clone(),
                sub_evidence_checksum: formal.evidence_checksum.clone(),
                policy_verdict: if formal.gate_passes { "pass" } else { "fail" }.to_string(),
                hotspot_context: na(),
                score_context: na(),
                gate_passed: formal.gate_passes,
                detail: format!(
                    "{} scenarios across {} assurance areas held their safe paths",
                    formal.summary.total_scenarios, formal.summary.areas_covered
                ),
                reproduction_command: formal.replay_command.clone(),
            },
        ));

        // 3. Graveyard verify.
        let verify = run_graveyard_verify_report(&sub("verify"));
        ledger.push(self.entry(
            RcSection::GraveyardVerify,
            SectionOutcome {
                stage_span: "contract completeness / reproducibility / fallback".to_string(),
                sub_report_id: verify.report_id.clone(),
                sub_evidence_checksum: verify.evidence_checksum.clone(),
                policy_verdict: if verify.gate_passes { "pass" } else { "fail" }.to_string(),
                hotspot_context: na(),
                score_context: na(),
                gate_passed: verify.gate_passes,
                detail: format!(
                    "{} scenarios, {} verify dimensions covered",
                    verify.summary.total_scenarios, verify.summary.dimensions_covered
                ),
                reproduction_command: verify.replay_command.clone(),
            },
        ));

        // 4. Graveyard chain.
        let chain = run_graveyard_gauntlet(&sub("chain"));
        ledger.push(self.entry(
            RcSection::GraveyardChain,
            SectionOutcome {
                stage_span: "route -> rank -> contract -> verify -> demo -> release".to_string(),
                sub_report_id: chain.report_id.clone(),
                sub_evidence_checksum: chain.evidence_checksum.clone(),
                policy_verdict: if chain.gate_passes { "pass" } else { "fail" }.to_string(),
                hotspot_context: na(),
                score_context: na(),
                gate_passed: chain.gate_passes,
                detail: format!(
                    "{} scenarios across {} fault families with actionable triage",
                    chain.summary.total_scenarios, chain.summary.families_covered
                ),
                reproduction_command: chain.replay_command.clone(),
            },
        ));

        // 5. Deep assurance.
        let deep = run_deep_assurance_gauntlet(&sub("deep"));
        ledger.push(self.entry(
            RcSection::DeepAssurance,
            SectionOutcome {
                stage_span:
                    "streaming fusion / FDR / counterfactuals / degradation / UX".to_string(),
                sub_report_id: deep.report_id.clone(),
                sub_evidence_checksum: deep.evidence_checksum.clone(),
                policy_verdict: if deep.gate_passes { "pass" } else { "fail" }.to_string(),
                hotspot_context: na(),
                score_context: na(),
                gate_passed: deep.gate_passes,
                detail: format!(
                    "{} scenarios across {} families with a complete evidence pack",
                    deep.summary.total_scenarios, deep.summary.families_covered
                ),
                reproduction_command: deep.replay_command.clone(),
            },
        ));

        // 6. Chaos drill.
        let chaos = run_chaos_drill_report(&sub("chaos"));
        ledger.push(self.entry(
            RcSection::ChaosDrill,
            SectionOutcome {
                stage_span: "reverse-round chaos perturbations".to_string(),
                sub_report_id: chaos.report_id.clone(),
                sub_evidence_checksum: chaos.evidence_checksum.clone(),
                policy_verdict: if chaos.gate_passes { "pass" } else { "fail" }.to_string(),
                hotspot_context: na(),
                score_context: na(),
                gate_passed: chaos.gate_passes,
                detail: "chaos perturbations degraded safely with surfaced fallbacks".to_string(),
                reproduction_command: chaos.replay_command.clone(),
            },
        ));

        // 7. Optimization gauntlet (hotspot/score context carrier).
        let optimization = run_optimization_gauntlet(&sub("optimization"));
        let hotspot_context = optimization
            .ledger
            .iter()
            .find(|line| line.hotspot_id != "n/a" && !line.hotspot_id.is_empty())
            .map_or_else(na, |line| line.hotspot_id.clone());
        let score_context = optimization
            .ledger
            .iter()
            .find(|line| !line.score_terms.is_empty())
            .map_or_else(na, |line| line.score_terms.join(","));
        ledger.push(
            self.entry(
                RcSection::OptimizationGauntlet,
                SectionOutcome {
                    stage_span: "baseline -> profile -> score -> one-lever -> isomorphism -> \
                             reprofile -> rollback"
                        .to_string(),
                    sub_report_id: optimization.summary.report_id.clone(),
                    sub_evidence_checksum: optimization.summary.evidence_checksum.clone(),
                    policy_verdict: if optimization.summary.gate_passes {
                        "pass"
                    } else {
                        "fail"
                    }
                    .to_string(),
                    hotspot_context,
                    score_context,
                    gate_passed: optimization.summary.gate_passes,
                    detail: format!(
                        "{} scenarios; rollback_restored={}, drift_reordered={}",
                        optimization.summary.total_scenarios,
                        optimization.summary.rollback_restored,
                        optimization.summary.drift_reordered
                    ),
                    reproduction_command: optimization.summary.replay_command.clone(),
                },
            ),
        );

        // 8. Multi-round drill.
        let drill = run_multi_round_drill(&sub("drill"));
        ledger.push(self.entry(
            RcSection::MultiRoundDrill,
            SectionOutcome {
                stage_span: "Round1 -> Round2 -> Round3 with rollback rehearsal".to_string(),
                sub_report_id: drill.report_id.clone(),
                sub_evidence_checksum: drill.evidence_checksum.clone(),
                policy_verdict: if drill.gate_passes { "pass" } else { "fail" }.to_string(),
                hotspot_context: na(),
                score_context: na(),
                gate_passed: drill.gate_passes,
                detail: format!(
                    "tier progression complete; rollback_rehearsed={}",
                    drill.summary.rollback_rehearsed
                ),
                reproduction_command: drill.replay_command.clone(),
            },
        ));

        // ── RC clauses (AC3) ────────────────────────────────────────────────
        let rollback_readiness =
            optimization.summary.rollback_restored && drill.summary.rollback_rehearsed;
        let behavior_regression_proofs =
            optimization.summary.red_paths_covered && verify.gate_passes;
        let drift_resolved = optimization.summary.drift_reordered;

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "release-candidate-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );

        let sections_covered = {
            let mut sections: Vec<RcSection> = ledger.iter().map(|e| e.section).collect();
            sections.sort();
            sections.dedup();
            sections.len()
        };
        let required_fields_complete = ledger.iter().all(RcLedgerEntry::has_required_fields);
        let all_sections_passed = ledger.iter().all(|e| e.gate_passed);

        let gate_passes = required_fields_complete
            && sections_covered == RcSection::ALL.len()
            && all_sections_passed
            && rollback_readiness
            && behavior_regression_proofs
            && drift_resolved;

        let replay_command = format!(
            "cargo run -p doctor_frankentui -- release-candidate --label '{}' # report {report_id}",
            self.label
        );

        let summary = ReleaseCandidateSummary {
            schema_version: RELEASE_CANDIDATE_GATE_SCHEMA_VERSION.to_string(),
            report_id: report_id.clone(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.clone(),
            total_sections: sections_covered,
            total_ledger_lines: ledger.len(),
            required_fields_complete,
            all_sections_passed,
            rollback_readiness,
            behavior_regression_proofs,
            drift_resolved,
            gate_passes,
            replay_command: replay_command.clone(),
        };

        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a ReleaseCandidateSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: RELEASE_CANDIDATE_GATE_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let exported_json_stats = ReleaseCandidateStatsArtifact {
            path: format!("release_candidate_gate/{report_id}.json"),
            sha256: sha256_hex(content.as_bytes()),
            content,
        };

        ReleaseCandidateReport {
            schema_version: RELEASE_CANDIDATE_GATE_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            ledger,
            summary,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }
}

/// Run the release-candidate gate under a label.
#[must_use]
pub fn run_release_candidate_gate(label: &str) -> ReleaseCandidateReport {
    ReleaseCandidateGate::new(label).run()
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Aggregate summary + gate booleans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCandidateSummary {
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
    /// Distinct sections exercised.
    pub total_sections: usize,
    /// Ledger lines emitted.
    pub total_ledger_lines: usize,
    /// Every ledger line carries all mandated fields.
    pub required_fields_complete: bool,
    /// Every section's own gate passed.
    pub all_sections_passed: bool,
    /// Rollback readiness proven (AC3).
    pub rollback_readiness: bool,
    /// Behavior-regression proofs held (AC3).
    pub behavior_regression_proofs: bool,
    /// High-risk drift signals resolved (AC3).
    pub drift_resolved: bool,
    /// Fail-closed RC go/no-go verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
}

/// Deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCandidateStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory release-candidate report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCandidateReport {
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
    /// One ledger line per section.
    pub ledger: Vec<RcLedgerEntry>,
    /// Aggregate summary.
    pub summary: ReleaseCandidateSummary,
    /// Fail-closed RC go/no-go verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: ReleaseCandidateStatsArtifact,
}

impl ReleaseCandidateReport {
    /// Render the ledger as JSONL.
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }

    /// The ledger line for a section.
    #[must_use]
    pub fn section(&self, section: RcSection) -> Option<&RcLedgerEntry> {
        self.ledger.iter().find(|e| e.section == section)
    }
}

// ── Pipeline materializer ────────────────────────────────────────────────────

/// Pipeline configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseCandidatePipelineConfig {
    /// Run directory name under the run root.
    pub run_name: String,
    /// Run label.
    pub label: String,
}

impl Default for ReleaseCandidatePipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "release_candidate".to_string(),
            label: "release-candidate/e2e".to_string(),
        }
    }
}

/// A materialized artifact with integrity metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCandidateArtifact {
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
pub struct ReleaseCandidatePipelineOutcome {
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
    pub summary: ReleaseCandidateSummary,
    /// Tracked artifacts (the manifest does not track itself).
    pub artifacts: Vec<ReleaseCandidateArtifact>,
}

fn artifact_of(file: &str, content: &str) -> ReleaseCandidateArtifact {
    ReleaseCandidateArtifact {
        name: file.replace(['.', '/'], "-"),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Materialize the RC evidence bundle under `run_root/<run_name>/`.
pub fn run_release_candidate_pipeline(
    run_root: &Path,
    config: &ReleaseCandidatePipelineConfig,
) -> crate::error::Result<ReleaseCandidatePipelineOutcome> {
    let report = run_release_candidate_gate(&config.label);
    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary).unwrap_or_default();

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "release_candidate_stats.json";
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
        artifacts: &'a [ReleaseCandidateArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: RELEASE_CANDIDATE_GATE_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })
    .unwrap_or_default();
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(ReleaseCandidatePipelineOutcome {
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

/// CLI arguments for the release-candidate subcommand.
#[derive(Debug, clap::Args)]
pub struct ReleaseCandidateArgs {
    /// Root directory for materialized evidence.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/release_candidate"
    )]
    pub run_root: PathBuf,

    /// Run directory name under the run root.
    #[arg(long = "run-name", default_value = "release_candidate")]
    pub run_name: String,

    /// Run label folded into run/report ids.
    #[arg(long = "label", default_value = "release-candidate/e2e")]
    pub label: String,
}

/// Run the release-candidate subcommand (fail-closed).
pub fn run_release_candidate_command(args: ReleaseCandidateArgs) -> crate::error::Result<()> {
    let config = ReleaseCandidatePipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_release_candidate_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("release-candidate gate"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "sections: {} (all passed: {})",
            summary.total_sections, summary.all_sections_passed
        ));
        ui.info(&format!(
            "rollback readiness: {}, regression proofs: {}, drift resolved: {}",
            summary.rollback_readiness, summary.behavior_regression_proofs, summary.drift_resolved
        ));
        if summary.gate_passes {
            ui.success("release-candidate gate PASSED (GO)");
        } else {
            ui.error("release-candidate gate FAILED (NO-GO)");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "release-candidate gate failed: sections_passed={}, rollback_readiness={}, \
                 behavior_regression_proofs={}, drift_resolved={}",
                summary.all_sections_passed,
                summary.rollback_readiness,
                summary.behavior_regression_proofs,
                summary.drift_resolved
            ),
        ))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn report() -> ReleaseCandidateReport {
        run_release_candidate_gate("test")
    }

    #[test]
    fn rc_gate_passes_with_all_sections_green() {
        let report = report();
        assert!(
            report.gate_passes,
            "failing sections: {:?}",
            report
                .ledger
                .iter()
                .filter(|e| !e.gate_passed)
                .map(|e| e.section)
                .collect::<Vec<_>>()
        );
        assert_eq!(report.summary.total_sections, 8);
        assert!(report.summary.all_sections_passed);
        assert!(report.summary.rollback_readiness);
        assert!(report.summary.behavior_regression_proofs);
        assert!(report.summary.drift_resolved);
        assert!(report.summary.required_fields_complete);
    }

    #[test]
    fn every_section_carries_operational_trace() {
        let report = report();
        assert_eq!(report.ledger.len(), 8);
        for entry in &report.ledger {
            assert!(entry.has_required_fields(), "incomplete: {entry:?}");
            assert_ne!(entry.sub_report_id, "n/a");
            assert_ne!(entry.sub_evidence_checksum, "n/a");
            assert_eq!(entry.policy_verdict, "pass");
            assert!(!entry.reproduction_command.is_empty());
        }
    }

    #[test]
    fn optimization_section_carries_hotspot_and_score_context() {
        let report = report();
        let opt = report
            .section(RcSection::OptimizationGauntlet)
            .expect("optimization section");
        assert_ne!(opt.hotspot_context, "n/a");
        assert_ne!(opt.score_context, "n/a");
        assert!(opt.detail.contains("rollback_restored=true"));
        assert!(opt.detail.contains("drift_reordered=true"));
    }

    #[test]
    fn rollback_readiness_is_proven_by_two_independent_sections() {
        let report = report();
        let opt = report
            .section(RcSection::OptimizationGauntlet)
            .expect("optimization section");
        let drill = report
            .section(RcSection::MultiRoundDrill)
            .expect("drill section");
        assert!(opt.gate_passed);
        assert!(drill.gate_passed);
        assert!(drill.detail.contains("rollback_rehearsed=true"));
        assert!(report.summary.rollback_readiness);
    }

    #[test]
    fn report_is_deterministic_and_replays_byte_identically() {
        let first = run_release_candidate_gate("determinism");
        let second = run_release_candidate_gate("determinism");
        assert_eq!(first, second);
        assert_eq!(first.render_ledger_jsonl(), second.render_ledger_jsonl());
    }

    #[test]
    fn ledger_jsonl_has_one_line_per_section() {
        let report = report();
        assert_eq!(report.render_ledger_jsonl().lines().count(), 8);
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
            run_release_candidate_pipeline(dir.path(), &ReleaseCandidatePipelineConfig::default())
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
        #![proptest_config(ProptestConfig::with_cases(4))]

        #[test]
        fn prop_rc_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_release_candidate_gate(&label);
            prop_assert!(report.gate_passes);
            prop_assert_eq!(report.summary.total_sections, 8);
        }
    }
}

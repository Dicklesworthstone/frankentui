//! Killer-demo contract: `demo.yaml` + claim linkage + CI-executable scenarios
//! (bd-3bxhj.10.34).
//!
//! Alien differentiators are only real if they are proven by demos that run in
//! CI, finish fast, and tie back to the claim/evidence records used in gate
//! decisions. This module enforces that contract:
//!
//! - **`demo.yaml` schema**: a [`DemoContract`] carries `demo_id`, `claim_id`,
//!   evidence/policy/contract/release-gate linkage, the commands that run the
//!   demo, expected output artifacts with checksums, and a replay scenario.
//!   The file is rendered as deterministic YAML and parsed back with a strict
//!   schema parser; the round-trip must reproduce the contract exactly.
//! - **CI execution hooks** (AC1/AC2): each demo scenario executes the real
//!   in-process migration pipelines through three independent materializations
//!   (golden -> verify -> replay). The verify run must reproduce the golden
//!   artifact hashes (`checksum_verdict`), and the replay run must reproduce
//!   the verify run byte-for-byte (`scenario_replay_status`). Any regression
//!   fails the gate with per-artifact diagnostics and a replay command.
//! - **Provenance + logs** (AC3): the deterministic ledger records `demo_id`,
//!   `claim_id`, `checksum_verdict`, and `scenario_replay_status` per demo,
//!   while the execution log adds measured `duration_ms` against the sub-60s
//!   budget. Claims link into recommendation-contract clauses and release-gate
//!   references.
//!
//! The ledger is float-free and derives [`Eq`], so it replays byte-identically
//! (durations live in the execution log, which is intentionally outside the
//! evidence checksum). The pipeline is exposed through the `killer-demo` CLI
//! command.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::feedback_ingestion::{FeedbackPipelineConfig, run_feedback_ingestion_pipeline};
use crate::flagship_migrations::{FlagshipPipelineConfig, run_flagship_migrations_pipeline};

/// Schema version for the killer-demo contract + report.
pub const KILLER_DEMO_SCHEMA_VERSION: &str = "killer-demo-v1";

/// Schema version for the materialized killer-demo pipeline artifacts.
pub const KILLER_DEMO_PIPELINE_SCHEMA_VERSION: &str = "killer-demo-pipeline-v1";

/// The sub-minute wall-clock budget every killer demo must respect (AC1).
pub const KILLER_DEMO_BUDGET_SECONDS: u64 = 60;

// ── Hashing helpers ──────────────────────────────────────────────────────────

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

// ── Contract schema ──────────────────────────────────────────────────────────

/// The demo scenario kind (which in-process pipeline the demo executes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoKind {
    /// Migration-focused: materialize the flagship migration evidence packs.
    FlagshipMigrationPacks,
    /// Governance-focused: run the privacy-enforced feedback ingestion loop.
    FeedbackGovernance,
}

impl DemoKind {
    /// Stable lowercase category tag.
    #[must_use]
    pub fn category(self) -> &'static str {
        match self {
            Self::FlagshipMigrationPacks => "migration",
            Self::FeedbackGovernance => "governance",
        }
    }
}

/// One expected output artifact with its reproducible checksum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedOutput {
    /// Relative artifact path within the demo's materialization directory.
    pub artifact: String,
    /// SHA-256 of the artifact content.
    pub sha256: String,
}

/// The replay scenario attached to a demo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayScenario {
    /// Stable scenario id.
    pub scenario_id: String,
    /// Human-readable replay steps.
    pub steps: Vec<String>,
}

/// The `demo.yaml` contract for one killer demo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemoContract {
    /// Contract schema version.
    pub schema_version: String,
    /// Stable demo id.
    pub demo_id: String,
    /// The demo category (`migration` / `governance`).
    pub category: String,
    /// The claim this demo proves.
    pub claim_id: String,
    /// The evidence record the claim consumes.
    pub evidence_id: String,
    /// The policy in force for the gate decision.
    pub policy_id: String,
    /// The recommendation-contract clause the claim traces to.
    pub contract_ref: String,
    /// The release-gate clause consuming this demo's evidence.
    pub release_gate_ref: String,
    /// Wall-clock budget in seconds (must be `<= 60`).
    pub max_duration_seconds: u64,
    /// The commands that run the demo.
    pub commands: Vec<String>,
    /// The replay scenario.
    pub replay_scenario: ReplayScenario,
    /// Expected output artifacts with reproducible checksums.
    pub expected_outputs: Vec<ExpectedOutput>,
}

impl DemoContract {
    fn has_required_fields(&self) -> bool {
        !self.schema_version.is_empty()
            && !self.demo_id.is_empty()
            && !self.category.is_empty()
            && !self.claim_id.is_empty()
            && !self.evidence_id.is_empty()
            && !self.policy_id.is_empty()
            && !self.contract_ref.is_empty()
            && !self.release_gate_ref.is_empty()
            && self.max_duration_seconds > 0
            && self.max_duration_seconds <= KILLER_DEMO_BUDGET_SECONDS
            && !self.commands.is_empty()
            && self.commands.iter().all(|c| !c.trim().is_empty())
            && !self.replay_scenario.scenario_id.is_empty()
            && !self.replay_scenario.steps.is_empty()
            && !self.expected_outputs.is_empty()
            && self
                .expected_outputs
                .iter()
                .all(|o| !o.artifact.is_empty() && !o.sha256.is_empty())
    }
}

// ── demo.yaml rendering + strict parsing ─────────────────────────────────────

/// Render a [`DemoContract`] as deterministic `demo.yaml` content.
#[must_use]
pub fn render_demo_yaml(contract: &DemoContract) -> String {
    let mut out = String::new();
    out.push_str(&format!("schema_version: {}\n", contract.schema_version));
    out.push_str(&format!("demo_id: {}\n", contract.demo_id));
    out.push_str(&format!("category: {}\n", contract.category));
    out.push_str(&format!("claim_id: {}\n", contract.claim_id));
    out.push_str(&format!("evidence_id: {}\n", contract.evidence_id));
    out.push_str(&format!("policy_id: {}\n", contract.policy_id));
    out.push_str(&format!("contract_ref: {}\n", contract.contract_ref));
    out.push_str(&format!(
        "release_gate_ref: {}\n",
        contract.release_gate_ref
    ));
    out.push_str(&format!(
        "max_duration_seconds: {}\n",
        contract.max_duration_seconds
    ));
    out.push_str("commands:\n");
    for command in &contract.commands {
        out.push_str(&format!("  - {command}\n"));
    }
    out.push_str("replay_scenario:\n");
    out.push_str(&format!(
        "  scenario_id: {}\n",
        contract.replay_scenario.scenario_id
    ));
    out.push_str("  steps:\n");
    for step in &contract.replay_scenario.steps {
        out.push_str(&format!("    - {step}\n"));
    }
    out.push_str("expected_outputs:\n");
    for output in &contract.expected_outputs {
        out.push_str(&format!("  - artifact: {}\n", output.artifact));
        out.push_str(&format!("    sha256: {}\n", output.sha256));
    }
    out
}

/// Parse `demo.yaml` content produced by [`render_demo_yaml`].
///
/// This is a strict schema parser for the killer-demo contract subset of YAML
/// (scalars, string lists, and the fixed nested shapes above) — unknown keys,
/// wrong indentation, or missing sections are errors, so a drifting contract
/// fails loudly instead of being silently misread.
///
/// # Errors
/// Returns a human-readable description of the first schema violation.
pub fn parse_demo_yaml(content: &str) -> Result<DemoContract, String> {
    #[derive(PartialEq)]
    enum Section {
        Top,
        Commands,
        ReplayScenario,
        ReplaySteps,
        ExpectedOutputs,
    }

    let mut schema_version = None;
    let mut demo_id = None;
    let mut category = None;
    let mut claim_id = None;
    let mut evidence_id = None;
    let mut policy_id = None;
    let mut contract_ref = None;
    let mut release_gate_ref = None;
    let mut max_duration_seconds: Option<u64> = None;
    let mut commands: Vec<String> = Vec::new();
    let mut scenario_id = None;
    let mut steps: Vec<String> = Vec::new();
    let mut expected_outputs: Vec<ExpectedOutput> = Vec::new();
    let mut pending_artifact: Option<String> = None;
    let mut section = Section::Top;

    for (index, raw) in content.lines().enumerate() {
        let line_no = index + 1;
        if raw.trim().is_empty() {
            continue;
        }
        // Nested list/section lines first (most-indented prefixes win).
        if let Some(step) = raw.strip_prefix("    - ") {
            if section != Section::ReplaySteps {
                return Err(format!("line {line_no}: unexpected nested list item"));
            }
            steps.push(step.to_string());
            continue;
        }
        if let Some(rest) = raw.strip_prefix("    sha256: ") {
            if section != Section::ExpectedOutputs {
                return Err(format!("line {line_no}: sha256 outside expected_outputs"));
            }
            let Some(artifact) = pending_artifact.take() else {
                return Err(format!(
                    "line {line_no}: sha256 without a preceding artifact"
                ));
            };
            expected_outputs.push(ExpectedOutput {
                artifact,
                sha256: rest.to_string(),
            });
            continue;
        }
        if let Some(rest) = raw.strip_prefix("  - artifact: ") {
            if section != Section::ExpectedOutputs {
                return Err(format!("line {line_no}: artifact outside expected_outputs"));
            }
            if pending_artifact.is_some() {
                return Err(format!("line {line_no}: artifact entry missing its sha256"));
            }
            pending_artifact = Some(rest.to_string());
            continue;
        }
        if let Some(item) = raw.strip_prefix("  - ") {
            if section != Section::Commands {
                return Err(format!("line {line_no}: unexpected list item"));
            }
            commands.push(item.to_string());
            continue;
        }
        if let Some(rest) = raw.strip_prefix("  scenario_id: ") {
            if section != Section::ReplayScenario {
                return Err(format!(
                    "line {line_no}: scenario_id outside replay_scenario"
                ));
            }
            scenario_id = Some(rest.to_string());
            continue;
        }
        if raw == "  steps:" {
            if section != Section::ReplayScenario {
                return Err(format!("line {line_no}: steps outside replay_scenario"));
            }
            section = Section::ReplaySteps;
            continue;
        }
        // Top-level keys / section openers.
        match raw {
            "commands:" => {
                section = Section::Commands;
                continue;
            }
            "replay_scenario:" => {
                section = Section::ReplayScenario;
                continue;
            }
            "expected_outputs:" => {
                section = Section::ExpectedOutputs;
                continue;
            }
            _ => {}
        }
        let Some((key, value)) = raw.split_once(": ") else {
            return Err(format!("line {line_no}: malformed line '{raw}'"));
        };
        section = Section::Top;
        match key {
            "schema_version" => schema_version = Some(value.to_string()),
            "demo_id" => demo_id = Some(value.to_string()),
            "category" => category = Some(value.to_string()),
            "claim_id" => claim_id = Some(value.to_string()),
            "evidence_id" => evidence_id = Some(value.to_string()),
            "policy_id" => policy_id = Some(value.to_string()),
            "contract_ref" => contract_ref = Some(value.to_string()),
            "release_gate_ref" => release_gate_ref = Some(value.to_string()),
            "max_duration_seconds" => {
                max_duration_seconds = Some(
                    value
                        .parse::<u64>()
                        .map_err(|e| format!("line {line_no}: bad max_duration_seconds: {e}"))?,
                );
            }
            other => return Err(format!("line {line_no}: unknown key '{other}'")),
        }
    }

    if pending_artifact.is_some() {
        return Err("trailing artifact entry missing its sha256".to_string());
    }

    Ok(DemoContract {
        schema_version: schema_version.ok_or("missing schema_version")?,
        demo_id: demo_id.ok_or("missing demo_id")?,
        category: category.ok_or("missing category")?,
        claim_id: claim_id.ok_or("missing claim_id")?,
        evidence_id: evidence_id.ok_or("missing evidence_id")?,
        policy_id: policy_id.ok_or("missing policy_id")?,
        contract_ref: contract_ref.ok_or("missing contract_ref")?,
        release_gate_ref: release_gate_ref.ok_or("missing release_gate_ref")?,
        max_duration_seconds: max_duration_seconds.ok_or("missing max_duration_seconds")?,
        commands,
        replay_scenario: ReplayScenario {
            scenario_id: scenario_id.ok_or("missing replay_scenario.scenario_id")?,
            steps,
        },
        expected_outputs,
    })
}

// ── Demo specs (defaults) ────────────────────────────────────────────────────

/// The specification of one demo scenario before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoSpec {
    /// The demo id.
    pub demo_id: String,
    /// Which in-process pipeline the demo executes.
    pub kind: DemoKind,
    /// The claim this demo proves.
    pub claim_id: String,
    /// The evidence record the claim consumes.
    pub evidence_id: String,
    /// The policy in force for the gate decision.
    pub policy_id: String,
    /// The recommendation-contract clause the claim traces to.
    pub contract_ref: String,
    /// The release-gate clause consuming this demo's evidence.
    pub release_gate_ref: String,
    /// Wall-clock budget in seconds.
    pub max_duration_seconds: u64,
}

/// The default killer-demo corpus: one migration-focused demo (AC1) plus one
/// governance demo, both sub-60s and claim-linked.
#[must_use]
pub fn default_demo_specs() -> Vec<DemoSpec> {
    vec![
        DemoSpec {
            demo_id: "demo-flagship-migration-packs".to_string(),
            kind: DemoKind::FlagshipMigrationPacks,
            claim_id: "claim-flagship-packs-reproducible".to_string(),
            evidence_id: "ev-flagship-pack-manifest".to_string(),
            policy_id: "pol-rollout-canary".to_string(),
            contract_ref: "clause-migration-traceability".to_string(),
            release_gate_ref: "release-gate/flagship-evidence".to_string(),
            max_duration_seconds: KILLER_DEMO_BUDGET_SECONDS,
        },
        DemoSpec {
            demo_id: "demo-feedback-governance".to_string(),
            kind: DemoKind::FeedbackGovernance,
            claim_id: "claim-feedback-privacy-enforced".to_string(),
            evidence_id: "ev-feedback-admission-ledger".to_string(),
            policy_id: "pol-feedback-privacy".to_string(),
            contract_ref: "clause-feedback-provenance".to_string(),
            release_gate_ref: "release-gate/feedback-loop".to_string(),
            max_duration_seconds: KILLER_DEMO_BUDGET_SECONDS,
        },
    ]
}

// ── Execution ────────────────────────────────────────────────────────────────

/// One demo's deterministic ledger record (no wall-clock; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemoLedgerEntry {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The demo id.
    pub demo_id: String,
    /// The demo category.
    pub category: String,
    /// The claim id (AC3).
    pub claim_id: String,
    /// The evidence id.
    pub evidence_id: String,
    /// The policy id.
    pub policy_id: String,
    /// The recommendation-contract clause reference.
    pub contract_ref: String,
    /// The release-gate reference.
    pub release_gate_ref: String,
    /// Number of expected output artifacts.
    pub expected_output_count: usize,
    /// Checksum over the expected-output set (provenance root).
    pub expected_outputs_checksum: String,
    /// Whether the demo.yaml round-trip reproduced the contract exactly.
    pub yaml_roundtrip_ok: bool,
    /// The checksum verdict (`match` / `mismatch`) (AC3).
    pub checksum_verdict: String,
    /// Per-artifact checksum diagnostics (empty when everything matches).
    pub checksum_gaps: Vec<String>,
    /// The replay status (`replay_match` / `replay_divergent`) (AC3).
    pub scenario_replay_status: String,
    /// Whether the measured duration fit the contract budget.
    pub within_budget: bool,
    /// Deterministic replay command (AC2).
    pub reproduction_command: String,
}

fn entry_has_required_fields(e: &DemoLedgerEntry) -> bool {
    !e.schema_version.is_empty()
        && !e.run_id.is_empty()
        && !e.demo_id.is_empty()
        && !e.category.is_empty()
        && !e.claim_id.is_empty()
        && !e.evidence_id.is_empty()
        && !e.policy_id.is_empty()
        && !e.contract_ref.is_empty()
        && !e.release_gate_ref.is_empty()
        && e.expected_output_count > 0
        && !e.expected_outputs_checksum.is_empty()
        && !e.checksum_verdict.is_empty()
        && !e.scenario_replay_status.is_empty()
        && !e.reproduction_command.is_empty()
}

/// One demo's execution-log line (adds measured duration; AC3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemoExecutionLogEntry {
    /// The demo id.
    pub demo_id: String,
    /// The claim id.
    pub claim_id: String,
    /// Measured wall-clock duration for golden+verify+replay, in milliseconds.
    pub duration_ms: u64,
    /// The contract budget in milliseconds.
    pub budget_ms: u64,
    /// The checksum verdict.
    pub checksum_verdict: String,
    /// The replay status.
    pub scenario_replay_status: String,
}

/// The result of executing one demo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoExecution {
    /// The contract (with golden expectations filled in).
    pub contract: DemoContract,
    /// The deterministic ledger entry.
    pub ledger_entry: DemoLedgerEntry,
    /// The execution-log line (with measured duration).
    pub execution_log: DemoExecutionLogEntry,
}

fn materialize_demo(kind: DemoKind, root: &Path) -> crate::error::Result<Vec<ExpectedOutput>> {
    match kind {
        DemoKind::FlagshipMigrationPacks => {
            let outcome = run_flagship_migrations_pipeline(
                root,
                &FlagshipPipelineConfig {
                    run_name: "flagship".to_string(),
                    label: "killer-demo/flagship".to_string(),
                },
            )?;
            Ok(outcome
                .artifacts
                .iter()
                .map(|a| ExpectedOutput {
                    artifact: format!("flagship/{}", a.file),
                    sha256: a.sha256.clone(),
                })
                .collect())
        }
        DemoKind::FeedbackGovernance => {
            let outcome = run_feedback_ingestion_pipeline(
                root,
                &FeedbackPipelineConfig {
                    run_name: "feedback".to_string(),
                    label: "killer-demo/feedback".to_string(),
                },
            )?;
            Ok(outcome
                .artifacts
                .iter()
                .map(|a| ExpectedOutput {
                    artifact: format!("feedback/{}", a.file),
                    sha256: a.sha256.clone(),
                })
                .collect())
        }
    }
}

fn compare_outputs(expected: &[ExpectedOutput], actual: &[ExpectedOutput]) -> Vec<String> {
    let mut gaps = Vec::new();
    let actual_by_path: std::collections::BTreeMap<&str, &str> = actual
        .iter()
        .map(|o| (o.artifact.as_str(), o.sha256.as_str()))
        .collect();
    for exp in expected {
        match actual_by_path.get(exp.artifact.as_str()) {
            None => gaps.push(format!("artifact '{}' missing from run", exp.artifact)),
            Some(got) if *got != exp.sha256 => gaps.push(format!(
                "artifact '{}' checksum drift: expected {} got {}",
                exp.artifact,
                short_hash(&exp.sha256),
                short_hash(got)
            )),
            Some(_) => {}
        }
    }
    let expected_paths: BTreeSet<&str> = expected.iter().map(|o| o.artifact.as_str()).collect();
    for act in actual {
        if !expected_paths.contains(act.artifact.as_str()) {
            gaps.push(format!("unexpected artifact '{}' in run", act.artifact));
        }
    }
    gaps
}

/// The deterministic killer-demo engine.
#[derive(Debug, Clone)]
pub struct KillerDemos {
    run_id: String,
    label: String,
}

impl KillerDemos {
    /// Construct an engine with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "killer-demo-{}",
            short_hash(&stable_hash(&format!(
                "{KILLER_DEMO_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Execute one demo spec: golden materialization (expectations), verify
    /// materialization (checksum verdict), replay materialization (replay
    /// status), and demo.yaml round-trip.
    ///
    /// # Errors
    /// Returns an error only when a materialization cannot write its artifacts;
    /// checksum/replay regressions are reported in the ledger, not as errors.
    pub fn execute_demo(
        &self,
        exec_root: &Path,
        spec: &DemoSpec,
    ) -> crate::error::Result<DemoExecution> {
        let started = Instant::now();
        let demo_root = exec_root.join(&spec.demo_id);

        let golden = materialize_demo(spec.kind, &demo_root.join("golden"))?;
        let contract = DemoContract {
            schema_version: KILLER_DEMO_SCHEMA_VERSION.to_string(),
            demo_id: spec.demo_id.clone(),
            category: spec.kind.category().to_string(),
            claim_id: spec.claim_id.clone(),
            evidence_id: spec.evidence_id.clone(),
            policy_id: spec.policy_id.clone(),
            contract_ref: spec.contract_ref.clone(),
            release_gate_ref: spec.release_gate_ref.clone(),
            max_duration_seconds: spec.max_duration_seconds,
            commands: vec![
                format!(
                    "cargo run -p doctor_frankentui -- killer-demo --run-name {}",
                    spec.demo_id
                ),
                "cargo test -p doctor_frankentui --lib killer_demo".to_string(),
            ],
            replay_scenario: ReplayScenario {
                scenario_id: format!("replay-{}", spec.demo_id),
                steps: vec![
                    "re-run the demo pipeline into a fresh directory".to_string(),
                    "compare every artifact checksum against demo.yaml expected_outputs"
                        .to_string(),
                    "re-run once more and require byte-identical artifacts".to_string(),
                ],
            },
            expected_outputs: golden.clone(),
        };

        let yaml = render_demo_yaml(&contract);
        let yaml_roundtrip_ok = parse_demo_yaml(&yaml).as_ref() == Ok(&contract);

        let verify = materialize_demo(spec.kind, &demo_root.join("verify"))?;
        let checksum_gaps = compare_outputs(&contract.expected_outputs, &verify);
        let checksum_verdict = if checksum_gaps.is_empty() {
            "match"
        } else {
            "mismatch"
        };

        let replay = materialize_demo(spec.kind, &demo_root.join("replay"))?;
        let replay_gaps = compare_outputs(&verify, &replay);
        let scenario_replay_status = if replay_gaps.is_empty() {
            "replay_match"
        } else {
            "replay_divergent"
        };

        // Round sub-millisecond executions up to 1ms so a zero-second budget
        // deterministically fails `within_budget` (0 <= 0 would pass it) and
        // the log never claims a demo took literally no time.
        let duration_ms = u64::try_from(started.elapsed().as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let budget_ms = spec.max_duration_seconds.saturating_mul(1000);
        let within_budget = duration_ms <= budget_ms;

        let ledger_entry = DemoLedgerEntry {
            schema_version: KILLER_DEMO_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            demo_id: spec.demo_id.clone(),
            category: spec.kind.category().to_string(),
            claim_id: spec.claim_id.clone(),
            evidence_id: spec.evidence_id.clone(),
            policy_id: spec.policy_id.clone(),
            contract_ref: spec.contract_ref.clone(),
            release_gate_ref: spec.release_gate_ref.clone(),
            expected_output_count: contract.expected_outputs.len(),
            expected_outputs_checksum: stable_hash(&contract.expected_outputs),
            yaml_roundtrip_ok,
            checksum_verdict: checksum_verdict.to_string(),
            checksum_gaps,
            scenario_replay_status: scenario_replay_status.to_string(),
            within_budget,
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- killer-demo --label '{}' # run {} demo {}",
                self.label, self.run_id, spec.demo_id
            ),
        };
        let execution_log = DemoExecutionLogEntry {
            demo_id: spec.demo_id.clone(),
            claim_id: spec.claim_id.clone(),
            duration_ms,
            budget_ms,
            checksum_verdict: checksum_verdict.to_string(),
            scenario_replay_status: scenario_replay_status.to_string(),
        };

        Ok(DemoExecution {
            contract,
            ledger_entry,
            execution_log,
        })
    }

    /// Execute every spec and produce the report.
    ///
    /// # Errors
    /// Returns an error if a demo materialization cannot write its artifacts.
    pub fn run(
        &self,
        exec_root: &Path,
        specs: &[DemoSpec],
    ) -> crate::error::Result<KillerDemoReport> {
        let mut ordered: Vec<&DemoSpec> = specs.iter().collect();
        ordered.sort_by(|a, b| a.demo_id.cmp(&b.demo_id));

        let mut executions = Vec::new();
        for spec in ordered {
            executions.push(self.execute_demo(exec_root, spec)?);
        }

        let entries: Vec<DemoLedgerEntry> =
            executions.iter().map(|e| e.ledger_entry.clone()).collect();
        let evidence_checksum = stable_hash(&entries);
        let report_id = format!(
            "killer-demo-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&executions, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;

        Ok(KillerDemoReport {
            schema_version: KILLER_DEMO_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            executions,
            summary,
            gate_passes,
        })
    }

    fn summarize(
        &self,
        executions: &[DemoExecution],
        report_id: &str,
        evidence_checksum: &str,
    ) -> KillerDemoSummary {
        let entries: Vec<&DemoLedgerEntry> = executions.iter().map(|e| &e.ledger_entry).collect();
        let required_fields_complete = !entries.is_empty()
            && entries.iter().all(|e| entry_has_required_fields(e))
            && executions.iter().all(|e| e.contract.has_required_fields());
        // AC1: at least one migration-focused killer demo executed.
        let migration_demo_present = entries.iter().any(|e| e.category == "migration");
        // Schema proof: demo.yaml round-trips exactly.
        let yaml_roundtrip_verified = entries.iter().all(|e| e.yaml_roundtrip_ok);
        // AC1/AC2: reproducible hashes + regression detection.
        let checksums_verified = entries.iter().all(|e| e.checksum_verdict == "match");
        let replay_verified = entries
            .iter()
            .all(|e| e.scenario_replay_status == "replay_match");
        // Sub-60s CI budget.
        let within_budget = entries.iter().all(|e| e.within_budget);
        // AC2: diagnostics stay actionable (replay command always present; any
        // mismatch must carry per-artifact gaps).
        let diagnostics_actionable = entries.iter().all(|e| {
            !e.reproduction_command.is_empty()
                && (e.checksum_verdict == "match" || !e.checksum_gaps.is_empty())
        });

        let gate_passes = required_fields_complete
            && migration_demo_present
            && yaml_roundtrip_verified
            && checksums_verified
            && replay_verified
            && within_budget
            && diagnostics_actionable;

        KillerDemoSummary {
            schema_version: KILLER_DEMO_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_demos: entries.len(),
            migration_demos: entries.iter().filter(|e| e.category == "migration").count(),
            required_fields_complete,
            migration_demo_present,
            yaml_roundtrip_verified,
            checksums_verified,
            replay_verified,
            within_budget,
            diagnostics_actionable,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- killer-demo --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

// ── Report + summary ─────────────────────────────────────────────────────────

/// Machine-readable summary of one killer-demo run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillerDemoSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the deterministic ledger.
    pub evidence_checksum: String,
    /// Total demos executed.
    pub total_demos: usize,
    /// Migration-focused demos executed.
    pub migration_demos: usize,
    /// Whether every entry/contract has all mandated fields.
    pub required_fields_complete: bool,
    /// Whether at least one migration-focused demo ran (AC1).
    pub migration_demo_present: bool,
    /// Whether every demo.yaml round-trips exactly.
    pub yaml_roundtrip_verified: bool,
    /// Whether every artifact checksum reproduced (AC1).
    pub checksums_verified: bool,
    /// Whether every replay reproduced byte-identically (AC3).
    pub replay_verified: bool,
    /// Whether every demo fit its sub-60s budget.
    pub within_budget: bool,
    /// Whether regression diagnostics are actionable (AC2).
    pub diagnostics_actionable: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// The in-memory killer-demo report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillerDemoReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the deterministic ledger.
    pub evidence_checksum: String,
    /// Per-demo executions (contract + ledger + execution log).
    pub executions: Vec<DemoExecution>,
    /// Aggregate summary.
    pub summary: KillerDemoSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
}

impl KillerDemoReport {
    /// Render the deterministic ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        let mut out = String::new();
        for execution in &self.executions {
            match serde_json::to_string(&execution.ledger_entry) {
                Ok(line) => out.push_str(&line),
                Err(error) => out.push_str(&error.to_string()),
            }
            out.push('\n');
        }
        out
    }

    /// Render the execution log (with measured durations) as JSONL.
    #[must_use]
    pub fn render_execution_log_jsonl(&self) -> String {
        let mut out = String::new();
        for execution in &self.executions {
            match serde_json::to_string(&execution.execution_log) {
                Ok(line) => out.push_str(&line),
                Err(error) => out.push_str(&error.to_string()),
            }
            out.push('\n');
        }
        out
    }
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized killer-demo pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct KillerDemoConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for KillerDemoConfig {
    fn default() -> Self {
        Self {
            run_name: "killer_demo".to_string(),
            label: "killer-demo/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillerDemoArtifact {
    /// Logical artifact name.
    pub name: String,
    /// Relative file path within the run directory.
    pub file: String,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// Byte length of the file content.
    pub bytes: u64,
}

/// The outcome of running and materializing the pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KillerDemoOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the deterministic JSONL ledger.
    pub ledger_path: String,
    /// Absolute path to the execution log JSONL (with durations).
    pub execution_log_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// The machine-readable summary.
    pub summary: KillerDemoSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<KillerDemoArtifact>,
}

fn artifact_of(file: &str, content: &str) -> KillerDemoArtifact {
    KillerDemoArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .trim_end_matches(".yaml")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Execute the default killer-demo corpus and materialize `demo.yaml` files,
/// the deterministic ledger, the execution log, and the summary/manifest set
/// under `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_killer_demo_pipeline(
    run_root: &Path,
    config: &KillerDemoConfig,
) -> crate::error::Result<KillerDemoOutcome> {
    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let engine = KillerDemos::new(config.label.as_str());
    let report = engine.run(&run_dir.join("exec"), &default_demo_specs())?;

    let mut artifacts: Vec<KillerDemoArtifact> = Vec::new();

    // Per-demo demo.yaml contracts.
    for execution in &report.executions {
        let demo_rel = format!("demos/{}", execution.contract.demo_id);
        let demo_dir = run_dir.join(&demo_rel);
        crate::util::ensure_dir(&demo_dir)?;
        let yaml = render_demo_yaml(&execution.contract);
        crate::util::write_string(&demo_dir.join("demo.yaml"), &yaml)?;
        artifacts.push(artifact_of(&format!("{demo_rel}/demo.yaml"), &yaml));
    }

    let ledger_content = report.render_ledger_jsonl();
    let execution_log_content = report.render_execution_log_jsonl();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let ledger_file = "evidence_ledger.jsonl";
    let execution_log_file = "execution_log.jsonl";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&run_dir.join(execution_log_file), &execution_log_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    artifacts.push(artifact_of(ledger_file, &ledger_content));
    artifacts.push(artifact_of(execution_log_file, &execution_log_content));
    artifacts.push(artifact_of(summary_file, &summary_content));

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        gate_passes: bool,
        artifacts: &'a [KillerDemoArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: KILLER_DEMO_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(KillerDemoOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        execution_log_path: run_dir.join(execution_log_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for the `killer-demo` command.
#[derive(Debug, clap::Args)]
pub struct KillerDemoArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/killer_demo"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "killer_demo")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "killer-demo/e2e")]
    pub label: String,
}

/// Run the `killer-demo` command: execute the killer-demo scenarios, materialize
/// `demo.yaml` contracts + ledgers, and apply the fail-closed reproducibility gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the
/// gate fails (a checksum drift, replay divergence, budget overrun, missing
/// migration demo, or a demo.yaml round-trip failure), or an I/O error if
/// artifacts cannot be materialized.
pub fn run_killer_demo_command(args: KillerDemoArgs) -> crate::error::Result<()> {
    let config = KillerDemoConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_killer_demo_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("killer-demo contract"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "demos: {} | migration demos: {}",
            summary.total_demos, summary.migration_demos
        ));
        ui.info(&format!(
            "checksums: {} | replay: {} | budget: {} | yaml round-trip: {}",
            summary.checksums_verified,
            summary.replay_verified,
            summary.within_budget,
            summary.yaml_roundtrip_verified
        ));
        if summary.gate_passes {
            ui.success("killer-demo gate PASSED");
        } else {
            ui.error("killer-demo gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "killer-demo gate failed: required_fields_complete={}, migration_demo_present={}, yaml_roundtrip_verified={}, checksums_verified={}, replay_verified={}, within_budget={}, diagnostics_actionable={}",
                summary.required_fields_complete,
                summary.migration_demo_present,
                summary.yaml_roundtrip_verified,
                summary.checksums_verified,
                summary.replay_verified,
                summary.within_budget,
                summary.diagnostics_actionable
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn run_default(label: &str) -> KillerDemoReport {
        let dir = tempfile::tempdir().unwrap();
        KillerDemos::new(label)
            .run(dir.path(), &default_demo_specs())
            .unwrap()
    }

    #[test]
    fn default_demos_pass_gate() {
        let report = run_default("kd/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_demos, 2);
        assert_eq!(report.summary.migration_demos, 1);
        assert!(report.summary.migration_demo_present);
        assert!(report.summary.checksums_verified);
        assert!(report.summary.replay_verified);
        assert!(report.summary.within_budget);
        assert!(report.summary.yaml_roundtrip_verified);
    }

    #[test]
    fn ledger_carries_ac3_fields() {
        let report = run_default("kd/test");
        for execution in &report.executions {
            let entry = &execution.ledger_entry;
            assert!(entry_has_required_fields(entry), "entry: {entry:?}");
            assert_eq!(entry.checksum_verdict, "match");
            assert_eq!(entry.scenario_replay_status, "replay_match");
            // Duration is logged in the execution log (AC3).
            assert_eq!(execution.execution_log.demo_id, entry.demo_id);
            assert_eq!(execution.execution_log.claim_id, entry.claim_id);
            assert!(execution.execution_log.budget_ms == 60_000);
            assert!(execution.execution_log.duration_ms <= execution.execution_log.budget_ms);
        }
    }

    #[test]
    fn demo_yaml_round_trips_exactly() {
        let report = run_default("kd/test");
        for execution in &report.executions {
            let yaml = render_demo_yaml(&execution.contract);
            let parsed = parse_demo_yaml(&yaml).expect("parse demo.yaml");
            assert_eq!(parsed, execution.contract);
        }
    }

    #[test]
    fn parser_rejects_unknown_keys_and_malformed_lines() {
        assert!(parse_demo_yaml("bogus_key: x\n").is_err());
        assert!(parse_demo_yaml("schema_version killer-demo-v1\n").is_err());
        assert!(
            parse_demo_yaml("expected_outputs:\n  - artifact: a.json\n").is_err(),
            "artifact without sha256 must fail"
        );
        assert!(parse_demo_yaml("    - orphan step\n").is_err());
    }

    #[test]
    fn parser_requires_all_mandatory_fields() {
        // A structurally valid document missing demo_id must fail.
        let report = run_default("kd/test");
        let yaml = render_demo_yaml(&report.executions[0].contract);
        let without_demo_id: String = yaml
            .lines()
            .filter(|l| !l.starts_with("demo_id: "))
            .map(|l| format!("{l}\n"))
            .collect();
        let err = parse_demo_yaml(&without_demo_id).unwrap_err();
        assert!(err.contains("demo_id"));
    }

    #[test]
    fn checksum_drift_is_detected_with_actionable_gaps() {
        let mut expected = vec![
            ExpectedOutput {
                artifact: "a.json".to_string(),
                sha256: "aaaa".to_string(),
            },
            ExpectedOutput {
                artifact: "b.json".to_string(),
                sha256: "bbbb".to_string(),
            },
        ];
        let actual = vec![
            ExpectedOutput {
                artifact: "a.json".to_string(),
                sha256: "aaaa".to_string(),
            },
            ExpectedOutput {
                artifact: "b.json".to_string(),
                sha256: "cccc".to_string(),
            },
        ];
        let gaps = compare_outputs(&expected, &actual);
        assert_eq!(gaps.len(), 1);
        assert!(gaps[0].contains("b.json"));
        assert!(gaps[0].contains("checksum drift"));

        // Missing + unexpected artifacts are both reported.
        expected.push(ExpectedOutput {
            artifact: "c.json".to_string(),
            sha256: "dddd".to_string(),
        });
        let gaps = compare_outputs(&expected, &actual);
        assert!(gaps.iter().any(|g| g.contains("'c.json' missing")));
    }

    #[test]
    fn tampered_expectations_fail_gate_via_summary() {
        // Simulate a regression by rewriting one ledger entry's verdict the way
        // execute_demo would report a drift, then re-summarize.
        let dir = tempfile::tempdir().unwrap();
        let engine = KillerDemos::new("kd/test");
        let mut report = engine.run(dir.path(), &default_demo_specs()).unwrap();
        report.executions[0].ledger_entry.checksum_verdict = "mismatch".to_string();
        report.executions[0]
            .ledger_entry
            .checksum_gaps
            .push("artifact 'x' checksum drift: expected aaaa got bbbb".to_string());
        let summary = engine.summarize(
            &report.executions,
            &report.report_id,
            &report.evidence_checksum,
        );
        assert!(!summary.checksums_verified);
        assert!(summary.diagnostics_actionable, "gaps keep it actionable");
        assert!(!summary.gate_passes);
    }

    #[test]
    fn mismatch_without_gaps_is_not_actionable() {
        let dir = tempfile::tempdir().unwrap();
        let engine = KillerDemos::new("kd/test");
        let mut report = engine.run(dir.path(), &default_demo_specs()).unwrap();
        report.executions[0].ledger_entry.checksum_verdict = "mismatch".to_string();
        report.executions[0].ledger_entry.checksum_gaps.clear();
        let summary = engine.summarize(
            &report.executions,
            &report.report_id,
            &report.evidence_checksum,
        );
        assert!(!summary.diagnostics_actionable);
        assert!(!summary.gate_passes);
    }

    #[test]
    fn corpus_without_migration_demo_fails_gate() {
        let dir = tempfile::tempdir().unwrap();
        let specs: Vec<DemoSpec> = default_demo_specs()
            .into_iter()
            .filter(|s| s.kind != DemoKind::FlagshipMigrationPacks)
            .collect();
        let report = KillerDemos::new("kd/test").run(dir.path(), &specs).unwrap();
        assert!(!report.summary.migration_demo_present);
        assert!(!report.gate_passes);
    }

    #[test]
    fn deterministic_ledger_is_replay_identical() {
        let a = run_default("kd/test");
        let b = run_default("kd/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
        // Contracts (including expected hashes) reproduce across fully
        // independent materialization roots (AC1: reproducible hashes).
        let hashes_a: Vec<&str> = a
            .executions
            .iter()
            .flat_map(|e| e.contract.expected_outputs.iter())
            .map(|o| o.sha256.as_str())
            .collect();
        let hashes_b: Vec<&str> = b
            .executions
            .iter()
            .flat_map(|e| e.contract.expected_outputs.iter())
            .map(|o| o.sha256.as_str())
            .collect();
        assert_eq!(hashes_a, hashes_b);
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_killer_demo_pipeline(dir.path(), &KillerDemoConfig::default()).unwrap();
        assert!(outcome.summary.gate_passes);
        // 2 demo.yaml + ledger + execution log + summary (manifest not self-listed).
        assert_eq!(outcome.artifacts.len(), 5);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(
                sha256_hex(&bytes),
                artifact.sha256,
                "file {}",
                artifact.file
            );
        }
        // demo.yaml on disk parses back to a valid contract.
        for demo_id in ["demo-flagship-migration-packs", "demo-feedback-governance"] {
            let yaml = std::fs::read_to_string(
                std::path::Path::new(&outcome.run_dir)
                    .join("demos")
                    .join(demo_id)
                    .join("demo.yaml"),
            )
            .unwrap();
            let contract = parse_demo_yaml(&yaml).expect("on-disk demo.yaml parses");
            assert_eq!(contract.demo_id, demo_id);
            assert!(contract.has_required_fields());
        }
    }

    #[test]
    fn budget_overrun_flags_entry() {
        // A zero-second budget cannot admit any measured duration.
        let dir = tempfile::tempdir().unwrap();
        let mut specs = default_demo_specs();
        specs[0].max_duration_seconds = 0;
        specs.truncate(1);
        let report = KillerDemos::new("kd/test").run(dir.path(), &specs).unwrap();
        // max_duration_seconds == 0 also violates the contract's required
        // fields, so the gate fails on both clauses.
        assert!(!report.summary.required_fields_complete);
        assert!(!report.summary.within_budget);
        assert!(!report.gate_passes);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(12))]

        #[test]
        fn prop_ledger_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_default(&label);
            let second = run_default(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(first.render_ledger_jsonl(), second.render_ledger_jsonl());
        }

        #[test]
        fn prop_gate_always_passes_on_default_corpus(label in "[a-z]{1,8}") {
            let report = run_default(&label);
            prop_assert!(report.gate_passes);
            prop_assert!(report.summary.migration_demo_present);
        }
    }
}

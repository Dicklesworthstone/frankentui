#![forbid(unsafe_code)]

//! Unified performance evidence ledger and artifact contract (bd-rw97d).
//!
//! One place where baseline IDs, hotspot IDs, proof artifacts, replay
//! commands, gauntlet runs, and rollout verdicts join coherently. Every
//! performance lane (render, runtime, doctor) feeds this ledger; every
//! rollout decision consumes it.
//!
//! This module deliberately **reuses** the existing artifact vocabularies
//! instead of inventing a second one:
//!
//! - [`crate::validation_matrix`] — lanes ([`PerfLane`]), levels, and the
//!   per-lane logging contract (required log fields, event vocabulary,
//!   mismatch categories);
//! - [`crate::artifact_manifest`] — artifact classes, retention, redaction,
//!   and manifest-entry validation;
//! - [`crate::failure_signatures`] — failure classes, canonical reason
//!   codes, and replay-friendly log-quality validation.
//!
//! # Information architecture
//!
//! ```text
//! LedgerEntry (one evidence record)
//! ├── entry_id                  — stable, content-derived identity
//! ├── lane                      — render | runtime | doctor
//! ├── kind                      — which stage of the program produced it
//! ├── ids: EvidenceIds          — the JOIN KEYS (baseline/hotspot/proof/...)
//! ├── artifact_refs             — files + sha256 (manifest vocabulary)
//! ├── reason_codes              — failure_signatures vocabulary only
//! ├── mismatch_class            — validation-matrix mismatch category
//! └── replay_command            — one-command reproduction
//! ```
//!
//! Required vs optional fields are declared per [`EvidenceKind`] in
//! [`LedgerSpec::canonical`]; [`PerfEvidenceLedger::validate`] turns any
//! missing or malformed evidence into an explicit [`LedgerDefect`] (AC2 —
//! absence is a visible defect, never a silent gap).
//!
//! # Navigation contract (diagnosis latency)
//!
//! Humans and CI navigate from a failed verdict to its evidence with
//! [`PerfEvidenceLedger::navigate`]: verdict → gauntlet run → proof →
//! baseline, collecting every replay command along the trail. A failed
//! verdict whose trail is incomplete is itself a defect.

use std::collections::BTreeMap;

use crate::validation_matrix::PerfLane;

/// Schema version for the unified performance evidence ledger.
pub const PERF_EVIDENCE_LEDGER_SCHEMA_VERSION: &str = "perf-evidence-ledger-v1";

// ============================================================================
// Evidence kinds
// ============================================================================

/// The stages of the optimization program that produce ledger evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceKind {
    /// Workflow/observable inventory (what is optimization-critical).
    WorkflowInventory,
    /// Deterministic baseline capture (latency/throughput/memory/output cost).
    Baseline,
    /// Profile run identifying hotspots.
    Profile,
    /// Behavior-preservation / golden / isomorphism proof.
    Proof,
    /// An implementation change (one lever) under evaluation.
    Change,
    /// A gauntlet run (render/optimization/assurance) over the change.
    GauntletRun,
    /// Validation-matrix obligation output.
    ValidationOutput,
    /// A rollout verdict (promotion scorecard / go-no-go decision).
    RolloutVerdict,
}

impl EvidenceKind {
    /// All kinds, in pipeline order.
    pub const ALL: &'static [EvidenceKind] = &[
        Self::WorkflowInventory,
        Self::Baseline,
        Self::Profile,
        Self::Proof,
        Self::Change,
        Self::GauntletRun,
        Self::ValidationOutput,
        Self::RolloutVerdict,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::WorkflowInventory => "workflow-inventory",
            Self::Baseline => "baseline",
            Self::Profile => "profile",
            Self::Proof => "proof",
            Self::Change => "change",
            Self::GauntletRun => "gauntlet-run",
            Self::ValidationOutput => "validation-output",
            Self::RolloutVerdict => "rollout-verdict",
        }
    }
}

// ============================================================================
// Join keys
// ============================================================================

/// The stable identifiers that join evidence across the program. Which of
/// these are required depends on the [`EvidenceKind`] (see [`LedgerSpec`]);
/// an empty string means "not provided".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvidenceIds {
    /// Baseline identity (from deterministic baseline capture).
    pub baseline_id: String,
    /// Hotspot identity (from profiling).
    pub hotspot_id: String,
    /// Proof-artifact identity (behavior preservation / isomorphism).
    pub proof_id: String,
    /// Change identity (the one lever under evaluation).
    pub change_id: String,
    /// Gauntlet run identity.
    pub gauntlet_run_id: String,
    /// Rollout verdict identity.
    pub verdict_id: String,
    /// Fixture identity (validation-matrix / fixture-suite id).
    pub fixture_id: String,
}

impl EvidenceIds {
    fn get(&self, field: IdField) -> &str {
        match field {
            IdField::BaselineId => &self.baseline_id,
            IdField::HotspotId => &self.hotspot_id,
            IdField::ProofId => &self.proof_id,
            IdField::ChangeId => &self.change_id,
            IdField::GauntletRunId => &self.gauntlet_run_id,
            IdField::VerdictId => &self.verdict_id,
            IdField::FixtureId => &self.fixture_id,
        }
    }
}

/// The addressable identifier fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdField {
    /// `EvidenceIds::baseline_id`.
    BaselineId,
    /// `EvidenceIds::hotspot_id`.
    HotspotId,
    /// `EvidenceIds::proof_id`.
    ProofId,
    /// `EvidenceIds::change_id`.
    ChangeId,
    /// `EvidenceIds::gauntlet_run_id`.
    GauntletRunId,
    /// `EvidenceIds::verdict_id`.
    VerdictId,
    /// `EvidenceIds::fixture_id`.
    FixtureId,
}

impl IdField {
    /// Stable lowercase tag.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::BaselineId => "baseline_id",
            Self::HotspotId => "hotspot_id",
            Self::ProofId => "proof_id",
            Self::ChangeId => "change_id",
            Self::GauntletRunId => "gauntlet_run_id",
            Self::VerdictId => "verdict_id",
            Self::FixtureId => "fixture_id",
        }
    }
}

// ============================================================================
// Artifact references
// ============================================================================

/// A reference to a materialized artifact (manifest vocabulary: relative
/// file path plus `sha256:`-prefixed digest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    /// Relative artifact path.
    pub file: String,
    /// `sha256:`-prefixed content digest (64 hex chars after the prefix).
    pub digest: String,
}

impl ArtifactRef {
    /// Whether the digest is a well-formed immutable reference.
    #[must_use]
    pub fn digest_is_immutable(&self) -> bool {
        self.digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    }
}

// ============================================================================
// Ledger entry
// ============================================================================

/// One evidence record in the unified ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Stable entry identity.
    pub entry_id: String,
    /// Which performance lane produced the evidence.
    pub lane: PerfLane,
    /// Which program stage produced the evidence.
    pub kind: EvidenceKind,
    /// The stable join keys.
    pub ids: EvidenceIds,
    /// Materialized artifact references (file + digest).
    pub artifact_refs: Vec<ArtifactRef>,
    /// Reason codes (must come from the failure-signatures vocabulary).
    pub reason_codes: Vec<String>,
    /// Mismatch class (validation-matrix vocabulary; empty when clean).
    pub mismatch_class: String,
    /// Whether the underlying check/gate passed.
    pub passed: bool,
    /// One-command reproduction handle.
    pub replay_command: String,
    /// Human-readable detail.
    pub detail: String,
}

impl LedgerEntry {
    /// Start a new entry for a kind + lane with empty evidence.
    #[must_use]
    pub fn new(entry_id: impl Into<String>, lane: PerfLane, kind: EvidenceKind) -> Self {
        Self {
            schema_version: PERF_EVIDENCE_LEDGER_SCHEMA_VERSION.to_string(),
            entry_id: entry_id.into(),
            lane,
            kind,
            ids: EvidenceIds::default(),
            artifact_refs: Vec::new(),
            reason_codes: Vec::new(),
            mismatch_class: String::new(),
            passed: true,
            replay_command: String::new(),
            detail: String::new(),
        }
    }
}

// ============================================================================
// Contract spec (required vs optional per kind)
// ============================================================================

/// The per-kind evidence contract: which join keys and evidence facets are
/// required for an entry of that kind to be complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindSpec {
    /// Evidence kind this spec governs.
    pub kind: EvidenceKind,
    /// Join keys that MUST be non-empty.
    pub required_ids: Vec<IdField>,
    /// Join keys that MAY be present (documented linkage).
    pub optional_ids: Vec<IdField>,
    /// Whether at least one artifact reference is required.
    pub requires_artifact: bool,
    /// Whether a replay command is required.
    pub requires_replay: bool,
}

/// The full ledger contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerSpec {
    /// Per-kind contracts (one per [`EvidenceKind`]).
    pub kinds: Vec<KindSpec>,
}

impl LedgerSpec {
    /// The canonical contract (single source of truth for AC1/AC2).
    #[must_use]
    pub fn canonical() -> Self {
        use EvidenceKind as K;
        use IdField as F;
        let spec = |kind: K,
                    required_ids: Vec<F>,
                    optional_ids: Vec<F>,
                    requires_artifact: bool,
                    requires_replay: bool| KindSpec {
            kind,
            required_ids,
            optional_ids,
            requires_artifact,
            requires_replay,
        };
        Self {
            kinds: vec![
                spec(
                    K::WorkflowInventory,
                    vec![F::FixtureId],
                    vec![],
                    true,
                    false,
                ),
                spec(
                    K::Baseline,
                    vec![F::BaselineId, F::FixtureId],
                    vec![],
                    true,
                    true,
                ),
                spec(
                    K::Profile,
                    vec![F::HotspotId, F::BaselineId],
                    vec![F::FixtureId],
                    true,
                    true,
                ),
                spec(
                    K::Proof,
                    vec![F::ProofId, F::ChangeId],
                    vec![F::BaselineId],
                    true,
                    true,
                ),
                spec(
                    K::Change,
                    vec![F::ChangeId, F::HotspotId],
                    vec![F::BaselineId, F::ProofId],
                    false,
                    true,
                ),
                spec(
                    K::GauntletRun,
                    vec![F::GauntletRunId, F::ChangeId],
                    vec![F::BaselineId, F::ProofId, F::FixtureId],
                    true,
                    true,
                ),
                spec(
                    K::ValidationOutput,
                    vec![F::FixtureId],
                    vec![F::GauntletRunId, F::ChangeId],
                    true,
                    true,
                ),
                spec(
                    K::RolloutVerdict,
                    vec![F::VerdictId, F::ChangeId, F::GauntletRunId],
                    vec![F::ProofId, F::BaselineId],
                    true,
                    true,
                ),
            ],
        }
    }

    /// The spec for one kind.
    #[must_use]
    pub fn for_kind(&self, kind: EvidenceKind) -> Option<&KindSpec> {
        self.kinds.iter().find(|s| s.kind == kind)
    }
}

// ============================================================================
// Defects (missing/malformed evidence is visible, never silent)
// ============================================================================

/// Why a ledger entry is defective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DefectKind {
    /// A required join key is empty.
    MissingRequiredId,
    /// No artifact reference where the contract requires one.
    MissingArtifact,
    /// An artifact digest is not an immutable `sha256:` reference.
    MalformedArtifactDigest,
    /// No replay command where the contract requires one.
    MissingReplayCommand,
    /// A failing entry carries no reason codes.
    SilentFailure,
    /// The schema version does not match this contract.
    SchemaMismatch,
    /// A failed verdict's navigation trail is incomplete.
    BrokenTrail,
}

impl DefectKind {
    /// Stable lowercase tag.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::MissingRequiredId => "missing-required-id",
            Self::MissingArtifact => "missing-artifact",
            Self::MalformedArtifactDigest => "malformed-artifact-digest",
            Self::MissingReplayCommand => "missing-replay-command",
            Self::SilentFailure => "silent-failure",
            Self::SchemaMismatch => "schema-mismatch",
            Self::BrokenTrail => "broken-trail",
        }
    }
}

/// One detected evidence defect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerDefect {
    /// The defective entry.
    pub entry_id: String,
    /// What is wrong.
    pub kind: DefectKind,
    /// Which field/facet is affected.
    pub subject: String,
    /// How to fix it.
    pub remediation: String,
}

// ============================================================================
// The ledger
// ============================================================================

/// The unified performance evidence ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerfEvidenceLedger {
    /// All evidence records.
    pub entries: Vec<LedgerEntry>,
}

/// The navigation trail from a failed verdict to its replayable evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationTrail {
    /// The verdict this trail explains.
    pub verdict_entry_id: String,
    /// Entry ids visited (verdict -> gauntlet -> proof -> baseline).
    pub trail: Vec<String>,
    /// Every replay command collected along the trail.
    pub replay_commands: Vec<String>,
    /// Whether every expected hop resolved.
    pub complete: bool,
}

impl PerfEvidenceLedger {
    /// Add an entry.
    pub fn record(&mut self, entry: LedgerEntry) {
        self.entries.push(entry);
    }

    /// Validate every entry against the canonical contract (AC2): missing or
    /// malformed evidence becomes an explicit defect list.
    #[must_use]
    pub fn validate(&self, spec: &LedgerSpec) -> Vec<LedgerDefect> {
        let mut defects = Vec::new();
        for entry in &self.entries {
            if entry.schema_version != PERF_EVIDENCE_LEDGER_SCHEMA_VERSION {
                defects.push(LedgerDefect {
                    entry_id: entry.entry_id.clone(),
                    kind: DefectKind::SchemaMismatch,
                    subject: entry.schema_version.clone(),
                    remediation: format!("re-emit under {PERF_EVIDENCE_LEDGER_SCHEMA_VERSION}"),
                });
            }
            let Some(kind_spec) = spec.for_kind(entry.kind) else {
                continue;
            };
            for field in &kind_spec.required_ids {
                if entry.ids.get(*field).trim().is_empty() {
                    defects.push(LedgerDefect {
                        entry_id: entry.entry_id.clone(),
                        kind: DefectKind::MissingRequiredId,
                        subject: field.label().to_string(),
                        remediation: format!(
                            "populate {} for every {} entry",
                            field.label(),
                            entry.kind.label()
                        ),
                    });
                }
            }
            if kind_spec.requires_artifact && entry.artifact_refs.is_empty() {
                defects.push(LedgerDefect {
                    entry_id: entry.entry_id.clone(),
                    kind: DefectKind::MissingArtifact,
                    subject: "artifact_refs".to_string(),
                    remediation: format!(
                        "attach at least one artifact reference to every {} entry",
                        entry.kind.label()
                    ),
                });
            }
            for artifact in &entry.artifact_refs {
                if !artifact.digest_is_immutable() {
                    defects.push(LedgerDefect {
                        entry_id: entry.entry_id.clone(),
                        kind: DefectKind::MalformedArtifactDigest,
                        subject: artifact.file.clone(),
                        remediation: "use a sha256:<64-hex> immutable digest".to_string(),
                    });
                }
            }
            if kind_spec.requires_replay && entry.replay_command.trim().is_empty() {
                defects.push(LedgerDefect {
                    entry_id: entry.entry_id.clone(),
                    kind: DefectKind::MissingReplayCommand,
                    subject: "replay_command".to_string(),
                    remediation: format!(
                        "record the one-command reproduction for every {} entry",
                        entry.kind.label()
                    ),
                });
            }
            if !entry.passed && entry.reason_codes.is_empty() {
                defects.push(LedgerDefect {
                    entry_id: entry.entry_id.clone(),
                    kind: DefectKind::SilentFailure,
                    subject: "reason_codes".to_string(),
                    remediation: "failing evidence must carry failure-signature reason codes"
                        .to_string(),
                });
            }
        }
        defects
    }

    fn find_by_id(&self, field: IdField, value: &str) -> Option<&LedgerEntry> {
        if value.trim().is_empty() {
            return None;
        }
        self.entries.iter().find(|e| e.ids.get(field) == value)
    }

    /// Navigate from a rollout verdict entry to its replayable evidence
    /// (verdict -> gauntlet run -> proof -> baseline), collecting replay
    /// commands along the way. The trail is `complete` only when every hop
    /// implied by the verdict's join keys resolves to a ledger entry.
    #[must_use]
    pub fn navigate(&self, verdict_entry_id: &str) -> Option<NavigationTrail> {
        let verdict = self
            .entries
            .iter()
            .find(|e| e.entry_id == verdict_entry_id && e.kind == EvidenceKind::RolloutVerdict)?;

        let mut trail = vec![verdict.entry_id.clone()];
        let mut replay_commands = Vec::new();
        if !verdict.replay_command.is_empty() {
            replay_commands.push(verdict.replay_command.clone());
        }
        let mut complete = true;

        let gauntlet = self.entries.iter().find(|e| {
            e.kind == EvidenceKind::GauntletRun
                && e.ids.gauntlet_run_id == verdict.ids.gauntlet_run_id
                && !verdict.ids.gauntlet_run_id.is_empty()
        });
        match gauntlet {
            Some(entry) => {
                trail.push(entry.entry_id.clone());
                if !entry.replay_command.is_empty() {
                    replay_commands.push(entry.replay_command.clone());
                }
                let proof_key = if entry.ids.proof_id.is_empty() {
                    verdict.ids.proof_id.clone()
                } else {
                    entry.ids.proof_id.clone()
                };
                if proof_key.is_empty() {
                    complete = false;
                } else if let Some(proof) = self
                    .entries
                    .iter()
                    .find(|e| e.kind == EvidenceKind::Proof && e.ids.proof_id == proof_key)
                {
                    trail.push(proof.entry_id.clone());
                    if !proof.replay_command.is_empty() {
                        replay_commands.push(proof.replay_command.clone());
                    }
                    if let Some(baseline) =
                        self.find_by_id(IdField::BaselineId, &proof.ids.baseline_id)
                    {
                        trail.push(baseline.entry_id.clone());
                        if !baseline.replay_command.is_empty() {
                            replay_commands.push(baseline.replay_command.clone());
                        }
                    } else {
                        complete = false;
                    }
                } else {
                    complete = false;
                }
            }
            None => complete = false,
        }

        Some(NavigationTrail {
            verdict_entry_id: verdict.entry_id.clone(),
            trail,
            replay_commands,
            complete,
        })
    }

    /// Trail defects for every FAILED verdict whose navigation is incomplete
    /// (the diagnosis-latency clause: a failed verdict must lead to replayable
    /// evidence without manual forensics).
    #[must_use]
    pub fn broken_trails(&self) -> Vec<LedgerDefect> {
        self.entries
            .iter()
            .filter(|e| e.kind == EvidenceKind::RolloutVerdict && !e.passed)
            .filter_map(|verdict| {
                let trail = self.navigate(&verdict.entry_id)?;
                if trail.complete {
                    None
                } else {
                    Some(LedgerDefect {
                        entry_id: verdict.entry_id.clone(),
                        kind: DefectKind::BrokenTrail,
                        subject: trail.trail.join(" -> "),
                        remediation: "link the verdict to its gauntlet run, proof, and \
                                      baseline so the failure replays without forensics"
                            .to_string(),
                    })
                }
            })
            .collect()
    }

    /// Coverage summary: how many entries each (lane, kind) cell holds. A
    /// scorecard can require specific cells to be non-zero (AC4: coverage
    /// completeness is inspectable, not implied).
    #[must_use]
    pub fn coverage(&self) -> BTreeMap<(PerfLane, EvidenceKind), usize> {
        let mut cells: BTreeMap<(PerfLane, EvidenceKind), usize> = BTreeMap::new();
        for entry in &self.entries {
            *cells.entry((entry.lane, entry.kind)).or_insert(0) += 1;
        }
        cells
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(file: &str) -> ArtifactRef {
        ArtifactRef {
            file: file.to_string(),
            digest: format!("sha256:{}", "ab".repeat(32)),
        }
    }

    fn complete_program_ledger() -> PerfEvidenceLedger {
        let mut ledger = PerfEvidenceLedger::default();

        let mut baseline = LedgerEntry::new("e-baseline", PerfLane::Render, EvidenceKind::Baseline);
        baseline.ids.baseline_id = "base-render-1".to_string();
        baseline.ids.fixture_id = "render_diff_sparse_80x24".to_string();
        baseline.artifact_refs.push(artifact("baseline.json"));
        baseline.replay_command =
            "cargo test -p ftui-harness --test fixture_runner_e2e".to_string();
        ledger.record(baseline);

        let mut profile = LedgerEntry::new("e-profile", PerfLane::Render, EvidenceKind::Profile);
        profile.ids.hotspot_id = "hs-diff-inner".to_string();
        profile.ids.baseline_id = "base-render-1".to_string();
        profile.artifact_refs.push(artifact("profile.json"));
        profile.replay_command = "cargo bench -p ftui-render".to_string();
        ledger.record(profile);

        let mut proof = LedgerEntry::new("e-proof", PerfLane::Render, EvidenceKind::Proof);
        proof.ids.proof_id = "proof-iso-1".to_string();
        proof.ids.change_id = "chg-lever-1".to_string();
        proof.ids.baseline_id = "base-render-1".to_string();
        proof.artifact_refs.push(artifact("proof.json"));
        proof.replay_command = "cargo test -p ftui-harness --test isomorphism_proofs".to_string();
        ledger.record(proof);

        let mut gauntlet =
            LedgerEntry::new("e-gauntlet", PerfLane::Render, EvidenceKind::GauntletRun);
        gauntlet.ids.gauntlet_run_id = "gr-render-1".to_string();
        gauntlet.ids.change_id = "chg-lever-1".to_string();
        gauntlet.ids.proof_id = "proof-iso-1".to_string();
        gauntlet.artifact_refs.push(artifact("gauntlet.json"));
        gauntlet.replay_command =
            "cargo test -p ftui-harness --test render_gauntlet_e2e".to_string();
        ledger.record(gauntlet);

        let mut verdict =
            LedgerEntry::new("e-verdict", PerfLane::Render, EvidenceKind::RolloutVerdict);
        verdict.ids.verdict_id = "verdict-1".to_string();
        verdict.ids.change_id = "chg-lever-1".to_string();
        verdict.ids.gauntlet_run_id = "gr-render-1".to_string();
        verdict.artifact_refs.push(artifact("verdict.json"));
        verdict.replay_command = "cargo run -p doctor_frankentui -- release-candidate".to_string();
        ledger.record(verdict);

        ledger
    }

    #[test]
    fn canonical_spec_covers_every_kind() {
        let spec = LedgerSpec::canonical();
        for kind in EvidenceKind::ALL {
            let kind_spec = spec.for_kind(*kind).expect("kind spec");
            assert!(
                !kind_spec.required_ids.is_empty(),
                "{} requires at least one join key",
                kind.label()
            );
        }
        // Every verdict must join to a change and a gauntlet run (AC3:
        // scorecards rely on this contract).
        let verdict = spec.for_kind(EvidenceKind::RolloutVerdict).unwrap();
        assert!(verdict.required_ids.contains(&IdField::ChangeId));
        assert!(verdict.required_ids.contains(&IdField::GauntletRunId));
        assert!(verdict.requires_replay);
    }

    #[test]
    fn complete_program_validates_clean() {
        let ledger = complete_program_ledger();
        let defects = ledger.validate(&LedgerSpec::canonical());
        assert!(defects.is_empty(), "{defects:?}");
    }

    #[test]
    fn missing_evidence_is_a_visible_defect() {
        let mut ledger = complete_program_ledger();
        // Strip the gauntlet's change link, replay, and artifact.
        let gauntlet = ledger
            .entries
            .iter_mut()
            .find(|e| e.kind == EvidenceKind::GauntletRun)
            .unwrap();
        gauntlet.ids.change_id.clear();
        gauntlet.replay_command.clear();
        gauntlet.artifact_refs.clear();

        let defects = ledger.validate(&LedgerSpec::canonical());
        let kinds: Vec<DefectKind> = defects.iter().map(|d| d.kind).collect();
        assert!(kinds.contains(&DefectKind::MissingRequiredId));
        assert!(kinds.contains(&DefectKind::MissingArtifact));
        assert!(kinds.contains(&DefectKind::MissingReplayCommand));
        assert!(defects.iter().all(|d| !d.remediation.is_empty()));
    }

    #[test]
    fn malformed_digest_and_silent_failure_are_flagged() {
        let mut ledger = complete_program_ledger();
        let verdict = ledger
            .entries
            .iter_mut()
            .find(|e| e.kind == EvidenceKind::RolloutVerdict)
            .unwrap();
        verdict.passed = false; // failing with no reason codes = silent
        verdict.artifact_refs[0].digest = "md5:nope".to_string();

        let defects = ledger.validate(&LedgerSpec::canonical());
        let kinds: Vec<DefectKind> = defects.iter().map(|d| d.kind).collect();
        assert!(kinds.contains(&DefectKind::MalformedArtifactDigest));
        assert!(kinds.contains(&DefectKind::SilentFailure));
    }

    #[test]
    fn navigation_reaches_replay_commands_from_a_verdict() {
        let ledger = complete_program_ledger();
        let trail = ledger.navigate("e-verdict").expect("trail");
        assert!(trail.complete, "{trail:?}");
        assert_eq!(trail.trail.len(), 4); // verdict -> gauntlet -> proof -> baseline
        assert_eq!(trail.replay_commands.len(), 4);
        assert!(
            trail
                .replay_commands
                .iter()
                .any(|c| c.contains("render_gauntlet_e2e"))
        );
    }

    #[test]
    fn failed_verdict_with_broken_trail_is_a_defect() {
        let mut ledger = complete_program_ledger();
        // Fail the verdict and remove the proof so the trail cannot resolve.
        ledger
            .entries
            .iter_mut()
            .find(|e| e.kind == EvidenceKind::RolloutVerdict)
            .unwrap()
            .passed = false;
        ledger.entries.retain(|e| e.kind != EvidenceKind::Proof);

        let broken = ledger.broken_trails();
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].kind, DefectKind::BrokenTrail);

        // A passing verdict with the same gap is NOT a trail defect (the
        // clause targets diagnosis latency for failures).
        ledger
            .entries
            .iter_mut()
            .find(|e| e.kind == EvidenceKind::RolloutVerdict)
            .unwrap()
            .passed = true;
        assert!(ledger.broken_trails().is_empty());
    }

    #[test]
    fn coverage_exposes_lane_kind_cells() {
        let ledger = complete_program_ledger();
        let coverage = ledger.coverage();
        assert_eq!(
            coverage.get(&(PerfLane::Render, EvidenceKind::Baseline)),
            Some(&1)
        );
        assert_eq!(
            coverage.get(&(PerfLane::Render, EvidenceKind::RolloutVerdict)),
            Some(&1)
        );
        assert!(!coverage.contains_key(&(PerfLane::Runtime, EvidenceKind::Baseline)));
    }

    #[test]
    fn schema_mismatch_is_flagged() {
        let mut ledger = complete_program_ledger();
        ledger.entries[0].schema_version = "perf-evidence-ledger-v0".to_string();
        let defects = ledger.validate(&LedgerSpec::canonical());
        assert!(defects.iter().any(|d| d.kind == DefectKind::SchemaMismatch));
    }
}

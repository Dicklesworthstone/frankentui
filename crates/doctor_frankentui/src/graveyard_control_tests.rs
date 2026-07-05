//! Unit/property test-evidence harness for the graveyard-executable control
//! plane (bd-3bxhj.10.36): symptom routing, composition/interference gating,
//! entry-header/graduation schema compilation, and the graveyardctl /
//! graveyard-verify gates.
//!
//! The harness drives the REAL control-plane kernels (`symptom_router`,
//! `composition_matrix`, `entry_header_compiler`, `graveyardctl`,
//! `graveyard_verify`) through a fixed fixture corpus spanning the mandated
//! acceptance categories:
//!
//! - happy path (AC1): fast-start routing over the canonical corpus, a
//!   requirement-satisfying composition with detected synergies, a complete
//!   entry header that graduates, and the all-pass graveyardctl verify gate;
//! - malformed metadata (AC1): non-finite EV scores, structural composition
//!   defects (empty/duplicate/unknown-controller), missing/empty header
//!   fields, and artifact-incomplete contracts;
//! - inconsistent contracts (AC1): linter-passing contracts whose risk
//!   posture trips the dangerous-combination gate, plus the four-scenario
//!   graveyard-verify release gauntlet;
//! - policy-override misuse (AC1): evidence-backed exceptions that promote
//!   vs unbacked exceptions that are refused, and Tbd/graduation-mode
//!   escalation semantics;
//! - adversarial combinations (AC1): unmitigated vs mitigated dangerous
//!   combinations, advisory medium-severity combos, and the multi-controller
//!   interference gate.
//!
//! Every diagnostic carries the AC3-mandated fields: `route_id`,
//! `candidate_set_hash`, `composition_id`, `entry_header_hash`,
//! `verify_status`, `violated_clause`, and `replay_cmd` (sentinel `n/a` /
//! `none` where a field does not apply — never empty). Determinism (AC2) is
//! proven by byte-for-byte re-runs of the harness report and of the
//! underlying engines via proptest.
//!
//! This module is a `pub mod` compiled into the lib (the CI gate runs
//! `cargo test -p doctor_frankentui --lib graveyard_control_tests`); all
//! `proptest` usage is confined to the `#[cfg(test)]` block. The envelope is
//! float-free (numeric terms are fixed-decimal strings via `fmt6`), so it
//! derives `Eq` and replays byte-identically. Precedents:
//! `alien_kernel_unit_tests` (bd-3bxhj.10.27) and `tier_escalator_tests`
//! (bd-3bxhj.8.23).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adversarial_fixtures::RiskClass;
use crate::composition_matrix::{
    CompositionGate, CompositionPlan, ControllerRole, HazardCode, HazardSeverity, canonical_matrix,
    canonical_registry,
};
use crate::ecosystem_scan::UpliftCandidate;
use crate::entry_header_compiler::{
    CriterionStatus, EntryHeaderCompiler, EntryHeaderCompilerConfig, EntryHeaderDraft,
    GraduationCriteriaTemplate, HeaderFieldId, HeaderFieldValue, SchemaFindingCode, SizeEstimate,
    estimate_effort_band, ev_effort_penalty, generate_graduation_template,
};
use crate::graveyard_verify::{ScenarioKind, VerifyDimension, run_graveyard_verify_report};
use crate::graveyardctl::{
    ActiveEntry, GraveyardctlEngine, GraveyardctlStage, VerifyResult, default_active_corpus,
};
use crate::milestone_policy::{PriorityTier, QualityBar};
use crate::recommendation_contract::{
    EffortSize, RecommendationContract, example_complete_contract,
};
use crate::semantic_contract::IpArtifactStatus;
use crate::symptom_router::{
    CanonicalEntry, FastStartPolicy, MigrationHotspot, PolicyException, SymptomClass,
    SymptomRouter, canonical_corpus,
};

/// Schema version for the graveyard control-plane test-evidence harness.
pub const GRAVEYARD_CONTROL_TESTS_SCHEMA_VERSION: &str = "graveyard-control-tests-v1";

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

fn fmt6(x: f64) -> String {
    if !x.is_finite() {
        return "0.000000".to_string();
    }
    let rendered = format!("{x:.6}");
    if rendered == "-0.000000" {
        "0.000000".to_string()
    } else {
        rendered
    }
}

fn replay(name: &str) -> String {
    format!("cargo test -p doctor_frankentui --lib graveyard_control_tests # {name}")
}

fn na() -> String {
    "n/a".to_string()
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// The control-plane component a diagnostic belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneComponent {
    /// Symptom router + Fast-Start harvester (`symptom_router`).
    Router,
    /// Composition/interference matrix gate (`composition_matrix`).
    Composition,
    /// Entry-header + graduation schema compiler (`entry_header_compiler`).
    HeaderSchema,
    /// graveyardctl / graveyard-verify gates.
    VerifyGate,
}

impl ControlPlaneComponent {
    /// All components, in canonical order.
    pub const ALL: &'static [ControlPlaneComponent] = &[
        ControlPlaneComponent::Router,
        ControlPlaneComponent::Composition,
        ControlPlaneComponent::HeaderSchema,
        ControlPlaneComponent::VerifyGate,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ControlPlaneComponent::Router => "router",
            ControlPlaneComponent::Composition => "composition",
            ControlPlaneComponent::HeaderSchema => "header_schema",
            ControlPlaneComponent::VerifyGate => "verify_gate",
        }
    }
}

/// Acceptance-criteria category exercised by a fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureCategory {
    /// Green-path behavior of a control-plane kernel.
    HappyPath,
    /// Malformed metadata (non-finite scores, missing fields, structural
    /// defects, artifact-incomplete contracts).
    MalformedMetadata,
    /// Contracts that are internally inconsistent (lint-passing but
    /// risk-gate-tripping) and the release gauntlet that catches them.
    InconsistentContract,
    /// Policy-override / exception semantics (evidence-backed vs refused,
    /// graduation-mode escalation).
    PolicyOverrideMisuse,
    /// Dangerous-combination and multi-controller interference gating.
    AdversarialCombination,
}

impl FixtureCategory {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FixtureCategory::HappyPath => "happy_path",
            FixtureCategory::MalformedMetadata => "malformed_metadata",
            FixtureCategory::InconsistentContract => "inconsistent_contract",
            FixtureCategory::PolicyOverrideMisuse => "policy_override_misuse",
            FixtureCategory::AdversarialCombination => "adversarial_combination",
        }
    }
}

// ── Diagnostic envelope (AC3) ────────────────────────────────────────────────

/// One machine-actionable diagnostic emitted while driving a control-plane
/// kernel. Carries every AC3-mandated field; `n/a` / `none` sentinels are
/// used where a field does not apply (never empty strings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardControlDiagnostic {
    /// Component under test.
    pub component: ControlPlaneComponent,
    /// Route identity (symptom id / engine run id; AC3 `route_id`).
    pub route_id: String,
    /// Candidate-pool / evidence checksum (AC3 `candidate_set_hash`).
    pub candidate_set_hash: String,
    /// Composition identity (AC3 `composition_id`).
    pub composition_id: String,
    /// Entry-header report identity (AC3 `entry_header_hash`).
    pub entry_header_hash: String,
    /// Verify verdict tag (AC3 `verify_status`).
    pub verify_status: String,
    /// The violated clause/finding/hazard code, or `none` (AC3
    /// `violated_clause`).
    pub violated_clause: String,
    /// Observed outcome tag.
    pub outcome: String,
    /// Human-readable detail (numeric terms fixed-decimal via `fmt6`).
    pub detail: String,
    /// Deterministic replay command (AC3 `replay_cmd`).
    pub replay_cmd: String,
}

impl GraveyardControlDiagnostic {
    /// Whether every mandated field is populated (sentinels count as
    /// populated; empty strings do not).
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.route_id.is_empty()
            && !self.candidate_set_hash.is_empty()
            && !self.composition_id.is_empty()
            && !self.entry_header_hash.is_empty()
            && !self.verify_status.is_empty()
            && !self.violated_clause.is_empty()
            && !self.outcome.is_empty()
            && !self.detail.is_empty()
            && !self.replay_cmd.is_empty()
    }

    /// Project the AC3 failure-log view of this diagnostic.
    #[must_use]
    pub fn failure_log(&self) -> GraveyardControlFailureLog {
        GraveyardControlFailureLog {
            route_id: self.route_id.clone(),
            candidate_set_hash: self.candidate_set_hash.clone(),
            composition_id: self.composition_id.clone(),
            entry_header_hash: self.entry_header_hash.clone(),
            verify_status: self.verify_status.clone(),
            violated_clause: self.violated_clause.clone(),
            replay_cmd: self.replay_cmd.clone(),
        }
    }
}

/// The AC3-mandated failure-log projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardControlFailureLog {
    /// Route identity.
    pub route_id: String,
    /// Candidate-pool / evidence checksum.
    pub candidate_set_hash: String,
    /// Composition identity.
    pub composition_id: String,
    /// Entry-header report identity.
    pub entry_header_hash: String,
    /// Verify verdict tag.
    pub verify_status: String,
    /// Violated clause/finding/hazard code.
    pub violated_clause: String,
    /// Deterministic replay command.
    pub replay_cmd: String,
}

// ── Oracle ───────────────────────────────────────────────────────────────────

/// Expected-vs-observed verdict for one fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeVerdict {
    /// Fixture label.
    pub fixture_label: String,
    /// Component under test.
    pub component: ControlPlaneComponent,
    /// Acceptance category exercised.
    pub category: FixtureCategory,
    /// Stable human statement of the oracle.
    pub expectation: String,
    /// Whether the observed behavior matched the oracle.
    pub matches_expected: bool,
    /// Mismatch reason (empty on pass).
    pub mismatch: String,
}

fn verdict(
    label: &str,
    component: ControlPlaneComponent,
    category: FixtureCategory,
    expectation: &str,
    matches_expected: bool,
    mismatch: impl Into<String>,
) -> OutcomeVerdict {
    OutcomeVerdict {
        fixture_label: label.to_string(),
        component,
        category,
        expectation: expectation.to_string(),
        matches_expected,
        mismatch: if matches_expected {
            String::new()
        } else {
            mismatch.into()
        },
    }
}

/// One evaluated fixture: its diagnostics plus the oracle verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlFixtureEvaluation {
    /// Fixture label (sort key).
    pub label: String,
    /// Component under test.
    pub component: ControlPlaneComponent,
    /// Acceptance category exercised.
    pub category: FixtureCategory,
    /// Emitted diagnostics.
    pub diagnostics: Vec<GraveyardControlDiagnostic>,
    /// Oracle verdict.
    pub verdict: OutcomeVerdict,
}

// ── Shared fixture builders ──────────────────────────────────────────────────

/// Bind a fresh complete contract to a canonical graveyard entry.
fn contract_for(
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

/// A complete (gate-passing) active entry.
fn complete_active_entry() -> ActiveEntry {
    ActiveEntry {
        entry_id: "gv-erasure-coding".to_string(),
        contract: contract_for(
            "gv-erasure-coding",
            "rc-control-complete",
            "Reed-Solomon tail-latency hedging",
            "claim-control-complete",
        ),
    }
}

/// An artifact-incomplete active entry (drops the fields the verify gate
/// must catch: repro/provenance artifacts, budget behavior + safe-mode
/// trigger, and entry-header metadata).
fn incomplete_active_entry() -> ActiveEntry {
    let mut contract = contract_for(
        "gv-metamorphic-equiv",
        "rc-control-incomplete",
        "Metamorphic equivalence with a hollow artifact pack",
        "claim-control-incomplete",
    );
    contract.failure_mode.repro_artifact = None;
    contract.failure_mode.provenance_artifact = None;
    contract.budgeted_mode.exhaustion_behavior.clear();
    contract.budgeted_mode.conservative_fallback_trigger.clear();
    contract.entry_header.tags.clear();
    contract.entry_header.archetype.clear();
    ActiveEntry {
        entry_id: "gv-metamorphic-equiv".to_string(),
        contract,
    }
}

/// An artifact-complete but risk-inconsistent active entry: the linter
/// passes (`needs_counsel` is advisory) while the high-severity primary
/// failure mode plus high-severity legal status trip the
/// dangerous-combination risk gate.
fn inconsistent_active_entry() -> ActiveEntry {
    let mut contract = contract_for(
        "gv-lockfree-queue",
        "rc-control-inconsistent",
        "Lock-free queue with unresolved IP posture",
        "claim-control-inconsistent",
    );
    contract.failure_mode.risk_class = RiskClass::High;
    contract.failure_mode.legal_status = IpArtifactStatus::NeedsCounsel;
    ActiveEntry {
        entry_id: "gv-lockfree-queue".to_string(),
        contract,
    }
}

/// A draft providing every mandatory header field.
fn complete_draft(entry_id: &str) -> EntryHeaderDraft {
    let mut draft = EntryHeaderDraft::new(entry_id);
    for field in HeaderFieldId::ALL {
        draft = draft.provide(*field, format!("{} evidence", field.as_str()));
    }
    draft
}

/// A graduation template whose every criterion is CI-linked and passed.
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

// ── Router fixtures ──────────────────────────────────────────────────────────

fn fix_router_fast_start_happy() -> ControlFixtureEvaluation {
    let label = "router-fast-start-happy";
    let router = SymptomRouter::default();
    let corpus = canonical_corpus();
    let hotspots = vec![MigrationHotspot::new(
        "hs-render-tail",
        SymptomClass::TailLatency,
        "ftui-render/src/diff.rs",
        0.9,
    )];
    let exceptions: Vec<PolicyException> = Vec::new();
    let report = router.evaluate(&corpus, &hotspots, &exceptions);
    let rerun = router.evaluate(&corpus, &hotspots, &exceptions);

    let deterministic =
        report.report_id == rerun.report_id && report.recommendations == rerun.recommendations;
    let all_fast_start = report.summary.selected_count == SymptomClass::ALL.len()
        && report.summary.fast_start_count == SymptomClass::ALL.len()
        && report.summary.fallback_count == 0
        && report.summary.empty_pool_count == 0;
    let routes_three_step = report.recommendations.iter().all(|rec| {
        rec.route_path.len() == 3
            && rec.route_path[0].starts_with("intake:")
            && rec.route_path[1].starts_with("route:")
            && rec.route_path[2].starts_with("harvest:s-tier-")
    });
    let tail_pick = report
        .recommendation_for(SymptomClass::TailLatency)
        .is_some_and(|rec| {
            rec.selected_entry_id.as_deref() == Some("gv-erasure-coding")
                && rec.fast_start_applied
                && !rec.fallback_used
                && rec.mapped_hotspots.iter().any(|h| h == "hs-render-tail")
        });
    let logging_contract = report.exported_json_stats.sha256
        == sha256_hex(report.exported_json_stats.content.as_bytes())
        && report.replay_command.contains(&report.report_id);
    let ok = deterministic && all_fast_start && routes_three_step && tail_pick && logging_contract;

    let diagnostics = report
        .recommendations
        .iter()
        .map(|rec| GraveyardControlDiagnostic {
            component: ControlPlaneComponent::Router,
            route_id: rec.symptom_id.clone(),
            candidate_set_hash: rec.candidate_pool_hash.clone(),
            composition_id: na(),
            entry_header_hash: na(),
            verify_status: na(),
            violated_clause: "none".to_string(),
            outcome: if rec.fast_start_applied {
                "fast_start_selected".to_string()
            } else {
                "selected".to_string()
            },
            detail: format!(
                "symptom {} selected {} from a pool of {}",
                rec.symptom_id,
                rec.selected_entry_id
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
                rec.candidate_pool_ids.len()
            ),
            replay_cmd: replay(label),
        })
        .collect();

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Router,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Router,
            FixtureCategory::HappyPath,
            "every symptom fast-starts an S-tier canonical entry deterministically",
            ok,
            "canonical corpus did not fast-start every symptom deterministically",
        ),
    }
}

fn fix_router_tie_break_determinism() -> ControlFixtureEvaluation {
    let label = "router-tie-break-determinism";
    let router = SymptomRouter::default();
    let hotspots: Vec<MigrationHotspot> = Vec::new();
    let exceptions: Vec<PolicyException> = Vec::new();
    let a = CanonicalEntry::new("s-aaa", "Tie candidate A", PriorityTier::S, 9.0)
        .with_symptoms([SymptomClass::TailLatency]);
    let b = CanonicalEntry::new("s-bbb", "Tie candidate B", PriorityTier::S, 9.0)
        .with_symptoms([SymptomClass::TailLatency]);

    let forward = router.route(
        SymptomClass::TailLatency,
        &[a.clone(), b.clone()],
        &hotspots,
        &exceptions,
    );
    let reversed = router.route(SymptomClass::TailLatency, &[b, a], &hotspots, &exceptions);

    let ok = forward.selected_entry_id.as_deref() == Some("s-aaa")
        && reversed.selected_entry_id.as_deref() == Some("s-aaa")
        && forward.candidate_pool_hash == reversed.candidate_pool_hash
        && forward.fast_start_applied
        && reversed.fast_start_applied;

    let diagnostics = vec![GraveyardControlDiagnostic {
        component: ControlPlaneComponent::Router,
        route_id: forward.symptom_id.clone(),
        candidate_set_hash: forward.candidate_pool_hash.clone(),
        composition_id: na(),
        entry_header_hash: na(),
        verify_status: na(),
        violated_clause: "none".to_string(),
        outcome: "tie_break_stable".to_string(),
        detail: format!(
            "equal-EV ({}) S-tier tie resolves to {} regardless of corpus order",
            fmt6(9.0),
            forward
                .selected_entry_id
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        replay_cmd: replay(label),
    }];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Router,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Router,
            FixtureCategory::HappyPath,
            "equal-EV ties break by entry id and are permutation-invariant",
            ok,
            "tie-break selection or pool hash changed under corpus permutation",
        ),
    }
}

fn fix_router_override_legitimate() -> ControlFixtureEvaluation {
    let label = "router-override-legitimate";
    let router = SymptomRouter::default();
    let hotspots: Vec<MigrationHotspot> = Vec::new();
    let exception = PolicyException::new(
        "gv-pid-tuning",
        "control-loop hotspot pinned by incident IR-42",
        ["evidence://ir-42/flamegraph.json".to_string()],
    );
    let rec = router.route(
        SymptomClass::AdaptiveControl,
        &canonical_corpus(),
        &hotspots,
        std::slice::from_ref(&exception),
    );

    let ok = exception.is_evidence_backed()
        && rec.policy_exception_applied
        && rec.selected_entry_id.as_deref() == Some("gv-pid-tuning")
        && !rec.fast_start_applied
        && rec
            .rejected
            .iter()
            .any(|r| r.entry_id == "gv-mpc-control" && !r.reason.trim().is_empty());

    let diagnostics = vec![GraveyardControlDiagnostic {
        component: ControlPlaneComponent::Router,
        route_id: rec.symptom_id.clone(),
        candidate_set_hash: rec.candidate_pool_hash.clone(),
        composition_id: na(),
        entry_header_hash: na(),
        verify_status: na(),
        violated_clause: "none".to_string(),
        outcome: "policy_exception_promoted".to_string(),
        detail: "evidence-backed exception promotes gv-pid-tuning over the S-tier fast-start"
            .to_string(),
        replay_cmd: replay(label),
    }];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Router,
        category: FixtureCategory::PolicyOverrideMisuse,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Router,
            FixtureCategory::PolicyOverrideMisuse,
            "an evidence-backed policy exception promotes its candidate",
            ok,
            "evidence-backed exception failed to promote its candidate",
        ),
    }
}

fn fix_router_override_misuse_refused() -> ControlFixtureEvaluation {
    let label = "router-override-misuse-refused";
    let router = SymptomRouter::default();
    let hotspots: Vec<MigrationHotspot> = Vec::new();
    let no_evidence = PolicyException::new("gv-pid-tuning", "gut feeling", Vec::<String>::new());
    let blank_reason = PolicyException::new(
        "gv-pid-tuning",
        "   ",
        ["evidence://unreviewed/blob".to_string()],
    );
    let rec = router.route(
        SymptomClass::AdaptiveControl,
        &canonical_corpus(),
        &hotspots,
        &[no_evidence.clone(), blank_reason.clone()],
    );

    let ok = !no_evidence.is_evidence_backed()
        && !blank_reason.is_evidence_backed()
        && !rec.policy_exception_applied
        && rec.selected_entry_id.as_deref() == Some("gv-mpc-control")
        && rec.fast_start_applied;

    let diagnostics = vec![GraveyardControlDiagnostic {
        component: ControlPlaneComponent::Router,
        route_id: rec.symptom_id.clone(),
        candidate_set_hash: rec.candidate_pool_hash.clone(),
        composition_id: na(),
        entry_header_hash: na(),
        verify_status: na(),
        violated_clause: "policy-exception-unbacked".to_string(),
        outcome: "policy_exception_refused".to_string(),
        detail: "exceptions without evidence refs or with blank reasons are ignored; the \
                 fast-start winner stands"
            .to_string(),
        replay_cmd: replay(label),
    }];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Router,
        category: FixtureCategory::PolicyOverrideMisuse,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Router,
            FixtureCategory::PolicyOverrideMisuse,
            "unbacked policy exceptions are refused and never promote",
            ok,
            "an unbacked policy exception changed the routing outcome",
        ),
    }
}

fn fix_router_malformed_ev_metadata() -> ControlFixtureEvaluation {
    let label = "router-malformed-ev-metadata";
    let strict = SymptomRouter::new(FastStartPolicy::default().with_speculative_fallback(false));
    let hotspots: Vec<MigrationHotspot> = Vec::new();
    let exceptions: Vec<PolicyException> = Vec::new();
    let nan_entry = CanonicalEntry::new(
        "s-nan-ev",
        "Malformed EV metadata",
        PriorityTier::S,
        f64::NAN,
    )
    .with_symptoms([SymptomClass::Correctness]);

    let coerced_to_zero = fmt6(nan_entry.ev_score) == "0.000000";
    let rec = strict.route(
        SymptomClass::Correctness,
        &[nan_entry],
        &hotspots,
        &exceptions,
    );

    let ok = coerced_to_zero
        && rec.selected_entry_id.is_none()
        && rec.fallback_used
        && !rec.fast_start_applied
        && !rec.rejected.is_empty()
        && rec.rejected.iter().all(|r| !r.reason.trim().is_empty());

    let diagnostics = vec![GraveyardControlDiagnostic {
        component: ControlPlaneComponent::Router,
        route_id: rec.symptom_id.clone(),
        candidate_set_hash: rec.candidate_pool_hash.clone(),
        composition_id: na(),
        entry_header_hash: na(),
        verify_status: na(),
        violated_clause: "fast-start-threshold".to_string(),
        outcome: "selection_declined".to_string(),
        detail: format!(
            "non-finite EV coerces to {}; the S-tier entry misses the fast-start bar and \
             speculative fallback is disallowed",
            fmt6(0.0)
        ),
        replay_cmd: replay(label),
    }];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Router,
        category: FixtureCategory::MalformedMetadata,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Router,
            FixtureCategory::MalformedMetadata,
            "non-finite EV metadata never fast-starts and declines cleanly with reasons",
            ok,
            "malformed EV metadata was not handled fail-closed",
        ),
    }
}

// ── Composition fixtures ─────────────────────────────────────────────────────

fn fix_composition_green_synergy() -> ControlFixtureEvaluation {
    let label = "composition-green-synergy";
    let gate = CompositionGate;
    let matrix = canonical_matrix();
    let registry = canonical_registry();
    let plan = CompositionPlan::new(
        "comp-green",
        [
            UpliftCandidate::ContractsAsCode,
            UpliftCandidate::EGraphOptimizer,
            UpliftCandidate::MetamorphicOracle,
            UpliftCandidate::ShadowRun,
        ],
    );
    let report = gate.evaluate(&plan, &matrix, &registry);
    let rerun = gate.evaluate(&plan, &matrix, &registry);

    let ok = report.gate_passes
        && report.hazards.is_empty()
        && report.summary.synergy_count >= 1
        && report.summary.blocking_hazard_count == 0
        && report.report_id == rerun.report_id
        && report.hazards == rerun.hazards
        && report.exported_json_stats.sha256
            == sha256_hex(report.exported_json_stats.content.as_bytes());

    let diagnostics = vec![GraveyardControlDiagnostic {
        component: ControlPlaneComponent::Composition,
        route_id: na(),
        candidate_set_hash: report.report_id.clone(),
        composition_id: report.composition_id.clone(),
        entry_header_hash: na(),
        verify_status: na(),
        violated_clause: "none".to_string(),
        outcome: "composition_passes".to_string(),
        detail: format!(
            "requirements satisfied with {} synergies detected and no hazards",
            report.summary.synergy_count
        ),
        replay_cmd: replay(label),
    }];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Composition,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Composition,
            FixtureCategory::HappyPath,
            "a requirement-satisfying composition passes with synergies detected",
            ok,
            "the green composition did not pass cleanly",
        ),
    }
}

fn fix_composition_adversarial_unmitigated() -> ControlFixtureEvaluation {
    let label = "composition-adversarial-unmitigated";
    let gate = CompositionGate;
    let matrix = canonical_matrix();
    let registry = canonical_registry();
    let primitives = [
        UpliftCandidate::CegisSynthesis,
        UpliftCandidate::EGraphOptimizer,
        UpliftCandidate::MetamorphicOracle,
    ];
    let unmitigated = gate.evaluate(
        &CompositionPlan::new("comp-danger-raw", primitives),
        &matrix,
        &registry,
    );
    let mitigated = gate.evaluate(
        &CompositionPlan::new("comp-danger-mitigated", primitives)
            .with_mitigation("include abstract_interpretation in the composition")
            .with_mitigation(
                "require a metamorphic_oracle equivalence pass on the optimized output",
            ),
        &matrix,
        &registry,
    );

    let blocked_hazard = unmitigated.hazards.iter().any(|h| {
        h.code == HazardCode::UnmitigatedDangerousCombination && h.severity == HazardSeverity::Block
    });
    let missing_tracked = unmitigated.mitigation_status.iter().any(|status| {
        status.combination_id == "dc-synthesis-rewrite-no-safety"
            && !status.mitigated
            && status.missing_mitigations.len() == 2
    });
    let ok = !unmitigated.gate_passes
        && blocked_hazard
        && missing_tracked
        && !unmitigated.blocking_remediations().is_empty()
        && mitigated.gate_passes
        && mitigated.summary.blocking_hazard_count == 0;

    let diagnostics = vec![
        GraveyardControlDiagnostic {
            component: ControlPlaneComponent::Composition,
            route_id: na(),
            candidate_set_hash: unmitigated.report_id.clone(),
            composition_id: unmitigated.composition_id.clone(),
            entry_header_hash: na(),
            verify_status: na(),
            violated_clause: HazardCode::UnmitigatedDangerousCombination
                .as_str()
                .to_string(),
            outcome: "composition_blocked".to_string(),
            detail: "cegis+egraph without safety mitigations trips the high-severity \
                     dangerous-combination registry"
                .to_string(),
            replay_cmd: replay(label),
        },
        GraveyardControlDiagnostic {
            component: ControlPlaneComponent::Composition,
            route_id: na(),
            candidate_set_hash: mitigated.report_id.clone(),
            composition_id: mitigated.composition_id.clone(),
            entry_header_hash: na(),
            verify_status: na(),
            violated_clause: "none".to_string(),
            outcome: "composition_passes".to_string(),
            detail: "applying both required mitigations verbatim clears the dangerous \
                     combination"
                .to_string(),
            replay_cmd: replay(label),
        },
    ];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Composition,
        category: FixtureCategory::AdversarialCombination,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Composition,
            FixtureCategory::AdversarialCombination,
            "high-severity dangerous combinations block unless every mitigation is applied",
            ok,
            "the dangerous-combination gate did not enforce mitigations",
        ),
    }
}

fn fix_composition_medium_advisory() -> ControlFixtureEvaluation {
    let label = "composition-medium-advisory";
    let gate = CompositionGate;
    let matrix = canonical_matrix();
    let registry = canonical_registry();
    let report = gate.evaluate(
        &CompositionPlan::new(
            "comp-medium",
            [
                UpliftCandidate::CegisSynthesis,
                UpliftCandidate::ConcolicDifferential,
                UpliftCandidate::MetamorphicOracle,
            ],
        ),
        &matrix,
        &registry,
    );

    let advisory_hazard = report.hazards.iter().any(|h| {
        h.code == HazardCode::UnmitigatedDangerousCombination && h.severity == HazardSeverity::Warn
    });
    let ok = report.gate_passes
        && advisory_hazard
        && report.summary.advisory_hazard_count >= 1
        && report.summary.blocking_hazard_count == 0
        && report.summary.dangerous_combinations_triggered >= 1;

    let diagnostics = vec![GraveyardControlDiagnostic {
        component: ControlPlaneComponent::Composition,
        route_id: na(),
        candidate_set_hash: report.report_id.clone(),
        composition_id: report.composition_id.clone(),
        entry_header_hash: na(),
        verify_status: na(),
        violated_clause: HazardCode::UnmitigatedDangerousCombination
            .as_str()
            .to_string(),
        outcome: "composition_advisory".to_string(),
        detail: "the medium-severity dual-hole combination warns without blocking".to_string(),
        replay_cmd: replay(label),
    }];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Composition,
        category: FixtureCategory::AdversarialCombination,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Composition,
            FixtureCategory::AdversarialCombination,
            "medium-severity combinations surface as advisories, not blocks",
            ok,
            "the medium-severity combination was not advisory",
        ),
    }
}

fn fix_composition_interference_gate() -> ControlFixtureEvaluation {
    let label = "composition-interference-gate";
    let gate = CompositionGate;
    let matrix = canonical_matrix();
    let registry = canonical_registry();
    let primitives = [
        UpliftCandidate::ShadowRun,
        UpliftCandidate::ConcolicDifferential,
    ];

    let bad = gate.evaluate(
        &CompositionPlan::new("comp-interference-bad", primitives)
            .with_controller(UpliftCandidate::ShadowRun, ControllerRole::Fast)
            .with_controller(UpliftCandidate::ConcolicDifferential, ControllerRole::Fast),
        &matrix,
        &registry,
    );
    let good = gate.evaluate(
        &CompositionPlan::new("comp-interference-good", primitives)
            .with_controller(UpliftCandidate::ShadowRun, ControllerRole::Fast)
            .with_controller(UpliftCandidate::ConcolicDifferential, ControllerRole::Slow)
            .with_interference_artifact("artifact://interference/shadow-vs-concolic.json"),
        &matrix,
        &registry,
    );

    let missing_artifacts = bad.hazards.iter().any(|h| {
        h.code == HazardCode::MultiControllerMissingInterferenceArtifacts
            && h.severity == HazardSeverity::Block
    });
    let missing_split = bad.hazards.iter().any(|h| {
        h.code == HazardCode::MultiControllerMissingFastSlowSplit
            && h.severity == HazardSeverity::Block
    });
    let ok = !bad.gate_passes
        && missing_artifacts
        && missing_split
        && good.gate_passes
        && good.summary.blocking_hazard_count == 0;

    let diagnostics = vec![
        GraveyardControlDiagnostic {
            component: ControlPlaneComponent::Composition,
            route_id: na(),
            candidate_set_hash: bad.report_id.clone(),
            composition_id: bad.composition_id.clone(),
            entry_header_hash: na(),
            verify_status: na(),
            violated_clause: HazardCode::MultiControllerMissingInterferenceArtifacts
                .as_str()
                .to_string(),
            outcome: "composition_blocked".to_string(),
            detail: "two controllers without interference artifacts or a fast/slow split are \
                     blocked"
                .to_string(),
            replay_cmd: replay(label),
        },
        GraveyardControlDiagnostic {
            component: ControlPlaneComponent::Composition,
            route_id: na(),
            candidate_set_hash: good.report_id.clone(),
            composition_id: good.composition_id.clone(),
            entry_header_hash: na(),
            verify_status: na(),
            violated_clause: "none".to_string(),
            outcome: "composition_passes".to_string(),
            detail: "a fast/slow split plus an interference artifact satisfies the \
                     multi-controller gate"
                .to_string(),
            replay_cmd: replay(label),
        },
    ];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Composition,
        category: FixtureCategory::AdversarialCombination,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Composition,
            FixtureCategory::AdversarialCombination,
            "multi-controller plans require interference artifacts and a fast/slow split",
            ok,
            "the multi-controller interference gate did not enforce its requirements",
        ),
    }
}

fn fix_composition_structural_malformed() -> ControlFixtureEvaluation {
    let label = "composition-structural-malformed";
    let gate = CompositionGate;
    let matrix = canonical_matrix();
    let registry = canonical_registry();

    let empty = gate.evaluate(
        &CompositionPlan::new("comp-empty", Vec::<UpliftCandidate>::new()),
        &matrix,
        &registry,
    );
    let duplicated = gate.evaluate(
        &CompositionPlan::new(
            "comp-dup",
            [UpliftCandidate::ShadowRun, UpliftCandidate::ShadowRun],
        ),
        &matrix,
        &registry,
    );
    let unknown = gate.evaluate(
        &CompositionPlan::new(
            "comp-unknown-controller",
            [
                UpliftCandidate::ShadowRun,
                UpliftCandidate::ConcolicDifferential,
            ],
        )
        .with_controller(UpliftCandidate::ShadowRun, ControllerRole::Fast)
        .with_controller(
            UpliftCandidate::AbstractInterpretation,
            ControllerRole::Slow,
        )
        .with_interference_artifact("artifact://interference/unknown-controller.json"),
        &matrix,
        &registry,
    );

    let empty_blocked = !empty.gate_passes
        && empty
            .hazards
            .iter()
            .any(|h| h.code == HazardCode::EmptyComposition);
    let dup_blocked = !duplicated.gate_passes
        && duplicated
            .hazards
            .iter()
            .any(|h| h.code == HazardCode::DuplicatePrimitive);
    let unknown_blocked = !unknown.gate_passes
        && unknown
            .hazards
            .iter()
            .any(|h| h.code == HazardCode::UnknownControllerPrimitive);
    let ok = empty_blocked && dup_blocked && unknown_blocked;

    let case = |report: &crate::composition_matrix::CompositionGateReport,
                clause: HazardCode,
                detail: &str| GraveyardControlDiagnostic {
        component: ControlPlaneComponent::Composition,
        route_id: na(),
        candidate_set_hash: report.report_id.clone(),
        composition_id: report.composition_id.clone(),
        entry_header_hash: na(),
        verify_status: na(),
        violated_clause: clause.as_str().to_string(),
        outcome: "composition_blocked".to_string(),
        detail: detail.to_string(),
        replay_cmd: replay(label),
    };
    let diagnostics = vec![
        case(
            &empty,
            HazardCode::EmptyComposition,
            "an empty composition is rejected",
        ),
        case(
            &duplicated,
            HazardCode::DuplicatePrimitive,
            "a duplicated primitive is rejected",
        ),
        case(
            &unknown,
            HazardCode::UnknownControllerPrimitive,
            "a controller naming a primitive outside the plan is rejected",
        ),
    ];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::Composition,
        category: FixtureCategory::MalformedMetadata,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::Composition,
            FixtureCategory::MalformedMetadata,
            "structurally malformed plans are always blocked",
            ok,
            "a structurally malformed plan was not blocked",
        ),
    }
}

// ── Header-schema fixtures ───────────────────────────────────────────────────

fn fix_header_complete_graduates() -> ControlFixtureEvaluation {
    let label = "header-complete-graduates";
    let compiler = EntryHeaderCompiler::default();
    let draft = complete_draft("gv-erasure-coding");
    let template = passing_template(QualityBar::Platinum);
    let size = SizeEstimate::new(450, EffortSize::Medium);
    let report = compiler.compile(&draft, &template, Some(&size));
    let rerun = compiler.compile(&draft, &template, Some(&size));

    let ok = report.can_progress
        && report.can_graduate
        && report.summary.blocking_finding_count == 0
        && report.summary.tbd_field_count == 0
        && report.summary.mandatory_provided == HeaderFieldId::ALL.len()
        && report.summary.criteria_passed == report.summary.criteria_total
        && report.summary.criteria_total > 0
        && report.effort_band == Some(EffortSize::Medium)
        && report.report_id == rerun.report_id
        && report.exported_json_stats.sha256
            == sha256_hex(report.exported_json_stats.content.as_bytes());

    let diagnostics = vec![GraveyardControlDiagnostic {
        component: ControlPlaneComponent::HeaderSchema,
        route_id: na(),
        candidate_set_hash: na(),
        composition_id: na(),
        entry_header_hash: report.report_id.clone(),
        verify_status: na(),
        violated_clause: "none".to_string(),
        outcome: "graduates".to_string(),
        detail: format!(
            "all {} fields provided, every criterion CI-linked and passed, effort penalty {}",
            HeaderFieldId::ALL.len(),
            fmt6(ev_effort_penalty(EffortSize::Medium))
        ),
        replay_cmd: replay(label),
    }];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::HeaderSchema,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::HeaderSchema,
            FixtureCategory::HappyPath,
            "a complete header with a fully-passed template graduates",
            ok,
            "the complete header did not graduate",
        ),
    }
}

fn fix_header_malformed_blocks() -> ControlFixtureEvaluation {
    let label = "header-malformed-blocks";
    let compiler = EntryHeaderCompiler::default();
    let template = passing_template(QualityBar::Gold);
    let size = SizeEstimate::new(450, EffortSize::Medium);

    // (a) A mandatory field is entirely absent.
    let mut missing = EntryHeaderDraft::new("gv-missing-field");
    for field in HeaderFieldId::ALL {
        if *field != HeaderFieldId::Papers {
            missing = missing.provide(*field, format!("{} evidence", field.as_str()));
        }
    }
    let missing_report = compiler.compile(&missing, &template, Some(&size));

    // (b) A provided value is blank after trimming.
    let blank = complete_draft("gv-blank-value")
        .set(HeaderFieldId::Repro, HeaderFieldValue::provided("   "));
    let blank_report = compiler.compile(&blank, &template, Some(&size));

    // (c) A Tbd marker without a reason.
    let unreasoned =
        complete_draft("gv-empty-tbd").set(HeaderFieldId::Papers, HeaderFieldValue::tbd(""));
    let unreasoned_report = compiler.compile(&unreasoned, &template, Some(&size));

    // (d) Declared effort contradicts the LOC-derived band (advisory).
    let mismatched_size = SizeEstimate::new(1500, EffortSize::Small);
    let mismatch_report = compiler.compile(
        &complete_draft("gv-band-mismatch"),
        &template,
        Some(&mismatched_size),
    );

    let has = |report: &crate::entry_header_compiler::EntryHeaderReport,
               code: SchemaFindingCode| {
        report.findings.iter().any(|f| f.code == code)
    };

    let bands_hold = estimate_effort_band(99) == EffortSize::Small
        && estimate_effort_band(100) == EffortSize::Medium
        && estimate_effort_band(499) == EffortSize::Medium
        && estimate_effort_band(500) == EffortSize::Large
        && estimate_effort_band(1999) == EffortSize::Large
        && estimate_effort_band(2000) == EffortSize::XLarge;
    let penalties_monotone = fmt6(ev_effort_penalty(EffortSize::Small)) == "0.000000"
        && fmt6(ev_effort_penalty(EffortSize::Medium)) == "0.500000"
        && fmt6(ev_effort_penalty(EffortSize::Large)) == "1.500000"
        && fmt6(ev_effort_penalty(EffortSize::XLarge)) == "3.000000";

    let ok = !missing_report.can_progress
        && has(
            &missing_report,
            SchemaFindingCode::MissingMandatoryHeaderField,
        )
        && !blank_report.can_progress
        && has(&blank_report, SchemaFindingCode::EmptyHeaderValue)
        && !unreasoned_report.can_progress
        && has(&unreasoned_report, SchemaFindingCode::EmptyTbdReason)
        && mismatch_report.can_progress
        && has(&mismatch_report, SchemaFindingCode::EffortBandMismatch)
        && bands_hold
        && penalties_monotone;

    let case = |report: &crate::entry_header_compiler::EntryHeaderReport,
                code: SchemaFindingCode,
                outcome: &str,
                detail: &str| GraveyardControlDiagnostic {
        component: ControlPlaneComponent::HeaderSchema,
        route_id: na(),
        candidate_set_hash: na(),
        composition_id: na(),
        entry_header_hash: report.report_id.clone(),
        verify_status: na(),
        violated_clause: code.as_str().to_string(),
        outcome: outcome.to_string(),
        detail: detail.to_string(),
        replay_cmd: replay(label),
    };
    let diagnostics = vec![
        case(
            &missing_report,
            SchemaFindingCode::MissingMandatoryHeaderField,
            "progress_blocked",
            "a draft missing the papers field cannot progress",
        ),
        case(
            &blank_report,
            SchemaFindingCode::EmptyHeaderValue,
            "progress_blocked",
            "a blank provided value cannot progress",
        ),
        case(
            &unreasoned_report,
            SchemaFindingCode::EmptyTbdReason,
            "progress_blocked",
            "a Tbd marker without a reason cannot progress",
        ),
        case(
            &mismatch_report,
            SchemaFindingCode::EffortBandMismatch,
            "advisory_flagged",
            "a declared effort contradicting the LOC band is flagged without blocking",
        ),
    ];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::HeaderSchema,
        category: FixtureCategory::MalformedMetadata,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::HeaderSchema,
            FixtureCategory::MalformedMetadata,
            "malformed header metadata blocks progress; band drift stays advisory",
            ok,
            "malformed header metadata was not classified correctly",
        ),
    }
}

fn fix_header_tbd_graduation_policy() -> ControlFixtureEvaluation {
    let label = "header-tbd-graduation-policy";
    let lenient = EntryHeaderCompiler::default();
    let strict =
        EntryHeaderCompiler::new(EntryHeaderCompilerConfig::default().with_graduation_mode(true));
    let draft = complete_draft("gv-tbd-papers").set(
        HeaderFieldId::Papers,
        HeaderFieldValue::tbd("awaiting arXiv verification"),
    );
    let template = passing_template(QualityBar::Gold);
    let size = SizeEstimate::new(450, EffortSize::Medium);

    let lenient_report = lenient.compile(&draft, &template, Some(&size));
    let strict_report = strict.compile(&draft, &template, Some(&size));

    let ok = lenient_report.can_progress
        && !lenient_report.can_graduate
        && lenient_report.summary.tbd_field_count == 1
        && !strict_report.can_progress
        && strict_report
            .findings
            .iter()
            .any(|f| f.code == SchemaFindingCode::TbdMandatoryField);

    let diagnostics = vec![
        GraveyardControlDiagnostic {
            component: ControlPlaneComponent::HeaderSchema,
            route_id: na(),
            candidate_set_hash: na(),
            composition_id: na(),
            entry_header_hash: lenient_report.report_id.clone(),
            verify_status: na(),
            violated_clause: "none".to_string(),
            outcome: "progresses_without_graduating".to_string(),
            detail: "a reasoned Tbd field progresses in lenient mode but withholds graduation"
                .to_string(),
            replay_cmd: replay(label),
        },
        GraveyardControlDiagnostic {
            component: ControlPlaneComponent::HeaderSchema,
            route_id: na(),
            candidate_set_hash: na(),
            composition_id: na(),
            entry_header_hash: strict_report.report_id.clone(),
            verify_status: na(),
            violated_clause: SchemaFindingCode::TbdMandatoryField.as_str().to_string(),
            outcome: "progress_blocked".to_string(),
            detail: "graduation mode escalates the same Tbd field to a blocking finding"
                .to_string(),
            replay_cmd: replay(label),
        },
    ];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::HeaderSchema,
        category: FixtureCategory::PolicyOverrideMisuse,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::HeaderSchema,
            FixtureCategory::PolicyOverrideMisuse,
            "Tbd escalation: lenient mode progresses, graduation mode blocks",
            ok,
            "graduation-mode escalation semantics did not hold",
        ),
    }
}

// ── Verify-gate fixtures ─────────────────────────────────────────────────────

fn verify_diagnostic_from_ledger(
    label: &str,
    report: &crate::graveyardctl::GraveyardctlReport,
    entry: &crate::graveyardctl::GraveyardctlLedgerEntry,
    violated_clause: String,
    outcome: &str,
) -> GraveyardControlDiagnostic {
    GraveyardControlDiagnostic {
        component: ControlPlaneComponent::VerifyGate,
        route_id: report.run_id.clone(),
        candidate_set_hash: report.evidence_checksum.clone(),
        composition_id: na(),
        entry_header_hash: na(),
        verify_status: entry.verify_result.as_str().to_string(),
        violated_clause,
        outcome: outcome.to_string(),
        detail: format!(
            "entry {} card {}: {}",
            entry.entry_id, entry.card_id, entry.detail
        ),
        replay_cmd: replay(label),
    }
}

fn fix_verify_default_corpus_passes() -> ControlFixtureEvaluation {
    let label = "verify-default-corpus-passes";
    let engine = GraveyardctlEngine::new("graveyard-control-tests/happy", default_active_corpus());
    let report = engine.run(None);
    let rerun = engine.run(None);

    let verify_lines: Vec<_> = report
        .ledger
        .iter()
        .filter(|line| line.stage == GraveyardctlStage::Verify)
        .collect();
    let ok = report.gate_applies
        && report.gate_passes
        && report.summary.verify_total == 3
        && report.summary.verify_pass == 3
        && report.summary.verify_incomplete == 0
        && report.summary.verify_inconsistent == 0
        && report.summary.stages_covered == 5
        && report.summary.required_fields_complete
        && verify_lines
            .iter()
            .all(|line| line.verify_result == VerifyResult::Pass)
        && report == rerun;

    let diagnostics = verify_lines
        .iter()
        .map(|line| {
            verify_diagnostic_from_ledger(label, &report, line, "none".to_string(), "verify_pass")
        })
        .collect();

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::VerifyGate,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::VerifyGate,
            FixtureCategory::HappyPath,
            "the default active corpus verifies all-pass across all five stages",
            ok,
            "the default corpus did not verify all-pass deterministically",
        ),
    }
}

fn fix_verify_incomplete_blocked() -> ControlFixtureEvaluation {
    let label = "verify-incomplete-blocked";
    let engine = GraveyardctlEngine::new(
        "graveyard-control-tests/incomplete",
        vec![complete_active_entry(), incomplete_active_entry()],
    );
    let report = engine.run(None);

    let incomplete_line = report
        .ledger
        .iter()
        .find(|line| {
            line.stage == GraveyardctlStage::Verify
                && line.verify_result == VerifyResult::Incomplete
        })
        .cloned();
    let ok = !report.gate_passes
        && report.summary.verify_total == 2
        && report.summary.verify_pass == 1
        && report.summary.verify_incomplete == 1
        && report.summary.entries_with_missing_artifacts >= 1
        && incomplete_line
            .as_ref()
            .is_some_and(|line| !line.missing_artifacts.is_empty() && !line.remediation.is_empty());

    let violated = incomplete_line
        .as_ref()
        .and_then(|line| line.missing_artifacts.first().cloned())
        .unwrap_or_else(|| "missing-artifacts-unreported".to_string());
    let diagnostics = incomplete_line
        .as_ref()
        .map(|line| {
            vec![verify_diagnostic_from_ledger(
                label,
                &report,
                line,
                violated,
                "verify_incomplete",
            )]
        })
        .unwrap_or_else(|| {
            vec![GraveyardControlDiagnostic {
                component: ControlPlaneComponent::VerifyGate,
                route_id: report.run_id.clone(),
                candidate_set_hash: report.evidence_checksum.clone(),
                composition_id: na(),
                entry_header_hash: na(),
                verify_status: "missing".to_string(),
                violated_clause: "incomplete-line-absent".to_string(),
                outcome: "verify_incomplete".to_string(),
                detail: "expected an incomplete verify line but none was emitted".to_string(),
                replay_cmd: replay(label),
            }]
        });

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::VerifyGate,
        category: FixtureCategory::MalformedMetadata,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::VerifyGate,
            FixtureCategory::MalformedMetadata,
            "an artifact-incomplete contract fails verify with named missing artifacts",
            ok,
            "the incomplete contract did not fail closed with missing artifacts",
        ),
    }
}

fn fix_verify_inconsistent_blocked() -> ControlFixtureEvaluation {
    let label = "verify-inconsistent-blocked";
    let engine = GraveyardctlEngine::new(
        "graveyard-control-tests/inconsistent",
        vec![complete_active_entry(), inconsistent_active_entry()],
    );
    let report = engine.run(None);

    let inconsistent_line = report
        .ledger
        .iter()
        .find(|line| {
            line.stage == GraveyardctlStage::Verify
                && line.verify_result == VerifyResult::Inconsistent
        })
        .cloned();
    let ok = !report.gate_passes
        && report.summary.verify_total == 2
        && report.summary.verify_pass == 1
        && report.summary.verify_incomplete == 0
        && report.summary.verify_inconsistent == 1
        && inconsistent_line
            .as_ref()
            .is_some_and(|line| !line.remediation.is_empty());

    let diagnostics = inconsistent_line
        .as_ref()
        .map(|line| {
            vec![verify_diagnostic_from_ledger(
                label,
                &report,
                line,
                "dangerous-combination-escalation".to_string(),
                "verify_inconsistent",
            )]
        })
        .unwrap_or_else(|| {
            vec![GraveyardControlDiagnostic {
                component: ControlPlaneComponent::VerifyGate,
                route_id: report.run_id.clone(),
                candidate_set_hash: report.evidence_checksum.clone(),
                composition_id: na(),
                entry_header_hash: na(),
                verify_status: "missing".to_string(),
                violated_clause: "inconsistent-line-absent".to_string(),
                outcome: "verify_inconsistent".to_string(),
                detail: "expected an inconsistent verify line but none was emitted".to_string(),
                replay_cmd: replay(label),
            }]
        });

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::VerifyGate,
        category: FixtureCategory::InconsistentContract,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::VerifyGate,
            FixtureCategory::InconsistentContract,
            "a lint-passing but risk-inconsistent contract fails verify as inconsistent",
            ok,
            "the inconsistent contract was not classified as inconsistent",
        ),
    }
}

fn fix_verify_release_gauntlet() -> ControlFixtureEvaluation {
    let label = "verify-release-gauntlet";
    let report = run_graveyard_verify_report("graveyard-control-tests/gauntlet");
    let rerun = run_graveyard_verify_report("graveyard-control-tests/gauntlet");

    let incomplete_caught = report.verdicts.iter().any(|v| {
        v.scenario_kind == ScenarioKind::IncompleteContract
            && !v.promoted
            && v.expectation_met
            && v.defect_dimensions
                .contains(&VerifyDimension::ContractCompleteness)
    });
    let ok = report.gate_passes
        && report.summary.total_scenarios == 4
        && report.summary.all_expectations_met
        && report.summary.dimensions_covered == VerifyDimension::ALL.len()
        && report.summary.required_fields_complete
        && incomplete_caught
        && report == rerun;

    let diagnostics = vec![GraveyardControlDiagnostic {
        component: ControlPlaneComponent::VerifyGate,
        route_id: report.report_id.clone(),
        candidate_set_hash: report.evidence_checksum.clone(),
        composition_id: na(),
        entry_header_hash: na(),
        verify_status: if report.gate_passes { "pass" } else { "fail" }.to_string(),
        violated_clause: VerifyDimension::ContractCompleteness
            .clause_prefix()
            .to_string(),
        outcome: "release_gauntlet_holds".to_string(),
        detail: format!(
            "{} scenarios ran; the incomplete-contract red path surfaced a {} defect and the \
             gate replayed byte-identically",
            report.summary.total_scenarios,
            VerifyDimension::ContractCompleteness.as_str()
        ),
        replay_cmd: replay(label),
    }];

    ControlFixtureEvaluation {
        label: label.to_string(),
        component: ControlPlaneComponent::VerifyGate,
        category: FixtureCategory::InconsistentContract,
        diagnostics,
        verdict: verdict(
            label,
            ControlPlaneComponent::VerifyGate,
            FixtureCategory::InconsistentContract,
            "the four-scenario release gauntlet passes while catching its red paths",
            ok,
            "the release gauntlet did not hold or missed a red path",
        ),
    }
}

// ── Corpus + report ──────────────────────────────────────────────────────────

/// The fixed fixture corpus (sorted by label).
#[must_use]
pub fn graveyard_control_corpus() -> Vec<ControlFixtureEvaluation> {
    let mut all = vec![
        fix_router_fast_start_happy(),
        fix_router_tie_break_determinism(),
        fix_router_override_legitimate(),
        fix_router_override_misuse_refused(),
        fix_router_malformed_ev_metadata(),
        fix_composition_green_synergy(),
        fix_composition_adversarial_unmitigated(),
        fix_composition_medium_advisory(),
        fix_composition_interference_gate(),
        fix_composition_structural_malformed(),
        fix_header_complete_graduates(),
        fix_header_malformed_blocks(),
        fix_header_tbd_graduation_policy(),
        fix_verify_default_corpus_passes(),
        fix_verify_incomplete_blocked(),
        fix_verify_inconsistent_blocked(),
        fix_verify_release_gauntlet(),
    ];
    all.sort_by(|a, b| a.label.cmp(&b.label));
    all
}

/// Aggregate summary + gate booleans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardControlSummary {
    /// Harness schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// Checksum over sorted diagnostics + verdicts.
    pub evidence_checksum: String,
    /// Total fixtures evaluated.
    pub total_fixtures: usize,
    /// Total diagnostics emitted.
    pub total_diagnostics: usize,
    /// Fixtures whose oracle matched.
    pub matched_fixtures: usize,
    /// Distinct control-plane components exercised.
    pub components_covered: usize,
    /// Happy-path category exercised and matched.
    pub happy_path_covered: bool,
    /// Malformed-metadata category exercised and matched.
    pub malformed_metadata_covered: bool,
    /// Inconsistent-contract category exercised and matched.
    pub inconsistent_contract_covered: bool,
    /// Policy-override category exercised and matched.
    pub policy_override_covered: bool,
    /// Adversarial-combination category exercised and matched.
    pub adversarial_combination_covered: bool,
    /// Every diagnostic carries all mandated fields.
    pub required_fields_complete: bool,
    /// Every fixture matched its oracle.
    pub all_expectations_met: bool,
    /// All four components exercised.
    pub all_components_covered: bool,
    /// Fail-closed gate verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
}

/// Deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardControlStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The full validation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraveyardControlValidationReport {
    /// Harness schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// Sorted diagnostics.
    pub diagnostics: Vec<GraveyardControlDiagnostic>,
    /// Sorted verdicts.
    pub verdicts: Vec<OutcomeVerdict>,
    /// Aggregate summary.
    pub summary: GraveyardControlSummary,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: GraveyardControlStatsArtifact,
    /// Checksum over sorted diagnostics + verdicts.
    pub evidence_checksum: String,
}

impl GraveyardControlValidationReport {
    /// AC3 failure logs: every diagnostic missing a mandated field, plus every
    /// diagnostic belonging to a component whose oracle mismatched (the
    /// builders always populate the structural fields, so the field-presence
    /// filter alone could never fire).
    #[must_use]
    pub fn failure_logs(&self) -> Vec<GraveyardControlFailureLog> {
        let failing_components: BTreeSet<ControlPlaneComponent> = self
            .verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .map(|v| v.component)
            .collect();
        self.diagnostics
            .iter()
            .filter(|d| !d.has_required_fields() || failing_components.contains(&d.component))
            .map(GraveyardControlDiagnostic::failure_log)
            .collect()
    }

    /// Verdicts whose oracle mismatched.
    #[must_use]
    pub fn failing_verdicts(&self) -> Vec<&OutcomeVerdict> {
        self.verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .collect()
    }

    /// Whether the fail-closed gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

fn category_matched(corpus: &[ControlFixtureEvaluation], category: FixtureCategory) -> bool {
    corpus
        .iter()
        .any(|f| f.category == category && f.verdict.matches_expected)
}

/// Run the full control-plane validation and assemble the fail-closed report.
#[must_use]
pub fn run_graveyard_control_validation(label: &str) -> GraveyardControlValidationReport {
    let corpus = graveyard_control_corpus();

    let mut diagnostics: Vec<GraveyardControlDiagnostic> =
        corpus.iter().flat_map(|f| f.diagnostics.clone()).collect();
    diagnostics.sort_by(|a, b| {
        a.component
            .cmp(&b.component)
            .then_with(|| a.route_id.cmp(&b.route_id))
            .then_with(|| a.composition_id.cmp(&b.composition_id))
            .then_with(|| a.entry_header_hash.cmp(&b.entry_header_hash))
            .then_with(|| a.outcome.cmp(&b.outcome))
            .then_with(|| a.detail.cmp(&b.detail))
    });

    let mut verdicts: Vec<OutcomeVerdict> = corpus.iter().map(|f| f.verdict.clone()).collect();
    verdicts.sort_by(|a, b| {
        a.component
            .cmp(&b.component)
            .then_with(|| a.fixture_label.cmp(&b.fixture_label))
    });

    #[derive(Serialize)]
    struct EvidenceInput<'a> {
        diagnostics: &'a [GraveyardControlDiagnostic],
        verdicts: &'a [OutcomeVerdict],
    }
    let evidence_checksum = stable_hash(&EvidenceInput {
        diagnostics: &diagnostics,
        verdicts: &verdicts,
    });

    #[derive(Serialize)]
    struct ReportIdInput<'a> {
        schema_version: &'a str,
        label: &'a str,
        evidence_checksum: &'a str,
    }
    let report_id = format!(
        "graveyard-control-tests-{}",
        short_hash(&stable_hash(&ReportIdInput {
            schema_version: GRAVEYARD_CONTROL_TESTS_SCHEMA_VERSION,
            label,
            evidence_checksum: &evidence_checksum,
        }))
    );

    let components_covered = {
        let mut components: Vec<ControlPlaneComponent> =
            diagnostics.iter().map(|d| d.component).collect();
        components.sort();
        components.dedup();
        components.len()
    };
    let all_components_covered = components_covered == ControlPlaneComponent::ALL.len();
    let required_fields_complete = diagnostics
        .iter()
        .all(GraveyardControlDiagnostic::has_required_fields);
    let matched_fixtures = verdicts.iter().filter(|v| v.matches_expected).count();
    let all_expectations_met = matched_fixtures == verdicts.len();

    let happy_path_covered = category_matched(&corpus, FixtureCategory::HappyPath);
    let malformed_metadata_covered = category_matched(&corpus, FixtureCategory::MalformedMetadata);
    let inconsistent_contract_covered =
        category_matched(&corpus, FixtureCategory::InconsistentContract);
    let policy_override_covered = category_matched(&corpus, FixtureCategory::PolicyOverrideMisuse);
    let adversarial_combination_covered =
        category_matched(&corpus, FixtureCategory::AdversarialCombination);

    let gate_passes = required_fields_complete
        && all_expectations_met
        && all_components_covered
        && happy_path_covered
        && malformed_metadata_covered
        && inconsistent_contract_covered
        && policy_override_covered
        && adversarial_combination_covered;

    let summary = GraveyardControlSummary {
        schema_version: GRAVEYARD_CONTROL_TESTS_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_fixtures: corpus.len(),
        total_diagnostics: diagnostics.len(),
        matched_fixtures,
        components_covered,
        happy_path_covered,
        malformed_metadata_covered,
        inconsistent_contract_covered,
        policy_override_covered,
        adversarial_combination_covered,
        required_fields_complete,
        all_expectations_met,
        all_components_covered,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib graveyard_control_tests # report {report_id}"
        ),
    };

    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a GraveyardControlSummary,
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: GRAVEYARD_CONTROL_TESTS_SCHEMA_VERSION,
        report_id: &report_id,
        summary: &summary,
    })
    .unwrap_or_default();
    let exported_json_stats = GraveyardControlStatsArtifact {
        path: format!("graveyard_control_tests/{report_id}.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    };

    GraveyardControlValidationReport {
        schema_version: GRAVEYARD_CONTROL_TESTS_SCHEMA_VERSION.to_string(),
        report_id,
        label: label.to_string(),
        diagnostics,
        verdicts,
        summary,
        exported_json_stats,
        evidence_checksum,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::graveyardctl::run_graveyardctl_report;

    fn fixtures_for(component: ControlPlaneComponent) -> Vec<ControlFixtureEvaluation> {
        graveyard_control_corpus()
            .into_iter()
            .filter(|f| f.component == component)
            .collect()
    }

    #[test]
    fn router_fixtures_all_match() {
        let fixtures = fixtures_for(ControlPlaneComponent::Router);
        assert_eq!(fixtures.len(), 5);
        for fixture in fixtures {
            assert!(
                fixture.verdict.matches_expected,
                "{}: {}",
                fixture.label, fixture.verdict.mismatch
            );
        }
    }

    #[test]
    fn composition_fixtures_all_match() {
        let fixtures = fixtures_for(ControlPlaneComponent::Composition);
        assert_eq!(fixtures.len(), 5);
        for fixture in fixtures {
            assert!(
                fixture.verdict.matches_expected,
                "{}: {}",
                fixture.label, fixture.verdict.mismatch
            );
        }
    }

    #[test]
    fn header_fixtures_all_match() {
        let fixtures = fixtures_for(ControlPlaneComponent::HeaderSchema);
        assert_eq!(fixtures.len(), 3);
        for fixture in fixtures {
            assert!(
                fixture.verdict.matches_expected,
                "{}: {}",
                fixture.label, fixture.verdict.mismatch
            );
        }
    }

    #[test]
    fn verify_fixtures_all_match() {
        let fixtures = fixtures_for(ControlPlaneComponent::VerifyGate);
        assert_eq!(fixtures.len(), 4);
        for fixture in fixtures {
            assert!(
                fixture.verdict.matches_expected,
                "{}: {}",
                fixture.label, fixture.verdict.mismatch
            );
        }
    }

    #[test]
    fn full_validation_passes_gate_and_covers_categories() {
        let report = run_graveyard_control_validation("ci");
        assert!(
            report.gate_passes(),
            "gate failed: {:?}",
            report.failing_verdicts()
        );
        assert_eq!(report.summary.total_fixtures, 17);
        assert_eq!(report.summary.matched_fixtures, 17);
        assert_eq!(report.summary.components_covered, 4);
        assert!(report.summary.all_components_covered);
        assert!(report.summary.happy_path_covered);
        assert!(report.summary.malformed_metadata_covered);
        assert!(report.summary.inconsistent_contract_covered);
        assert!(report.summary.policy_override_covered);
        assert!(report.summary.adversarial_combination_covered);
        assert!(report.summary.required_fields_complete);
        assert!(report.summary.all_expectations_met);
        assert!(report.failing_verdicts().is_empty());
        assert!(report.failure_logs().is_empty());
    }

    #[test]
    fn every_diagnostic_carries_ac3_fields() {
        let report = run_graveyard_control_validation("ac3");
        assert!(!report.diagnostics.is_empty());
        for diagnostic in &report.diagnostics {
            assert!(
                diagnostic.has_required_fields(),
                "incomplete: {diagnostic:?}"
            );
            assert!(!diagnostic.route_id.is_empty());
            assert!(!diagnostic.candidate_set_hash.is_empty());
            assert!(!diagnostic.composition_id.is_empty());
            assert!(!diagnostic.entry_header_hash.is_empty());
            assert!(!diagnostic.verify_status.is_empty());
            assert!(!diagnostic.violated_clause.is_empty());
            assert!(diagnostic.replay_cmd.contains("graveyard_control_tests"));
        }
    }

    #[test]
    fn oracle_mismatch_yields_replayable_failure_logs() {
        let mut report = run_graveyard_control_validation("mismatch");
        assert!(report.failure_logs().is_empty());

        report.verdicts[0].matches_expected = false;
        report.verdicts[0].mismatch = "forced mismatch for the failure-log contract".to_string();
        let failing_component = report.verdicts[0].component;
        let logs = report.failure_logs();
        assert!(!logs.is_empty());
        assert!(logs.iter().all(|log| !log.replay_cmd.is_empty()));
        let expected = report
            .diagnostics
            .iter()
            .filter(|d| d.component == failing_component)
            .count();
        assert_eq!(logs.len(), expected);
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_graveyard_control_validation("stats");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert!(report.exported_json_stats.path.contains(&report.report_id));
    }

    #[test]
    fn report_id_and_replay_reference_module() {
        let report = run_graveyard_control_validation("replay");
        assert!(report.report_id.starts_with("graveyard-control-tests-"));
        assert!(
            report
                .summary
                .replay_command
                .contains("graveyard_control_tests")
        );
        assert!(report.summary.replay_command.contains(&report.report_id));
    }

    #[test]
    fn diagnostics_roundtrip_serde_byte_identically() {
        let report = run_graveyard_control_validation("serde");
        let encoded = serde_json::to_string(&report.diagnostics).expect("serialize");
        let decoded: Vec<GraveyardControlDiagnostic> =
            serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(report.diagnostics, decoded);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_graveyard_control_validation(&label);
            let second = run_graveyard_control_validation(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
        }

        #[test]
        fn prop_diagnostics_label_independent(a in "[a-z]{1,8}", b in "[a-z]{1,8}") {
            let first = run_graveyard_control_validation(&a);
            let second = run_graveyard_control_validation(&b);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_graveyard_control_validation(&label);
            prop_assert!(report.gate_passes());
            prop_assert_eq!(report.summary.components_covered, 4);
        }

        #[test]
        fn prop_router_selection_is_permutation_invariant(
            corpus in Just(canonical_corpus()).prop_shuffle()
        ) {
            let router = SymptomRouter::default();
            let hotspots: Vec<MigrationHotspot> = Vec::new();
            let exceptions: Vec<PolicyException> = Vec::new();
            let baseline = router.evaluate(&canonical_corpus(), &hotspots, &exceptions);
            let shuffled = router.evaluate(&corpus, &hotspots, &exceptions);
            for symptom in SymptomClass::ALL {
                let expected = baseline.recommendation_for(*symptom).expect("baseline");
                let observed = shuffled.recommendation_for(*symptom).expect("shuffled");
                prop_assert_eq!(&expected.selected_entry_id, &observed.selected_entry_id);
                prop_assert_eq!(&expected.candidate_pool_hash, &observed.candidate_pool_hash);
            }
        }

        #[test]
        fn prop_router_ev_boost_keeps_fast_start_selection(delta in 0.0f64..3.0) {
            let router = SymptomRouter::default();
            let hotspots: Vec<MigrationHotspot> = Vec::new();
            let exceptions: Vec<PolicyException> = Vec::new();
            let corpus: Vec<CanonicalEntry> = canonical_corpus()
                .into_iter()
                .map(|entry| {
                    if entry.entry_id == "gv-erasure-coding" {
                        let boosted = entry.ev_score + delta;
                        CanonicalEntry::new(
                            entry.entry_id.clone(),
                            entry.title.clone(),
                            entry.tier,
                            boosted,
                        )
                        .with_symptoms(entry.symptom_tags.clone())
                    } else {
                        entry
                    }
                })
                .collect();
            let rec = router.route(SymptomClass::TailLatency, &corpus, &hotspots, &exceptions);
            prop_assert_eq!(rec.selected_entry_id.as_deref(), Some("gv-erasure-coding"));
            prop_assert!(rec.fast_start_applied);
        }

        #[test]
        fn prop_safe_mitigation_additions_are_monotone(m1 in any::<bool>(), m2 in any::<bool>()) {
            let gate = CompositionGate;
            let matrix = canonical_matrix();
            let registry = canonical_registry();
            let build = |first: bool, second: bool| {
                let mut plan = CompositionPlan::new(
                    "comp-monotone",
                    [
                        UpliftCandidate::CegisSynthesis,
                        UpliftCandidate::EGraphOptimizer,
                        UpliftCandidate::MetamorphicOracle,
                    ],
                );
                if first {
                    plan = plan
                        .with_mitigation("include abstract_interpretation in the composition");
                }
                if second {
                    plan = plan.with_mitigation(
                        "require a metamorphic_oracle equivalence pass on the optimized output",
                    );
                }
                gate.evaluate(&plan, &matrix, &registry)
            };
            let partial = build(m1, m2);
            let full = build(true, true);
            prop_assert!(
                full.summary.blocking_hazard_count <= partial.summary.blocking_hazard_count
            );
            prop_assert!(full.gate_passes);
        }

        #[test]
        fn prop_underlying_engines_replay_deterministically(label in "[a-z]{1,8}") {
            let first = run_graveyardctl_report(&label);
            let second = run_graveyardctl_report(&label);
            prop_assert_eq!(first, second);

            let compiler = EntryHeaderCompiler::default();
            let draft = complete_draft("gv-prop-entry");
            let template = passing_template(QualityBar::Gold);
            let size = SizeEstimate::new(320, EffortSize::Medium);
            let lhs = compiler.compile(&draft, &template, Some(&size));
            let rhs = compiler.compile(&draft, &template, Some(&size));
            prop_assert_eq!(lhs.report_id, rhs.report_id);
        }
    }
}

//! E2E alien-artifact deep-assurance gauntlet (bd-3bxhj.10.45): streaming
//! evidence fusion, sequential multi-testing, counterfactual/fragility
//! drills, degradation + recovery campaigns, guarantee-applicability faults,
//! and galaxy-brain UX contracts — a mandatory pre-release-candidate gate.
//!
//! Twelve scenarios (one green anchor + one adversarial red path per family)
//! drive the REAL artifact-coding kernels end to end:
//!
//! - streaming fusion: online conjugate windows tighten the posterior;
//!   dependence correction deflates; sparse/drift windows degrade
//!   conservatively with sensitivity bands and predictive checks;
//! - sequential multi-testing: interleaved async gates certify an
//!   order-invariant e-BH set under optional stopping; adversarial spikes
//!   clamp, exhaustion surfaces, malformed evidence fails closed;
//! - counterfactual drills: every critical decision carries a minimal-flip
//!   explanation or an explicit unsat proof; fragile decisions force the
//!   mitigation path;
//! - degradation + recovery: MNAR/contradictory campaigns clamp with
//!   operator review and bounded inflation; recovery only passes through
//!   the hysteresis guard;
//! - guarantee faults: assumption violations injected mid-run invalidate or
//!   downgrade certificates into conservative fallback, with unaffected
//!   mechanisms staying valid;
//! - galaxy contracts: L0-L3 deterministic rendering under accessibility and
//!   perf budgets, hard non-interference, and visible (never silent)
//!   truncation of adversarial wide cards.
//!
//! The JSONL evidence pack carries posterior, wealth, guarantee,
//! counterfactual, degradation, and UX records on every line (`n/a`
//! sentinels where a record does not apply), plus a machine-checkable
//! `failure_signature` on every red path and a per-line reproduction
//! command (AC2 + AC4). The ledger is float-free, derives `Eq`, and replays
//! byte-identically (AC1; proven by proptest). Precedents:
//! `formal_assurance_gauntlet` (bd-3bxhj.10.28) and `graveyard_gauntlet`
//! (bd-3bxhj.10.37); kernel fixture recipes from `artifact_coding_tests`
//! (bd-3bxhj.10.44).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::counterfactual_audit::{FragilityBand, run_default_counterfactual_audit};
use crate::degradation_policy::{
    DegradationMode, DegradationPolicy, DegradationReason, EvidenceSignal, MissingnessClass,
    SignalStatus, default_recovery_sequence,
};
use crate::galaxy_brain_ux::{default_ux_sources, run_galaxy_ux};
use crate::guarantee_assumption_registry::{
    ApplicabilityVerdict, AssumptionObservation, AssumptionRegistry, GuaranteeMechanism,
    run_default_guarantee_assumption_report,
};
use crate::hierarchical_fusion::{
    FusionChannel, FusionClaim, FusionConfig, KernelObservation, KernelPrior, run_fusion_report,
};
use crate::semantic_contract::MigrationDecision;
use crate::sequential_fdr::{
    ConservativeReason, FdrStage, FdrTest, GateFamily, SequentialFdrConfig,
    SequentialFdrController, default_test_corpus,
};

/// Schema version for the deep-assurance gauntlet report/ledger.
pub const DEEP_ASSURANCE_SCHEMA_VERSION: &str = "deep-assurance-gauntlet-v1";

/// Schema version for the materialized pipeline manifest.
pub const DEEP_ASSURANCE_PIPELINE_SCHEMA_VERSION: &str = "deep-assurance-gauntlet-pipeline-v1";

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

fn parse_f64(value: &str) -> f64 {
    value.parse::<f64>().unwrap_or(f64::NAN)
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// The six deep-assurance scenario families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceFamily {
    /// Online conjugate fusion over long-running sessions.
    StreamingFusion,
    /// Interleaved asynchronous e-BH + alpha-investing gates.
    SequentialTesting,
    /// Minimal-flip explanations and fragility policing.
    CounterfactualDrills,
    /// Missing/contradictory evidence campaigns and recovery.
    DegradationRecovery,
    /// Mid-run assumption violations and guarantee downgrades.
    GuaranteeFaults,
    /// Galaxy-brain L0-L3 rendering/accessibility/perf contracts.
    GalaxyContracts,
}

impl AssuranceFamily {
    /// All families.
    pub const ALL: [AssuranceFamily; 6] = [
        AssuranceFamily::StreamingFusion,
        AssuranceFamily::SequentialTesting,
        AssuranceFamily::CounterfactualDrills,
        AssuranceFamily::DegradationRecovery,
        AssuranceFamily::GuaranteeFaults,
        AssuranceFamily::GalaxyContracts,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AssuranceFamily::StreamingFusion => "streaming_fusion",
            AssuranceFamily::SequentialTesting => "sequential_testing",
            AssuranceFamily::CounterfactualDrills => "counterfactual_drills",
            AssuranceFamily::DegradationRecovery => "degradation_recovery",
            AssuranceFamily::GuaranteeFaults => "guarantee_faults",
            AssuranceFamily::GalaxyContracts => "galaxy_contracts",
        }
    }
}

/// The twelve gauntlet scenarios (a green anchor and a red path per family).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceScenario {
    /// Streaming windows tighten the posterior; dependence deflates.
    FusionStreamingHolds,
    /// Sparse + drift windows degrade conservatively.
    FusionSparseDrift,
    /// Interleaved gates certify an order-invariant set.
    FdrInterleavedHolds,
    /// Spikes clamp, exhaustion surfaces, malformed fails closed.
    FdrAdversarialStopping,
    /// Every critical decision explained; fragile ones policed.
    CounterfactualExplained,
    /// A fully-constrained decision yields an unsat proof.
    CounterfactualUnsat,
    /// Recovery passes only through the hysteresis guard.
    DegradationRecovers,
    /// MNAR/contradictory campaigns clamp with review.
    DegradationCampaign,
    /// The default guarantee corpus validates end to end.
    GuaranteeAllValid,
    /// A mid-run assumption violation invalidates to fallback.
    GuaranteeMidRunFault,
    /// The default deck passes every UX contract clause.
    UxContractsHold,
    /// An adversarial wide card truncates visibly within budgets.
    UxAdversarialStress,
}

impl AssuranceScenario {
    /// All scenarios, in canonical order.
    pub const ALL: [AssuranceScenario; 12] = [
        AssuranceScenario::FusionStreamingHolds,
        AssuranceScenario::FusionSparseDrift,
        AssuranceScenario::FdrInterleavedHolds,
        AssuranceScenario::FdrAdversarialStopping,
        AssuranceScenario::CounterfactualExplained,
        AssuranceScenario::CounterfactualUnsat,
        AssuranceScenario::DegradationRecovers,
        AssuranceScenario::DegradationCampaign,
        AssuranceScenario::GuaranteeAllValid,
        AssuranceScenario::GuaranteeMidRunFault,
        AssuranceScenario::UxContractsHold,
        AssuranceScenario::UxAdversarialStress,
    ];

    /// Stable scenario id.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AssuranceScenario::FusionStreamingHolds => "fusion_streaming_holds",
            AssuranceScenario::FusionSparseDrift => "fusion_sparse_drift",
            AssuranceScenario::FdrInterleavedHolds => "fdr_interleaved_holds",
            AssuranceScenario::FdrAdversarialStopping => "fdr_adversarial_stopping",
            AssuranceScenario::CounterfactualExplained => "counterfactual_explained",
            AssuranceScenario::CounterfactualUnsat => "counterfactual_unsat",
            AssuranceScenario::DegradationRecovers => "degradation_recovers",
            AssuranceScenario::DegradationCampaign => "degradation_campaign",
            AssuranceScenario::GuaranteeAllValid => "guarantee_all_valid",
            AssuranceScenario::GuaranteeMidRunFault => "guarantee_mid_run_fault",
            AssuranceScenario::UxContractsHold => "ux_contracts_hold",
            AssuranceScenario::UxAdversarialStress => "ux_adversarial_stress",
        }
    }

    /// The scenario family.
    #[must_use]
    pub fn family(self) -> AssuranceFamily {
        match self {
            AssuranceScenario::FusionStreamingHolds | AssuranceScenario::FusionSparseDrift => {
                AssuranceFamily::StreamingFusion
            }
            AssuranceScenario::FdrInterleavedHolds | AssuranceScenario::FdrAdversarialStopping => {
                AssuranceFamily::SequentialTesting
            }
            AssuranceScenario::CounterfactualExplained | AssuranceScenario::CounterfactualUnsat => {
                AssuranceFamily::CounterfactualDrills
            }
            AssuranceScenario::DegradationRecovers | AssuranceScenario::DegradationCampaign => {
                AssuranceFamily::DegradationRecovery
            }
            AssuranceScenario::GuaranteeAllValid | AssuranceScenario::GuaranteeMidRunFault => {
                AssuranceFamily::GuaranteeFaults
            }
            AssuranceScenario::UxContractsHold | AssuranceScenario::UxAdversarialStress => {
                AssuranceFamily::GalaxyContracts
            }
        }
    }

    /// The safe action path the scenario must reach.
    #[must_use]
    pub fn expected_path(self) -> &'static str {
        match self {
            AssuranceScenario::FusionStreamingHolds => "posterior_tightens",
            AssuranceScenario::FusionSparseDrift => "conservative_fallback",
            AssuranceScenario::FdrInterleavedHolds => "certified_order_invariant",
            AssuranceScenario::FdrAdversarialStopping => "withheld_and_fail_closed",
            AssuranceScenario::CounterfactualExplained => "fragility_policed",
            AssuranceScenario::CounterfactualUnsat => "unsat_proven",
            AssuranceScenario::DegradationRecovers => "recovered_through_guard",
            AssuranceScenario::DegradationCampaign => "clamped_with_review",
            AssuranceScenario::GuaranteeAllValid => "all_valid",
            AssuranceScenario::GuaranteeMidRunFault => "invalidated_to_fallback",
            AssuranceScenario::UxContractsHold => "contracts_hold",
            AssuranceScenario::UxAdversarialStress => "truncated_visibly",
        }
    }

    /// Whether the scenario is an adversarial red path.
    #[must_use]
    pub fn is_red_path(self) -> bool {
        matches!(
            self,
            AssuranceScenario::FusionSparseDrift
                | AssuranceScenario::FdrAdversarialStopping
                | AssuranceScenario::CounterfactualUnsat
                | AssuranceScenario::DegradationCampaign
                | AssuranceScenario::GuaranteeMidRunFault
                | AssuranceScenario::UxAdversarialStress
        )
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One deep-assurance ledger line (float-free; derives `Eq`). Carries the
/// full evidence-pack record set with `n/a` sentinels where a record does
/// not apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepAssuranceLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Scenario id.
    pub scenario_id: String,
    /// Scenario family.
    pub family: AssuranceFamily,
    /// Backing kernel module.
    pub kernel: String,
    /// Campaign phase (e.g. `window-2`, `inject`, `recover`).
    pub phase: String,
    /// Posterior record (fusion state id; `n/a` otherwise).
    pub posterior_record: String,
    /// Wealth record (`before->after`; `n/a` otherwise).
    pub wealth_record: String,
    /// Guarantee record (certificate id; `n/a` otherwise).
    pub guarantee_record: String,
    /// Counterfactual record (decision id + norm; `n/a` otherwise).
    pub counterfactual_record: String,
    /// Degradation record (reason CSV; `n/a` otherwise).
    pub degradation_record: String,
    /// UX record (view content id; `n/a` otherwise).
    pub ux_record: String,
    /// Observed safe action path.
    pub action_path: String,
    /// Required safe action path.
    pub expected_path: String,
    /// Whether the phase behaved safely.
    pub safe: bool,
    /// Machine-checkable failure signature (`none` on green phases; AC2).
    pub failure_signature: String,
    /// Human-readable detail.
    pub detail: String,
    /// One-command replay handle (AC4).
    pub reproduction_command: String,
}

impl DeepAssuranceLedgerEntry {
    /// Whether every mandated field is populated (sentinels count; empty
    /// strings do not).
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.run_id.is_empty()
            && !self.scenario_id.is_empty()
            && !self.phase.is_empty()
            && !self.posterior_record.is_empty()
            && !self.wealth_record.is_empty()
            && !self.guarantee_record.is_empty()
            && !self.counterfactual_record.is_empty()
            && !self.degradation_record.is_empty()
            && !self.ux_record.is_empty()
            && !self.action_path.is_empty()
            && !self.expected_path.is_empty()
            && !self.failure_signature.is_empty()
            && !self.detail.is_empty()
            && !self.reproduction_command.is_empty()
    }
}

/// Render the ledger as one JSON object per line.
#[must_use]
pub fn render_ledger_jsonl(ledger: &[DeepAssuranceLedgerEntry]) -> String {
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

struct PhaseOutcome {
    phase: String,
    kernel: &'static str,
    posterior_record: String,
    wealth_record: String,
    guarantee_record: String,
    counterfactual_record: String,
    degradation_record: String,
    ux_record: String,
    action_path: String,
    safe: bool,
    failure_signature: String,
    detail: String,
}

impl PhaseOutcome {
    fn new(phase: &str, kernel: &'static str) -> Self {
        Self {
            phase: phase.to_string(),
            kernel,
            posterior_record: na(),
            wealth_record: na(),
            guarantee_record: na(),
            counterfactual_record: na(),
            degradation_record: na(),
            ux_record: na(),
            action_path: String::new(),
            safe: false,
            failure_signature: "none".to_string(),
            detail: String::new(),
        }
    }
}

/// The deep-assurance E2E gauntlet engine.
#[derive(Debug, Clone)]
pub struct DeepAssuranceGauntlet {
    label: String,
    run_id: String,
}

impl DeepAssuranceGauntlet {
    /// Build an engine with a deterministic run id derived from the label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "deep-assurance-{}",
            short_hash(&stable_hash(&format!(
                "{DEEP_ASSURANCE_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { label, run_id }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn entry(
        &self,
        scenario: AssuranceScenario,
        outcome: PhaseOutcome,
    ) -> DeepAssuranceLedgerEntry {
        DeepAssuranceLedgerEntry {
            schema_version: DEEP_ASSURANCE_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            scenario_id: scenario.as_str().to_string(),
            family: scenario.family(),
            kernel: outcome.kernel.to_string(),
            phase: outcome.phase,
            posterior_record: outcome.posterior_record,
            wealth_record: outcome.wealth_record,
            guarantee_record: outcome.guarantee_record,
            counterfactual_record: outcome.counterfactual_record,
            degradation_record: outcome.degradation_record,
            ux_record: outcome.ux_record,
            action_path: outcome.action_path,
            expected_path: scenario.expected_path().to_string(),
            safe: outcome.safe,
            failure_signature: outcome.failure_signature,
            detail: outcome.detail,
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- deep-assurance --label '{}' # run {} scenario {}",
                self.label,
                self.run_id,
                scenario.as_str()
            ),
        }
    }

    fn scenario_fusion_streaming(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::FusionStreamingHolds;
        let prior = || KernelPrior::Beta {
            alpha: 2.0,
            beta: 2.0,
        };
        let window = |channels: Vec<FusionChannel>| {
            run_fusion_report(
                "deep-assurance/streaming",
                &[FusionClaim::new(
                    "claim.stream",
                    "stratum.stream",
                    prior(),
                    channels,
                )],
                FusionConfig::default(),
            )
        };
        let ch = |id: &str, successes: f64, failures: f64, corr: f64| {
            FusionChannel::new(
                id,
                KernelObservation::Bernoulli {
                    successes,
                    failures,
                },
                corr,
            )
        };

        let w1 = window(vec![ch("ch.a", 20.0, 5.0, 0.0)]);
        let w2 = window(vec![ch("ch.a", 20.0, 5.0, 0.0), ch("ch.b", 30.0, 8.0, 0.0)]);
        let w3 = window(vec![
            ch("ch.a", 20.0, 5.0, 0.0),
            ch("ch.b", 30.0, 8.0, 0.0),
            ch("ch.c", 45.0, 12.0, 0.0),
        ]);
        let dependent = window(vec![
            ch("ch.a", 20.0, 5.0, 0.8),
            ch("ch.b", 30.0, 8.0, 0.8),
            ch("ch.c", 45.0, 12.0, 0.8),
        ]);

        let var = |report: &crate::hierarchical_fusion::FusionReport| {
            report
                .entry("claim.stream")
                .map_or(f64::NAN, |e| parse_f64(&e.posterior_variance))
        };
        let tightens = var(&w3).is_finite() && var(&w3) <= var(&w1) + 1e-9;
        let dep_entry = dependent.entry("claim.stream");
        let deflates = dep_entry.is_some_and(|e| {
            let factor = parse_f64(&e.dependence_factor);
            factor.is_finite() && factor < 1.0 && factor > 0.0
        }) && var(&dependent) >= var(&w3) - 1e-12;
        let all_green = [&w1, &w2, &w3].iter().all(|r| {
            r.entry("claim.stream")
                .is_some_and(|e| !e.degraded_confidence)
        });

        let mut entries = Vec::new();
        for (phase, report) in [("window-1", &w1), ("window-2", &w2), ("window-3", &w3)] {
            let entry = report.entry("claim.stream");
            let mut outcome = PhaseOutcome::new(phase, "hierarchical_fusion");
            outcome.posterior_record = entry.map_or_else(na, |e| e.posterior_state_id.clone());
            outcome.action_path = "posterior_tightens".to_string();
            outcome.safe = tightens && all_green;
            outcome.detail = format!(
                "posterior variance {} under accumulating evidence",
                entry.map_or_else(na, |e| e.posterior_variance.clone())
            );
            entries.push(self.entry(scenario, outcome));
        }
        let mut dep = PhaseOutcome::new("dependence", "hierarchical_fusion");
        dep.posterior_record = dep_entry.map_or_else(na, |e| e.posterior_state_id.clone());
        dep.action_path = "posterior_tightens".to_string();
        dep.safe = deflates;
        dep.detail = format!(
            "correlated session deflates evidence (factor {}) without inflating confidence",
            dep_entry.map_or_else(na, |e| e.dependence_factor.clone())
        );
        entries.push(self.entry(scenario, dep));
        entries
    }

    fn scenario_fusion_sparse_drift(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::FusionSparseDrift;
        let sparse = run_fusion_report(
            "deep-assurance/sparse",
            &[FusionClaim::new(
                "claim.sparse",
                "stratum.sparse",
                KernelPrior::Beta {
                    alpha: 1.0,
                    beta: 1.0,
                },
                vec![FusionChannel::new(
                    "ch.sparse",
                    KernelObservation::Bernoulli {
                        successes: 1.0,
                        failures: 0.0,
                    },
                    0.0,
                )],
            )],
            FusionConfig::default(),
        );
        let drift = run_fusion_report(
            "deep-assurance/drift",
            &[FusionClaim::new(
                "claim.drift",
                "stratum.drift",
                KernelPrior::Beta {
                    alpha: 190.0,
                    beta: 10.0,
                },
                vec![FusionChannel::new(
                    "ch.drift",
                    KernelObservation::Bernoulli {
                        successes: 5.0,
                        failures: 45.0,
                    },
                    0.0,
                )],
            )],
            FusionConfig::default(),
        );

        let sparse_entry = sparse.entry("claim.sparse");
        let drift_entry = drift.entry("claim.drift");
        let sparse_safe = sparse_entry.is_some_and(|e| {
            e.high_sensitivity && e.recommended_decision == MigrationDecision::ConservativeFallback
        });
        let drift_safe = drift_entry.is_some_and(|e| {
            !e.predictive_check_passed
                && e.recommended_decision == MigrationDecision::ConservativeFallback
        });

        let mut sparse_out = PhaseOutcome::new("sparse-window", "hierarchical_fusion");
        sparse_out.posterior_record =
            sparse_entry.map_or_else(na, |e| e.posterior_state_id.clone());
        sparse_out.degradation_record = "high_sensitivity".to_string();
        sparse_out.action_path = "conservative_fallback".to_string();
        sparse_out.safe = sparse_safe;
        sparse_out.failure_signature = "sensitivity-band-exceeded".to_string();
        sparse_out.detail = format!(
            "sparse window band {} exceeds the threshold and degrades conservatively",
            sparse_entry.map_or_else(na, |e| e.sensitivity_band.clone())
        );

        let mut drift_out = PhaseOutcome::new("drift-window", "hierarchical_fusion");
        drift_out.posterior_record = drift_entry.map_or_else(na, |e| e.posterior_state_id.clone());
        drift_out.degradation_record = "predictive_failure".to_string();
        drift_out.action_path = "conservative_fallback".to_string();
        drift_out.safe = drift_safe;
        drift_out.failure_signature = "predictive-check-failed".to_string();
        drift_out.detail = "drifted evidence contradicts the strong prior and degrades \
                            conservatively"
            .to_string();

        vec![
            self.entry(scenario, sparse_out),
            self.entry(scenario, drift_out),
        ]
    }

    fn scenario_fdr_interleaved(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::FdrInterleavedHolds;
        let corpus = default_test_corpus();
        let mut reversed = corpus.clone();
        reversed.reverse();
        let forward = SequentialFdrController::new(
            "deep-assurance/interleaved",
            SequentialFdrConfig::default(),
            corpus,
        )
        .run(None);
        let backward = SequentialFdrController::new(
            "deep-assurance/interleaved",
            SequentialFdrConfig::default(),
            reversed,
        )
        .run(None);

        let invest_line = forward
            .ledger
            .iter()
            .find(|line| line.stage == FdrStage::Invest);
        let safe = forward.gate_passes
            && forward == backward
            && forward.summary.ebh_count_matches
            && forward.summary.wealth_conserved
            && forward.summary.fdr_monotone_ok;

        let mut outcome = PhaseOutcome::new("interleaved-stream", "sequential_fdr");
        outcome.wealth_record = invest_line.map_or_else(na, |line| {
            format!("{}->{}", line.wealth_before, line.wealth_after)
        });
        outcome.action_path = "certified_order_invariant".to_string();
        outcome.safe = safe;
        outcome.detail = format!(
            "e-BH certified {} of {} identically under reversed async arrival",
            forward.summary.ebh_certified, forward.summary.ebh_total
        );
        vec![self.entry(scenario, outcome)]
    }

    fn scenario_fdr_adversarial(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::FdrAdversarialStopping;
        let adversarial = SequentialFdrController::new(
            "deep-assurance/adversarial",
            SequentialFdrConfig::default().with_evalue_cap(1_000.0),
            vec![
                FdrTest::with_evalue("q.spike", GateFamily::Quality, 0, 1.0e9),
                FdrTest::with_evalue("q.normal", GateFamily::Quality, 1, 25.0),
            ],
        )
        .run(None);
        let exhausted = SequentialFdrController::new(
            "deep-assurance/exhaustion",
            SequentialFdrConfig::default()
                .with_initial_wealth(0.02)
                .with_wealth_floor(0.015),
            vec![
                FdrTest::with_evalue("p.weak1", GateFamily::Performance, 0, 1.0),
                FdrTest::with_evalue("p.weak2", GateFamily::Performance, 1, 1.0),
                FdrTest::with_evalue("p.weak3", GateFamily::Performance, 2, 1.0),
            ],
        )
        .run(None);
        let malformed = SequentialFdrController::new(
            "deep-assurance/malformed",
            SequentialFdrConfig::default(),
            vec![FdrTest::with_evalue(
                "bad.negative",
                GateFamily::Quality,
                0,
                -1.0,
            )],
        )
        .run(None);

        let exhausted_line = exhausted.ledger.iter().find(|line| {
            line.stage == FdrStage::Invest
                && line.conservative_reason == ConservativeReason::WealthExhausted
        });

        let mut clamp = PhaseOutcome::new("adversarial-spike", "sequential_fdr");
        clamp.action_path = "withheld_and_fail_closed".to_string();
        clamp.safe = adversarial.gate_passes && adversarial.summary.adversarial >= 1;
        clamp.failure_signature = "adversarial-evidence-clamped".to_string();
        clamp.detail = "a 1e9 e-value clamps at the cap and is withheld, never silently \
                        certified"
            .to_string();

        let mut drain = PhaseOutcome::new("optional-stopping-drain", "sequential_fdr");
        drain.wealth_record = exhausted_line.map_or_else(na, |line| {
            format!("{}->{}", line.wealth_before, line.wealth_after)
        });
        drain.action_path = "withheld_and_fail_closed".to_string();
        drain.safe = exhausted_line.is_some() && exhausted.summary.conservative_events >= 1;
        drain.failure_signature = "wealth-exhausted".to_string();
        drain.detail = "stopping early on a weak streak exhausts wealth and surfaces a \
                        conservative withhold"
            .to_string();

        let mut invalid = PhaseOutcome::new("malformed-evidence", "sequential_fdr");
        invalid.action_path = "withheld_and_fail_closed".to_string();
        invalid.safe = !malformed.gate_passes && malformed.summary.invalid >= 1;
        invalid.failure_signature = "invalid-evalue-fails-closed".to_string();
        invalid.detail = "a negative e-value invalidates the corpus and the gate fails \
                          closed"
            .to_string();

        vec![
            self.entry(scenario, clamp),
            self.entry(scenario, drain),
            self.entry(scenario, invalid),
        ]
    }

    fn scenario_counterfactual_explained(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::CounterfactualExplained;
        let report = run_default_counterfactual_audit("deep-assurance/cf");

        let every_explained = report
            .cards
            .iter()
            .all(|card| card.satisfiable || !card.unsat_proof.is_empty());
        let fragile_policed = report
            .cards
            .iter()
            .filter(|card| card.fragile)
            .all(|card| card.requires_mitigation && !card.policy_clause.is_empty());
        let minimal = report.cards.iter().filter(|c| c.satisfiable).all(|card| {
            let nearest = parse_f64(&card.perturbation_norm);
            nearest.is_finite()
                && card
                    .all_flips
                    .iter()
                    .filter(|flip| flip.satisfiable)
                    .all(|flip| nearest <= parse_f64(&flip.l1_norm) + 1e-9)
        });
        let safe = report.gate_passes()
            && report.summary.counterfactual_present
            && every_explained
            && fragile_policed
            && minimal;

        let fragile_card = report.cards.iter().find(|c| c.fragile);
        let sample = fragile_card.or_else(|| report.cards.first());
        let mut outcome = PhaseOutcome::new("critical-decisions", "counterfactual_audit");
        outcome.counterfactual_record = sample.map_or_else(na, |card| {
            format!("{}:{}", card.decision_id, card.perturbation_norm)
        });
        outcome.action_path = "fragility_policed".to_string();
        outcome.safe = safe;
        outcome.detail = format!(
            "{} decisions carry minimal-flip explanations; fragile ones are blocked behind \
             mitigation",
            report.summary.total_decisions
        );
        vec![self.entry(scenario, outcome)]
    }

    fn scenario_counterfactual_unsat(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::CounterfactualUnsat;
        let report = run_default_counterfactual_audit("deep-assurance/cf-unsat");
        let unsat = report.card("dec.immutable_bad");
        let safe = unsat.is_some_and(|card| {
            !card.satisfiable
                && card.fragility_band == FragilityBand::Unflippable
                && !card.unsat_proof.is_empty()
        }) && report.gate_passes();

        let mut outcome = PhaseOutcome::new("constrained-decision", "counterfactual_audit");
        outcome.counterfactual_record =
            unsat.map_or_else(na, |card| format!("{}:unflippable", card.decision_id));
        outcome.action_path = "unsat_proven".to_string();
        outcome.safe = safe;
        outcome.failure_signature = "no-feasible-flip".to_string();
        outcome.detail = unsat.map_or_else(na, |card| card.unsat_proof.clone());
        vec![self.entry(scenario, outcome)]
    }

    fn scenario_degradation_recovers(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::DegradationRecovers;
        let policy = DegradationPolicy::default();
        let transitions = policy.simulate("deep-assurance/recovery", &default_recovery_sequence());

        let entered = transitions
            .iter()
            .any(|t| t.reason.starts_with("enter_degraded"));
        let held = transitions
            .iter()
            .any(|t| t.reason == "hold_degraded:guard_unmet");
        let recovered = transitions
            .iter()
            .any(|t| t.reason == "recover:guard_satisfied");
        let ends_normal = transitions
            .last()
            .is_some_and(|t| t.to_mode == DegradationMode::Normal);
        let safe = entered && held && recovered && ends_normal;

        let mut outcome = PhaseOutcome::new("recovery-cycle", "degradation_policy");
        outcome.degradation_record = "enter,hold:guard_unmet,recover".to_string();
        outcome.action_path = "recovered_through_guard".to_string();
        outcome.safe = safe;
        outcome.detail = format!(
            "{} cycles: quality inside the hysteresis band holds Degraded until the guard \
             is satisfied",
            transitions.len()
        );
        vec![self.entry(scenario, outcome)]
    }

    fn scenario_degradation_campaign(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::DegradationCampaign;
        let policy = DegradationPolicy::default();
        let mnar = policy.compile(
            "deep-assurance/deg",
            "campaign.mnar",
            MigrationDecision::AutoApprove,
            &[
                EvidenceSignal::new(
                    "safety",
                    2.0,
                    SignalStatus::Missing {
                        class: MissingnessClass::Mnar,
                    },
                )
                .critical(),
                EvidenceSignal::new("coverage", 1.0, SignalStatus::Present { quality: 0.90 }),
            ],
        );
        let contradictory = policy.compile(
            "deep-assurance/deg",
            "campaign.contradiction",
            MigrationDecision::AutoApprove,
            &[
                EvidenceSignal::new("coverage", 1.0, SignalStatus::Present { quality: 0.90 }),
                EvidenceSignal::new(
                    "dissent",
                    1.0,
                    SignalStatus::Contradictory { dissent: 0.85 },
                ),
            ],
        );

        let reasons_csv = |reasons: &[DegradationReason]| {
            reasons
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        let mnar_safe = mnar.final_action == MigrationDecision::ConservativeFallback
            && mnar.operator_review_required
            && mnar
                .degradation_reasons
                .contains(&DegradationReason::CriticalHazard);
        let contradiction_safe = contradictory.final_action
            == MigrationDecision::ConservativeFallback
            && contradictory
                .degradation_reasons
                .contains(&DegradationReason::AnomalyClamp)
            && parse_f64(&contradictory.inflation_factor) <= 4.0 + 1e-9;

        let mut mnar_out = PhaseOutcome::new("mnar-campaign", "degradation_policy");
        mnar_out.degradation_record = reasons_csv(&mnar.degradation_reasons);
        mnar_out.action_path = "clamped_with_review".to_string();
        mnar_out.safe = mnar_safe;
        mnar_out.failure_signature = "critical-mnar-clamp".to_string();
        mnar_out.detail = "a critical MNAR gap clamps to conservative fallback with operator \
                           review"
            .to_string();

        let mut con_out = PhaseOutcome::new("contradiction-campaign", "degradation_policy");
        con_out.degradation_record = reasons_csv(&contradictory.degradation_reasons);
        con_out.action_path = "clamped_with_review".to_string();
        con_out.safe = contradiction_safe;
        con_out.failure_signature = "anomaly-hard-clamp".to_string();
        con_out.detail = "high dissent trips the anomaly clamp with bounded inflation".to_string();

        vec![
            self.entry(scenario, mnar_out),
            self.entry(scenario, con_out),
        ]
    }

    fn scenario_guarantee_all_valid(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::GuaranteeAllValid;
        let report = run_default_guarantee_assumption_report("deep-assurance/reg");
        let safe = report.gate_passes
            && report.summary.valid == report.summary.total_claims
            && report.summary.mechanisms_covered == GuaranteeMechanism::ALL.len();

        let mut outcome = PhaseOutcome::new("baseline", "guarantee_assumption_registry");
        outcome.guarantee_record = report
            .certificates
            .first()
            .map_or_else(na, |cert| cert.guarantee_id.clone());
        outcome.action_path = "all_valid".to_string();
        outcome.safe = safe;
        outcome.detail = format!(
            "all {} mechanisms validate under fully-satisfied assumptions",
            report.summary.mechanisms_covered
        );
        vec![self.entry(scenario, outcome)]
    }

    fn scenario_guarantee_mid_run_fault(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::GuaranteeMidRunFault;
        let registry = AssumptionRegistry::default();
        let baseline = registry.evaluate(
            GuaranteeMechanism::Conformal,
            "claim.midrun",
            &AssumptionObservation::fully_satisfied(),
        );
        let injected = registry.evaluate(
            GuaranteeMechanism::Conformal,
            "claim.midrun",
            &AssumptionObservation::fully_satisfied().with_exchangeability(0.30),
        );
        let control = registry.evaluate(
            GuaranteeMechanism::EProcess,
            "claim.midrun-control",
            &AssumptionObservation::fully_satisfied().with_drift(0.90),
        );

        let mut base_out = PhaseOutcome::new("baseline", "guarantee_assumption_registry");
        base_out.guarantee_record = baseline.guarantee_id.clone();
        base_out.action_path = "invalidated_to_fallback".to_string();
        base_out.safe = baseline.applicability_verdict == ApplicabilityVerdict::Valid;
        base_out.detail = "the conformal guarantee is valid before the fault".to_string();

        let mut inject_out = PhaseOutcome::new("inject", "guarantee_assumption_registry");
        inject_out.guarantee_record = injected.guarantee_id.clone();
        inject_out.action_path = "invalidated_to_fallback".to_string();
        inject_out.safe = injected.applicability_verdict == ApplicabilityVerdict::Invalid
            && injected.recommended_decision == MigrationDecision::ConservativeFallback
            && injected
                .violated_assumptions
                .iter()
                .any(|id| id == "ASM-EXCH")
            && !injected.fallback_clause.is_empty();
        inject_out.failure_signature = "assumption-invalidated:ASM-EXCH".to_string();
        inject_out.detail = "a mid-run exchangeability break voids the guarantee and forces \
                             conservative fallback"
            .to_string();

        let mut control_out = PhaseOutcome::new("control", "guarantee_assumption_registry");
        control_out.guarantee_record = control.guarantee_id.clone();
        control_out.action_path = "invalidated_to_fallback".to_string();
        control_out.safe = control.applicability_verdict == ApplicabilityVerdict::Valid;
        control_out.detail = "the e-process guarantee is untouched by drift (stationarity is \
                              not in its assumption set)"
            .to_string();

        vec![
            self.entry(scenario, base_out),
            self.entry(scenario, inject_out),
            self.entry(scenario, control_out),
        ]
    }

    fn scenario_ux_contracts_hold(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::UxContractsHold;
        let report = run_galaxy_ux("deep-assurance/ux", &default_ux_sources());
        let safe = report.gate_passes
            && report.summary.non_interference_proven
            && report.summary.accessibility_pass
            && report.summary.perf_within_budget
            && report.summary.interaction_coverage;

        let mut outcome = PhaseOutcome::new("full-deck", "galaxy_brain_ux");
        outcome.ux_record = report
            .views
            .first()
            .map_or_else(na, |view| view.content_id.clone());
        outcome.action_path = "contracts_hold".to_string();
        outcome.safe = safe;
        outcome.detail = format!(
            "{} views render deterministically under accessibility and perf budgets with \
             hard non-interference",
            report.summary.total_views
        );
        vec![self.entry(scenario, outcome)]
    }

    fn scenario_ux_adversarial_stress(&self) -> Vec<DeepAssuranceLedgerEntry> {
        let scenario = AssuranceScenario::UxAdversarialStress;
        let report = run_galaxy_ux("deep-assurance/ux-stress", &default_ux_sources());
        let mut reversed_sources = default_ux_sources();
        reversed_sources.reverse();
        let reordered = run_galaxy_ux("deep-assurance/ux-stress", &reversed_sources);

        let truncates_visibly = report
            .views
            .iter()
            .any(|view| view.lines.iter().any(|line| line.contains("more terms")));
        let budgets_hold = report.summary.perf_within_budget;
        let hashes_rederive = report.views.iter().all(|view| {
            view.content_hash == short_hash(&stable_hash(&(&view.content_id, &view.lines)))
        });
        let order_independent = report == reordered;
        let safe = truncates_visibly && budgets_hold && hashes_rederive && order_independent;

        let stress_view = report
            .views
            .iter()
            .find(|view| view.lines.iter().any(|line| line.contains("more terms")));
        let mut outcome = PhaseOutcome::new("wide-card-stress", "galaxy_brain_ux");
        outcome.ux_record = stress_view.map_or_else(na, |view| view.content_id.clone());
        outcome.action_path = "truncated_visibly".to_string();
        outcome.safe = safe;
        outcome.failure_signature = "adversarial-width-contained".to_string();
        outcome.detail = "a 40-term wide card truncates with an explicit more-terms line, \
                          stays within budgets, and hashes re-derive independently"
            .to_string();
        vec![self.entry(scenario, outcome)]
    }

    /// Run the full twelve-scenario gauntlet.
    #[must_use]
    pub fn run(&self) -> DeepAssuranceReport {
        let mut ledger: Vec<DeepAssuranceLedgerEntry> = Vec::new();
        for scenario in AssuranceScenario::ALL {
            let entries = match scenario {
                AssuranceScenario::FusionStreamingHolds => self.scenario_fusion_streaming(),
                AssuranceScenario::FusionSparseDrift => self.scenario_fusion_sparse_drift(),
                AssuranceScenario::FdrInterleavedHolds => self.scenario_fdr_interleaved(),
                AssuranceScenario::FdrAdversarialStopping => self.scenario_fdr_adversarial(),
                AssuranceScenario::CounterfactualExplained => {
                    self.scenario_counterfactual_explained()
                }
                AssuranceScenario::CounterfactualUnsat => self.scenario_counterfactual_unsat(),
                AssuranceScenario::DegradationRecovers => self.scenario_degradation_recovers(),
                AssuranceScenario::DegradationCampaign => self.scenario_degradation_campaign(),
                AssuranceScenario::GuaranteeAllValid => self.scenario_guarantee_all_valid(),
                AssuranceScenario::GuaranteeMidRunFault => self.scenario_guarantee_mid_run_fault(),
                AssuranceScenario::UxContractsHold => self.scenario_ux_contracts_hold(),
                AssuranceScenario::UxAdversarialStress => self.scenario_ux_adversarial_stress(),
            };
            ledger.extend(entries);
        }

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "deep-assurance-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );

        let required_fields_complete = ledger
            .iter()
            .all(DeepAssuranceLedgerEntry::has_required_fields);
        let all_safe = ledger.iter().all(|entry| entry.safe);
        let scenarios_covered = {
            let mut ids: Vec<&str> = ledger.iter().map(|e| e.scenario_id.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            ids.len()
        };
        let families_covered = {
            let mut families: Vec<AssuranceFamily> = ledger.iter().map(|e| e.family).collect();
            families.sort();
            families.dedup();
            families.len()
        };
        let red_paths_covered = AssuranceScenario::ALL
            .iter()
            .filter(|s| s.is_red_path())
            .all(|s| {
                ledger.iter().any(|e| {
                    e.scenario_id == s.as_str()
                        && e.action_path == s.expected_path()
                        && e.safe
                        && e.failure_signature != "none"
                })
            });
        let green_anchors = AssuranceScenario::ALL
            .iter()
            .filter(|s| !s.is_red_path())
            .all(|s| ledger.iter().any(|e| e.scenario_id == s.as_str() && e.safe));
        let record_pack_complete = ledger.iter().any(|e| e.posterior_record != "n/a")
            && ledger.iter().any(|e| e.wealth_record != "n/a")
            && ledger.iter().any(|e| e.guarantee_record != "n/a")
            && ledger.iter().any(|e| e.counterfactual_record != "n/a")
            && ledger.iter().any(|e| e.degradation_record != "n/a")
            && ledger.iter().any(|e| e.ux_record != "n/a");

        let gate_passes = required_fields_complete
            && all_safe
            && scenarios_covered == AssuranceScenario::ALL.len()
            && families_covered == AssuranceFamily::ALL.len()
            && red_paths_covered
            && green_anchors
            && record_pack_complete;

        let replay_command = format!(
            "cargo run -p doctor_frankentui -- deep-assurance --label '{}' # report {report_id}",
            self.label
        );

        let summary = DeepAssuranceSummary {
            schema_version: DEEP_ASSURANCE_SCHEMA_VERSION.to_string(),
            report_id: report_id.clone(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.clone(),
            total_scenarios: scenarios_covered,
            total_ledger_lines: ledger.len(),
            families_covered,
            safe_lines: ledger.iter().filter(|e| e.safe).count(),
            required_fields_complete,
            all_safe,
            red_paths_covered,
            green_anchors,
            record_pack_complete,
            gate_passes,
            replay_command: replay_command.clone(),
        };

        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a DeepAssuranceSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: DEEP_ASSURANCE_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let exported_json_stats = DeepAssuranceStatsArtifact {
            path: format!("deep_assurance_gauntlet/{report_id}.json"),
            sha256: sha256_hex(content.as_bytes()),
            content,
        };

        DeepAssuranceReport {
            schema_version: DEEP_ASSURANCE_SCHEMA_VERSION.to_string(),
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

/// Run the gauntlet under a label.
#[must_use]
pub fn run_deep_assurance_gauntlet(label: &str) -> DeepAssuranceReport {
    DeepAssuranceGauntlet::new(label).run()
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Aggregate summary + gate booleans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepAssuranceSummary {
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
    /// Distinct scenarios exercised.
    pub total_scenarios: usize,
    /// Ledger lines emitted.
    pub total_ledger_lines: usize,
    /// Distinct families exercised.
    pub families_covered: usize,
    /// Lines that behaved safely.
    pub safe_lines: usize,
    /// Every ledger line carries all mandated fields.
    pub required_fields_complete: bool,
    /// Every phase behaved safely.
    pub all_safe: bool,
    /// Every red path reached its expected safe action with a signature.
    pub red_paths_covered: bool,
    /// Every green anchor held.
    pub green_anchors: bool,
    /// The evidence pack carries all six record types.
    pub record_pack_complete: bool,
    /// Fail-closed gate verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
}

/// Deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepAssuranceStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory deep-assurance report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepAssuranceReport {
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
    /// The evidence ledger.
    pub ledger: Vec<DeepAssuranceLedgerEntry>,
    /// Aggregate summary.
    pub summary: DeepAssuranceSummary,
    /// Fail-closed gate verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: DeepAssuranceStatsArtifact,
}

impl DeepAssuranceReport {
    /// Render the ledger as JSONL.
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }

    /// Ledger lines for a scenario.
    #[must_use]
    pub fn lines_for(&self, scenario: AssuranceScenario) -> Vec<&DeepAssuranceLedgerEntry> {
        self.ledger
            .iter()
            .filter(|e| e.scenario_id == scenario.as_str())
            .collect()
    }
}

// ── Pipeline materializer ────────────────────────────────────────────────────

/// Pipeline configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct DeepAssurancePipelineConfig {
    /// Run directory name under the run root.
    pub run_name: String,
    /// Run label.
    pub label: String,
}

impl Default for DeepAssurancePipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "deep_assurance".to_string(),
            label: "deep-assurance/e2e".to_string(),
        }
    }
}

/// A materialized artifact with integrity metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepAssuranceArtifact {
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
pub struct DeepAssurancePipelineOutcome {
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
    pub summary: DeepAssuranceSummary,
    /// Tracked artifacts (the manifest does not track itself).
    pub artifacts: Vec<DeepAssuranceArtifact>,
}

fn artifact_of(file: &str, content: &str) -> DeepAssuranceArtifact {
    DeepAssuranceArtifact {
        name: file.replace(['.', '/'], "-"),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Materialize the gauntlet evidence bundle under `run_root/<run_name>/`.
pub fn run_deep_assurance_pipeline(
    run_root: &Path,
    config: &DeepAssurancePipelineConfig,
) -> crate::error::Result<DeepAssurancePipelineOutcome> {
    let report = run_deep_assurance_gauntlet(&config.label);
    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary).unwrap_or_default();

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "deep_assurance_stats.json";
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
        artifacts: &'a [DeepAssuranceArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: DEEP_ASSURANCE_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })
    .unwrap_or_default();
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(DeepAssurancePipelineOutcome {
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

/// CLI arguments for the deep-assurance subcommand.
#[derive(Debug, clap::Args)]
pub struct DeepAssuranceArgs {
    /// Root directory for materialized evidence.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/deep_assurance"
    )]
    pub run_root: PathBuf,

    /// Run directory name under the run root.
    #[arg(long = "run-name", default_value = "deep_assurance")]
    pub run_name: String,

    /// Run label folded into run/report ids.
    #[arg(long = "label", default_value = "deep-assurance/e2e")]
    pub label: String,
}

/// Run the deep-assurance subcommand (fail-closed).
pub fn run_deep_assurance_command(args: DeepAssuranceArgs) -> crate::error::Result<()> {
    let config = DeepAssurancePipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_deep_assurance_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("deep-assurance gauntlet"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "scenarios: {}, families: {}, ledger lines: {} (safe: {})",
            summary.total_scenarios,
            summary.families_covered,
            summary.total_ledger_lines,
            summary.safe_lines
        ));
        ui.info(&format!(
            "red paths: {}, green anchors: {}, record pack complete: {}",
            summary.red_paths_covered, summary.green_anchors, summary.record_pack_complete
        ));
        if summary.gate_passes {
            ui.success("deep-assurance gate PASSED");
        } else {
            ui.error("deep-assurance gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "deep-assurance gate failed: all_safe={}, families_covered={}, \
                 red_paths_covered={}, green_anchors={}, record_pack_complete={}",
                summary.all_safe,
                summary.families_covered,
                summary.red_paths_covered,
                summary.green_anchors,
                summary.record_pack_complete
            ),
        ))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn report() -> DeepAssuranceReport {
        run_deep_assurance_gauntlet("test")
    }

    #[test]
    fn gauntlet_gate_passes_and_covers_all_families() {
        let report = report();
        assert!(
            report.gate_passes,
            "unsafe lines: {:?}",
            report
                .ledger
                .iter()
                .filter(|e| !e.safe)
                .map(|e| (&e.scenario_id, &e.phase))
                .collect::<Vec<_>>()
        );
        assert_eq!(report.summary.total_scenarios, 12);
        assert_eq!(report.summary.families_covered, 6);
        assert!(report.summary.all_safe);
        assert!(report.summary.red_paths_covered);
        assert!(report.summary.green_anchors);
        assert!(report.summary.record_pack_complete);
        assert!(report.summary.required_fields_complete);
    }

    #[test]
    fn every_scenario_reaches_its_expected_path() {
        let report = report();
        for scenario in AssuranceScenario::ALL {
            let lines = report.lines_for(scenario);
            assert!(!lines.is_empty(), "{}", scenario.as_str());
            assert!(
                lines
                    .iter()
                    .all(|line| line.action_path == scenario.expected_path() && line.safe),
                "{}",
                scenario.as_str()
            );
        }
    }

    #[test]
    fn red_paths_carry_machine_signatures() {
        let report = report();
        for scenario in AssuranceScenario::ALL {
            if !scenario.is_red_path() {
                continue;
            }
            let lines = report.lines_for(scenario);
            assert!(
                lines.iter().any(|line| line.failure_signature != "none"),
                "{}",
                scenario.as_str()
            );
        }
    }

    #[test]
    fn streaming_fusion_tightens_and_deflates() {
        let report = report();
        let lines = report.lines_for(AssuranceScenario::FusionStreamingHolds);
        assert_eq!(lines.len(), 4);
        assert!(lines.iter().all(|line| line.posterior_record != "n/a"));
        assert!(lines.iter().any(|line| line.phase == "dependence"));
    }

    #[test]
    fn adversarial_stopping_covers_clamp_drain_and_fail_closed() {
        let report = report();
        let lines = report.lines_for(AssuranceScenario::FdrAdversarialStopping);
        assert_eq!(lines.len(), 3);
        let signatures: Vec<&str> = lines
            .iter()
            .map(|line| line.failure_signature.as_str())
            .collect();
        assert!(signatures.contains(&"adversarial-evidence-clamped"));
        assert!(signatures.contains(&"wealth-exhausted"));
        assert!(signatures.contains(&"invalid-evalue-fails-closed"));
        assert!(
            lines
                .iter()
                .any(|line| line.wealth_record != "n/a" && line.wealth_record.contains("->"))
        );
    }

    #[test]
    fn guarantee_fault_shows_valid_invalid_control_arc() {
        let report = report();
        let lines = report.lines_for(AssuranceScenario::GuaranteeMidRunFault);
        assert_eq!(lines.len(), 3);
        let phases: Vec<&str> = lines.iter().map(|line| line.phase.as_str()).collect();
        assert_eq!(phases, vec!["baseline", "inject", "control"]);
        assert!(
            lines
                .iter()
                .any(|line| line.failure_signature == "assumption-invalidated:ASM-EXCH")
        );
        assert!(lines.iter().all(|line| line.guarantee_record != "n/a"));
    }

    #[test]
    fn evidence_pack_carries_all_six_record_types() {
        let report = report();
        assert!(report.ledger.iter().any(|e| e.posterior_record != "n/a"));
        assert!(report.ledger.iter().any(|e| e.wealth_record != "n/a"));
        assert!(report.ledger.iter().any(|e| e.guarantee_record != "n/a"));
        assert!(
            report
                .ledger
                .iter()
                .any(|e| e.counterfactual_record != "n/a")
        );
        assert!(report.ledger.iter().any(|e| e.degradation_record != "n/a"));
        assert!(report.ledger.iter().any(|e| e.ux_record != "n/a"));
    }

    #[test]
    fn every_ledger_line_is_replayable() {
        let report = report();
        for entry in &report.ledger {
            assert!(entry.has_required_fields(), "incomplete: {entry:?}");
            assert!(entry.reproduction_command.contains("deep-assurance"));
            assert!(
                entry
                    .reproduction_command
                    .contains(entry.scenario_id.as_str())
            );
        }
    }

    #[test]
    fn report_is_deterministic_and_replays_byte_identically() {
        let first = run_deep_assurance_gauntlet("determinism");
        let second = run_deep_assurance_gauntlet("determinism");
        assert_eq!(first, second);
        assert_eq!(first.render_ledger_jsonl(), second.render_ledger_jsonl());
    }

    #[test]
    fn ledger_jsonl_has_one_line_per_entry() {
        let report = report();
        assert_eq!(
            report.render_ledger_jsonl().lines().count(),
            report.ledger.len()
        );
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
            run_deep_assurance_pipeline(dir.path(), &DeepAssurancePipelineConfig::default())
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
        #![proptest_config(ProptestConfig::with_cases(12))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_deep_assurance_gauntlet(&label);
            let second = run_deep_assurance_gauntlet(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_deep_assurance_gauntlet(&label);
            prop_assert!(report.gate_passes);
            prop_assert_eq!(report.summary.families_covered, 6);
        }
    }
}

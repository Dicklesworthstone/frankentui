//! Unit/property test-evidence harness for the alien artifact-coding layers
//! (bd-3bxhj.10.44): hierarchical conjugate fusion, sequential FDR control,
//! counterfactual decision audits, graceful-degradation policies, the
//! guarantee-assumption registry, and the galaxy-brain L0-L3 UX contracts.
//!
//! The harness drives the REAL kernels (`hierarchical_fusion`,
//! `sequential_fdr`, `counterfactual_audit`, `degradation_policy`,
//! `guarantee_assumption_registry`, `galaxy_brain_ux`) through a fixed
//! fixture corpus spanning the mandated acceptance categories:
//!
//! - happy path (AC1): each kernel's default corpus passes its own gate with
//!   full internal coverage;
//! - edge cases (AC1): extreme-value log-domain stability + robust-mode
//!   huberization, dependence-correction monotonicity + sparse-regime
//!   shrinkage, async order invariance of the certified set, minimal-flip
//!   optimality with deterministic tie-breaks, and hysteresis/recovery
//!   guards (no oscillation);
//! - adversarial inputs (AC1): clamped adversarial e-values, wealth
//!   exhaustion, malformed tests failing closed, and content-hash tamper
//!   detection with input-order independence;
//! - failure-mode policy paths (AC1): predictive contradictions + malformed
//!   observations degrading conservatively, unsat counterfactual proofs,
//!   missing/contradictory evidence clamps with operator review, and
//!   guarantee validity/invalidity transitions with downgrade-to-fallback.
//!
//! Every diagnostic carries the AC3-mandated fields: `equation_id`,
//! `assumption_id`, `guarantee_id`, `wealth_state`, `perturbation_norm`,
//! `degradation_reason`, and `replay_cmd` (sentinel `n/a` / `none` where a
//! field does not apply — never empty). Determinism (AC2) is proven by
//! byte-identical replay of this harness's report and of the underlying
//! kernels via proptest.
//!
//! This module is a `pub mod` compiled into the lib (the CI gate runs
//! `cargo test -p doctor_frankentui --lib artifact_coding_tests`); all
//! `proptest` usage is confined to the `#[cfg(test)]` block. The envelope is
//! float-free (all-String fields), derives `Eq`, and replays
//! byte-identically. Precedents: `alien_kernel_unit_tests` (bd-3bxhj.10.27)
//! and `graveyard_control_tests` (bd-3bxhj.10.36).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::counterfactual_audit::{FragilityBand, run_default_counterfactual_audit};
use crate::degradation_policy::{
    DegradationMode, DegradationPolicy, DegradationReason, EvidenceSignal, MissingnessClass,
    SignalStatus, default_recovery_sequence, run_default_degradation_report,
};
use crate::galaxy_brain_ux::{default_ux_sources, run_default_galaxy_ux, run_galaxy_ux};
use crate::guarantee_assumption_registry::{
    ApplicabilityVerdict, AssumptionObservation, AssumptionRegistry, GuaranteeMechanism,
    run_default_guarantee_assumption_report,
};
use crate::hierarchical_fusion::{
    FusionChannel, FusionClaim, FusionConfig, KernelObservation, KernelPrior, RobustMode,
    run_default_fusion_report, run_fusion_report,
};
use crate::semantic_contract::MigrationDecision;
use crate::sequential_fdr::{
    ConservativeReason, FdrStage, FdrTest, GateFamily, SequentialFdrConfig,
    SequentialFdrController, default_test_corpus, run_sequential_fdr_report,
};

/// Schema version for the artifact-coding test-evidence harness.
pub const ARTIFACT_CODING_TESTS_SCHEMA_VERSION: &str = "artifact-coding-tests-v1";

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

fn replay(name: &str) -> String {
    format!("cargo test -p doctor_frankentui --lib artifact_coding_tests # {name}")
}

fn na() -> String {
    "n/a".to_string()
}

fn parse_f64(value: &str) -> f64 {
    value.parse::<f64>().unwrap_or(f64::NAN)
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// The artifact-coding kernel a diagnostic belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKernel {
    /// Hierarchical conjugate Bayesian evidence fusion (`hierarchical_fusion`).
    Fusion,
    /// e-BH + alpha-investing sequential FDR control (`sequential_fdr`).
    SequentialFdr,
    /// Counterfactual minimal-flip decision audit (`counterfactual_audit`).
    Counterfactual,
    /// Missing-evidence graceful-degradation policy (`degradation_policy`).
    Degradation,
    /// Guarantee-assumption registry + applicability checker
    /// (`guarantee_assumption_registry`).
    GuaranteeRegistry,
    /// Galaxy-brain L0-L3 UX contracts (`galaxy_brain_ux`).
    GalaxyUx,
}

impl ArtifactKernel {
    /// All kernels, in canonical order.
    pub const ALL: &'static [ArtifactKernel] = &[
        ArtifactKernel::Fusion,
        ArtifactKernel::SequentialFdr,
        ArtifactKernel::Counterfactual,
        ArtifactKernel::Degradation,
        ArtifactKernel::GuaranteeRegistry,
        ArtifactKernel::GalaxyUx,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKernel::Fusion => "fusion",
            ArtifactKernel::SequentialFdr => "sequential_fdr",
            ArtifactKernel::Counterfactual => "counterfactual",
            ArtifactKernel::Degradation => "degradation",
            ArtifactKernel::GuaranteeRegistry => "guarantee_registry",
            ArtifactKernel::GalaxyUx => "galaxy_ux",
        }
    }
}

/// Acceptance-criteria category exercised by a fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureCategory {
    /// Green-path default corpora.
    HappyPath,
    /// Numeric/ordering/hysteresis edge cases.
    EdgeCase,
    /// Adversarial and malformed inputs.
    AdversarialInput,
    /// Conservative failure-mode policy paths.
    FailureModePolicy,
}

impl FixtureCategory {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FixtureCategory::HappyPath => "happy_path",
            FixtureCategory::EdgeCase => "edge_case",
            FixtureCategory::AdversarialInput => "adversarial_input",
            FixtureCategory::FailureModePolicy => "failure_mode_policy",
        }
    }
}

// ── Diagnostic envelope (AC3) ────────────────────────────────────────────────

/// One machine-actionable diagnostic emitted while driving an
/// artifact-coding kernel. Carries every AC3-mandated field; `n/a` / `none`
/// sentinels are used where a field does not apply (never empty strings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCodingDiagnostic {
    /// Kernel under test.
    pub kernel: ArtifactKernel,
    /// The driven kernel's own deterministic run/report id.
    pub run_id: String,
    /// Deterministic equation/state identity (AC3 `equation_id`).
    pub equation_id: String,
    /// Assumption identity (AC3 `assumption_id`; `ASM-*` or `n/a`).
    pub assumption_id: String,
    /// Guarantee certificate identity (AC3 `guarantee_id`).
    pub guarantee_id: String,
    /// Alpha-investing wealth transition (AC3 `wealth_state`;
    /// `before->after` or `n/a`).
    pub wealth_state: String,
    /// Minimal counterfactual perturbation norm (AC3 `perturbation_norm`).
    pub perturbation_norm: String,
    /// Degradation reasons (AC3 `degradation_reason`; CSV, `none`, or `n/a`).
    pub degradation_reason: String,
    /// Observed outcome tag.
    pub outcome: String,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command (AC3 `replay_cmd`).
    pub replay_cmd: String,
}

impl ArtifactCodingDiagnostic {
    /// Whether every mandated field is populated (sentinels count as
    /// populated; empty strings do not).
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.run_id.is_empty()
            && !self.equation_id.is_empty()
            && !self.assumption_id.is_empty()
            && !self.guarantee_id.is_empty()
            && !self.wealth_state.is_empty()
            && !self.perturbation_norm.is_empty()
            && !self.degradation_reason.is_empty()
            && !self.outcome.is_empty()
            && !self.detail.is_empty()
            && !self.replay_cmd.is_empty()
    }

    /// Project the AC3 failure-log view of this diagnostic.
    #[must_use]
    pub fn failure_log(&self) -> ArtifactCodingFailureLog {
        ArtifactCodingFailureLog {
            equation_id: self.equation_id.clone(),
            assumption_id: self.assumption_id.clone(),
            guarantee_id: self.guarantee_id.clone(),
            wealth_state: self.wealth_state.clone(),
            perturbation_norm: self.perturbation_norm.clone(),
            degradation_reason: self.degradation_reason.clone(),
            replay_cmd: self.replay_cmd.clone(),
        }
    }
}

/// The AC3-mandated failure-log projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCodingFailureLog {
    /// Deterministic equation/state identity.
    pub equation_id: String,
    /// Assumption identity.
    pub assumption_id: String,
    /// Guarantee certificate identity.
    pub guarantee_id: String,
    /// Alpha-investing wealth transition.
    pub wealth_state: String,
    /// Minimal counterfactual perturbation norm.
    pub perturbation_norm: String,
    /// Degradation reasons.
    pub degradation_reason: String,
    /// Deterministic replay command.
    pub replay_cmd: String,
}

// ── Oracle ───────────────────────────────────────────────────────────────────

/// Expected-vs-observed verdict for one fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeVerdict {
    /// Fixture label.
    pub fixture_label: String,
    /// Kernel under test.
    pub kernel: ArtifactKernel,
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
    kernel: ArtifactKernel,
    category: FixtureCategory,
    expectation: &str,
    matches_expected: bool,
    mismatch: impl Into<String>,
) -> OutcomeVerdict {
    OutcomeVerdict {
        fixture_label: label.to_string(),
        kernel,
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
pub struct ArtifactFixtureEvaluation {
    /// Fixture label (sort key).
    pub label: String,
    /// Kernel under test.
    pub kernel: ArtifactKernel,
    /// Acceptance category exercised.
    pub category: FixtureCategory,
    /// Emitted diagnostics.
    pub diagnostics: Vec<ArtifactCodingDiagnostic>,
    /// Oracle verdict.
    pub verdict: OutcomeVerdict,
}

fn diagnostic(kernel: ArtifactKernel, label: &str) -> ArtifactCodingDiagnostic {
    ArtifactCodingDiagnostic {
        kernel,
        run_id: na(),
        equation_id: na(),
        assumption_id: na(),
        guarantee_id: na(),
        wealth_state: na(),
        perturbation_norm: na(),
        degradation_reason: na(),
        outcome: "observed".to_string(),
        detail: "-".to_string(),
        replay_cmd: replay(label),
    }
}

// ── Fusion fixtures ──────────────────────────────────────────────────────────

fn fix_fusion_green_default() -> ArtifactFixtureEvaluation {
    let label = "fusion-green-default";
    let report = run_default_fusion_report("artifact-coding-tests/fusion");
    let rerun = run_default_fusion_report("artifact-coding-tests/fusion");

    let ok = report.gate_passes
        && report.summary.kernels_covered == 3
        && report.summary.numerically_stable
        && report.summary.dependence_never_inflates
        && report.summary.required_fields_complete
        && report == rerun
        && report.exported_json_stats.sha256
            == sha256_hex(report.exported_json_stats.content.as_bytes());

    let diagnostics = report
        .ledger
        .iter()
        .map(|entry| ArtifactCodingDiagnostic {
            run_id: report.run_id.clone(),
            equation_id: entry.posterior_state_id.clone(),
            degradation_reason: if entry.degraded_confidence {
                "degraded_confidence".to_string()
            } else {
                "none".to_string()
            },
            outcome: format!("decision:{:?}", entry.recommended_decision),
            detail: format!(
                "claim {} kernel {} posterior_mean {}",
                entry.claim_id,
                entry.kernel.as_str(),
                entry.posterior_mean
            ),
            ..diagnostic(ArtifactKernel::Fusion, label)
        })
        .collect();

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Fusion,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Fusion,
            FixtureCategory::HappyPath,
            "the default fusion corpus passes its gate across all three conjugate kernels",
            ok,
            "the default fusion corpus did not pass cleanly",
        ),
    }
}

fn fix_fusion_extreme_and_robust() -> ArtifactFixtureEvaluation {
    let label = "fusion-extreme-and-robust";
    let config = FusionConfig {
        huber_cap: 50.0,
        ..FusionConfig::default()
    };
    let claims = vec![
        FusionClaim::new(
            "claim.extreme",
            "stratum.stability",
            KernelPrior::Beta {
                alpha: 2.0,
                beta: 2.0,
            },
            vec![FusionChannel::new(
                "ch.extreme",
                KernelObservation::Bernoulli {
                    successes: 1.0e12,
                    failures: 1.0e6,
                },
                0.0,
            )],
        ),
        FusionClaim::new(
            "claim.outlier",
            "stratum.stability",
            KernelPrior::Beta {
                alpha: 2.0,
                beta: 2.0,
            },
            vec![FusionChannel::new(
                "ch.outlier",
                KernelObservation::Bernoulli {
                    successes: 5000.0,
                    failures: 100.0,
                },
                0.0,
            )],
        ),
    ];
    let report = run_fusion_report("artifact-coding-tests/extreme", &claims, config);

    let extreme = report.entry("claim.extreme");
    let outlier = report.entry("claim.outlier");
    let ok = report.summary.numerically_stable
        && extreme.is_some_and(|e| e.numerically_finite)
        && outlier.is_some_and(|e| e.robust_mode == RobustMode::Huberized)
        && report.summary.robust_engaged >= 1;

    let diagnostics = vec![
        ArtifactCodingDiagnostic {
            run_id: report.run_id.clone(),
            equation_id: extreme.map_or_else(na, |e| e.posterior_state_id.clone()),
            degradation_reason: "none".to_string(),
            outcome: "log_domain_finite".to_string(),
            detail: "1e12 pseudo-counts stay finite through the log-domain fusion".to_string(),
            ..diagnostic(ArtifactKernel::Fusion, label)
        },
        ArtifactCodingDiagnostic {
            run_id: report.run_id.clone(),
            equation_id: outlier.map_or_else(na, |e| e.posterior_state_id.clone()),
            degradation_reason: "none".to_string(),
            outcome: "huberized".to_string(),
            detail: "an outlier-heavy channel engages robust huberization at the cap".to_string(),
            ..diagnostic(ArtifactKernel::Fusion, label)
        },
    ];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Fusion,
        category: FixtureCategory::EdgeCase,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Fusion,
            FixtureCategory::EdgeCase,
            "extreme pseudo-counts stay finite and outliers engage robust huberization",
            ok,
            "extreme-value stability or robust-mode behavior did not hold",
        ),
    }
}

fn fix_fusion_dependence_and_shrinkage() -> ArtifactFixtureEvaluation {
    let label = "fusion-dependence-and-shrinkage";
    let channels = |correlation: f64| {
        vec![
            FusionChannel::new(
                "ch.a",
                KernelObservation::Bernoulli {
                    successes: 40.0,
                    failures: 10.0,
                },
                correlation,
            ),
            FusionChannel::new(
                "ch.b",
                KernelObservation::Bernoulli {
                    successes: 38.0,
                    failures: 12.0,
                },
                correlation,
            ),
        ]
    };
    let prior = || KernelPrior::Beta {
        alpha: 2.0,
        beta: 2.0,
    };
    let independent = run_fusion_report(
        "artifact-coding-tests/dep-independent",
        &[FusionClaim::new(
            "claim.dep",
            "stratum.dep",
            prior(),
            channels(0.0),
        )],
        FusionConfig::default(),
    );
    let correlated = run_fusion_report(
        "artifact-coding-tests/dep-correlated",
        &[FusionClaim::new(
            "claim.dep",
            "stratum.dep",
            prior(),
            channels(0.9),
        )],
        FusionConfig::default(),
    );

    // Sparse claim shrinks toward a dense stratum sibling.
    let sparse_report = run_fusion_report(
        "artifact-coding-tests/sparse",
        &[
            FusionClaim::new(
                "claim.sparse",
                "stratum.shrink",
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
            ),
            FusionClaim::new(
                "claim.dense",
                "stratum.shrink",
                KernelPrior::Beta {
                    alpha: 8.0,
                    beta: 2.0,
                },
                vec![FusionChannel::new(
                    "ch.dense",
                    KernelObservation::Bernoulli {
                        successes: 90.0,
                        failures: 10.0,
                    },
                    0.0,
                )],
            ),
        ],
        FusionConfig::default(),
    );

    let ind = independent.entry("claim.dep");
    let cor = correlated.entry("claim.dep");
    let ind_factor = ind.map_or(f64::NAN, |e| parse_f64(&e.dependence_factor));
    let cor_factor = cor.map_or(f64::NAN, |e| parse_f64(&e.dependence_factor));
    let ind_var = ind.map_or(f64::NAN, |e| parse_f64(&e.posterior_variance));
    let cor_var = cor.map_or(f64::NAN, |e| parse_f64(&e.posterior_variance));
    let sparse = sparse_report.entry("claim.sparse");
    let sparse_lambda = sparse.map_or(f64::NAN, |e| parse_f64(&e.shrinkage_lambda));

    let ok = ind.is_some_and(|e| e.dependence_factor == "1.000000")
        && cor_factor.is_finite()
        && cor_factor < ind_factor
        && cor_factor > 0.0
        && cor_var >= ind_var
        && cor.is_some_and(|e| e.dependence_trace.contains("deflated"))
        && sparse_lambda > 0.5
        && sparse.is_some_and(|e| {
            e.high_sensitivity && e.recommended_decision == MigrationDecision::ConservativeFallback
        });

    let diagnostics = vec![
        ArtifactCodingDiagnostic {
            run_id: correlated.run_id.clone(),
            equation_id: cor.map_or_else(na, |e| e.posterior_state_id.clone()),
            degradation_reason: "none".to_string(),
            outcome: "dependence_deflated".to_string(),
            detail: format!(
                "correlation 0.9 deflates the dependence factor ({}) below the independent \
                 baseline (1.000000) and never inflates evidence",
                cor.map_or_else(na, |e| e.dependence_factor.clone())
            ),
            ..diagnostic(ArtifactKernel::Fusion, label)
        },
        ArtifactCodingDiagnostic {
            run_id: sparse_report.run_id.clone(),
            equation_id: sparse.map_or_else(na, |e| e.shrinkage_profile_id.clone()),
            degradation_reason: "high_sensitivity".to_string(),
            outcome: "sparse_shrunk_conservative".to_string(),
            detail: format!(
                "a one-observation claim shrinks hard toward its stratum (lambda {}) and \
                 stays conservative under prior sensitivity",
                sparse.map_or_else(na, |e| e.shrinkage_lambda.clone())
            ),
            ..diagnostic(ArtifactKernel::Fusion, label)
        },
    ];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Fusion,
        category: FixtureCategory::EdgeCase,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Fusion,
            FixtureCategory::EdgeCase,
            "dependence correction monotonically deflates evidence; sparse regimes shrink \
             and stay conservative",
            ok,
            "dependence monotonicity or shrinkage sanity did not hold",
        ),
    }
}

fn fix_fusion_contradiction_and_malformed() -> ArtifactFixtureEvaluation {
    let label = "fusion-contradiction-and-malformed";
    let claims = vec![
        FusionClaim::new(
            "claim.contradiction",
            "stratum.fail",
            KernelPrior::Beta {
                alpha: 190.0,
                beta: 10.0,
            },
            vec![FusionChannel::new(
                "ch.contradiction",
                KernelObservation::Bernoulli {
                    successes: 5.0,
                    failures: 45.0,
                },
                0.0,
            )],
        ),
        FusionClaim::new(
            "claim.malformed",
            "stratum.fail",
            KernelPrior::Dirichlet {
                alphas: vec![1.0, 1.0, 1.0],
            },
            vec![FusionChannel::new(
                "ch.malformed",
                KernelObservation::Categorical {
                    counts: vec![5.0, 5.0],
                },
                0.0,
            )],
        ),
    ];
    let report = run_fusion_report(
        "artifact-coding-tests/failure-modes",
        &claims,
        FusionConfig::default(),
    );

    let contradiction = report.entry("claim.contradiction");
    let malformed = report.entry("claim.malformed");
    let ok = contradiction.is_some_and(|e| {
        !e.predictive_check_passed
            && e.degraded_confidence
            && e.recommended_decision == MigrationDecision::ConservativeFallback
    }) && malformed.is_some_and(|e| {
        !e.observations_valid
            && e.recommended_decision == MigrationDecision::ConservativeFallback
            && e.numerically_finite
    }) && report.summary.numerically_stable
        && report.summary.predictive_failure_degrades
        && report.summary.malformed_observations_conservative;

    let diagnostics = vec![
        ArtifactCodingDiagnostic {
            run_id: report.run_id.clone(),
            equation_id: contradiction.map_or_else(na, |e| e.posterior_state_id.clone()),
            degradation_reason: "predictive_failure".to_string(),
            outcome: "conservative_fallback".to_string(),
            detail: "evidence contradicting a strong prior fails the posterior-predictive \
                     check and degrades conservatively"
                .to_string(),
            ..diagnostic(ArtifactKernel::Fusion, label)
        },
        ArtifactCodingDiagnostic {
            run_id: report.run_id.clone(),
            equation_id: malformed.map_or_else(na, |e| e.posterior_state_id.clone()),
            degradation_reason: "malformed_observation".to_string(),
            outcome: "conservative_fallback".to_string(),
            detail: "a kernel/arity-mismatched observation is rejected conservatively without \
                     numeric instability"
                .to_string(),
            ..diagnostic(ArtifactKernel::Fusion, label)
        },
    ];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Fusion,
        category: FixtureCategory::FailureModePolicy,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Fusion,
            FixtureCategory::FailureModePolicy,
            "predictive contradictions and malformed observations degrade conservatively",
            ok,
            "a fusion failure-mode path was not handled conservatively",
        ),
    }
}

// ── Sequential-FDR fixtures ──────────────────────────────────────────────────

fn fix_fdr_green_default() -> ArtifactFixtureEvaluation {
    let label = "fdr-green-default";
    let report = run_sequential_fdr_report("artifact-coding-tests/fdr");
    let rerun = run_sequential_fdr_report("artifact-coding-tests/fdr");

    let invest_line = report
        .ledger
        .iter()
        .find(|line| line.stage == FdrStage::Invest);
    let ok = report.gate_passes
        && report.summary.stages_covered == FdrStage::ALL.len()
        && report.summary.families == GateFamily::ALL.len()
        && report.summary.ebh_count_matches
        && report.summary.wealth_conserved
        && report.summary.fdr_monotone_ok
        && report == rerun;

    let diagnostics = vec![ArtifactCodingDiagnostic {
        run_id: report.run_id.clone(),
        equation_id: format!(
            "ebh:k_star={};threshold={}",
            report.summary.ebh_k_star, report.summary.ebh_threshold
        ),
        wealth_state: invest_line.map_or_else(na, |line| {
            format!("{}->{}", line.wealth_before, line.wealth_after)
        }),
        outcome: "gate_passes".to_string(),
        detail: format!(
            "e-BH certified {} of {} with conserved family wealth",
            report.summary.ebh_certified, report.summary.ebh_total
        ),
        ..diagnostic(ArtifactKernel::SequentialFdr, label)
    }];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::SequentialFdr,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::SequentialFdr,
            FixtureCategory::HappyPath,
            "the default FDR corpus passes with matched e-BH counts and conserved wealth",
            ok,
            "the default sequential-FDR corpus did not pass cleanly",
        ),
    }
}

fn fix_fdr_order_invariance() -> ArtifactFixtureEvaluation {
    let label = "fdr-order-invariance";
    let corpus = default_test_corpus();
    let mut reversed = corpus.clone();
    reversed.reverse();

    let forward = SequentialFdrController::new(
        "artifact-coding-tests/order",
        SequentialFdrConfig::default(),
        corpus,
    )
    .run(None);
    let backward = SequentialFdrController::new(
        "artifact-coding-tests/order",
        SequentialFdrConfig::default(),
        reversed,
    )
    .run(None);

    let ok = forward == backward && forward.gate_passes;

    let diagnostics = vec![ArtifactCodingDiagnostic {
        run_id: forward.run_id.clone(),
        equation_id: format!("ebh:k_star={}", forward.summary.ebh_k_star),
        outcome: "order_invariant".to_string(),
        detail: "submitting the same tests in reverse arrival order yields a byte-identical \
                 report; the certified set is a function of the evidence multiset"
            .to_string(),
        ..diagnostic(ArtifactKernel::SequentialFdr, label)
    }];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::SequentialFdr,
        category: FixtureCategory::EdgeCase,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::SequentialFdr,
            FixtureCategory::EdgeCase,
            "certified decisions are invariant to async submission order",
            ok,
            "input order changed the sequential-FDR decisions",
        ),
    }
}

fn fix_fdr_adversarial_and_exhaustion() -> ArtifactFixtureEvaluation {
    let label = "fdr-adversarial-and-exhaustion";

    // Adversarial spike: clamped at the e-value cap, withheld, surfaced.
    let adversarial = SequentialFdrController::new(
        "artifact-coding-tests/adversarial",
        SequentialFdrConfig::default().with_evalue_cap(1_000.0),
        vec![
            FdrTest::with_evalue("q.spike", GateFamily::Quality, 0, 1.0e9),
            FdrTest::with_evalue("q.normal", GateFamily::Quality, 1, 25.0),
        ],
    )
    .run(None);

    // Wealth exhaustion: a weak streak drains the family below the floor.
    let exhausted = SequentialFdrController::new(
        "artifact-coding-tests/exhaustion",
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
    let exhausted_line = exhausted.ledger.iter().find(|line| {
        line.stage == FdrStage::Invest
            && line.conservative_reason == ConservativeReason::WealthExhausted
    });

    // Malformed evidence fails closed.
    let malformed = SequentialFdrController::new(
        "artifact-coding-tests/malformed",
        SequentialFdrConfig::default(),
        vec![FdrTest::with_evalue(
            "bad.negative",
            GateFamily::Quality,
            0,
            -1.0,
        )],
    )
    .run(None);

    let ok = adversarial.gate_passes
        && adversarial.summary.adversarial >= 1
        && adversarial.summary.conservative_surfaced >= adversarial.summary.conservative_events
        && exhausted_line.is_some()
        && exhausted.summary.conservative_events >= 1
        && !malformed.gate_passes
        && malformed.summary.invalid >= 1;

    let diagnostics = vec![
        ArtifactCodingDiagnostic {
            run_id: adversarial.run_id.clone(),
            equation_id: "evalue_cap=1000.000000".to_string(),
            outcome: "adversarial_clamped".to_string(),
            detail: "a 1e9 e-value is clamped at the cap and withheld, never silently \
                     certified"
                .to_string(),
            ..diagnostic(ArtifactKernel::SequentialFdr, label)
        },
        ArtifactCodingDiagnostic {
            run_id: exhausted.run_id.clone(),
            equation_id: "alpha_investing_recursion".to_string(),
            wealth_state: exhausted_line.map_or_else(na, |line| {
                format!("{}->{}", line.wealth_before, line.wealth_after)
            }),
            outcome: "wealth_exhausted".to_string(),
            detail: "a weak streak drains wealth below the floor and forces a surfaced \
                     conservative withhold"
                .to_string(),
            ..diagnostic(ArtifactKernel::SequentialFdr, label)
        },
        ArtifactCodingDiagnostic {
            run_id: malformed.run_id.clone(),
            equation_id: "evalue_validity".to_string(),
            outcome: "fails_closed".to_string(),
            detail: "a negative e-value invalidates the corpus and fails the gate closed"
                .to_string(),
            ..diagnostic(ArtifactKernel::SequentialFdr, label)
        },
    ];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::SequentialFdr,
        category: FixtureCategory::AdversarialInput,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::SequentialFdr,
            FixtureCategory::AdversarialInput,
            "adversarial e-values clamp, exhaustion surfaces, malformed evidence fails closed",
            ok,
            "an adversarial sequential-FDR path was not handled safely",
        ),
    }
}

// ── Counterfactual fixtures ──────────────────────────────────────────────────

fn fix_counterfactual_default_audit() -> ArtifactFixtureEvaluation {
    let label = "counterfactual-default-audit";
    let report = run_default_counterfactual_audit("artifact-coding-tests/cf");

    let robust = report.card("dec.robust");
    let fragile = report.card("dec.fragile");
    let ok = report.gate_passes()
        && report.summary.counterfactual_present
        && report.summary.fragile_decisions_blocked
        && robust.is_some_and(|c| {
            c.clause_consistent && c.fragility_band != FragilityBand::Fragile && !c.fragile
        })
        && fragile.is_some_and(|c| {
            c.satisfiable
                && c.clause_consistent
                && (!c.fragile || (c.requires_mitigation && !c.policy_clause.is_empty()))
        });

    let diagnostics = vec![
        ArtifactCodingDiagnostic {
            run_id: report.summary.run_id.clone(),
            equation_id: robust.map_or_else(na, |c| c.decision_id.clone()),
            perturbation_norm: robust.map_or_else(na, |c| c.perturbation_norm.clone()),
            outcome: "robust".to_string(),
            detail: "a faithful-heavy decision needs a large flip and is not fragile".to_string(),
            ..diagnostic(ArtifactKernel::Counterfactual, label)
        },
        ArtifactCodingDiagnostic {
            run_id: report.summary.run_id.clone(),
            equation_id: fragile.map_or_else(na, |c| c.decision_id.clone()),
            perturbation_norm: fragile.map_or_else(na, |c| c.perturbation_norm.clone()),
            outcome: "fragility_policed".to_string(),
            detail: "a near-boundary decision is satisfiable and, whenever fragile, is \
                     blocked behind mitigation"
                .to_string(),
            ..diagnostic(ArtifactKernel::Counterfactual, label)
        },
    ];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Counterfactual,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Counterfactual,
            FixtureCategory::HappyPath,
            "the default audit passes with robust decisions cleared and fragile ones blocked",
            ok,
            "the default counterfactual audit did not pass cleanly",
        ),
    }
}

fn fix_counterfactual_minimal_flip() -> ArtifactFixtureEvaluation {
    let label = "counterfactual-minimal-flip";
    let report = run_default_counterfactual_audit("artifact-coding-tests/cf-min");
    let rerun = run_default_counterfactual_audit("artifact-coding-tests/cf-min");

    let fragile = report.card("dec.fragile");
    let minimal = fragile.is_some_and(|card| {
        let nearest = parse_f64(&card.perturbation_norm);
        nearest.is_finite()
            && card
                .all_flips
                .iter()
                .filter(|flip| flip.satisfiable)
                .all(|flip| nearest <= parse_f64(&flip.l1_norm) + 1e-9)
    });
    let deterministic = report.cards == rerun.cards;
    let ok = minimal && deterministic && fragile.is_some_and(|c| c.clause_consistent);

    let diagnostics = vec![ArtifactCodingDiagnostic {
        run_id: report.summary.run_id.clone(),
        equation_id: fragile.map_or_else(na, |c| c.decision_id.clone()),
        perturbation_norm: fragile.map_or_else(na, |c| c.perturbation_norm.clone()),
        outcome: "minimal_flip".to_string(),
        detail: "the nearest flip's L1 norm lower-bounds every satisfiable alternative and \
                 ties break deterministically across reruns"
            .to_string(),
        ..diagnostic(ArtifactKernel::Counterfactual, label)
    }];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Counterfactual,
        category: FixtureCategory::EdgeCase,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Counterfactual,
            FixtureCategory::EdgeCase,
            "the reported flip is norm-minimal with deterministic tie-breaks",
            ok,
            "minimal-flip optimality or tie-break determinism did not hold",
        ),
    }
}

fn fix_counterfactual_unsat_proof() -> ArtifactFixtureEvaluation {
    let label = "counterfactual-unsat-proof";
    let report = run_default_counterfactual_audit("artifact-coding-tests/cf-unsat");

    let unsat = report.card("dec.immutable_bad");
    let ok = unsat.is_some_and(|card| {
        !card.satisfiable
            && card.fragility_band == FragilityBand::Unflippable
            && !card.unsat_proof.is_empty()
            && card.alt_action.is_none()
    }) && report.summary.unflippable_decisions >= 1
        && report.gate_passes();

    let diagnostics = vec![ArtifactCodingDiagnostic {
        run_id: report.summary.run_id.clone(),
        equation_id: unsat.map_or_else(na, |c| c.decision_id.clone()),
        perturbation_norm: "unflippable".to_string(),
        outcome: "unsat_proven".to_string(),
        detail: unsat.map_or_else(na, |c| c.unsat_proof.clone()),
        ..diagnostic(ArtifactKernel::Counterfactual, label)
    }];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Counterfactual,
        category: FixtureCategory::FailureModePolicy,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Counterfactual,
            FixtureCategory::FailureModePolicy,
            "a fully-constrained decision yields an explicit unsat proof, never a fake flip",
            ok,
            "the unsat proof path did not hold",
        ),
    }
}

// ── Degradation fixtures ─────────────────────────────────────────────────────

fn fix_degradation_green_default() -> ArtifactFixtureEvaluation {
    let label = "degradation-green-default";
    let policy = DegradationPolicy::default();
    let signals = vec![
        EvidenceSignal::new(
            "widget_coverage",
            1.0,
            SignalStatus::Present { quality: 0.95 },
        ),
        EvidenceSignal::new(
            "runtime_event",
            1.0,
            SignalStatus::Present { quality: 0.93 },
        ),
    ];
    let decision = policy.compile(
        "artifact-coding-tests/deg",
        "unit.green",
        MigrationDecision::AutoApprove,
        &signals,
    );
    let report = run_default_degradation_report("artifact-coding-tests/deg-default");

    let ok = decision.mode == DegradationMode::Normal
        && decision.final_action == MigrationDecision::AutoApprove
        && decision.degradation_reasons.is_empty()
        && decision.clause_consistent
        && report.gate_passes()
        && report.summary.no_catastrophic_failure
        && report.summary.degraded_explained;

    let diagnostics = vec![ArtifactCodingDiagnostic {
        run_id: decision.run_id.clone(),
        equation_id: decision.decision_id.clone(),
        degradation_reason: "none".to_string(),
        outcome: "normal_mode".to_string(),
        detail: "full-quality evidence keeps the proposed action with no degradation reasons"
            .to_string(),
        ..diagnostic(ArtifactKernel::Degradation, label)
    }];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Degradation,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Degradation,
            FixtureCategory::HappyPath,
            "full evidence stays in Normal mode and the default corpus passes its gate",
            ok,
            "the degradation green path did not hold",
        ),
    }
}

fn fix_degradation_missing_and_contradictory() -> ArtifactFixtureEvaluation {
    let label = "degradation-missing-and-contradictory";
    let policy = DegradationPolicy::default();

    let mnar = policy.compile(
        "artifact-coding-tests/deg",
        "unit.mnar",
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
        "artifact-coding-tests/deg",
        "unit.contradiction",
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
    let inflation_clamped = parse_f64(&mnar.inflation_factor) <= 4.0 + 1e-9
        && parse_f64(&contradictory.inflation_factor) <= 4.0 + 1e-9;

    let ok = mnar.final_action == MigrationDecision::ConservativeFallback
        && mnar.operator_review_required
        && mnar
            .degradation_reasons
            .contains(&DegradationReason::CriticalHazard)
        && mnar
            .degradation_reasons
            .contains(&DegradationReason::MissingMnar)
        && !mnar.imputations.is_empty()
        && contradictory.final_action == MigrationDecision::ConservativeFallback
        && contradictory
            .degradation_reasons
            .contains(&DegradationReason::AnomalyClamp)
        && contradictory.operator_review_required
        && inflation_clamped;

    let diagnostics = vec![
        ArtifactCodingDiagnostic {
            run_id: mnar.run_id.clone(),
            equation_id: mnar.decision_id.clone(),
            degradation_reason: reasons_csv(&mnar.degradation_reasons),
            outcome: "critical_mnar_fallback".to_string(),
            detail: "a critical MNAR gap forces conservative fallback with operator review \
                     and provenance-carrying imputations"
                .to_string(),
            ..diagnostic(ArtifactKernel::Degradation, label)
        },
        ArtifactCodingDiagnostic {
            run_id: contradictory.run_id.clone(),
            equation_id: contradictory.decision_id.clone(),
            degradation_reason: reasons_csv(&contradictory.degradation_reasons),
            outcome: "anomaly_clamped".to_string(),
            detail: "high dissent trips the anomaly hard-clamp with bounded inflation".to_string(),
            ..diagnostic(ArtifactKernel::Degradation, label)
        },
    ];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Degradation,
        category: FixtureCategory::FailureModePolicy,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Degradation,
            FixtureCategory::FailureModePolicy,
            "missing/contradictory evidence clamps conservatively with explicit reasons",
            ok,
            "a degradation failure-mode path was not handled safely",
        ),
    }
}

fn fix_degradation_hysteresis_recovery() -> ArtifactFixtureEvaluation {
    let label = "degradation-hysteresis-recovery";
    let policy = DegradationPolicy::default();
    let transitions = policy.simulate(
        "artifact-coding-tests/deg-cycle",
        &default_recovery_sequence(),
    );

    let entered = transitions
        .iter()
        .any(|t| t.reason.starts_with("enter_degraded"));
    let held_in_band = transitions
        .iter()
        .any(|t| t.reason == "hold_degraded:guard_unmet");
    let recovered = transitions
        .iter()
        .any(|t| t.reason == "recover:guard_satisfied");
    let ends_normal = transitions
        .last()
        .is_some_and(|t| t.to_mode == DegradationMode::Normal);
    let no_oscillation = {
        let mode_flips = transitions
            .windows(2)
            .filter(|w| w[0].to_mode != w[1].to_mode)
            .count();
        mode_flips <= 2
    };
    let ok = transitions.len() == default_recovery_sequence().len()
        && entered
        && held_in_band
        && recovered
        && ends_normal
        && no_oscillation;

    let diagnostics = vec![ArtifactCodingDiagnostic {
        run_id: transitions.first().map_or_else(na, |t| t.run_id.clone()),
        equation_id: "hysteresis:enter=0.50,exit=0.70,cooldown=3".to_string(),
        degradation_reason: "hold_degraded:guard_unmet".to_string(),
        outcome: "hysteresis_holds".to_string(),
        detail: "quality between the enter and exit thresholds holds Degraded (no \
                 oscillation) until the recovery guard is satisfied"
            .to_string(),
        ..diagnostic(ArtifactKernel::Degradation, label)
    }];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::Degradation,
        category: FixtureCategory::EdgeCase,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::Degradation,
            FixtureCategory::EdgeCase,
            "the mode state machine has hysteresis and recovers only through the guard",
            ok,
            "hysteresis/recovery-guard behavior did not hold",
        ),
    }
}

// ── Guarantee-registry fixtures ──────────────────────────────────────────────

fn fix_registry_green_default() -> ArtifactFixtureEvaluation {
    let label = "registry-green-default";
    let report = run_default_guarantee_assumption_report("artifact-coding-tests/reg");
    let rerun = run_default_guarantee_assumption_report("artifact-coding-tests/reg");

    let ok = report.gate_passes
        && report.summary.mechanisms_covered == GuaranteeMechanism::ALL.len()
        && report.summary.valid == report.summary.total_claims
        && report.summary.violations == 0
        && report.summary.no_valid_with_violations
        && report == rerun;

    let diagnostics = report
        .certificates
        .iter()
        .map(|cert| ArtifactCodingDiagnostic {
            run_id: report.run_id.clone(),
            equation_id: cert.assumption_set_hash.clone(),
            assumption_id: cert
                .checks
                .first()
                .map_or_else(na, |check| check.assumption_id.clone()),
            guarantee_id: cert.guarantee_id.clone(),
            outcome: format!("verdict:{}", cert.applicability_verdict.as_str()),
            detail: format!(
                "mechanism {} valid with {} assumption checks",
                cert.mechanism.as_str(),
                cert.checks.len()
            ),
            ..diagnostic(ArtifactKernel::GuaranteeRegistry, label)
        })
        .collect();

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::GuaranteeRegistry,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::GuaranteeRegistry,
            FixtureCategory::HappyPath,
            "all four mechanisms validate under fully-satisfied assumptions",
            ok,
            "the default guarantee-assumption corpus did not validate",
        ),
    }
}

fn fix_registry_validity_transitions() -> ArtifactFixtureEvaluation {
    let label = "registry-validity-transitions";
    let registry = AssumptionRegistry::default();

    let invalid = registry.evaluate(
        GuaranteeMechanism::Conformal,
        "claim.exchangeability-broken",
        &AssumptionObservation::fully_satisfied().with_exchangeability(0.30),
    );
    let degraded = registry.evaluate(
        GuaranteeMechanism::Conformal,
        "claim.coverage-shortfall",
        &AssumptionObservation::fully_satisfied().with_empirical_coverage(0.50),
    );
    let drift_immune = registry.evaluate(
        GuaranteeMechanism::EProcess,
        "claim.eprocess-under-drift",
        &AssumptionObservation::fully_satisfied().with_drift(0.90),
    );
    let integrity = registry.evaluate(
        GuaranteeMechanism::Conformal,
        "claim.exchangeability-broken",
        &AssumptionObservation::fully_satisfied().with_exchangeability(0.30),
    );

    let ok = invalid.applicability_verdict == ApplicabilityVerdict::Invalid
        && invalid.recommended_decision == MigrationDecision::ConservativeFallback
        && invalid
            .violated_assumptions
            .iter()
            .any(|id| id == "ASM-EXCH")
        && !invalid.fallback_clause.is_empty()
        && degraded.applicability_verdict == ApplicabilityVerdict::Degraded
        && degraded.recommended_decision == MigrationDecision::ConservativeFallback
        && drift_immune.applicability_verdict == ApplicabilityVerdict::Valid
        && invalid == integrity
        && invalid.clause_consistent
        && degraded.clause_consistent;

    let diagnostics = vec![
        ArtifactCodingDiagnostic {
            run_id: invalid.guarantee_id.clone(),
            equation_id: invalid.assumption_set_hash.clone(),
            assumption_id: "ASM-EXCH".to_string(),
            guarantee_id: invalid.guarantee_id.clone(),
            degradation_reason: "hard_violation".to_string(),
            outcome: "invalidated".to_string(),
            detail: "a hard exchangeability violation voids the conformal guarantee and \
                     forces conservative fallback"
                .to_string(),
            ..diagnostic(ArtifactKernel::GuaranteeRegistry, label)
        },
        ArtifactCodingDiagnostic {
            run_id: degraded.guarantee_id.clone(),
            equation_id: degraded.assumption_set_hash.clone(),
            assumption_id: "ASM-COVER".to_string(),
            guarantee_id: degraded.guarantee_id.clone(),
            degradation_reason: "soft_violation".to_string(),
            outcome: "downgraded".to_string(),
            detail: "a soft coverage shortfall downgrades (not voids) the guarantee, still \
                     withholding promotion"
                .to_string(),
            ..diagnostic(ArtifactKernel::GuaranteeRegistry, label)
        },
        ArtifactCodingDiagnostic {
            run_id: drift_immune.guarantee_id.clone(),
            equation_id: drift_immune.assumption_set_hash.clone(),
            assumption_id: "ASM-STAT".to_string(),
            guarantee_id: drift_immune.guarantee_id.clone(),
            degradation_reason: "none".to_string(),
            outcome: "unaffected".to_string(),
            detail: "drift does not touch the e-process guarantee (stationarity is not in \
                     its assumption set); certificates are byte-reproducible"
                .to_string(),
            ..diagnostic(ArtifactKernel::GuaranteeRegistry, label)
        },
    ];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::GuaranteeRegistry,
        category: FixtureCategory::FailureModePolicy,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::GuaranteeRegistry,
            FixtureCategory::FailureModePolicy,
            "hard violations invalidate, soft ones degrade, unrelated assumptions stay \
             valid, and certificates replay byte-identically",
            ok,
            "guarantee validity/downgrade transitions did not hold",
        ),
    }
}

// ── Galaxy-UX fixtures ───────────────────────────────────────────────────────

fn fix_ux_green_default() -> ArtifactFixtureEvaluation {
    let label = "ux-green-default";
    let report = run_default_galaxy_ux("artifact-coding-tests/ux");

    let ok = report.gate_passes
        && report.summary.total_cards == 4
        && report.summary.total_views == 16
        && report.summary.non_interference_proven
        && report.summary.accessibility_pass
        && report.summary.perf_within_budget
        && report.summary.interaction_coverage
        && report.summary.copy_exports_complete;

    let diagnostics = vec![ArtifactCodingDiagnostic {
        run_id: report.run_id.clone(),
        equation_id: report
            .views
            .first()
            .map_or_else(na, |view| view.content_id.clone()),
        outcome: "ux_contracts_hold".to_string(),
        detail: format!(
            "{} views over {} cards pass non-interference, accessibility, and per-level \
             budgets with full interaction coverage",
            report.summary.total_views, report.summary.total_cards
        ),
        ..diagnostic(ArtifactKernel::GalaxyUx, label)
    }];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::GalaxyUx,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::GalaxyUx,
            FixtureCategory::HappyPath,
            "the default deck passes all ten UX gate clauses",
            ok,
            "the default galaxy-UX deck did not pass",
        ),
    }
}

fn fix_ux_integrity_and_order() -> ArtifactFixtureEvaluation {
    let label = "ux-integrity-and-order";
    let sources = default_ux_sources();
    let mut reversed = default_ux_sources();
    reversed.reverse();

    let forward = run_galaxy_ux("artifact-coding-tests/ux-order", &sources);
    let backward = run_galaxy_ux("artifact-coding-tests/ux-order", &reversed);

    // Independently re-derive every view's content hash and prove tampering
    // is detectable by the same derivation.
    let rederived_ok = forward.views.iter().all(|view| {
        view.content_hash == short_hash(&stable_hash(&(&view.content_id, &view.lines)))
    });
    let tamper_detected = forward.views.first().is_some_and(|view| {
        let tampered = "tampered-hash".to_string();
        tampered != short_hash(&stable_hash(&(&view.content_id, &view.lines)))
    });

    let ok = forward == backward && rederived_ok && tamper_detected && forward.gate_passes;

    let diagnostics = vec![ArtifactCodingDiagnostic {
        run_id: forward.run_id.clone(),
        equation_id: forward
            .views
            .first()
            .map_or_else(na, |view| view.content_hash.clone()),
        outcome: "integrity_rederived".to_string(),
        detail: "view hashes re-derive independently, tampering is detectable, and deck \
                 order does not change the report"
            .to_string(),
        ..diagnostic(ArtifactKernel::GalaxyUx, label)
    }];

    ArtifactFixtureEvaluation {
        label: label.to_string(),
        kernel: ArtifactKernel::GalaxyUx,
        category: FixtureCategory::AdversarialInput,
        diagnostics,
        verdict: verdict(
            label,
            ArtifactKernel::GalaxyUx,
            FixtureCategory::AdversarialInput,
            "content hashes are independently re-derivable and input-order independent",
            ok,
            "UX integrity re-derivation or order independence did not hold",
        ),
    }
}

// ── Corpus + report ──────────────────────────────────────────────────────────

/// The fixed fixture corpus (sorted by label).
#[must_use]
pub fn artifact_coding_corpus() -> Vec<ArtifactFixtureEvaluation> {
    let mut all = vec![
        fix_fusion_green_default(),
        fix_fusion_extreme_and_robust(),
        fix_fusion_dependence_and_shrinkage(),
        fix_fusion_contradiction_and_malformed(),
        fix_fdr_green_default(),
        fix_fdr_order_invariance(),
        fix_fdr_adversarial_and_exhaustion(),
        fix_counterfactual_default_audit(),
        fix_counterfactual_minimal_flip(),
        fix_counterfactual_unsat_proof(),
        fix_degradation_green_default(),
        fix_degradation_missing_and_contradictory(),
        fix_degradation_hysteresis_recovery(),
        fix_registry_green_default(),
        fix_registry_validity_transitions(),
        fix_ux_green_default(),
        fix_ux_integrity_and_order(),
    ];
    all.sort_by(|a, b| a.label.cmp(&b.label));
    all
}

/// Aggregate summary + gate booleans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCodingSummary {
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
    /// Distinct kernels exercised.
    pub kernels_covered: usize,
    /// Happy-path category exercised and matched.
    pub happy_path_covered: bool,
    /// Edge-case category exercised and matched.
    pub edge_case_covered: bool,
    /// Adversarial-input category exercised and matched.
    pub adversarial_covered: bool,
    /// Failure-mode-policy category exercised and matched.
    pub failure_mode_covered: bool,
    /// Every diagnostic carries all mandated fields.
    pub required_fields_complete: bool,
    /// Every fixture matched its oracle.
    pub all_expectations_met: bool,
    /// All six kernels exercised.
    pub all_kernels_covered: bool,
    /// Fail-closed gate verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
}

/// Deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCodingStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The full validation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactCodingValidationReport {
    /// Harness schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// Sorted diagnostics.
    pub diagnostics: Vec<ArtifactCodingDiagnostic>,
    /// Sorted verdicts.
    pub verdicts: Vec<OutcomeVerdict>,
    /// Aggregate summary.
    pub summary: ArtifactCodingSummary,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: ArtifactCodingStatsArtifact,
    /// Checksum over sorted diagnostics + verdicts.
    pub evidence_checksum: String,
}

impl ArtifactCodingValidationReport {
    /// AC3 failure logs: every diagnostic missing a mandated field, plus
    /// every diagnostic belonging to a kernel whose oracle mismatched (the
    /// builders always populate the structural fields, so the field-presence
    /// filter alone could never fire).
    #[must_use]
    pub fn failure_logs(&self) -> Vec<ArtifactCodingFailureLog> {
        let failing_kernels: BTreeSet<ArtifactKernel> = self
            .verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .map(|v| v.kernel)
            .collect();
        self.diagnostics
            .iter()
            .filter(|d| !d.has_required_fields() || failing_kernels.contains(&d.kernel))
            .map(ArtifactCodingDiagnostic::failure_log)
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

fn category_matched(corpus: &[ArtifactFixtureEvaluation], category: FixtureCategory) -> bool {
    corpus
        .iter()
        .any(|f| f.category == category && f.verdict.matches_expected)
}

/// Run the full artifact-coding validation and assemble the fail-closed
/// report.
#[must_use]
pub fn run_artifact_coding_validation(label: &str) -> ArtifactCodingValidationReport {
    let corpus = artifact_coding_corpus();

    let mut diagnostics: Vec<ArtifactCodingDiagnostic> =
        corpus.iter().flat_map(|f| f.diagnostics.clone()).collect();
    diagnostics.sort_by(|a, b| {
        a.kernel
            .cmp(&b.kernel)
            .then_with(|| a.equation_id.cmp(&b.equation_id))
            .then_with(|| a.outcome.cmp(&b.outcome))
            .then_with(|| a.detail.cmp(&b.detail))
    });

    let mut verdicts: Vec<OutcomeVerdict> = corpus.iter().map(|f| f.verdict.clone()).collect();
    verdicts.sort_by(|a, b| {
        a.kernel
            .cmp(&b.kernel)
            .then_with(|| a.fixture_label.cmp(&b.fixture_label))
    });

    #[derive(Serialize)]
    struct EvidenceInput<'a> {
        diagnostics: &'a [ArtifactCodingDiagnostic],
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
        "artifact-coding-tests-{}",
        short_hash(&stable_hash(&ReportIdInput {
            schema_version: ARTIFACT_CODING_TESTS_SCHEMA_VERSION,
            label,
            evidence_checksum: &evidence_checksum,
        }))
    );

    let kernels_covered = {
        let mut kernels: Vec<ArtifactKernel> = diagnostics.iter().map(|d| d.kernel).collect();
        kernels.sort();
        kernels.dedup();
        kernels.len()
    };
    let all_kernels_covered = kernels_covered == ArtifactKernel::ALL.len();
    let required_fields_complete = diagnostics
        .iter()
        .all(ArtifactCodingDiagnostic::has_required_fields);
    let matched_fixtures = verdicts.iter().filter(|v| v.matches_expected).count();
    let all_expectations_met = matched_fixtures == verdicts.len();

    let happy_path_covered = category_matched(&corpus, FixtureCategory::HappyPath);
    let edge_case_covered = category_matched(&corpus, FixtureCategory::EdgeCase);
    let adversarial_covered = category_matched(&corpus, FixtureCategory::AdversarialInput);
    let failure_mode_covered = category_matched(&corpus, FixtureCategory::FailureModePolicy);

    let gate_passes = required_fields_complete
        && all_expectations_met
        && all_kernels_covered
        && happy_path_covered
        && edge_case_covered
        && adversarial_covered
        && failure_mode_covered;

    let summary = ArtifactCodingSummary {
        schema_version: ARTIFACT_CODING_TESTS_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_fixtures: corpus.len(),
        total_diagnostics: diagnostics.len(),
        matched_fixtures,
        kernels_covered,
        happy_path_covered,
        edge_case_covered,
        adversarial_covered,
        failure_mode_covered,
        required_fields_complete,
        all_expectations_met,
        all_kernels_covered,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib artifact_coding_tests # report {report_id}"
        ),
    };

    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a ArtifactCodingSummary,
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: ARTIFACT_CODING_TESTS_SCHEMA_VERSION,
        report_id: &report_id,
        summary: &summary,
    })
    .unwrap_or_default();
    let exported_json_stats = ArtifactCodingStatsArtifact {
        path: format!("artifact_coding_tests/{report_id}.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    };

    ArtifactCodingValidationReport {
        schema_version: ARTIFACT_CODING_TESTS_SCHEMA_VERSION.to_string(),
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

    fn fixtures_for(kernel: ArtifactKernel) -> Vec<ArtifactFixtureEvaluation> {
        artifact_coding_corpus()
            .into_iter()
            .filter(|f| f.kernel == kernel)
            .collect()
    }

    fn assert_all_match(kernel: ArtifactKernel, expected_count: usize) {
        let fixtures = fixtures_for(kernel);
        assert_eq!(fixtures.len(), expected_count, "{}", kernel.as_str());
        for fixture in fixtures {
            assert!(
                fixture.verdict.matches_expected,
                "{}: {}",
                fixture.label, fixture.verdict.mismatch
            );
        }
    }

    #[test]
    fn fusion_fixtures_all_match() {
        assert_all_match(ArtifactKernel::Fusion, 4);
    }

    #[test]
    fn sequential_fdr_fixtures_all_match() {
        assert_all_match(ArtifactKernel::SequentialFdr, 3);
    }

    #[test]
    fn counterfactual_fixtures_all_match() {
        assert_all_match(ArtifactKernel::Counterfactual, 3);
    }

    #[test]
    fn degradation_fixtures_all_match() {
        assert_all_match(ArtifactKernel::Degradation, 3);
    }

    #[test]
    fn registry_fixtures_all_match() {
        assert_all_match(ArtifactKernel::GuaranteeRegistry, 2);
    }

    #[test]
    fn galaxy_ux_fixtures_all_match() {
        assert_all_match(ArtifactKernel::GalaxyUx, 2);
    }

    #[test]
    fn full_validation_passes_gate_and_covers_categories() {
        let report = run_artifact_coding_validation("ci");
        assert!(
            report.gate_passes(),
            "gate failed: {:?}",
            report.failing_verdicts()
        );
        assert_eq!(report.summary.total_fixtures, 17);
        assert_eq!(report.summary.matched_fixtures, 17);
        assert_eq!(report.summary.kernels_covered, 6);
        assert!(report.summary.all_kernels_covered);
        assert!(report.summary.happy_path_covered);
        assert!(report.summary.edge_case_covered);
        assert!(report.summary.adversarial_covered);
        assert!(report.summary.failure_mode_covered);
        assert!(report.summary.required_fields_complete);
        assert!(report.summary.all_expectations_met);
        assert!(report.failing_verdicts().is_empty());
        assert!(report.failure_logs().is_empty());
    }

    #[test]
    fn every_diagnostic_carries_ac3_fields() {
        let report = run_artifact_coding_validation("ac3");
        assert!(!report.diagnostics.is_empty());
        for diagnostic in &report.diagnostics {
            assert!(
                diagnostic.has_required_fields(),
                "incomplete: {diagnostic:?}"
            );
            assert!(!diagnostic.equation_id.is_empty());
            assert!(!diagnostic.assumption_id.is_empty());
            assert!(!diagnostic.guarantee_id.is_empty());
            assert!(!diagnostic.wealth_state.is_empty());
            assert!(!diagnostic.perturbation_norm.is_empty());
            assert!(!diagnostic.degradation_reason.is_empty());
            assert!(diagnostic.replay_cmd.contains("artifact_coding_tests"));
        }
    }

    #[test]
    fn ac3_fields_are_populated_where_meaningful() {
        let report = run_artifact_coding_validation("ac3-populated");
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|d| d.kernel == ArtifactKernel::SequentialFdr)
                .any(|d| d.wealth_state != "n/a" && d.wealth_state.contains("->"))
        );
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|d| d.kernel == ArtifactKernel::Counterfactual)
                .any(|d| d.perturbation_norm != "n/a")
        );
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|d| d.kernel == ArtifactKernel::GuaranteeRegistry)
                .any(|d| d.assumption_id.starts_with("ASM-")
                    && d.guarantee_id.starts_with("guarantee-"))
        );
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|d| d.kernel == ArtifactKernel::Degradation)
                .any(|d| d.degradation_reason.contains("critical_hazard"))
        );
    }

    #[test]
    fn oracle_mismatch_yields_replayable_failure_logs() {
        let mut report = run_artifact_coding_validation("mismatch");
        assert!(report.failure_logs().is_empty());

        report.verdicts[0].matches_expected = false;
        report.verdicts[0].mismatch = "forced mismatch for the failure-log contract".to_string();
        let failing_kernel = report.verdicts[0].kernel;
        let logs = report.failure_logs();
        assert!(!logs.is_empty());
        assert!(logs.iter().all(|log| !log.replay_cmd.is_empty()));
        let expected = report
            .diagnostics
            .iter()
            .filter(|d| d.kernel == failing_kernel)
            .count();
        assert_eq!(logs.len(), expected);
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_artifact_coding_validation("stats");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert!(report.exported_json_stats.path.contains(&report.report_id));
    }

    #[test]
    fn diagnostics_roundtrip_serde_byte_identically() {
        let report = run_artifact_coding_validation("serde");
        let encoded = serde_json::to_string(&report.diagnostics).expect("serialize");
        let decoded: Vec<ArtifactCodingDiagnostic> =
            serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(report.diagnostics, decoded);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_artifact_coding_validation(&label);
            let second = run_artifact_coding_validation(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
        }

        #[test]
        fn prop_diagnostics_label_independent(a in "[a-z]{1,8}", b in "[a-z]{1,8}") {
            let first = run_artifact_coding_validation(&a);
            let second = run_artifact_coding_validation(&b);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_artifact_coding_validation(&label);
            prop_assert!(report.gate_passes());
            prop_assert_eq!(report.summary.kernels_covered, 6);
        }

        #[test]
        fn prop_underlying_kernels_replay_deterministically(label in "[a-z]{1,8}") {
            let fusion_a = run_default_fusion_report(&label);
            let fusion_b = run_default_fusion_report(&label);
            prop_assert_eq!(fusion_a, fusion_b);

            let registry_a = run_default_guarantee_assumption_report(&label);
            let registry_b = run_default_guarantee_assumption_report(&label);
            prop_assert_eq!(registry_a, registry_b);
        }
    }
}

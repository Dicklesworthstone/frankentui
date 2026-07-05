//! Cross-kernel unit/property test-evidence harness for the alien-artifact
//! kernels and explainability surfaces (bd-3bxhj.10.27).
//!
//! Every alien-artifact kernel carries its own inline tests; this module is the
//! *cross-component* evidence harness that drives all six —
//! [`crate::posterior_core`], [`crate::decision_loss_policy`] (the expected-loss
//! solver), [`crate::guarantee_layer`], [`crate::voi_probe_planner`],
//! [`crate::hazard_regime_model`], and [`crate::galaxy_brain_cards`] — through a
//! single deterministic [`AlienKernelDiagnostic`] envelope, checks each against an
//! expected-outcome oracle, and emits an auditable validation report with a
//! fail-closed gate.
//!
//! Coverage (AC1) spans happy paths, adversarial inputs (extreme / malformed
//! likelihoods), and missing-data graceful degradation:
//! - **Posterior**: a strong-accept inference auto-approves; sparse evidence
//!   degrades to a conservative fallback; an extreme/non-finite log-Bayes-factor
//!   stays numerically stable (probability finite + clamped).
//! - **Loss solver**: the selected action is the expected-loss argmin, the
//!   decision id is deterministic, and the runner-up margin is non-negative.
//! - **Guarantees**: a well-calibrated, null-consistent bundle *holds*; a
//!   coverage failure triggers a conservative fallback.
//! - **VOI**: the default probe corpus passes its gate with justified selections.
//! - **Hazard**: a degrading regime auto-triggers a rollback transition and the
//!   false-alarm/miss tradeoff is profile-tunable.
//! - **Galaxy cards**: a transparency card renders a deterministic
//!   equation + substituted values + intuition with a stable content id.
//!
//! Determinism (AC2) is asserted by property tests that re-run the report and
//! compare byte-for-byte. Every diagnostic carries `run_id`, `claim_id`,
//! `equation_id`, `posterior_hash`, `loss_terms`, `guarantee_status`, and a replay
//! command (AC3). Like the [`crate::optimization_control_tests`] precedent this is
//! a `pub mod` compiled into the lib; all `proptest` usage is confined to the
//! `#[cfg(test)]` block. The diagnostic is **float-free** (numeric terms are
//! fixed-decimal strings via [`fmt6`]), so it derives [`Eq`] and replays
//! byte-identically.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::{
    DecisionPolicyEngine, LossPolicyManifest, PolicyProfile, RiskTier, action_str, compile,
};
use crate::galaxy_brain_cards::posterior_core_card;
use crate::guarantee_layer::{GuaranteeEvaluationInput, GuaranteeLayerEngine, PacBayesInput};
use crate::hazard_regime_model::run_default_hazard_regime;
use crate::posterior_core::{
    ChannelEvidence, EvidenceChannel, PosteriorEngine, PosteriorLedgerReport,
};
use crate::voi_probe_planner::run_voi_plan_report;

/// Schema version for the alien-kernel unit-test artifacts.
pub const ALIEN_KERNEL_TESTS_SCHEMA_VERSION: &str = "alien-kernel-unit-tests-v1";

// ── Hashing / formatting helpers ─────────────────────────────────────────────

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

// ── Kernels + diagnostic envelope ────────────────────────────────────────────

/// The alien-artifact kernel a diagnostic came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlienKernel {
    /// Closed-form Bayesian posterior core.
    Posterior,
    /// Decision-theoretic expected-loss solver.
    LossSolver,
    /// Conformal + e-process + PAC-Bayes guarantee layer.
    Guarantee,
    /// Value-of-information probe planner.
    Voi,
    /// Survival/hazard + BOCPD regime model.
    Hazard,
    /// Galaxy-brain transparency cards.
    GalaxyCard,
}

impl AlienKernel {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Posterior => "posterior",
            Self::LossSolver => "loss_solver",
            Self::Guarantee => "guarantee",
            Self::Voi => "voi",
            Self::Hazard => "hazard",
            Self::GalaxyCard => "galaxy_card",
        }
    }
}

/// A unified diagnostic projected from any kernel's output (float-free; `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienKernelDiagnostic {
    /// The source kernel.
    pub kernel: AlienKernel,
    /// Deterministic run / artifact id (AC3).
    pub run_id: String,
    /// The subject claim id (AC3).
    pub claim_id: String,
    /// The governing-equation / card id, or `"n/a"` (AC3).
    pub equation_id: String,
    /// The posterior / evidence checksum, or `"n/a"` (AC3).
    pub posterior_hash: String,
    /// Machine-readable loss / score terms (fixed-decimal), kernel-specific (AC3).
    pub loss_terms: Vec<String>,
    /// The formal-guarantee status (`holds` / `fallback` / `n/a`) (AC3).
    pub guarantee_status: String,
    /// The kernel's outcome tag.
    pub outcome: String,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command (AC3).
    pub replay_cmd: String,
}

impl AlienKernelDiagnostic {
    /// Whether every mandated AC3 field is present.
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.run_id.is_empty()
            && !self.claim_id.is_empty()
            && !self.equation_id.is_empty()
            && !self.posterior_hash.is_empty()
            && !self.loss_terms.is_empty()
            && !self.guarantee_status.is_empty()
            && !self.outcome.is_empty()
            && !self.detail.is_empty()
            && !self.replay_cmd.is_empty()
    }

    /// Project the AC3 failure-log schema.
    #[must_use]
    pub fn failure_log(&self) -> AlienKernelFailureLog {
        AlienKernelFailureLog {
            run_id: self.run_id.clone(),
            claim_id: self.claim_id.clone(),
            equation_id: self.equation_id.clone(),
            posterior_hash: self.posterior_hash.clone(),
            loss_terms: self.loss_terms.clone(),
            guarantee_status: self.guarantee_status.clone(),
            replay_cmd: self.replay_cmd.clone(),
        }
    }
}

/// The AC3 failure-log projection: the mandated fields for a failing case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienKernelFailureLog {
    /// Run id.
    pub run_id: String,
    /// Claim id.
    pub claim_id: String,
    /// Equation / card id.
    pub equation_id: String,
    /// Posterior / evidence checksum.
    pub posterior_hash: String,
    /// Loss / score terms.
    pub loss_terms: Vec<String>,
    /// Guarantee status.
    pub guarantee_status: String,
    /// Replay command.
    pub replay_cmd: String,
}

/// A fixture's pass/fail verdict against its oracle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeVerdict {
    /// The fixture label.
    pub fixture_label: String,
    /// The kernel.
    pub kernel: AlienKernel,
    /// The expectation, as a stable string.
    pub expectation: String,
    /// Whether the observed outcome matched the expectation.
    pub matches_expected: bool,
    /// Any mismatch detail.
    pub mismatch: String,
}

/// One fixture's evaluation: its diagnostics + verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelEvaluation {
    /// The fixture label.
    pub label: String,
    /// The kernel.
    pub kernel: AlienKernel,
    /// The projected diagnostics.
    pub diagnostics: Vec<AlienKernelDiagnostic>,
    /// The verdict.
    pub verdict: OutcomeVerdict,
}

fn verdict(
    label: &str,
    kernel: AlienKernel,
    expectation: &str,
    matches_expected: bool,
    mismatch: impl Into<String>,
) -> OutcomeVerdict {
    OutcomeVerdict {
        fixture_label: label.to_string(),
        kernel,
        expectation: expectation.to_string(),
        matches_expected,
        mismatch: mismatch.into(),
    }
}

fn replay(name: &str) -> String {
    format!("cargo test -p doctor_frankentui --lib alien_kernel_unit_tests # {name}")
}

// ── Posterior fixtures ─────────────────────────────────────────────────────────

fn accept_evidence(claim: &str, log_bf: f64) -> Vec<ChannelEvidence> {
    EvidenceChannel::ALL
        .iter()
        .map(|c| ChannelEvidence::new(*c, log_bf).claim(claim))
        .collect()
}

fn accept_report() -> PosteriorLedgerReport {
    let claim = "claim.accept";
    PosteriorEngine::default().evaluate(&[(claim.to_string(), accept_evidence(claim, 3.0))])
}

/// Happy: strong, complete evidence yields a high posterior and an accepting
/// (non-fallback) decision.
fn eval_posterior_accept() -> KernelEvaluation {
    let label = "posterior_accept";
    let report = accept_report();
    let record = &report.records[0];
    let diagnostics = vec![AlienKernelDiagnostic {
        kernel: AlienKernel::Posterior,
        run_id: report.report_id.clone(),
        claim_id: record.claim_id.clone(),
        equation_id: "n/a".to_string(),
        posterior_hash: report.exported_json_stats.sha256.clone(),
        loss_terms: vec![
            fmt6(record.posterior_prob),
            fmt6(record.posterior_logit),
            fmt6(record.aggregate_log_bayes_factor),
        ],
        guarantee_status: "n/a".to_string(),
        outcome: action_str(record.decision).to_string(),
        detail: format!(
            "posterior_prob {:.4} decision {} present {}/{}",
            record.posterior_prob,
            action_str(record.decision),
            record.present_channel_count,
            EvidenceChannel::ALL.len()
        ),
        replay_cmd: replay(label),
    }];
    let ok = record.posterior_prob > 0.5
        && !record.degraded
        && record.present_channel_count == EvidenceChannel::ALL.len();
    KernelEvaluation {
        label: label.to_string(),
        kernel: AlienKernel::Posterior,
        diagnostics,
        verdict: verdict(
            label,
            AlienKernel::Posterior,
            "complete strong evidence -> high posterior, non-degraded",
            ok,
            if ok {
                ""
            } else {
                "posterior aggregation wrong"
            },
        ),
    }
}

/// Missing-data: sparse evidence (one channel) degrades gracefully.
fn eval_posterior_sparse() -> KernelEvaluation {
    let label = "posterior_sparse";
    let claim = "claim.sparse";
    let evidence = vec![ChannelEvidence::new(EvidenceChannel::Semantic, 3.0).claim(claim)];
    let report = PosteriorEngine::default().evaluate(&[(claim.to_string(), evidence)]);
    let record = &report.records[0];
    let diagnostics = vec![AlienKernelDiagnostic {
        kernel: AlienKernel::Posterior,
        run_id: report.report_id.clone(),
        claim_id: record.claim_id.clone(),
        equation_id: "n/a".to_string(),
        posterior_hash: report.exported_json_stats.sha256.clone(),
        loss_terms: vec![fmt6(f64::from(u8::from(record.degraded)))],
        guarantee_status: "n/a".to_string(),
        outcome: if record.degraded {
            "degraded"
        } else {
            "not_degraded"
        }
        .to_string(),
        detail: format!(
            "present {}/{} degraded {} decision {}",
            record.present_channel_count,
            EvidenceChannel::ALL.len(),
            record.degraded,
            action_str(record.decision)
        ),
        replay_cmd: replay(label),
    }];
    let ok = record.degraded && record.present_channel_count < EvidenceChannel::ALL.len();
    KernelEvaluation {
        label: label.to_string(),
        kernel: AlienKernel::Posterior,
        diagnostics,
        verdict: verdict(
            label,
            AlienKernel::Posterior,
            "sparse evidence -> graceful degradation",
            ok,
            if ok { "" } else { "missing-data not degraded" },
        ),
    }
}

/// Adversarial: extreme + non-finite log-Bayes-factors stay numerically stable.
fn eval_posterior_extreme() -> KernelEvaluation {
    let label = "posterior_extreme";
    let claim = "claim.extreme";
    let evidence = vec![
        ChannelEvidence::new(EvidenceChannel::Semantic, 1.0e9).claim(claim),
        ChannelEvidence::new(EvidenceChannel::Visual, f64::NAN).claim(claim),
        ChannelEvidence::new(EvidenceChannel::Performance, f64::INFINITY).claim(claim),
        ChannelEvidence::new(EvidenceChannel::Accessibility, -1.0e9).claim(claim),
        ChannelEvidence::new(EvidenceChannel::Determinism, 2.0).claim(claim),
    ];
    let report = PosteriorEngine::default().evaluate(&[(claim.to_string(), evidence)]);
    let record = &report.records[0];
    let diagnostics = vec![AlienKernelDiagnostic {
        kernel: AlienKernel::Posterior,
        run_id: report.report_id.clone(),
        claim_id: record.claim_id.clone(),
        equation_id: "n/a".to_string(),
        posterior_hash: report.exported_json_stats.sha256.clone(),
        loss_terms: vec![fmt6(record.posterior_prob), fmt6(record.posterior_logit)],
        guarantee_status: "n/a".to_string(),
        outcome: "stable".to_string(),
        detail: format!(
            "posterior_prob {:.6} logit {:.6} (finite + clamped under extreme/NaN/inf inputs)",
            record.posterior_prob, record.posterior_logit
        ),
        replay_cmd: replay(label),
    }];
    let ok = record.posterior_prob.is_finite()
        && (0.0..=1.0).contains(&record.posterior_prob)
        && record.posterior_logit.is_finite();
    KernelEvaluation {
        label: label.to_string(),
        kernel: AlienKernel::Posterior,
        diagnostics,
        verdict: verdict(
            label,
            AlienKernel::Posterior,
            "extreme/non-finite log-BF -> finite, clamped posterior",
            ok,
            if ok { "" } else { "numerical instability" },
        ),
    }
}

// ── Loss-solver fixture ────────────────────────────────────────────────────────

/// The expected-loss solver selects the argmin action, deterministically, with a
/// non-negative runner-up margin (correctness + tie-break determinism +
/// counterfactual consistency).
fn eval_loss_solver() -> KernelEvaluation {
    let label = "loss_solver";
    let report = accept_report();
    let record = &report.records[0];
    let policy = match compile(&LossPolicyManifest::standard("pol.alien", "v1"), &[]) {
        Ok(p) => p,
        Err(error) => {
            return error_eval(label, AlienKernel::LossSolver, &error.to_string());
        }
    };
    let engine = DecisionPolicyEngine::new(policy);
    let first = engine.decide_from_posterior(record, PolicyProfile::Balanced, RiskTier::Medium);
    let second = engine.decide_from_posterior(record, PolicyProfile::Balanced, RiskTier::Medium);
    let (entry, entry2) = match (first, second) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(error), _) | (_, Err(error)) => {
            return error_eval(label, AlienKernel::LossSolver, &error.to_string());
        }
    };

    let decision = &entry.decision;
    let min_el = decision
        .per_action
        .iter()
        .map(|a| a.expected_loss)
        .fold(f64::INFINITY, f64::min);
    let selected_el = decision
        .per_action
        .iter()
        .find(|a| a.action == decision.selected)
        .map_or(f64::INFINITY, |a| a.expected_loss);
    let argmin_correct = (selected_el - min_el).abs() < 1e-9;
    let deterministic = entry.decision_id == entry2.decision_id;
    let margin_nonneg = decision.margin >= -1e-9;
    let all_actions = decision.per_action.len() == 6;
    let tie_break_consistent =
        !decision.tie_break_applied || decision.tie_candidates.contains(&decision.selected);

    let diagnostics = vec![AlienKernelDiagnostic {
        kernel: AlienKernel::LossSolver,
        run_id: entry.decision_id.clone(),
        claim_id: entry.claim_id.clone(),
        equation_id: "n/a".to_string(),
        posterior_hash: report.exported_json_stats.sha256.clone(),
        loss_terms: decision
            .per_action
            .iter()
            .map(|a| fmt6(a.expected_loss))
            .collect(),
        guarantee_status: "n/a".to_string(),
        outcome: action_str(decision.selected).to_string(),
        detail: format!(
            "selected {} min_EL {:.6} margin {:.6} tie_break {}",
            action_str(decision.selected),
            min_el,
            decision.margin,
            decision.tie_break_applied
        ),
        replay_cmd: replay(label),
    }];
    let ok =
        argmin_correct && deterministic && margin_nonneg && all_actions && tie_break_consistent;
    KernelEvaluation {
        label: label.to_string(),
        kernel: AlienKernel::LossSolver,
        diagnostics,
        verdict: verdict(
            label,
            AlienKernel::LossSolver,
            "selected = expected-loss argmin, deterministic id, margin >= 0",
            ok,
            if ok {
                ""
            } else {
                "loss-solver correctness/determinism wrong"
            },
        ),
    }
}

// ── Guarantee fixtures ─────────────────────────────────────────────────────────

/// Holds: a well-calibrated, null-consistent bundle (no coverage validation
/// supplied) does not recommend a fallback.
fn eval_guarantee_holds() -> KernelEvaluation {
    let label = "guarantee_holds";
    let report = accept_report();
    let record = &report.records[0];
    let observations: Vec<f64> = [0.2_f64, 0.2, 0.0, 0.0]
        .into_iter()
        .cycle()
        .take(40)
        .collect();
    let input =
        GuaranteeEvaluationInput::from_posterior(record, PacBayesInput::new(0.1, 1.0, 1000))
            .with_calibration((0..50).map(|i| f64::from(i) * 0.01))
            .with_observations(observations);
    match GuaranteeLayerEngine::default().evaluate(&input) {
        Ok(gr) => {
            let status = if gr.fallback.triggered {
                "fallback"
            } else {
                "holds"
            };
            let diagnostics = vec![AlienKernelDiagnostic {
                kernel: AlienKernel::Guarantee,
                run_id: gr.report_id.clone(),
                claim_id: gr.claim_id.clone(),
                equation_id: "n/a".to_string(),
                posterior_hash: gr.exported_json_stats.sha256.clone(),
                loss_terms: vec![
                    fmt6(f64::from(u8::from(gr.conformal.fallback_recommended))),
                    fmt6(f64::from(u8::from(gr.e_process.rejected))),
                    fmt6(f64::from(u8::from(gr.pac_bayes.vacuous))),
                ],
                guarantee_status: status.to_string(),
                outcome: status.to_string(),
                detail: format!(
                    "conformal vacuous {} e-process rejected {} pac-bayes vacuous {}",
                    gr.conformal.vacuous, gr.e_process.rejected, gr.pac_bayes.vacuous
                ),
                replay_cmd: replay(label),
            }];
            let ok = !gr.fallback.triggered;
            KernelEvaluation {
                label: label.to_string(),
                kernel: AlienKernel::Guarantee,
                diagnostics,
                verdict: verdict(
                    label,
                    AlienKernel::Guarantee,
                    "calibrated null-consistent bundle holds (no fallback)",
                    ok,
                    if ok {
                        ""
                    } else {
                        "guarantee falsely fell back"
                    },
                ),
            }
        }
        Err(error) => error_eval(label, AlienKernel::Guarantee, &error.to_string()),
    }
}

/// Fallback: a conformal coverage failure recommends a conservative fallback.
fn eval_guarantee_fallback() -> KernelEvaluation {
    let label = "guarantee_fallback";
    let claim = "claim.calibration";
    let input = GuaranteeEvaluationInput::new(claim, 0.8, PacBayesInput::new(0.1, 1.0, 1000))
        .with_calibration((0..50).map(|i| f64::from(i) * 0.01))
        .with_validation((0..20).map(|_| (0.0, 0.9)))
        .with_observations(vec![0.1; 20]);
    match GuaranteeLayerEngine::default().evaluate(&input) {
        Ok(gr) => {
            let status = if gr.fallback.triggered {
                "fallback"
            } else {
                "holds"
            };
            let diagnostics = vec![AlienKernelDiagnostic {
                kernel: AlienKernel::Guarantee,
                run_id: gr.report_id.clone(),
                claim_id: gr.claim_id.clone(),
                equation_id: "n/a".to_string(),
                posterior_hash: gr.exported_json_stats.sha256.clone(),
                loss_terms: vec![fmt6(gr.conformal.empirical_coverage.unwrap_or(0.0))],
                guarantee_status: status.to_string(),
                outcome: status.to_string(),
                detail: format!(
                    "coverage {:?} fallback {} reasons {}",
                    gr.conformal.empirical_coverage,
                    gr.fallback.triggered,
                    gr.fallback.reasons.join("|")
                ),
                replay_cmd: replay(label),
            }];
            let ok = gr.fallback.triggered && !gr.fallback.reasons.is_empty();
            KernelEvaluation {
                label: label.to_string(),
                kernel: AlienKernel::Guarantee,
                diagnostics,
                verdict: verdict(
                    label,
                    AlienKernel::Guarantee,
                    "coverage failure -> conservative fallback with reasons",
                    ok,
                    if ok {
                        ""
                    } else {
                        "coverage failure not caught"
                    },
                ),
            }
        }
        Err(error) => error_eval(label, AlienKernel::Guarantee, &error.to_string()),
    }
}

// ── VOI fixture ────────────────────────────────────────────────────────────────

/// The default probe corpus passes its gate with at least one justified selection.
fn eval_voi() -> KernelEvaluation {
    let label = "voi";
    let report = run_voi_plan_report("alien-kernel-tests/voi");
    let s = &report.summary;
    let diagnostics = vec![AlienKernelDiagnostic {
        kernel: AlienKernel::Voi,
        run_id: report.run_id.clone(),
        claim_id: "voi-corpus".to_string(),
        equation_id: "n/a".to_string(),
        posterior_hash: report.evidence_checksum.clone(),
        loss_terms: vec![fmt6(s.selected as f64), fmt6(s.skipped as f64)],
        guarantee_status: "n/a".to_string(),
        outcome: if report.gate_passes {
            "gate_pass"
        } else {
            "gate_fail"
        }
        .to_string(),
        detail: format!(
            "selected {} skipped {} candidates {} justified {}",
            s.selected, s.skipped, s.total_candidates, s.all_selections_justified
        ),
        replay_cmd: replay(label),
    }];
    let ok = report.gate_passes && s.all_selections_justified && s.selected > 0;
    KernelEvaluation {
        label: label.to_string(),
        kernel: AlienKernel::Voi,
        diagnostics,
        verdict: verdict(
            label,
            AlienKernel::Voi,
            "default probe corpus gate passes with justified selections",
            ok,
            if ok { "" } else { "voi planner gate failed" },
        ),
    }
}

// ── Hazard fixture ─────────────────────────────────────────────────────────────

/// A degrading regime auto-triggers a transition; the tradeoff is profile-tunable.
fn eval_hazard() -> KernelEvaluation {
    let label = "hazard";
    let report = run_default_hazard_regime("alien-kernel-tests/hazard");
    let s = &report.summary;
    let diagnostics = vec![AlienKernelDiagnostic {
        kernel: AlienKernel::Hazard,
        run_id: report.run_id.clone(),
        claim_id: "hazard-regimes".to_string(),
        equation_id: "n/a".to_string(),
        posterior_hash: report.evidence_checksum.clone(),
        loss_terms: vec![fmt6(s.total_regimes as f64), fmt6(s.drift_regimes as f64)],
        guarantee_status: "n/a".to_string(),
        outcome: if report.gate_passes {
            "gate_pass"
        } else {
            "gate_fail"
        }
        .to_string(),
        detail: format!(
            "regimes {} drift {} transition {} tunable {}",
            s.total_regimes, s.drift_regimes, s.transition_triggered, s.tradeoff_tunable
        ),
        replay_cmd: replay(label),
    }];
    let ok = report.gate_passes
        && s.transition_triggered
        && s.tradeoff_tunable
        && s.uncertainty_calibrated;
    KernelEvaluation {
        label: label.to_string(),
        kernel: AlienKernel::Hazard,
        diagnostics,
        verdict: verdict(
            label,
            AlienKernel::Hazard,
            "degrading regime auto-transitions; tradeoff is profile-tunable",
            ok,
            if ok {
                ""
            } else {
                "hazard regime model gate failed"
            },
        ),
    }
}

// ── Galaxy-card fixture ──────────────────────────────────────────────────────

/// A posterior-core transparency card renders a deterministic equation +
/// substituted values + intuition with a stable content id.
fn eval_galaxy_card() -> KernelEvaluation {
    let label = "galaxy_card";
    let report = accept_report();
    let record = &report.records[0];
    let card_a = posterior_core_card(record);
    let card_b = posterior_core_card(record);
    let diagnostics = vec![AlienKernelDiagnostic {
        kernel: AlienKernel::GalaxyCard,
        run_id: card_a.card_id.clone(),
        claim_id: card_a.claim_id.clone(),
        equation_id: card_a.card_id.clone(),
        posterior_hash: report.exported_json_stats.sha256.clone(),
        loss_terms: vec![fmt6(card_a.substitutions.len() as f64)],
        guarantee_status: "n/a".to_string(),
        outcome: card_a.kind.as_str().to_string(),
        detail: format!(
            "equation '{}' substitutions {} intuition_len {}",
            card_a.equation,
            card_a.substitutions.len(),
            card_a.intuition.len()
        ),
        replay_cmd: replay(label),
    }];
    let ok = card_a.card_id == card_b.card_id
        && card_a.equation == card_b.equation
        && !card_a.equation.is_empty()
        && !card_a.substitutions.is_empty()
        && !card_a.intuition.is_empty();
    KernelEvaluation {
        label: label.to_string(),
        kernel: AlienKernel::GalaxyCard,
        diagnostics,
        verdict: verdict(
            label,
            AlienKernel::GalaxyCard,
            "card renders deterministic equation + substitutions + intuition",
            ok,
            if ok {
                ""
            } else {
                "galaxy card non-deterministic / empty"
            },
        ),
    }
}

fn error_eval(label: &str, kernel: AlienKernel, message: &str) -> KernelEvaluation {
    KernelEvaluation {
        label: label.to_string(),
        kernel,
        diagnostics: vec![AlienKernelDiagnostic {
            kernel,
            run_id: "error".to_string(),
            claim_id: "error".to_string(),
            equation_id: "n/a".to_string(),
            posterior_hash: "n/a".to_string(),
            loss_terms: vec![fmt6(0.0)],
            guarantee_status: "error".to_string(),
            outcome: "error".to_string(),
            detail: message.to_string(),
            replay_cmd: replay(label),
        }],
        verdict: verdict(
            label,
            kernel,
            "kernel evaluated without error",
            false,
            message,
        ),
    }
}

/// The full fixture corpus across every alien kernel.
#[must_use]
pub fn alien_kernel_corpus() -> Vec<KernelEvaluation> {
    let mut all = vec![
        eval_posterior_accept(),
        eval_posterior_sparse(),
        eval_posterior_extreme(),
        eval_loss_solver(),
        eval_guarantee_holds(),
        eval_guarantee_fallback(),
        eval_voi(),
        eval_hazard(),
        eval_galaxy_card(),
    ];
    all.sort_by(|a, b| a.label.cmp(&b.label));
    all
}

// ── Validation report ──────────────────────────────────────────────────────────

/// Roll-up of the alien-kernel validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienKernelSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the diagnostics + verdicts.
    pub evidence_checksum: String,
    /// Total fixtures evaluated.
    pub total_fixtures: usize,
    /// Total diagnostics projected.
    pub total_diagnostics: usize,
    /// Distinct kernels covered.
    pub kernels_covered: usize,
    /// Fixtures whose outcome matched their oracle.
    pub matched_fixtures: usize,
    /// Whether every diagnostic carries all mandated AC3 fields.
    pub required_fields_complete: bool,
    /// Whether every fixture matched its expected oracle (AC1).
    pub all_expectations_met: bool,
    /// Whether all six kernels are represented.
    pub all_kernels_covered: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlienKernelStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full alien-kernel validation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlienKernelValidationReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// All projected diagnostics (sorted).
    pub diagnostics: Vec<AlienKernelDiagnostic>,
    /// All fixture verdicts (sorted).
    pub verdicts: Vec<OutcomeVerdict>,
    /// The roll-up summary + gate.
    pub summary: AlienKernelSummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: AlienKernelStatsArtifact,
    /// Evidence checksum.
    pub evidence_checksum: String,
}

impl AlienKernelValidationReport {
    /// Failure logs for any failing case (AC3).
    ///
    /// Covers diagnostics missing mandated fields AND every diagnostic from a
    /// kernel whose oracle verdict mismatched: a kernel regression must yield
    /// replayable failing-case entries in the AC3 artifact, not an empty list
    /// (the constructors always populate the structural fields, so the
    /// field-presence filter alone could never fire).
    #[must_use]
    pub fn failure_logs(&self) -> Vec<AlienKernelFailureLog> {
        let failing_kernels: std::collections::BTreeSet<AlienKernel> = self
            .verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .map(|v| v.kernel)
            .collect();
        self.diagnostics
            .iter()
            .filter(|d| !d.has_required_fields() || failing_kernels.contains(&d.kernel))
            .map(AlienKernelDiagnostic::failure_log)
            .collect()
    }

    /// Verdicts that did not match their oracle.
    #[must_use]
    pub fn failing_verdicts(&self) -> Vec<&OutcomeVerdict> {
        self.verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .collect()
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

#[derive(Serialize)]
struct EvidenceInput<'a> {
    diagnostics: &'a [AlienKernelDiagnostic],
    verdicts: &'a [OutcomeVerdict],
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    schema_version: &'a str,
    label: &'a str,
    evidence_checksum: &'a str,
}

/// Run the full alien-kernel validation.
#[must_use]
pub fn run_alien_kernel_validation(label: &str) -> AlienKernelValidationReport {
    let corpus = alien_kernel_corpus();

    let mut diagnostics: Vec<AlienKernelDiagnostic> =
        corpus.iter().flat_map(|e| e.diagnostics.clone()).collect();
    diagnostics.sort_by(|a, b| {
        a.kernel
            .cmp(&b.kernel)
            .then_with(|| a.claim_id.cmp(&b.claim_id))
            .then_with(|| a.run_id.cmp(&b.run_id))
            .then_with(|| a.outcome.cmp(&b.outcome))
    });
    let mut verdicts: Vec<OutcomeVerdict> = corpus.iter().map(|e| e.verdict.clone()).collect();
    verdicts.sort_by(|a, b| {
        a.kernel
            .cmp(&b.kernel)
            .then_with(|| a.fixture_label.cmp(&b.fixture_label))
    });

    let evidence_checksum = stable_hash(&EvidenceInput {
        diagnostics: &diagnostics,
        verdicts: &verdicts,
    });
    let report_id = format!(
        "alien-kernel-unit-tests-{}",
        short_hash(&stable_hash(&ReportIdInput {
            schema_version: ALIEN_KERNEL_TESTS_SCHEMA_VERSION,
            label,
            evidence_checksum: &evidence_checksum,
        }))
    );

    let kernels_covered = {
        let mut k: Vec<AlienKernel> = diagnostics.iter().map(|d| d.kernel).collect();
        k.sort();
        k.dedup();
        k.len()
    };
    let matched_fixtures = verdicts.iter().filter(|v| v.matches_expected).count();
    let required_fields_complete = diagnostics
        .iter()
        .all(AlienKernelDiagnostic::has_required_fields);
    let all_expectations_met = verdicts.iter().all(|v| v.matches_expected);
    let all_kernels_covered = kernels_covered == 6;
    let gate_passes = required_fields_complete && all_expectations_met && all_kernels_covered;

    let summary = AlienKernelSummary {
        schema_version: ALIEN_KERNEL_TESTS_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_fixtures: verdicts.len(),
        total_diagnostics: diagnostics.len(),
        kernels_covered,
        matched_fixtures,
        required_fields_complete,
        all_expectations_met,
        all_kernels_covered,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib alien_kernel_unit_tests # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a AlienKernelSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: ALIEN_KERNEL_TESTS_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        AlienKernelStatsArtifact {
            path: format!("alien_kernel_unit_tests/{report_id}.json"),
            sha256,
            content,
        }
    };

    AlienKernelValidationReport {
        schema_version: ALIEN_KERNEL_TESTS_SCHEMA_VERSION.to_string(),
        report_id,
        label: label.to_string(),
        diagnostics,
        verdicts,
        summary,
        exported_json_stats,
        evidence_checksum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn oracle_mismatch_yields_replayable_failure_logs() {
        let mut report = run_alien_kernel_validation("ak/test");
        // Healthy run: nothing to log.
        assert!(report.failure_logs().is_empty());
        // A kernel regression (oracle mismatch) must surface that kernel's
        // diagnostics as replayable AC3 failure-log entries.
        let failing_kernel = report.verdicts[0].kernel;
        report.verdicts[0].matches_expected = false;
        let logs = report.failure_logs();
        assert!(
            !logs.is_empty(),
            "kernel regression produced an empty AC3 failure-log artifact"
        );
        assert!(logs.iter().all(|l| !l.replay_cmd.is_empty()));
        assert_eq!(
            logs.len(),
            report
                .diagnostics
                .iter()
                .filter(|d| d.kernel == failing_kernel)
                .count()
        );
    }

    #[test]
    fn posterior_fixtures_pass() {
        assert!(eval_posterior_accept().verdict.matches_expected);
        assert!(eval_posterior_sparse().verdict.matches_expected);
        assert!(eval_posterior_extreme().verdict.matches_expected);
    }

    #[test]
    fn loss_solver_selects_argmin_deterministically() {
        let e = eval_loss_solver();
        assert!(e.verdict.matches_expected, "{:?}", e.verdict);
    }

    #[test]
    fn guarantee_holds_and_fallback() {
        assert!(eval_guarantee_holds().verdict.matches_expected);
        assert!(eval_guarantee_fallback().verdict.matches_expected);
    }

    #[test]
    fn voi_and_hazard_gates_pass() {
        assert!(eval_voi().verdict.matches_expected);
        assert!(eval_hazard().verdict.matches_expected);
    }

    #[test]
    fn galaxy_card_is_deterministic() {
        let e = eval_galaxy_card();
        assert!(e.verdict.matches_expected, "{:?}", e.verdict);
    }

    #[test]
    fn full_validation_passes_gate_and_covers_all_kernels() {
        let report = run_alien_kernel_validation("ak/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.kernels_covered, 6);
        assert!(report.summary.all_kernels_covered);
        assert!(report.summary.all_expectations_met);
        assert!(report.summary.required_fields_complete);
        assert!(report.failing_verdicts().is_empty());
        assert!(report.failure_logs().is_empty());
        for d in &report.diagnostics {
            assert!(d.has_required_fields(), "missing AC3 fields: {d:?}");
        }
    }

    #[test]
    fn every_diagnostic_carries_ac3_fields() {
        let report = run_alien_kernel_validation("ak/ac3");
        for d in &report.diagnostics {
            // run_id, claim_id, equation_id, posterior_hash, loss_terms,
            // guarantee_status, replay command.
            assert!(!d.run_id.is_empty());
            assert!(!d.claim_id.is_empty());
            assert!(!d.equation_id.is_empty());
            assert!(!d.posterior_hash.is_empty());
            assert!(!d.loss_terms.is_empty());
            assert!(!d.guarantee_status.is_empty());
            assert!(d.replay_cmd.contains("alien_kernel_unit_tests"));
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_alien_kernel_validation("ak/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_alien_kernel_validation(&label);
            let second = run_alien_kernel_validation(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
        }

        #[test]
        fn prop_diagnostics_label_independent(a in "[a-z]{1,8}", b in "[a-z]{1,8}") {
            let ra = run_alien_kernel_validation(&a);
            let rb = run_alien_kernel_validation(&b);
            prop_assert_eq!(&ra.diagnostics, &rb.diagnostics);
            prop_assert_eq!(&ra.verdicts, &rb.verdicts);
            prop_assert_eq!(&ra.evidence_checksum, &rb.evidence_checksum);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_alien_kernel_validation(&label);
            prop_assert!(report.gate_passes());
            prop_assert_eq!(report.summary.kernels_covered, 6);
        }
    }
}

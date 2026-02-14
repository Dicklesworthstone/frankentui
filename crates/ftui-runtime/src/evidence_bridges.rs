#![forbid(unsafe_code)]

//! Evidence bridges: convert domain-specific decision types into unified
//! [`EvidenceEntry`] records (bd-xox.4).
//!
//! Each bridge function takes a domain-specific decision/evidence struct and
//! a timestamp, and returns a unified `EvidenceEntry` suitable for the
//! [`UnifiedEvidenceLedger`].
//!
//! # Supported domains
//!
//! | Domain | Source type | Bridge function |
//! |--------|-----------|-----------------|
//! | DiffStrategy | `StrategyEvidence` | [`from_diff_strategy`] |
//! | ResizeCoalescing / BOCPD | `BocpdEvidence` | [`from_bocpd`] |
//! | FrameBudget (e-process) | `ThrottleDecision` | [`from_eprocess`] |
//! | VoiSampling | `VoiDecision` | [`from_voi`] |
//! | Conformal (Degradation) | `ConformalPrediction` | [`from_conformal`] |

use crate::unified_evidence::{DecisionDomain, EvidenceEntry, EvidenceEntryBuilder};

// ============================================================================
// 1. Diff Strategy
// ============================================================================

/// Convert a [`StrategyEvidence`] into a unified evidence entry.
///
/// Maps the Beta-Binomial posterior on change rate and per-strategy costs
/// to the unified schema.
pub fn from_diff_strategy(
    evidence: &ftui_render::diff_strategy::StrategyEvidence,
    timestamp_ns: u64,
) -> EvidenceEntry {
    // Determine the action string from the strategy enum.
    let action: &'static str = match evidence.strategy {
        ftui_render::diff_strategy::DiffStrategy::Full => "full",
        ftui_render::diff_strategy::DiffStrategy::DirtyRows => "dirty_rows",
        ftui_render::diff_strategy::DiffStrategy::FullRedraw => "full_redraw",
    };

    // Compute log-posterior from posterior_mean (change rate p).
    // Map p → log-odds that the chosen strategy is optimal.
    let chosen_cost = match evidence.strategy {
        ftui_render::diff_strategy::DiffStrategy::Full => evidence.cost_full,
        ftui_render::diff_strategy::DiffStrategy::DirtyRows => evidence.cost_dirty,
        ftui_render::diff_strategy::DiffStrategy::FullRedraw => evidence.cost_redraw,
    };
    let min_other_cost = [
        evidence.cost_full,
        evidence.cost_dirty,
        evidence.cost_redraw,
    ]
    .into_iter()
    .filter(|&c| (c - chosen_cost).abs() > 1e-12)
    .fold(f64::MAX, f64::min);
    let loss_avoided = if min_other_cost < f64::MAX {
        (min_other_cost - chosen_cost).max(0.0)
    } else {
        0.0
    };

    // Log-posterior from posterior mean (approximate).
    let p = evidence.posterior_mean.clamp(1e-6, 1.0 - 1e-6);
    let log_posterior = (p / (1.0 - p)).ln();

    // Confidence interval from posterior variance.
    let std_dev = evidence.posterior_variance.sqrt();
    let lower = (p - 1.96 * std_dev).clamp(0.0, 1.0);
    let upper = (p + 1.96 * std_dev).clamp(0.0, 1.0);

    // Evidence terms: the cost ratios serve as Bayes factors.
    let mut builder = EvidenceEntryBuilder::new(DecisionDomain::DiffStrategy, 0, timestamp_ns)
        .log_posterior(log_posterior)
        .action(action)
        .loss_avoided(loss_avoided)
        .confidence_interval(lower, upper);

    // BF for change rate: how much the observed rate supports the chosen strategy.
    if evidence.posterior_mean > 0.0 {
        builder = builder.evidence("change_rate", evidence.posterior_mean * 20.0);
    }
    // BF for dirty-row ratio.
    if evidence.total_rows > 0 {
        let dirty_ratio = evidence.dirty_rows as f64 / evidence.total_rows as f64;
        builder = builder.evidence("dirty_ratio", 1.0 + dirty_ratio * 5.0);
    }
    // Hysteresis applied as negative evidence.
    if evidence.hysteresis_applied {
        builder = builder.evidence("hysteresis", 0.8);
    }

    builder.build()
}

// ============================================================================
// 2. E-Process Throttle
// ============================================================================

/// Convert a [`ThrottleDecision`] into a unified evidence entry.
///
/// Maps the wealth-based e-process and empirical rate to the unified schema.
pub fn from_eprocess(
    decision: &crate::eprocess_throttle::ThrottleDecision,
    timestamp_ns: u64,
) -> EvidenceEntry {
    let action: &'static str = if decision.forced_by_deadline {
        "recompute_forced"
    } else if decision.should_recompute {
        "recompute"
    } else {
        "hold"
    };

    // Wealth W_t as log-posterior (log-odds of needing recompute).
    let log_posterior = decision.wealth.max(1e-12).ln();

    // Evidence terms.
    let mut builder = EvidenceEntryBuilder::new(DecisionDomain::FrameBudget, 0, timestamp_ns)
        .log_posterior(log_posterior)
        .action(action)
        .loss_avoided(if decision.should_recompute {
            decision.wealth.ln().max(0.0)
        } else {
            0.0
        })
        .confidence_interval(
            decision.empirical_rate.max(0.0),
            (decision.empirical_rate + 0.1).min(1.0),
        );

    // Wealth as Bayes factor (directly interpretable as evidence strength).
    builder = builder.evidence("wealth", decision.wealth);

    // Lambda (betting fraction).
    if decision.lambda.abs() > 1e-12 {
        builder = builder.evidence("lambda", (1.0 + decision.lambda.abs()).max(0.01));
    }

    // Empirical rate.
    builder = builder.evidence("empirical_rate", 1.0 + decision.empirical_rate * 5.0);

    builder.build()
}

// ============================================================================
// 3. VOI Sampling
// ============================================================================

/// Convert a [`VoiDecision`] into a unified evidence entry.
///
/// Maps the VOI score, e-process wealth, and posterior statistics
/// to the unified schema.
pub fn from_voi(decision: &crate::voi_sampling::VoiDecision, timestamp_ns: u64) -> EvidenceEntry {
    let action: &'static str = decision.reason;

    // Log-posterior from posterior_mean.
    let p = decision.posterior_mean.clamp(1e-6, 1.0 - 1e-6);
    let log_posterior = (p / (1.0 - p)).ln();

    let std_dev = decision.posterior_variance.sqrt();
    let lower = (p - 1.96 * std_dev).clamp(0.0, 1.0);
    let upper = (p + 1.96 * std_dev).clamp(0.0, 1.0);

    let mut builder = EvidenceEntryBuilder::new(DecisionDomain::VoiSampling, 0, timestamp_ns)
        .log_posterior(log_posterior)
        .action(action)
        .loss_avoided(decision.voi_gain)
        .confidence_interval(lower, upper);

    // VOI score as Bayes factor.
    if decision.score > 0.0 {
        builder = builder.evidence("voi_score", 1.0 + decision.score * 10.0);
    }

    // E-value.
    if decision.e_value > 0.0 {
        builder = builder.evidence("e_value", decision.e_value);
    }

    // Boundary score.
    if decision.boundary_score > 0.0 {
        builder = builder.evidence("boundary_score", 1.0 + decision.boundary_score * 3.0);
    }

    builder.build()
}

// ============================================================================
// 4. Conformal Prediction (Degradation)
// ============================================================================

/// Convert a [`ConformalPrediction`] into a unified evidence entry.
///
/// Maps the conformal prediction bound and budget risk to the unified
/// degradation decision schema.
pub fn from_conformal(
    prediction: &crate::conformal_predictor::ConformalPrediction,
    timestamp_ns: u64,
) -> EvidenceEntry {
    let action: &'static str = if prediction.risk { "degrade" } else { "hold" };

    // Log-odds of needing degradation.
    let risk_ratio = if prediction.budget_us > 0.0 {
        prediction.upper_us / prediction.budget_us
    } else {
        1.0
    };
    let log_posterior = (risk_ratio.clamp(0.01, 100.0)).ln();

    let mut builder = EvidenceEntryBuilder::new(DecisionDomain::Degradation, 0, timestamp_ns)
        .log_posterior(log_posterior)
        .action(action)
        .loss_avoided(if prediction.risk {
            (prediction.upper_us - prediction.budget_us).max(0.0) / prediction.budget_us.max(1.0)
        } else {
            0.0
        })
        .confidence_interval(prediction.confidence - 0.05, prediction.confidence);

    // Budget headroom as BF (< 1.0 means over budget).
    if prediction.budget_us > 0.0 {
        builder = builder.evidence(
            "budget_headroom",
            (prediction.budget_us / prediction.upper_us.max(1.0)).max(0.01),
        );
    }

    // Conformal quantile.
    if prediction.quantile > 0.0 {
        builder = builder.evidence("quantile", 1.0 + prediction.quantile / 1000.0);
    }

    // Sample count (more samples = stronger evidence).
    if prediction.sample_count > 0 {
        builder = builder.evidence(
            "sample_strength",
            1.0 + (prediction.sample_count as f64).ln() / 5.0,
        );
    }

    builder.build()
}

// ============================================================================
// 5. BOCPD (Resize Coalescing)
// ============================================================================

/// Convert a [`BocpdEvidence`] into a unified evidence entry.
///
/// Maps the BOCPD regime posterior and run-length statistics to the
/// resize coalescing decision schema.
pub fn from_bocpd(evidence: &crate::bocpd::BocpdEvidence, timestamp_ns: u64) -> EvidenceEntry {
    let action: &'static str = match evidence.regime {
        crate::bocpd::BocpdRegime::Steady => "apply",
        crate::bocpd::BocpdRegime::Burst => "coalesce",
        crate::bocpd::BocpdRegime::Transitional => "placeholder",
    };

    // Log-posterior from burst probability.
    let p = evidence.p_burst.clamp(1e-6, 1.0 - 1e-6);
    let log_posterior = (p / (1.0 - p)).ln();

    // Confidence interval from run-length variance.
    let rl_std = evidence.run_length_variance.sqrt();
    let rl_mean = evidence.expected_run_length;
    let lower = ((rl_mean - 1.96 * rl_std) / (rl_mean + 1.96 * rl_std + 1.0)).clamp(0.0, 1.0);
    let upper = ((rl_mean + 1.96 * rl_std) / (rl_mean + 1.96 * rl_std + 1.0)).clamp(0.0, 1.0);

    let mut builder = EvidenceEntryBuilder::new(DecisionDomain::ResizeCoalescing, 0, timestamp_ns)
        .log_posterior(log_posterior)
        .action(action)
        .loss_avoided(evidence.log_bayes_factor.abs() * 0.1)
        .confidence_interval(lower, upper);

    // Burst probability as evidence.
    builder = builder.evidence(
        "burst_prob",
        evidence.p_burst / (1.0 - evidence.p_burst + 1e-12),
    );

    // Likelihood ratio.
    if evidence.likelihood_steady > 0.0 {
        builder = builder.evidence(
            "likelihood_ratio",
            evidence.likelihood_burst / evidence.likelihood_steady.max(1e-12),
        );
    }

    // Run-length tail mass (high tail = long run, stable).
    if evidence.run_length_tail_mass > 0.0 {
        builder = builder.evidence("tail_mass", 1.0 / (evidence.run_length_tail_mass + 0.01));
    }

    builder.build()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unified_evidence::DecisionDomain;

    #[test]
    fn diff_strategy_bridge() {
        let evidence = ftui_render::diff_strategy::StrategyEvidence {
            strategy: ftui_render::diff_strategy::DiffStrategy::DirtyRows,
            cost_full: 1.0,
            cost_dirty: 0.5,
            cost_redraw: 2.0,
            posterior_mean: 0.05,
            posterior_variance: 0.001,
            alpha: 2.0,
            beta: 38.0,
            dirty_rows: 3,
            total_rows: 24,
            total_cells: 1920,
            guard_reason: "none",
            hysteresis_applied: false,
            hysteresis_ratio: 0.05,
        };

        let entry = from_diff_strategy(&evidence, 1_000_000);
        assert_eq!(entry.domain, DecisionDomain::DiffStrategy);
        assert_eq!(entry.action, "dirty_rows");
        assert!(
            entry.loss_avoided > 0.0,
            "chosen is cheapest, loss_avoided > 0"
        );
        assert!(entry.evidence_count() >= 2);
    }

    #[test]
    fn eprocess_bridge() {
        let decision = crate::eprocess_throttle::ThrottleDecision {
            should_recompute: true,
            wealth: 25.0,
            lambda: 0.3,
            empirical_rate: 0.4,
            forced_by_deadline: false,
            observations_since_recompute: 50,
        };

        let entry = from_eprocess(&decision, 2_000_000);
        assert_eq!(entry.domain, DecisionDomain::FrameBudget);
        assert_eq!(entry.action, "recompute");
        assert!(entry.log_posterior > 0.0, "wealth > 1 → positive log");
        assert!(entry.evidence_count() >= 2);
    }

    #[test]
    fn eprocess_bridge_forced() {
        let decision = crate::eprocess_throttle::ThrottleDecision {
            should_recompute: true,
            wealth: 0.5,
            lambda: 0.1,
            empirical_rate: 0.2,
            forced_by_deadline: true,
            observations_since_recompute: 200,
        };

        let entry = from_eprocess(&decision, 3_000_000);
        assert_eq!(entry.action, "recompute_forced");
    }

    #[test]
    fn voi_bridge() {
        let decision = crate::voi_sampling::VoiDecision {
            event_idx: 100,
            should_sample: true,
            forced_by_interval: false,
            blocked_by_min_interval: false,
            voi_gain: 0.05,
            score: 0.8,
            cost: 0.3,
            log_bayes_factor: 1.5,
            posterior_mean: 0.1,
            posterior_variance: 0.005,
            e_value: 5.0,
            e_threshold: 20.0,
            boundary_score: 0.7,
            events_since_sample: 30,
            time_since_sample_ms: 500.0,
            reason: "voi_ge_cost",
        };

        let entry = from_voi(&decision, 4_000_000);
        assert_eq!(entry.domain, DecisionDomain::VoiSampling);
        assert_eq!(entry.action, "voi_ge_cost");
        assert!(entry.evidence_count() >= 2);
    }

    #[test]
    fn conformal_bridge() {
        let prediction = crate::conformal_predictor::ConformalPrediction {
            upper_us: 18_000.0,
            risk: true,
            confidence: 0.95,
            bucket: crate::conformal_predictor::BucketKey {
                mode: crate::conformal_predictor::ModeBucket::AltScreen,
                diff: crate::conformal_predictor::DiffBucket::Full,
                size_bucket: 2,
            },
            sample_count: 50,
            quantile: 15_000.0,
            fallback_level: 0,
            window_size: 100,
            reset_count: 0,
            y_hat: 12_000.0,
            budget_us: 16_666.0,
        };

        let entry = from_conformal(&prediction, 5_000_000);
        assert_eq!(entry.domain, DecisionDomain::Degradation);
        assert_eq!(entry.action, "degrade");
        assert!(entry.log_posterior > 0.0, "over budget → positive log");
        assert!(entry.evidence_count() >= 2);
    }

    #[test]
    fn bocpd_bridge_burst() {
        let evidence = crate::bocpd::BocpdEvidence {
            p_burst: 0.85,
            log_bayes_factor: 2.3,
            observation_ms: 5.0,
            regime: crate::bocpd::BocpdRegime::Burst,
            likelihood_steady: 0.01,
            likelihood_burst: 0.5,
            expected_run_length: 3.0,
            run_length_variance: 2.0,
            run_length_mode: 2,
            run_length_p95: 8,
            run_length_tail_mass: 0.02,
            recommended_delay_ms: Some(50),
            hard_deadline_forced: None,
            observation_count: 100,
            timestamp: std::time::Instant::now(),
        };

        let entry = from_bocpd(&evidence, 6_000_000);
        assert_eq!(entry.domain, DecisionDomain::ResizeCoalescing);
        assert_eq!(entry.action, "coalesce");
        assert!(entry.log_posterior > 0.0, "high p_burst → positive log");
        assert!(entry.evidence_count() >= 2);
    }

    #[test]
    fn bocpd_bridge_steady() {
        let evidence = crate::bocpd::BocpdEvidence {
            p_burst: 0.1,
            log_bayes_factor: -1.5,
            observation_ms: 200.0,
            regime: crate::bocpd::BocpdRegime::Steady,
            likelihood_steady: 0.8,
            likelihood_burst: 0.01,
            expected_run_length: 50.0,
            run_length_variance: 10.0,
            run_length_mode: 48,
            run_length_p95: 65,
            run_length_tail_mass: 0.001,
            recommended_delay_ms: None,
            hard_deadline_forced: None,
            observation_count: 500,
            timestamp: std::time::Instant::now(),
        };

        let entry = from_bocpd(&evidence, 7_000_000);
        assert_eq!(entry.action, "apply");
        assert!(entry.log_posterior < 0.0, "low p_burst → negative log");
    }

    #[test]
    fn all_bridges_produce_valid_jsonl() {
        let diff = from_diff_strategy(
            &ftui_render::diff_strategy::StrategyEvidence {
                strategy: ftui_render::diff_strategy::DiffStrategy::Full,
                cost_full: 0.5,
                cost_dirty: 0.8,
                cost_redraw: 1.5,
                posterior_mean: 0.3,
                posterior_variance: 0.01,
                alpha: 5.0,
                beta: 12.0,
                dirty_rows: 10,
                total_rows: 24,
                total_cells: 1920,
                guard_reason: "none",
                hysteresis_applied: true,
                hysteresis_ratio: 0.05,
            },
            0,
        );

        let eproc = from_eprocess(
            &crate::eprocess_throttle::ThrottleDecision {
                should_recompute: false,
                wealth: 0.5,
                lambda: 0.1,
                empirical_rate: 0.2,
                forced_by_deadline: false,
                observations_since_recompute: 10,
            },
            1000,
        );

        let entries = [diff, eproc];
        for (i, entry) in entries.iter().enumerate() {
            let jsonl = entry.to_jsonl();
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&jsonl);
            assert!(
                parsed.is_ok(),
                "Bridge {} produced invalid JSONL: {}",
                i,
                &jsonl[..jsonl.len().min(100)]
            );
        }
    }
}

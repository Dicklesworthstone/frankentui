#![forbid(unsafe_code)]

//! Tail-risk and regime-shift monitors for optimization regressions
//! (bd-zzfhe).
//!
//! Optimization work that improves averages while harming p95/p99, or that
//! moves the system into a different operating regime, is the classic silent
//! failure mode of a performance program. This module makes both concerns
//! first-class rollout gates instead of footnotes in average-focused reports.
//!
//! # Monitors
//!
//! Given a baseline [`MetricSeries`] and a candidate [`MetricSeries`] for the
//! same (lane, metric), [`TailRegimeMonitor::evaluate`] runs:
//!
//! * **Tail gates** — nearest-rank p95 / p99 / max of the candidate compared
//!   against the baseline with separate warning and hard-gate margins. A
//!   median improvement never excuses a tail regression.
//! * **Envelope regime shift** — the fraction of candidate samples that fall
//!   outside the baseline's trusted envelope (baseline p5..p95). A shifted or
//!   bimodal candidate trips this even when individual quantile ratios look
//!   tolerable.
//! * **Sequential drift (CUSUM)** — a one-sided integer CUSUM over the
//!   candidate sample sequence against the baseline median. This catches
//!   within-run regime changes (first half healthy, second half degraded)
//!   that aggregate quantiles smear away.
//!
//! # Warning vs hard gate
//!
//! Every check has two thresholds. Crossing the *warning* margin produces a
//! [`Verdict::Warn`] finding: rollout may proceed, but only with review, and
//! the finding is preserved in the machine-readable report. Crossing the
//! *hard* margin produces [`Verdict::HardFail`]: the rollout gate must block.
//! [`MonitorReport::gate_action`] maps the worst finding to the action a
//! rollout pipeline consumes directly (`proceed` / `proceed_with_review` /
//! `block_rollout`).
//!
//! # Conservative fallback outside the trusted envelope
//!
//! When the baseline is too small to define a trusted envelope
//! (`min_baseline_samples`), the monitors refuse to certify anything: they
//! emit an `insufficient_baseline` **warning** with an explicit conservative
//! recommendation instead of silently passing. Absence of evidence is never
//! treated as evidence of health.
//!
//! # Determinism and replay
//!
//! All samples are integers (typically microseconds); quantiles are
//! nearest-rank; thresholds are permille integers; CUSUM accumulates in
//! `i128`. No floating point participates in any verdict, so a report is
//! byte-identical across runs and platforms — the same property the perf
//! evidence ledger ([`crate::perf_evidence_ledger`]) relies on. Ratios shown
//! to humans are derived from integer permille values.
//!
//! # Challenge fixtures and negative controls
//!
//! [`challenge_fixtures`] ships deterministic synthetic scenarios — including
//! the canonical *mean-masked tail regression* and a *mid-run drift* the
//! aggregate quantiles barely notice — plus a clean negative control.
//! [`run_self_test`] executes all of them and verifies each monitor fires
//! (or stays quiet) exactly as designed, so CI can prove the alerting works
//! before trusting it (bead AC #4).

use crate::validation_matrix::PerfLane;

/// Schema version for monitor reports.
pub const TAIL_REGIME_MONITOR_SCHEMA_VERSION: &str = "tail-regime-monitor-v1";

// ============================================================================
// Inputs
// ============================================================================

/// Unit of the metric samples, carried through to reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetricUnit {
    /// Microseconds (latency-style metrics).
    Micros,
    /// Bytes (size-style metrics).
    Bytes,
    /// Dimensionless counts.
    Count,
}

impl MetricUnit {
    /// Stable string for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Micros => "us",
            Self::Bytes => "bytes",
            Self::Count => "count",
        }
    }
}

/// A series of integer samples for one metric in one lane, for one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricSeries {
    /// Which performance lane produced the samples.
    pub lane: PerfLane,
    /// Metric name (stable, snake_case).
    pub metric: String,
    /// Sample unit.
    pub unit: MetricUnit,
    /// Raw samples in observation order (order matters for drift detection).
    pub samples: Vec<u64>,
}

impl MetricSeries {
    /// Convenience constructor.
    #[must_use]
    pub fn new(
        lane: PerfLane,
        metric: impl Into<String>,
        unit: MetricUnit,
        samples: Vec<u64>,
    ) -> Self {
        Self {
            lane,
            metric: metric.into(),
            unit,
            samples,
        }
    }

    /// Nearest-rank quantile at `permille` (0..=1000) over a sorted copy.
    /// Returns `None` for an empty series.
    #[must_use]
    pub fn quantile_permille(&self, permille: u64) -> Option<u64> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let n = sorted.len() as u64;
        let rank = (permille.min(1000) * n).div_ceil(1000).max(1);
        Some(sorted[(rank - 1) as usize])
    }

    fn summary(&self) -> SeriesSummary {
        SeriesSummary {
            n: self.samples.len(),
            p50: self.quantile_permille(500).unwrap_or(0),
            p95: self.quantile_permille(950).unwrap_or(0),
            p99: self.quantile_permille(990).unwrap_or(0),
            max: self.samples.iter().copied().max().unwrap_or(0),
        }
    }
}

/// Integer summary statistics for one series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeriesSummary {
    /// Sample count.
    pub n: usize,
    /// Nearest-rank median.
    pub p50: u64,
    /// Nearest-rank 95th percentile.
    pub p95: u64,
    /// Nearest-rank 99th percentile.
    pub p99: u64,
    /// Maximum observed sample.
    pub max: u64,
}

impl SeriesSummary {
    fn to_json(self) -> String {
        format!(
            "{{\"n\":{},\"p50\":{},\"p95\":{},\"p99\":{},\"max\":{}}}",
            self.n, self.p50, self.p95, self.p99, self.max
        )
    }
}

// ============================================================================
// Configuration
// ============================================================================

/// Warning / hard-gate margins for one tail quantile, in permille of the
/// baseline value (e.g. `warn_permille = 100` trips at +10%).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailMargin {
    /// Regression margin that produces a warning finding.
    pub warn_permille: u64,
    /// Regression margin that produces a hard-gate failure.
    pub hard_permille: u64,
}

/// Monitor configuration. All thresholds are integers; defaults are
/// deliberately conservative for TUI frame/latency style metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorConfig {
    /// Margins for the p95 tail gate.
    pub p95: TailMargin,
    /// Margins for the p99 tail gate.
    pub p99: TailMargin,
    /// Margins for the max-sample gate (noisier, so looser).
    pub max: TailMargin,
    /// Baseline envelope bounds (permille quantiles) defining the trusted
    /// operating regime.
    pub envelope_lo_permille: u64,
    /// Upper envelope bound (permille quantile).
    pub envelope_hi_permille: u64,
    /// Fraction (permille) of candidate samples outside the envelope that
    /// produces a warning.
    pub shift_warn_permille: u64,
    /// Fraction (permille) outside the envelope that hard-fails.
    pub shift_hard_permille: u64,
    /// CUSUM drift allowance as permille of the baseline median.
    pub drift_allowance_permille: u64,
    /// CUSUM hard threshold as permille of the baseline median (the warning
    /// threshold is half of this).
    pub drift_threshold_permille: u64,
    /// Minimum baseline samples required to trust the envelope at all.
    pub min_baseline_samples: usize,
    /// Minimum candidate samples required to evaluate.
    pub min_candidate_samples: usize,
    /// Baseline quantiles below this value are treated as noise: ratio gates
    /// use `max(baseline_q, noise_floor)` as denominator so a 1→2 unit jump
    /// on a microscopic metric cannot masquerade as a 100% regression.
    pub noise_floor: u64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            p95: TailMargin {
                warn_permille: 100,
                hard_permille: 250,
            },
            p99: TailMargin {
                warn_permille: 100,
                hard_permille: 250,
            },
            max: TailMargin {
                warn_permille: 300,
                hard_permille: 1000,
            },
            envelope_lo_permille: 50,
            envelope_hi_permille: 950,
            shift_warn_permille: 150,
            shift_hard_permille: 350,
            drift_allowance_permille: 50,
            drift_threshold_permille: 2000,
            min_baseline_samples: 20,
            min_candidate_samples: 20,
            noise_floor: 5,
        }
    }
}

// ============================================================================
// Findings and verdicts
// ============================================================================

/// Which monitor produced a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckKind {
    /// p95 tail-latency gate.
    TailP95,
    /// p99 tail-latency gate.
    TailP99,
    /// Max-sample gate.
    TailMax,
    /// Baseline-envelope regime-shift gate.
    EnvelopeShift,
    /// Sequential (CUSUM) drift gate.
    SequentialDrift,
    /// Conservative fallback: baseline/candidate too small to trust.
    InsufficientSamples,
}

impl CheckKind {
    /// Stable string for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TailP95 => "tail_p95",
            Self::TailP99 => "tail_p99",
            Self::TailMax => "tail_max",
            Self::EnvelopeShift => "envelope_shift",
            Self::SequentialDrift => "sequential_drift",
            Self::InsufficientSamples => "insufficient_samples",
        }
    }
}

/// Severity of a finding / overall report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verdict {
    /// Within all margins.
    Pass,
    /// Crossed a warning margin (or conservative fallback engaged).
    Warn,
    /// Crossed a hard-gate margin: rollout must block.
    HardFail,
}

impl Verdict {
    /// Stable string for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::HardFail => "hard_fail",
        }
    }
}

/// One monitor finding with its machine fields and human explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorFinding {
    /// Which monitor fired.
    pub check: CheckKind,
    /// Finding severity.
    pub verdict: Verdict,
    /// Stable snake_case reason code.
    pub reason_code: &'static str,
    /// Observed integer value (meaning depends on the check; documented in
    /// the explanation).
    pub observed: u64,
    /// Threshold the observation was compared against.
    pub threshold: u64,
    /// Human-readable explanation with the actual numbers — the
    /// threshold-explanation log the bead requires.
    pub explanation: String,
}

impl MonitorFinding {
    fn to_json(&self) -> String {
        format!(
            "{{\"check\":\"{}\",\"verdict\":\"{}\",\"reason_code\":\"{}\",\"observed\":{},\"threshold\":{},\"explanation\":\"{}\"}}",
            self.check.as_str(),
            self.verdict.as_str(),
            self.reason_code,
            self.observed,
            self.threshold,
            escape_json(&self.explanation),
        )
    }
}

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Format an integer permille delta as a human percentage (one decimal).
fn permille_as_pct(permille: u64) -> String {
    format!("{}.{}%", permille / 10, permille % 10)
}

/// The action a rollout pipeline should take for a verdict.
#[must_use]
pub const fn gate_action_for(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "proceed",
        Verdict::Warn => "proceed_with_review",
        Verdict::HardFail => "block_rollout",
    }
}

/// Full evaluation report for one (lane, metric) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorReport {
    /// Lane the metric belongs to.
    pub lane: PerfLane,
    /// Metric name.
    pub metric: String,
    /// Sample unit.
    pub unit: MetricUnit,
    /// Baseline summary statistics.
    pub baseline: SeriesSummary,
    /// Candidate summary statistics.
    pub candidate: SeriesSummary,
    /// All findings (including passes, so quiet checks are auditable).
    pub findings: Vec<MonitorFinding>,
    /// Worst finding severity.
    pub overall: Verdict,
}

impl MonitorReport {
    /// The rollout action implied by the worst finding.
    #[must_use]
    pub const fn gate_action(&self) -> &'static str {
        gate_action_for(self.overall)
    }

    /// Machine-readable JSON (single line, deterministic byte-for-byte).
    #[must_use]
    pub fn to_json(&self) -> String {
        let findings = self
            .findings
            .iter()
            .map(MonitorFinding::to_json)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema_version\":\"{}\",\"lane\":\"{}\",\"metric\":\"{}\",\"unit\":\"{}\",\"baseline\":{},\"candidate\":{},\"findings\":[{}],\"overall\":\"{}\",\"gate_action\":\"{}\"}}",
            TAIL_REGIME_MONITOR_SCHEMA_VERSION,
            self.lane.label(),
            escape_json(&self.metric),
            self.unit.as_str(),
            self.baseline.to_json(),
            self.candidate.to_json(),
            findings,
            self.overall.as_str(),
            self.gate_action(),
        )
    }

    /// Findings at or above `Warn`, for operators triaging a tripped gate.
    #[must_use]
    pub fn active_findings(&self) -> Vec<&MonitorFinding> {
        self.findings
            .iter()
            .filter(|f| f.verdict != Verdict::Pass)
            .collect()
    }
}

// ============================================================================
// The monitor
// ============================================================================

/// Tail-risk and regime-shift monitor.
#[derive(Debug, Clone, Default)]
pub struct TailRegimeMonitor {
    config: MonitorConfig,
}

impl TailRegimeMonitor {
    /// Build a monitor with an explicit configuration.
    #[must_use]
    pub const fn new(config: MonitorConfig) -> Self {
        Self { config }
    }

    /// Access the active configuration.
    #[must_use]
    pub const fn config(&self) -> &MonitorConfig {
        &self.config
    }

    /// Evaluate a candidate series against its baseline.
    ///
    /// # Panics
    ///
    /// Never panics: empty or undersized series route to the conservative
    /// `insufficient_samples` fallback.
    #[must_use]
    pub fn evaluate(&self, baseline: &MetricSeries, candidate: &MetricSeries) -> MonitorReport {
        let base_summary = baseline.summary();
        let cand_summary = candidate.summary();
        let mut findings = Vec::new();

        if base_summary.n < self.config.min_baseline_samples
            || cand_summary.n < self.config.min_candidate_samples
        {
            findings.push(MonitorFinding {
                check: CheckKind::InsufficientSamples,
                verdict: Verdict::Warn,
                reason_code: "insufficient_baseline",
                observed: base_summary.n.min(cand_summary.n) as u64,
                threshold: self.config.min_baseline_samples as u64,
                explanation: format!(
                    "only {} baseline / {} candidate samples (need {}/{}); the trusted \
                     envelope cannot be established — conservative fallback: do not certify, \
                     keep the prior revision and require manual review",
                    base_summary.n,
                    cand_summary.n,
                    self.config.min_baseline_samples,
                    self.config.min_candidate_samples,
                ),
            });
            let overall = worst_verdict(&findings);
            return MonitorReport {
                lane: candidate.lane,
                metric: candidate.metric.clone(),
                unit: candidate.unit,
                baseline: base_summary,
                candidate: cand_summary,
                findings,
                overall,
            };
        }

        findings.push(self.tail_finding(
            CheckKind::TailP95,
            "p95",
            base_summary.p95,
            cand_summary.p95,
            self.config.p95,
            candidate.unit,
        ));
        findings.push(self.tail_finding(
            CheckKind::TailP99,
            "p99",
            base_summary.p99,
            cand_summary.p99,
            self.config.p99,
            candidate.unit,
        ));
        findings.push(self.tail_finding(
            CheckKind::TailMax,
            "max",
            base_summary.max,
            cand_summary.max,
            self.config.max,
            candidate.unit,
        ));
        findings.push(self.envelope_finding(baseline, candidate));
        findings.push(self.drift_finding(base_summary.p50, candidate));

        let overall = worst_verdict(&findings);
        MonitorReport {
            lane: candidate.lane,
            metric: candidate.metric.clone(),
            unit: candidate.unit,
            baseline: base_summary,
            candidate: cand_summary,
            findings,
            overall,
        }
    }

    fn tail_finding(
        &self,
        check: CheckKind,
        label: &str,
        base_q: u64,
        cand_q: u64,
        margin: TailMargin,
        unit: MetricUnit,
    ) -> MonitorFinding {
        // Noise-floor the denominator so microscopic baselines cannot turn a
        // 1-unit jump into a giant ratio.
        let denom = base_q.max(self.config.noise_floor).max(1);
        // Regression delta in permille of the (floored) baseline.
        let delta_permille = if cand_q > base_q {
            ((u128::from(cand_q) - u128::from(base_q)) * 1000 / u128::from(denom)) as u64
        } else {
            0
        };
        let (verdict, reason_code, gate_permille) = if delta_permille > margin.hard_permille {
            (
                Verdict::HardFail,
                "tail_regression_hard",
                margin.hard_permille,
            )
        } else if delta_permille > margin.warn_permille {
            (Verdict::Warn, "tail_regression_warn", margin.warn_permille)
        } else {
            (Verdict::Pass, "within_thresholds", margin.warn_permille)
        };
        let explanation = match verdict {
            Verdict::Pass => format!(
                "candidate {label} {cand_q}{u} vs baseline {base_q}{u}: within +{} warning margin",
                permille_as_pct(margin.warn_permille),
                u = unit.as_str(),
            ),
            Verdict::Warn => format!(
                "candidate {label} {cand_q}{u} regressed {} over baseline {base_q}{u} \
                 (warning gate at +{}, hard gate at +{})",
                permille_as_pct(delta_permille),
                permille_as_pct(margin.warn_permille),
                permille_as_pct(margin.hard_permille),
                u = unit.as_str(),
            ),
            Verdict::HardFail => format!(
                "candidate {label} {cand_q}{u} regressed {} over baseline {base_q}{u}, \
                 past the hard gate at +{} — a median improvement cannot excuse this",
                permille_as_pct(delta_permille),
                permille_as_pct(margin.hard_permille),
                u = unit.as_str(),
            ),
        };
        MonitorFinding {
            check,
            verdict,
            reason_code,
            observed: delta_permille,
            threshold: gate_permille,
            explanation,
        }
    }

    fn envelope_finding(
        &self,
        baseline: &MetricSeries,
        candidate: &MetricSeries,
    ) -> MonitorFinding {
        let lo = baseline
            .quantile_permille(self.config.envelope_lo_permille)
            .unwrap_or(0);
        let hi = baseline
            .quantile_permille(self.config.envelope_hi_permille)
            .unwrap_or(u64::MAX);
        // Widen the upper bound by the noise floor so a flat baseline (all
        // samples identical) does not flag ordinary 1-unit jitter as a shift.
        let hi = hi.saturating_add(self.config.noise_floor);
        let lo = lo.saturating_sub(self.config.noise_floor);
        let n = candidate.samples.len() as u64;
        let outside_above = candidate.samples.iter().filter(|&&s| s > hi).count() as u64;
        let outside_below = candidate.samples.iter().filter(|&&s| s < lo).count() as u64;
        let outside_permille = (outside_above + outside_below) * 1000 / n.max(1);

        let (verdict, reason_code, threshold) =
            if outside_permille > self.config.shift_hard_permille {
                (
                    Verdict::HardFail,
                    "regime_shift_hard",
                    self.config.shift_hard_permille,
                )
            } else if outside_permille > self.config.shift_warn_permille {
                (
                    Verdict::Warn,
                    "regime_shift_warn",
                    self.config.shift_warn_permille,
                )
            } else {
                (
                    Verdict::Pass,
                    "within_trusted_envelope",
                    self.config.shift_warn_permille,
                )
            };
        let explanation = if verdict == Verdict::Pass {
            format!(
                "{} of candidate samples left the trusted baseline envelope \
                 [{lo}..{hi}]{u} (warning gate at {})",
                permille_as_pct(outside_permille),
                permille_as_pct(self.config.shift_warn_permille),
                u = candidate.unit.as_str(),
            )
        } else {
            format!(
                "{} of candidate samples left the trusted baseline envelope [{lo}..{hi}]{u} \
                 ({} above, {} below of {n}); the system is operating outside the regime the \
                 baseline certified — conservative action: treat prior tuning as untrusted",
                permille_as_pct(outside_permille),
                outside_above,
                outside_below,
                u = candidate.unit.as_str(),
            )
        };
        MonitorFinding {
            check: CheckKind::EnvelopeShift,
            verdict,
            reason_code,
            observed: outside_permille,
            threshold,
            explanation,
        }
    }

    fn drift_finding(&self, baseline_median: u64, candidate: &MetricSeries) -> MonitorFinding {
        let m = i128::from(baseline_median);
        let allowance = i128::from(
            (u128::from(baseline_median) * u128::from(self.config.drift_allowance_permille) / 1000)
                .max(1) as u64,
        );
        let hard = i128::from(
            (u128::from(baseline_median.max(self.config.noise_floor).max(1))
                * u128::from(self.config.drift_threshold_permille)
                / 1000)
                .max(1) as u64,
        );
        let warn = (hard / 2).max(1);

        let mut s: i128 = 0;
        let mut peak: i128 = 0;
        let mut peak_index: usize = 0;
        for (idx, &sample) in candidate.samples.iter().enumerate() {
            s = (s + i128::from(sample) - m - allowance).max(0);
            if s > peak {
                peak = s;
                peak_index = idx;
            }
        }

        let observed = u64::try_from(peak).unwrap_or(u64::MAX);
        let (verdict, reason_code, threshold) = if peak > hard {
            (
                Verdict::HardFail,
                "sequential_drift_hard",
                u64::try_from(hard).unwrap_or(u64::MAX),
            )
        } else if peak > warn {
            (
                Verdict::Warn,
                "sequential_drift_warn",
                u64::try_from(warn).unwrap_or(u64::MAX),
            )
        } else {
            (
                Verdict::Pass,
                "no_sequential_drift",
                u64::try_from(warn).unwrap_or(u64::MAX),
            )
        };
        let explanation = if verdict == Verdict::Pass {
            format!(
                "CUSUM peak {observed}{u} against baseline median {baseline_median}{u} stayed \
                 under the warning threshold {threshold}{u}",
                u = candidate.unit.as_str(),
            )
        } else {
            format!(
                "CUSUM over the candidate sequence peaked at {observed}{u} (sample #{peak_index}) \
                 against baseline median {baseline_median}{u} with allowance {allowance}{u}; the \
                 run drifted into a different regime mid-stream even if aggregate quantiles look \
                 acceptable (warning at {warn}{u}, hard gate at {hard}{u})",
                u = candidate.unit.as_str(),
            )
        };
        MonitorFinding {
            check: CheckKind::SequentialDrift,
            verdict,
            reason_code,
            observed,
            threshold,
            explanation,
        }
    }
}

fn worst_verdict(findings: &[MonitorFinding]) -> Verdict {
    findings
        .iter()
        .map(|f| f.verdict)
        .max()
        .unwrap_or(Verdict::Pass)
}

// ============================================================================
// Challenge fixtures + monitor self-test (negative controls)
// ============================================================================

/// A deterministic synthetic scenario with the monitor behavior it must
/// produce.
#[derive(Debug, Clone)]
pub struct ChallengeFixture {
    /// Stable fixture name.
    pub name: &'static str,
    /// What the fixture demonstrates.
    pub description: &'static str,
    /// Baseline series.
    pub baseline: MetricSeries,
    /// Candidate series.
    pub candidate: MetricSeries,
    /// Expected overall verdict.
    pub expected_overall: Verdict,
    /// Checks that must fire at `Warn` or above.
    pub expected_active_checks: Vec<CheckKind>,
}

fn series(samples: Vec<u64>) -> MetricSeries {
    MetricSeries::new(
        PerfLane::Render,
        "frame_time_us",
        MetricUnit::Micros,
        samples,
    )
}

/// Deterministic challenge fixtures covering the failure modes the bead
/// names, plus a clean negative control.
#[must_use]
pub fn challenge_fixtures() -> Vec<ChallengeFixture> {
    // Healthy baseline: 60 samples, deterministic mild spread 100..=109.
    let healthy: Vec<u64> = (0..60).map(|i| 100 + (i % 10)).collect();

    // Clean candidate: same generator, different phase — must stay quiet.
    let clean: Vec<u64> = (0..60).map(|i| 100 + ((i + 3) % 10)).collect();

    // Tail regression: median identical, top ~8% inflated well past the
    // +25% hard gate (5 of 60 samples at 150, so nearest-rank p95 and p99
    // both land on 150 while the median stays at the healthy value).
    let tail_regression: Vec<u64> = (0..60)
        .map(|i| if i >= 55 { 150 } else { 100 + (i % 10) })
        .collect();

    // Mean-masked tail regression: median IMPROVES (100 -> 90) while the
    // p99/max blow out — the classic failure mode averages hide.
    let mean_masked: Vec<u64> = (0..60)
        .map(|i| if i >= 58 { 220 } else { 90 + (i % 5) })
        .collect();

    // Regime shift: every sample uniformly +40% — individual samples are not
    // extreme outliers relative to each other, but the whole distribution
    // left the trusted envelope.
    let regime_shift: Vec<u64> = (0..60).map(|i| 140 + (i % 10)).collect();

    // Mid-run drift: first half healthy, second half +18%. Aggregate p95
    // lands near the warning margin, but the SEQUENCE shows a regime change
    // the CUSUM must catch hard.
    let mid_run_drift: Vec<u64> = (0..60)
        .map(|i| {
            if i < 30 {
                100 + (i % 10)
            } else {
                118 + (i % 5)
            }
        })
        .collect();

    // Insufficient baseline: five samples cannot define a trusted envelope.
    let tiny_baseline: Vec<u64> = vec![100, 101, 102, 103, 104];

    vec![
        ChallengeFixture {
            name: "clean_negative_control",
            description: "healthy candidate from the same regime must stay quiet",
            baseline: series(healthy.clone()),
            candidate: series(clean),
            expected_overall: Verdict::Pass,
            expected_active_checks: vec![],
        },
        ChallengeFixture {
            name: "tail_regression_hard",
            description: "median unchanged, p95/p99 inflated past the hard gate",
            baseline: series(healthy.clone()),
            candidate: series(tail_regression),
            expected_overall: Verdict::HardFail,
            expected_active_checks: vec![CheckKind::TailP95, CheckKind::TailP99],
        },
        ChallengeFixture {
            name: "mean_masked_tail_regression",
            description: "median improves while the tail blows out — averages lie",
            baseline: series(healthy.clone()),
            candidate: series(mean_masked),
            expected_overall: Verdict::HardFail,
            expected_active_checks: vec![CheckKind::TailP99],
        },
        ChallengeFixture {
            name: "uniform_regime_shift",
            description: "whole distribution moved outside the trusted envelope",
            baseline: series(healthy.clone()),
            candidate: series(regime_shift),
            expected_overall: Verdict::HardFail,
            expected_active_checks: vec![CheckKind::EnvelopeShift],
        },
        ChallengeFixture {
            name: "mid_run_drift",
            description: "second half of the run drifts into a hotter regime",
            baseline: series(healthy.clone()),
            candidate: series(mid_run_drift),
            expected_overall: Verdict::HardFail,
            expected_active_checks: vec![CheckKind::SequentialDrift],
        },
        ChallengeFixture {
            name: "insufficient_baseline_fallback",
            description: "tiny baseline must refuse to certify, not silently pass",
            baseline: series(tiny_baseline),
            candidate: series(healthy),
            expected_overall: Verdict::Warn,
            expected_active_checks: vec![CheckKind::InsufficientSamples],
        },
    ]
}

/// Result of one self-test case.
#[derive(Debug, Clone)]
pub struct SelfTestCase {
    /// Fixture name.
    pub name: &'static str,
    /// Expected overall verdict.
    pub expected: Verdict,
    /// Observed overall verdict.
    pub observed: Verdict,
    /// Checks that fired at `Warn` or above.
    pub active_checks: Vec<CheckKind>,
    /// Whether expectation held (verdict matches and every expected check fired).
    pub passed: bool,
    /// Full report for artifact capture.
    pub report: MonitorReport,
}

/// Aggregate self-test result.
#[derive(Debug, Clone)]
pub struct SelfTestReport {
    /// Per-fixture outcomes.
    pub cases: Vec<SelfTestCase>,
}

impl SelfTestReport {
    /// True when every fixture behaved exactly as designed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.cases.iter().all(|case| case.passed)
    }

    /// Machine-readable JSON summary.
    #[must_use]
    pub fn to_json(&self) -> String {
        let cases = self
            .cases
            .iter()
            .map(|case| {
                let checks = case
                    .active_checks
                    .iter()
                    .map(|c| format!("\"{}\"", c.as_str()))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "{{\"name\":\"{}\",\"expected\":\"{}\",\"observed\":\"{}\",\"active_checks\":[{}],\"passed\":{}}}",
                    case.name,
                    case.expected.as_str(),
                    case.observed.as_str(),
                    checks,
                    case.passed,
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema_version\":\"{}\",\"self_test\":\"tail_regime_monitor\",\"passed\":{},\"cases\":[{}]}}",
            TAIL_REGIME_MONITOR_SCHEMA_VERSION,
            self.passed(),
            cases,
        )
    }
}

/// Run every challenge fixture through a default-configured monitor and
/// verify the expected alerting behavior. CI runs this before trusting the
/// monitors: a gate that cannot fire is worse than no gate.
#[must_use]
pub fn run_self_test() -> SelfTestReport {
    let monitor = TailRegimeMonitor::default();
    let cases = challenge_fixtures()
        .into_iter()
        .map(|fixture| {
            let report = monitor.evaluate(&fixture.baseline, &fixture.candidate);
            let active_checks: Vec<CheckKind> =
                report.active_findings().iter().map(|f| f.check).collect();
            let verdict_ok = report.overall == fixture.expected_overall;
            let checks_ok = fixture
                .expected_active_checks
                .iter()
                .all(|check| active_checks.contains(check));
            let clean_ok = fixture.expected_overall != Verdict::Pass || active_checks.is_empty();
            SelfTestCase {
                name: fixture.name,
                expected: fixture.expected_overall,
                observed: report.overall,
                active_checks,
                passed: verdict_ok && checks_ok && clean_ok,
                report,
            }
        })
        .collect();
    SelfTestReport { cases }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> TailRegimeMonitor {
        TailRegimeMonitor::default()
    }

    #[test]
    fn nearest_rank_quantiles_are_exact_on_known_series() {
        let s = series((1..=100).collect());
        assert_eq!(s.quantile_permille(500), Some(50));
        assert_eq!(s.quantile_permille(950), Some(95));
        assert_eq!(s.quantile_permille(990), Some(99));
        assert_eq!(s.quantile_permille(1000), Some(100));
    }

    #[test]
    fn empty_series_has_no_quantiles_and_routes_to_fallback() {
        let empty = series(vec![]);
        assert_eq!(empty.quantile_permille(500), None);
        let report = monitor().evaluate(&empty, &empty);
        assert_eq!(report.overall, Verdict::Warn);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].check, CheckKind::InsufficientSamples);
    }

    #[test]
    fn clean_candidate_passes_every_check() {
        let base = series((0..40).map(|i| 100 + (i % 10)).collect());
        let cand = series((0..40).map(|i| 100 + ((i + 1) % 10)).collect());
        let report = monitor().evaluate(&base, &cand);
        assert_eq!(report.overall, Verdict::Pass);
        assert!(report.active_findings().is_empty());
        assert_eq!(report.gate_action(), "proceed");
        // Quiet checks are still recorded for audit.
        assert_eq!(report.findings.len(), 5);
    }

    #[test]
    fn warning_band_produces_warn_not_hard() {
        let base = series(vec![100; 40]);
        // p95 +15%: past the +10% warning margin, under the +25% hard gate.
        // 4 of 40 elevated samples put nearest-rank p95 (rank 38) on 115.
        let cand = series((0..40).map(|i| if i >= 36 { 115 } else { 100 }).collect());
        let report = monitor().evaluate(&base, &cand);
        let p95 = report
            .findings
            .iter()
            .find(|f| f.check == CheckKind::TailP95)
            .expect("p95 finding");
        assert_eq!(p95.verdict, Verdict::Warn);
        assert_eq!(report.overall, Verdict::Warn);
        assert_eq!(report.gate_action(), "proceed_with_review");
        assert!(p95.explanation.contains("warning gate"));
    }

    #[test]
    fn hard_gate_blocks_rollout_with_explained_threshold() {
        let base = series(vec![100; 40]);
        // 4 of 40 elevated samples put nearest-rank p95 (rank 38) on 140.
        let cand = series((0..40).map(|i| if i >= 36 { 140 } else { 100 }).collect());
        let report = monitor().evaluate(&base, &cand);
        assert_eq!(report.overall, Verdict::HardFail);
        assert_eq!(report.gate_action(), "block_rollout");
        let p95 = report
            .findings
            .iter()
            .find(|f| f.check == CheckKind::TailP95)
            .expect("p95 finding");
        assert_eq!(p95.verdict, Verdict::HardFail);
        assert!(p95.explanation.contains("hard gate"));
        assert!(p95.explanation.contains("140us"));
    }

    #[test]
    fn median_improvement_cannot_excuse_tail_blowout() {
        let base = series((0..60).map(|i| 100 + (i % 10)).collect());
        let cand = series(
            (0..60)
                .map(|i| if i >= 58 { 220 } else { 90 + (i % 5) })
                .collect(),
        );
        let report = monitor().evaluate(&base, &cand);
        assert!(
            report.candidate.p50 < report.baseline.p50,
            "median improved"
        );
        assert_eq!(report.overall, Verdict::HardFail, "tail still gates");
    }

    #[test]
    fn uniform_shift_trips_envelope_even_without_extreme_outliers() {
        let base = series((0..60).map(|i| 100 + (i % 10)).collect());
        let cand = series((0..60).map(|i| 140 + (i % 10)).collect());
        let report = monitor().evaluate(&base, &cand);
        let shift = report
            .findings
            .iter()
            .find(|f| f.check == CheckKind::EnvelopeShift)
            .expect("envelope finding");
        assert_eq!(shift.verdict, Verdict::HardFail);
        assert!(shift.explanation.contains("outside the regime"));
    }

    #[test]
    fn cusum_catches_mid_run_drift() {
        let base = series((0..60).map(|i| 100 + (i % 10)).collect());
        let cand = series(
            (0..60)
                .map(|i| {
                    if i < 30 {
                        100 + (i % 10)
                    } else {
                        118 + (i % 5)
                    }
                })
                .collect(),
        );
        let report = monitor().evaluate(&base, &cand);
        let drift = report
            .findings
            .iter()
            .find(|f| f.check == CheckKind::SequentialDrift)
            .expect("drift finding");
        assert_eq!(drift.verdict, Verdict::HardFail);
        assert!(drift.explanation.contains("mid-stream"));
    }

    #[test]
    fn drift_check_stays_quiet_on_reordered_healthy_samples() {
        let base = series((0..60).map(|i| 100 + (i % 10)).collect());
        // Same values as the baseline generator, reversed order: no drift.
        let mut reordered: Vec<u64> = (0..60).map(|i| 100 + (i % 10)).collect();
        reordered.reverse();
        let report = monitor().evaluate(&base, &series(reordered));
        let drift = report
            .findings
            .iter()
            .find(|f| f.check == CheckKind::SequentialDrift)
            .expect("drift finding");
        assert_eq!(drift.verdict, Verdict::Pass);
    }

    #[test]
    fn noise_floor_suppresses_microscopic_ratio_explosions() {
        let base = series(vec![1; 40]);
        let cand = series(vec![2; 40]);
        let report = monitor().evaluate(&base, &cand);
        // 1 -> 2 is +100% raw, but the noise floor (5) caps the permille
        // delta at 200 (+20%): warning band, never a hard gate.
        let p95 = report
            .findings
            .iter()
            .find(|f| f.check == CheckKind::TailP95)
            .expect("p95 finding");
        assert_ne!(p95.verdict, Verdict::HardFail);
    }

    #[test]
    fn insufficient_baseline_is_a_warning_with_conservative_language() {
        let base = series(vec![100, 101, 102]);
        let cand = series((0..40).map(|i| 100 + (i % 10)).collect());
        let report = monitor().evaluate(&base, &cand);
        assert_eq!(report.overall, Verdict::Warn);
        assert_eq!(report.findings.len(), 1);
        let finding = &report.findings[0];
        assert_eq!(finding.reason_code, "insufficient_baseline");
        assert!(finding.explanation.contains("conservative fallback"));
        assert_eq!(report.gate_action(), "proceed_with_review");
    }

    #[test]
    fn report_json_is_deterministic_and_parseable() {
        let base = series((0..40).map(|i| 100 + (i % 10)).collect());
        let cand = series((0..40).map(|i| if i >= 38 { 150 } else { 100 }).collect());
        let m = monitor();
        let a = m.evaluate(&base, &cand).to_json();
        let b = m.evaluate(&base, &cand).to_json();
        assert_eq!(a, b, "reports must be byte-identical across runs");
        let parsed: serde_json::Value = serde_json::from_str(&a).expect("report JSON parses");
        assert_eq!(
            parsed["schema_version"].as_str(),
            Some(TAIL_REGIME_MONITOR_SCHEMA_VERSION)
        );
        assert_eq!(parsed["lane"].as_str(), Some("render"));
        assert!(parsed["findings"].as_array().is_some_and(|f| !f.is_empty()));
        assert!(parsed["gate_action"].as_str().is_some());
    }

    #[test]
    fn self_test_proves_alerting_fires_and_stays_quiet() {
        let report = run_self_test();
        for case in &report.cases {
            assert!(
                case.passed,
                "self-test case {} expected {:?} got {:?} (active: {:?})",
                case.name, case.expected, case.observed, case.active_checks
            );
        }
        assert!(report.passed());
        let parsed: serde_json::Value =
            serde_json::from_str(&report.to_json()).expect("self-test JSON parses");
        assert_eq!(parsed["passed"].as_bool(), Some(true));
    }

    #[test]
    fn vocabulary_strings_are_stable() {
        assert_eq!(CheckKind::TailP99.as_str(), "tail_p99");
        assert_eq!(CheckKind::EnvelopeShift.as_str(), "envelope_shift");
        assert_eq!(CheckKind::SequentialDrift.as_str(), "sequential_drift");
        assert_eq!(Verdict::HardFail.as_str(), "hard_fail");
        assert_eq!(gate_action_for(Verdict::HardFail), "block_rollout");
        assert_eq!(gate_action_for(Verdict::Warn), "proceed_with_review");
        assert_eq!(gate_action_for(Verdict::Pass), "proceed");
    }
}

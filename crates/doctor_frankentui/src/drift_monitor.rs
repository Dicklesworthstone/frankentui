//! BOCPD + CUSUM drift monitors for migration quality metrics.
//!
//! Migration success rate, confidence, and performance are tracked over time.
//! This module flags two kinds of degradation:
//!
//! * **CUSUM** (Cumulative Sum control chart) catches sustained shifts quickly.
//!   Observations are standardized against a baseline window and accumulated:
//!
//!   ```text
//!   z_t   = (x_t - mu0) / sigma
//!   S_up  = max(0, S_up + z_t - k)     // upward shift
//!   S_dn  = max(0, S_dn - z_t - k)     // downward shift
//!   alert when S_up >= h or S_dn >= h  (k = allowance, h = threshold)
//!   ```
//!
//! * **BOCPD** (Bayesian Online Change-Point Detection) catches regime changes.
//!   A run-length posterior is propagated with a Gaussian predictive and a
//!   constant hazard `H = 1/hazard_lambda`. Because `P(r_t = 0 | x_{1:t})`
//!   equals `H` exactly under a constant hazard, the changepoint *signal* is the
//!   collapse of the run-length posterior onto short runs:
//!
//!   ```text
//!   recent_mass_t = sum_{r=0..recent_window} P(r_t = r)
//!   changepoint when recent_mass_t crosses changepoint_threshold (rising edge)
//!   ```
//!
//! Every drift event is interpretable (regression vs improvement, with a
//! confidence score in `[0, 1]`) and the report exposes a triage summary plus a
//! deterministic JSON-stats artifact for nightly evaluation pipelines. False
//! positive rates are controlled with [`DriftMonitor::calibrate`], which scores
//! the detectors against drift-free calibration fixtures. Metric collection
//! stays outside the library boundary.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version for drift-monitor reports.
pub const DRIFT_MONITOR_SCHEMA_VERSION: &str = "drift-monitor-v1";

const EPS: f64 = 1e-9;

/// Whether a higher or lower metric value is the desirable direction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MetricDirection {
    /// Larger is better (e.g. success rate, confidence).
    #[default]
    HigherIsBetter,
    /// Smaller is better (e.g. latency, allocation bytes).
    LowerIsBetter,
}

/// A time-ordered series of observations for one migration quality metric.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftMetricSeries {
    pub metric_id: String,
    pub direction: MetricDirection,
    pub observations: Vec<f64>,
}

impl DriftMetricSeries {
    #[must_use]
    pub fn new(metric_id: impl Into<String>, direction: MetricDirection) -> Self {
        Self {
            metric_id: metric_id.into(),
            direction,
            observations: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_observations(mut self, observations: Vec<f64>) -> Self {
        self.observations = observations
            .into_iter()
            .map(|value| if value.is_finite() { value } else { 0.0 })
            .collect();
        self
    }
}

/// CUSUM control-chart parameters (in standardized units).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CusumConfig {
    /// Allowance `k` (slack), typically half the target shift in sigma units.
    pub allowance_k: f64,
    /// Decision threshold `h`; larger means fewer false alarms, slower detection.
    pub threshold_h: f64,
}

impl Default for CusumConfig {
    fn default() -> Self {
        Self {
            allowance_k: 0.5,
            threshold_h: 5.0,
        }
    }
}

/// BOCPD parameters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BocpdConfig {
    /// Expected run length between changepoints (hazard = 1/hazard_lambda).
    pub hazard_lambda: f64,
    /// Run-length truncation K for O(K) updates.
    pub max_run_length: usize,
    /// Window of "short" run lengths summed for the collapse signal.
    pub recent_window: usize,
    /// Rising-edge threshold on the recent-mass collapse signal.
    pub changepoint_threshold: f64,
    /// Floor on the Gaussian predictive sigma.
    pub min_sigma: f64,
    /// Width multiplier for the fresh-run (`counts == 0`) prior predictive.
    ///
    /// A fresh run has no data, so its predictive must be wide enough to also
    /// accept a *shifted* mean; otherwise the model can never re-acquire a new
    /// regime after a changepoint. The effective prior sigma is
    /// `max(sigma * prior_predictive_width, overall_series_std)`.
    pub prior_predictive_width: f64,
}

impl Default for BocpdConfig {
    fn default() -> Self {
        Self {
            hazard_lambda: 50.0,
            max_run_length: 100,
            recent_window: 3,
            changepoint_threshold: 0.5,
            min_sigma: 1e-3,
            prior_predictive_width: 4.0,
        }
    }
}

/// Drift-monitor configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct DriftMonitorConfig {
    /// Number of leading observations used to estimate baseline mean/sigma.
    pub baseline_window: usize,
    pub cusum: CusumConfig,
    pub bocpd: BocpdConfig,
}

impl Default for DriftMonitorConfig {
    fn default() -> Self {
        Self {
            baseline_window: 8,
            cusum: CusumConfig::default(),
            bocpd: BocpdConfig::default(),
        }
    }
}

impl DriftMonitorConfig {
    #[must_use]
    pub fn with_baseline_window(mut self, baseline_window: usize) -> Self {
        self.baseline_window = baseline_window.max(1);
        self
    }

    #[must_use]
    pub fn with_cusum(mut self, cusum: CusumConfig) -> Self {
        self.cusum = cusum;
        self
    }

    #[must_use]
    pub fn with_bocpd(mut self, bocpd: BocpdConfig) -> Self {
        self.bocpd = bocpd;
        self
    }

    fn fingerprint(&self) -> String {
        stable_hash(self)
    }
}

/// Which detector fired.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DriftDetector {
    CusumUpward,
    CusumDownward,
    BocpdChangepoint,
}

impl DriftDetector {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CusumUpward => "cusum_upward",
            Self::CusumDownward => "cusum_downward",
            Self::BocpdChangepoint => "bocpd_changepoint",
        }
    }
}

/// Whether the drift made the metric better or worse for its direction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    Regression,
    Improvement,
    Neutral,
}

impl DriftKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regression => "regression",
            Self::Improvement => "improvement",
            Self::Neutral => "neutral",
        }
    }
}

/// One interpretable drift event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftEvent {
    pub metric_id: String,
    pub observation_index: u32,
    pub detector: DriftDetector,
    pub kind: DriftKind,
    pub value: f64,
    pub baseline_mean: f64,
    pub baseline_std: f64,
    pub statistic: f64,
    pub confidence: f64,
    pub description: String,
}

/// Per-metric rollup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftMetricSummary {
    pub metric_id: String,
    pub direction: MetricDirection,
    pub observation_count: usize,
    pub baseline_mean: f64,
    pub baseline_std: f64,
    pub final_value: f64,
    pub event_count: usize,
    pub regression_count: usize,
    pub improvement_count: usize,
    pub max_confidence: f64,
    pub expected_run_length_final: f64,
}

/// Compact triage view for nightly evaluation / incident reports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftTriageSummary {
    pub total_events: usize,
    pub regression_count: usize,
    pub improvement_count: usize,
    pub metrics_with_regression: Vec<String>,
    pub top_events: Vec<DriftEvent>,
}

/// Exported JSON stats artifact (deterministic content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriftJsonStatsArtifact {
    pub path: String,
    pub sha256: String,
    pub content: String,
}

/// Full drift-monitor report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftMonitorReport {
    pub schema_version: String,
    pub monitor_id: String,
    pub config: DriftMonitorConfig,
    pub summaries: Vec<DriftMetricSummary>,
    pub events: Vec<DriftEvent>,
    pub triage: DriftTriageSummary,
    pub exported_json_stats: DriftJsonStatsArtifact,
    pub replay_command: String,
}

impl DriftMonitorReport {
    /// Metric identifiers that registered at least one regression event.
    #[must_use]
    pub fn regressed_metric_ids(&self) -> Vec<String> {
        self.triage.metrics_with_regression.clone()
    }
}

/// Calibration result over drift-free fixtures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftCalibrationReport {
    pub schema_version: String,
    pub calibration_id: String,
    pub config: DriftMonitorConfig,
    pub evaluated_series: usize,
    pub total_observations: usize,
    pub false_positive_events: usize,
    pub false_positive_rate: f64,
    pub target_false_positive_rate: f64,
    pub meets_target: bool,
    pub recommended_cusum_threshold: f64,
    pub recommended_changepoint_threshold: f64,
}

/// Drift monitor combining CUSUM and BOCPD detectors.
#[derive(Debug, Clone, Default)]
pub struct DriftMonitor {
    config: DriftMonitorConfig,
}

impl DriftMonitor {
    #[must_use]
    pub fn new(config: DriftMonitorConfig) -> Self {
        Self { config }
    }

    /// Analyze `series` and produce a deterministic drift report.
    #[must_use]
    pub fn analyze(&self, series: Vec<DriftMetricSeries>) -> DriftMonitorReport {
        let mut series = series;
        series.sort_by(|left, right| left.metric_id.cmp(&right.metric_id));

        let mut summaries = Vec::new();
        let mut events = Vec::new();
        for metric in &series {
            let outcome = self.analyze_metric(metric);
            summaries.push(outcome.summary);
            events.extend(outcome.events);
        }
        sort_events(&mut events);

        let triage = build_triage(&events);
        let monitor_id = monitor_id_for(self.config, &series, &events);
        let replay_command = format!(
            "doctor_frankentui drift-monitor --monitor-id {monitor_id} --baseline-window {}",
            self.config.baseline_window
        );
        let exported_json_stats =
            export_json_stats(&monitor_id, self.config, &summaries, &events, &triage);

        DriftMonitorReport {
            schema_version: DRIFT_MONITOR_SCHEMA_VERSION.to_string(),
            monitor_id,
            config: self.config,
            summaries,
            events,
            triage,
            exported_json_stats,
            replay_command,
        }
    }

    /// Score the detectors against drift-free fixtures to bound false positives.
    ///
    /// Every event raised on a stable series is a false positive; the reported
    /// rate is `false_positive_events / total_observations`. Recommended
    /// thresholds clear the largest spurious statistic observed.
    #[must_use]
    pub fn calibrate(
        &self,
        stable_series: Vec<DriftMetricSeries>,
        target_false_positive_rate: f64,
    ) -> DriftCalibrationReport {
        let mut stable_series = stable_series;
        stable_series.sort_by(|left, right| left.metric_id.cmp(&right.metric_id));

        let total_observations = stable_series
            .iter()
            .map(|metric| metric.observations.len())
            .sum::<usize>();

        let mut false_positive_events = 0usize;
        let mut max_cusum_statistic = self.config.cusum.threshold_h;
        let mut max_changepoint_confidence = self.config.bocpd.changepoint_threshold;
        for metric in &stable_series {
            let outcome = self.analyze_metric(metric);
            for event in &outcome.events {
                false_positive_events += 1;
                match event.detector {
                    DriftDetector::CusumUpward | DriftDetector::CusumDownward => {
                        max_cusum_statistic = max_cusum_statistic.max(event.statistic);
                    }
                    DriftDetector::BocpdChangepoint => {
                        max_changepoint_confidence =
                            max_changepoint_confidence.max(event.confidence);
                    }
                }
            }
        }

        let target = target_false_positive_rate.clamp(0.0, 1.0);
        let false_positive_rate = safe_div(
            usize_to_f64(false_positive_events),
            usize_to_f64(total_observations),
        );
        let calibration_id = calibration_id_for(self.config, &stable_series, false_positive_events);

        DriftCalibrationReport {
            schema_version: DRIFT_MONITOR_SCHEMA_VERSION.to_string(),
            calibration_id,
            config: self.config,
            evaluated_series: stable_series.len(),
            total_observations,
            false_positive_events,
            false_positive_rate,
            target_false_positive_rate: target,
            meets_target: false_positive_rate <= target + EPS,
            // Nudge above the largest spurious statistic so a re-run is clean.
            recommended_cusum_threshold: max_cusum_statistic + 0.5,
            recommended_changepoint_threshold: (max_changepoint_confidence + 0.05).min(0.999),
        }
    }

    fn analyze_metric(&self, metric: &DriftMetricSeries) -> MetricOutcome {
        let baseline = baseline_stats(
            &metric.observations,
            self.config.baseline_window,
            self.config.bocpd.min_sigma,
        );
        let mut events = Vec::new();
        let mut max_confidence = 0.0f64;
        let mut regression_count = 0usize;
        let mut improvement_count = 0usize;

        self.run_cusum(metric, &baseline, &mut events);
        let expected_run_length_final = self.run_bocpd(metric, &baseline, &mut events);

        for event in &events {
            max_confidence = max_confidence.max(event.confidence);
            match event.kind {
                DriftKind::Regression => regression_count += 1,
                DriftKind::Improvement => improvement_count += 1,
                DriftKind::Neutral => {}
            }
        }

        let summary = DriftMetricSummary {
            metric_id: metric.metric_id.clone(),
            direction: metric.direction,
            observation_count: metric.observations.len(),
            baseline_mean: baseline.mean,
            baseline_std: baseline.std,
            final_value: metric.observations.last().copied().unwrap_or(baseline.mean),
            event_count: events.len(),
            regression_count,
            improvement_count,
            max_confidence,
            expected_run_length_final,
        };
        MetricOutcome { summary, events }
    }

    fn run_cusum(
        &self,
        metric: &DriftMetricSeries,
        baseline: &BaselineStats,
        events: &mut Vec<DriftEvent>,
    ) {
        let k = self.config.cusum.allowance_k.max(0.0);
        let h = self.config.cusum.threshold_h.max(EPS);
        let mut s_up = 0.0;
        let mut s_dn = 0.0;
        // Adaptive reference level. After an alarm we re-center on the new level
        // so one sustained shift produces ONE event (not one per observation),
        // while a genuinely new shift still re-triggers.
        let mut center = baseline.mean;
        for (index, &value) in metric.observations.iter().enumerate() {
            let z = (value - center) / baseline.std;
            s_up = (s_up + z - k).max(0.0);
            s_dn = (s_dn - z - k).max(0.0);
            if s_up >= h {
                events.push(cusum_event(
                    metric,
                    (center, baseline.std),
                    index,
                    DriftDetector::CusumUpward,
                    value,
                    s_up,
                    h,
                ));
                s_up = 0.0;
                s_dn = 0.0;
                center = value;
            } else if s_dn >= h {
                events.push(cusum_event(
                    metric,
                    (center, baseline.std),
                    index,
                    DriftDetector::CusumDownward,
                    value,
                    s_dn,
                    h,
                ));
                s_up = 0.0;
                s_dn = 0.0;
                center = value;
            }
        }
    }

    /// Run BOCPD, push changepoint events, return the final expected run length.
    fn run_bocpd(
        &self,
        metric: &DriftMetricSeries,
        baseline: &BaselineStats,
        events: &mut Vec<DriftEvent>,
    ) -> f64 {
        let cfg = &self.config.bocpd;
        let hazard = 1.0 / cfg.hazard_lambda.max(1.0);
        let max_run = cfg.max_run_length.max(1);
        let sigma = baseline.std.max(cfg.min_sigma);
        // Fresh-run prior predictive: wide enough to also accept a shifted mean
        // (covers the full observed spread), so the model can re-acquire a new
        // regime after a changepoint instead of collapsing onto the old mean.
        let prior_sigma = (sigma * cfg.prior_predictive_width.max(1.0))
            .max(series_std(&metric.observations))
            .max(cfg.min_sigma);

        // Run-length posterior plus per-hypothesis sufficient stats (count, sum).
        let mut rl_prob = vec![1.0];
        let mut counts = vec![0.0];
        let mut sums = vec![0.0];
        let mut expected_run_length = 0.0;
        let mut previous_recent_mass = 0.0;

        for (index, &value) in metric.observations.iter().enumerate() {
            let n = rl_prob.len();
            let mut growth = vec![0.0; n + 1];
            let mut cp_mass = 0.0;
            for i in 0..n {
                // Established runs predict their own running mean with the
                // baseline sigma; a fresh run (no data yet) uses the wide prior
                // predictive so it can fit a shifted regime.
                let (mean, pred_sigma) = if counts[i] > 0.0 {
                    (sums[i] / counts[i], sigma)
                } else {
                    (baseline.mean, prior_sigma)
                };
                let pred = gaussian_pdf(value, mean, pred_sigma);
                let joint = rl_prob[i] * pred;
                growth[i + 1] = joint * (1.0 - hazard);
                cp_mass += joint * hazard;
            }
            growth[0] = cp_mass;

            let total: f64 = growth.iter().sum();
            if total.is_finite() && total > f64::MIN_POSITIVE {
                for prob in &mut growth {
                    *prob /= total;
                }
            } else {
                // True predictive underflow (every hypothesis assigned ~0
                // density). Declare a fresh run; the wide prior predictive lets
                // the next observation re-acquire the new regime.
                growth.iter_mut().for_each(|prob| *prob = 0.0);
                growth[0] = 1.0;
            }

            // Update sufficient statistics, then truncate to max_run.
            let mut new_counts = vec![0.0; n + 1];
            let mut new_sums = vec![0.0; n + 1];
            for i in 0..n {
                new_counts[i + 1] = counts[i] + 1.0;
                new_sums[i + 1] = sums[i] + value;
            }
            rl_prob = growth;
            counts = new_counts;
            sums = new_sums;
            truncate_run_length(&mut rl_prob, &mut counts, &mut sums, max_run);

            expected_run_length = rl_prob
                .iter()
                .enumerate()
                .map(|(r, prob)| usize_to_f64(r) * *prob)
                .sum();

            let recent_window = cfg.recent_window.min(rl_prob.len().saturating_sub(1));
            let recent_mass: f64 = rl_prob.iter().take(recent_window + 1).sum();

            // Rising-edge detection avoids re-flagging the whole post-change tail.
            if recent_mass >= cfg.changepoint_threshold
                && previous_recent_mass < cfg.changepoint_threshold
                && index > 0
            {
                events.push(bocpd_event(metric, baseline, index, value, recent_mass));
            }
            previous_recent_mass = recent_mass;
        }

        expected_run_length
    }
}

struct MetricOutcome {
    summary: DriftMetricSummary,
    events: Vec<DriftEvent>,
}

struct BaselineStats {
    mean: f64,
    std: f64,
}

fn baseline_stats(observations: &[f64], window: usize, min_sigma: f64) -> BaselineStats {
    if observations.is_empty() {
        return BaselineStats {
            mean: 0.0,
            std: min_sigma.max(EPS),
        };
    }
    let take = window.clamp(1, observations.len());
    let slice = &observations[..take];
    let mean = slice.iter().sum::<f64>() / usize_to_f64(slice.len());
    let variance = if slice.len() > 1 {
        slice
            .iter()
            .map(|value| (*value - mean).powi(2))
            .sum::<f64>()
            / usize_to_f64(slice.len() - 1)
    } else {
        0.0
    };
    BaselineStats {
        mean,
        std: variance.sqrt().max(min_sigma).max(EPS),
    }
}

/// Standard deviation over the entire series, used to scale the BOCPD fresh-run
/// prior predictive so it spans the full observed range (0.0 for <2 points).
fn series_std(observations: &[f64]) -> f64 {
    if observations.len() < 2 {
        return 0.0;
    }
    let mean = observations.iter().sum::<f64>() / usize_to_f64(observations.len());
    let variance = observations
        .iter()
        .map(|value| (*value - mean).powi(2))
        .sum::<f64>()
        / usize_to_f64(observations.len() - 1);
    variance.sqrt()
}

fn cusum_event(
    metric: &DriftMetricSeries,
    reference: (f64, f64),
    index: usize,
    detector: DriftDetector,
    value: f64,
    statistic: f64,
    threshold: f64,
) -> DriftEvent {
    let (center, baseline_std) = reference;
    let upward = matches!(detector, DriftDetector::CusumUpward);
    let kind = drift_kind(metric.direction, upward);
    let confidence = (statistic / (2.0 * threshold)).clamp(0.0, 1.0);
    let arrow = if upward { "rose above" } else { "fell below" };
    // `center` is the reference the CUSUM was tracking at alarm time: the
    // original baseline for the first alarm, or the previous shifted level for
    // subsequent ones.
    DriftEvent {
        metric_id: metric.metric_id.clone(),
        observation_index: index_to_u32(index),
        detector,
        kind,
        value,
        baseline_mean: center,
        baseline_std,
        statistic,
        confidence,
        description: format!(
            "{} {} reference {:.4} (S={:.3} >= h={:.3}) => {}",
            metric.metric_id,
            arrow,
            center,
            statistic,
            threshold,
            kind.as_str()
        ),
    }
}

fn bocpd_event(
    metric: &DriftMetricSeries,
    baseline: &BaselineStats,
    index: usize,
    value: f64,
    recent_mass: f64,
) -> DriftEvent {
    // A regime change is a regression when the new value is on the bad side of
    // the baseline for the metric's direction.
    let worse = match metric.direction {
        MetricDirection::HigherIsBetter => value < baseline.mean,
        MetricDirection::LowerIsBetter => value > baseline.mean,
    };
    let kind = if (value - baseline.mean).abs() <= baseline.std * 0.25 {
        DriftKind::Neutral
    } else if worse {
        DriftKind::Regression
    } else {
        DriftKind::Improvement
    };
    DriftEvent {
        metric_id: metric.metric_id.clone(),
        observation_index: index_to_u32(index),
        detector: DriftDetector::BocpdChangepoint,
        kind,
        value,
        baseline_mean: baseline.mean,
        baseline_std: baseline.std,
        statistic: recent_mass,
        confidence: recent_mass.clamp(0.0, 1.0),
        description: format!(
            "{} regime change near baseline {:.4} (recent_mass={:.3}) => {}",
            metric.metric_id,
            baseline.mean,
            recent_mass,
            kind.as_str()
        ),
    }
}

fn drift_kind(direction: MetricDirection, upward: bool) -> DriftKind {
    match (direction, upward) {
        (MetricDirection::HigherIsBetter, true) | (MetricDirection::LowerIsBetter, false) => {
            DriftKind::Improvement
        }
        (MetricDirection::HigherIsBetter, false) | (MetricDirection::LowerIsBetter, true) => {
            DriftKind::Regression
        }
    }
}

fn truncate_run_length(
    rl_prob: &mut Vec<f64>,
    counts: &mut Vec<f64>,
    sums: &mut Vec<f64>,
    max_run: usize,
) {
    if rl_prob.len() <= max_run + 1 {
        return;
    }
    // Fold the tail mass into the last retained hypothesis and renormalize.
    let tail_mass: f64 = rl_prob.iter().skip(max_run + 1).sum();
    rl_prob.truncate(max_run + 1);
    counts.truncate(max_run + 1);
    sums.truncate(max_run + 1);
    if let Some(last) = rl_prob.last_mut() {
        *last += tail_mass;
    }
    let total: f64 = rl_prob.iter().sum();
    if total > EPS {
        for prob in rl_prob.iter_mut() {
            *prob /= total;
        }
    }
}

fn build_triage(events: &[DriftEvent]) -> DriftTriageSummary {
    let regression_count = events
        .iter()
        .filter(|event| matches!(event.kind, DriftKind::Regression))
        .count();
    let improvement_count = events
        .iter()
        .filter(|event| matches!(event.kind, DriftKind::Improvement))
        .count();

    let mut metrics_with_regression = events
        .iter()
        .filter(|event| matches!(event.kind, DriftKind::Regression))
        .map(|event| event.metric_id.clone())
        .collect::<Vec<_>>();
    metrics_with_regression.sort();
    metrics_with_regression.dedup();

    let mut ranked = events.to_vec();
    ranked.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| left.metric_id.cmp(&right.metric_id))
            .then_with(|| left.observation_index.cmp(&right.observation_index))
            .then_with(|| left.detector.as_str().cmp(right.detector.as_str()))
    });
    ranked.truncate(5);

    DriftTriageSummary {
        total_events: events.len(),
        regression_count,
        improvement_count,
        metrics_with_regression,
        top_events: ranked,
    }
}

fn sort_events(events: &mut [DriftEvent]) {
    events.sort_by(|left, right| {
        left.metric_id
            .cmp(&right.metric_id)
            .then_with(|| left.observation_index.cmp(&right.observation_index))
            .then_with(|| left.detector.as_str().cmp(right.detector.as_str()))
    });
}

fn monitor_id_for(
    config: DriftMonitorConfig,
    series: &[DriftMetricSeries],
    events: &[DriftEvent],
) -> String {
    let series_digests = series
        .iter()
        .map(|metric| format!("{}:{}", metric.metric_id, metric.observations.len()))
        .collect::<Vec<_>>();
    let event_digests = events
        .iter()
        .map(|event| {
            format!(
                "{}:{}:{}:{:.6}",
                event.metric_id,
                event.observation_index,
                event.detector.as_str(),
                event.confidence
            )
        })
        .collect::<Vec<_>>();
    let hash = stable_hash(&MonitorIdInput {
        schema_version: DRIFT_MONITOR_SCHEMA_VERSION,
        config_fingerprint: config.fingerprint().as_str(),
        series_digests: &series_digests,
        event_digests: &event_digests,
    });
    format!("drift-monitor-{}", short_hash(&hash))
}

#[derive(Serialize)]
struct MonitorIdInput<'a> {
    schema_version: &'a str,
    config_fingerprint: &'a str,
    series_digests: &'a [String],
    event_digests: &'a [String],
}

fn calibration_id_for(
    config: DriftMonitorConfig,
    series: &[DriftMetricSeries],
    false_positive_events: usize,
) -> String {
    let series_digests = series
        .iter()
        .map(|metric| format!("{}:{}", metric.metric_id, metric.observations.len()))
        .collect::<Vec<_>>();
    let hash = stable_hash(&CalibrationIdInput {
        schema_version: DRIFT_MONITOR_SCHEMA_VERSION,
        config_fingerprint: config.fingerprint().as_str(),
        series_digests: &series_digests,
        false_positive_events,
    });
    format!("drift-calibration-{}", short_hash(&hash))
}

#[derive(Serialize)]
struct CalibrationIdInput<'a> {
    schema_version: &'a str,
    config_fingerprint: &'a str,
    series_digests: &'a [String],
    false_positive_events: usize,
}

fn export_json_stats(
    monitor_id: &str,
    config: DriftMonitorConfig,
    summaries: &[DriftMetricSummary],
    events: &[DriftEvent],
    triage: &DriftTriageSummary,
) -> DriftJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        monitor_id: &'a str,
        config: DriftMonitorConfig,
        summaries: &'a [DriftMetricSummary],
        events: &'a [DriftEvent],
        triage: &'a DriftTriageSummary,
    }

    let payload = Export {
        schema_version: DRIFT_MONITOR_SCHEMA_VERSION,
        monitor_id,
        config,
        summaries,
        events,
        triage,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    DriftJsonStatsArtifact {
        path: format!("{monitor_id}/drift_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn gaussian_pdf(x: f64, mean: f64, std: f64) -> f64 {
    let std = std.max(EPS);
    let z = (x - mean) / std;
    (1.0 / (std * (2.0 * PI).sqrt())) * (-0.5 * z * z).exp()
}

fn safe_div(numerator: f64, denominator: f64) -> f64 {
    if denominator.abs() <= EPS {
        0.0
    } else {
        numerator / denominator
    }
}

fn usize_to_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::from(u32::MAX), f64::from)
}

fn index_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Stable series: small noise around 0.9 (deterministic, no PRNG).
    fn stable_success_rate() -> DriftMetricSeries {
        let values = vec![
            0.90, 0.91, 0.89, 0.90, 0.92, 0.88, 0.91, 0.90, 0.89, 0.91, 0.90, 0.92, 0.89, 0.90,
            0.91, 0.90,
        ];
        DriftMetricSeries::new("migration_success_rate", MetricDirection::HigherIsBetter)
            .with_observations(values)
    }

    /// Success rate that drops sharply partway through (a regression).
    fn dropping_success_rate() -> DriftMetricSeries {
        let mut values = vec![0.90, 0.91, 0.89, 0.90, 0.92, 0.88, 0.91, 0.90, 0.89, 0.91];
        values.extend([0.60, 0.58, 0.61, 0.59, 0.60, 0.62, 0.59, 0.60]);
        DriftMetricSeries::new("migration_success_rate", MetricDirection::HigherIsBetter)
            .with_observations(values)
    }

    /// Latency that rises sharply partway through (also a regression).
    fn rising_latency() -> DriftMetricSeries {
        let mut values = vec![10.0, 10.2, 9.8, 10.1, 9.9, 10.0, 10.3, 9.7, 10.0, 10.1];
        values.extend([18.0, 18.5, 17.9, 18.2, 18.1, 18.0]);
        DriftMetricSeries::new("p99_latency_ms", MetricDirection::LowerIsBetter)
            .with_observations(values)
    }

    /// Two successive downward level shifts (0.90 -> 0.60 -> 0.30), each regime
    /// held long enough for the detectors to re-acquire between shifts.
    fn two_step_down_series() -> DriftMetricSeries {
        let mut values = vec![0.90, 0.91, 0.89, 0.90, 0.91, 0.89, 0.90, 0.91];
        values.extend([0.60; 10]);
        values.extend([0.30; 10]);
        DriftMetricSeries::new("two_step", MetricDirection::HigherIsBetter)
            .with_observations(values)
    }

    fn monitor() -> DriftMonitor {
        DriftMonitor::new(DriftMonitorConfig::default().with_baseline_window(8))
    }

    #[test]
    fn analysis_is_deterministic_for_fixed_inputs() {
        let first = monitor().analyze(vec![dropping_success_rate(), rising_latency()]);
        let second = monitor().analyze(vec![dropping_success_rate(), rising_latency()]);
        assert_eq!(first.monitor_id, second.monitor_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.events, second.events);
    }

    #[test]
    fn analysis_is_independent_of_series_order() {
        let forward = monitor().analyze(vec![dropping_success_rate(), rising_latency()]);
        let backward = monitor().analyze(vec![rising_latency(), dropping_success_rate()]);
        assert_eq!(forward.monitor_id, backward.monitor_id);
        assert_eq!(forward.events, backward.events);
    }

    #[test]
    fn cusum_detects_downward_regression_in_success_rate() {
        let report = monitor().analyze(vec![dropping_success_rate()]);
        let downward = report.events.iter().find(|event| {
            event.detector == DriftDetector::CusumDownward && event.kind == DriftKind::Regression
        });
        assert!(downward.is_some(), "expected a downward CUSUM regression");
        let event = downward.expect("downward event");
        assert!(event.confidence > 0.0);
        assert!(event.value < event.baseline_mean);
        // The drop happens at/after index 10.
        assert!(event.observation_index >= 10);
    }

    #[test]
    fn cusum_detects_upward_regression_in_latency() {
        let report = monitor().analyze(vec![rising_latency()]);
        let upward = report.events.iter().find(|event| {
            event.detector == DriftDetector::CusumUpward && event.kind == DriftKind::Regression
        });
        assert!(upward.is_some(), "expected an upward CUSUM regression");
        let event = upward.expect("upward event");
        assert!(event.value > event.baseline_mean);
    }

    #[test]
    fn bocpd_flags_a_regime_change_at_the_jump() {
        let report = monitor().analyze(vec![dropping_success_rate()]);
        let changepoint = report
            .events
            .iter()
            .find(|event| event.detector == DriftDetector::BocpdChangepoint);
        assert!(changepoint.is_some(), "expected a BOCPD changepoint");
        let event = changepoint.expect("changepoint event");
        // Jump is at index 10; detection should land within a few observations.
        assert!(
            event.observation_index >= 9 && event.observation_index <= 13,
            "changepoint at unexpected index {}",
            event.observation_index
        );
        assert!(event.confidence >= report.config.bocpd.changepoint_threshold);
    }

    #[test]
    fn stable_series_raises_no_drift_events() {
        let report = monitor().analyze(vec![stable_success_rate()]);
        assert!(
            report.events.is_empty(),
            "stable series produced events: {:?}",
            report.events
        );
        assert_eq!(report.triage.total_events, 0);
    }

    #[test]
    fn triage_summary_counts_regressions_and_lists_metrics() {
        let report = monitor().analyze(vec![dropping_success_rate(), rising_latency()]);
        assert!(report.triage.regression_count >= 2);
        assert!(report.triage.total_events >= report.triage.top_events.len());
        assert!(report.triage.top_events.len() <= 5);
        assert!(
            report
                .triage
                .metrics_with_regression
                .contains(&"migration_success_rate".to_string())
        );
        assert!(
            report
                .triage
                .metrics_with_regression
                .contains(&"p99_latency_ms".to_string())
        );
        // Top events are sorted by descending confidence.
        let mut previous = f64::INFINITY;
        for event in &report.triage.top_events {
            assert!(event.confidence <= previous + 1e-9);
            previous = event.confidence;
        }
    }

    #[test]
    fn calibration_passes_for_stable_fixtures() {
        let report = monitor().calibrate(vec![stable_success_rate(), stable_success_rate()], 0.01);
        assert_eq!(report.false_positive_events, 0);
        assert!(report.false_positive_rate.abs() < 1e-12);
        assert!(report.meets_target);
        assert!(report.total_observations > 0);
    }

    #[test]
    fn calibration_flags_oversensitive_config() {
        // A tiny CUSUM threshold + low changepoint threshold over-fires on noise.
        let config = DriftMonitorConfig::default()
            .with_baseline_window(6)
            .with_cusum(CusumConfig {
                allowance_k: 0.0,
                threshold_h: 0.5,
            })
            .with_bocpd(BocpdConfig {
                changepoint_threshold: 0.05,
                ..BocpdConfig::default()
            });
        let report = DriftMonitor::new(config).calibrate(vec![stable_success_rate()], 0.0);
        assert!(report.false_positive_events > 0);
        assert!(!report.meets_target);
        // Recommended thresholds exceed the defaults that over-fired.
        assert!(report.recommended_cusum_threshold > 0.5);
    }

    #[test]
    fn empty_series_produces_a_valid_report() {
        let report = monitor().analyze(Vec::new());
        assert!(report.events.is_empty());
        assert!(report.summaries.is_empty());
        assert!(!report.exported_json_stats.sha256.is_empty());
        assert!(report.replay_command.contains("drift-monitor"));
    }

    #[test]
    fn single_metric_summary_is_populated() {
        let report = monitor().analyze(vec![dropping_success_rate()]);
        let summary = report.summaries.first().expect("summary present");
        assert_eq!(summary.metric_id, "migration_success_rate");
        assert_eq!(summary.direction, MetricDirection::HigherIsBetter);
        assert!(summary.baseline_mean > 0.8 && summary.baseline_mean < 1.0);
        assert!(summary.baseline_std > 0.0);
        assert!(summary.event_count > 0);
        assert!(summary.regression_count >= 1);
        assert!(summary.expected_run_length_final >= 0.0);
    }

    #[test]
    fn gaussian_pdf_peaks_at_mean() {
        let at_mean = gaussian_pdf(1.0, 1.0, 1.0);
        let off_mean = gaussian_pdf(3.0, 1.0, 1.0);
        assert!(at_mean > off_mean);
        // Standard-normal peak height 1/sqrt(2pi) ~= 0.39894.
        assert!((at_mean - 0.398_942_280_4).abs() < 1e-6);
    }

    #[test]
    fn degenerate_baseline_does_not_panic() {
        // Constant series → zero variance baseline; sigma is floored.
        let flat = DriftMetricSeries::new("flat", MetricDirection::HigherIsBetter)
            .with_observations(vec![0.5; 12]);
        let report = monitor().analyze(vec![flat]);
        let summary = report.summaries.first().expect("summary present");
        assert!(summary.baseline_std >= 0.0);
        assert!(summary.baseline_std.is_finite());
    }

    #[test]
    fn cusum_emits_one_event_per_sustained_shift() {
        // A single sustained downward shift must alarm exactly ONCE, not once
        // per observation in the degraded regime (no duplicate-alarm flood).
        let report = monitor().analyze(vec![dropping_success_rate()]);
        let downward = report
            .events
            .iter()
            .filter(|event| event.detector == DriftDetector::CusumDownward)
            .count();
        assert_eq!(
            downward, 1,
            "sustained shift should alarm once, got {downward}"
        );
    }

    #[test]
    fn detectors_catch_two_successive_shifts() {
        // After re-baselining (CUSUM) / re-acquiring the regime (BOCPD), a
        // SECOND distinct shift must still be detected — neither detector is
        // permanently stuck after the first event.
        let report = monitor().analyze(vec![two_step_down_series()]);
        let cusum_down = report
            .events
            .iter()
            .filter(|event| event.detector == DriftDetector::CusumDownward)
            .count();
        assert_eq!(
            cusum_down, 2,
            "expected one CUSUM alarm per shift, got {cusum_down}"
        );
        let changepoints = report
            .events
            .iter()
            .filter(|event| event.detector == DriftDetector::BocpdChangepoint)
            .count();
        assert!(
            changepoints >= 2,
            "BOCPD should detect both shifts after recovery, got {changepoints}"
        );
    }

    #[test]
    fn bocpd_run_length_recovers_within_a_stable_regime() {
        // After a changepoint the model must re-acquire the new regime, so the
        // expected run length grows again instead of staying pinned near 0 (the
        // signature of the previously-stuck collapse).
        let report = monitor().analyze(vec![two_step_down_series()]);
        let summary = report.summaries.first().expect("summary present");
        assert!(
            summary.expected_run_length_final > 1.0,
            "expected_run_length_final pinned low ({}); detector did not recover",
            summary.expected_run_length_final
        );
    }
}

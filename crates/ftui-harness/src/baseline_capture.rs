#![forbid(unsafe_code)]

//! Deterministic baseline capture for performance measurement (bd-760ih).
//!
//! This module provides the infrastructure for capturing trustworthy performance
//! baselines that later optimization work compares against. A baseline captures:
//!
//! - **Latency percentiles**: p50, p95, p99, p999 for each measured stage
//! - **Throughput**: operations per second where applicable
//! - **Output cost**: cells touched, ANSI bytes emitted per frame
//! - **Environment fingerprint**: CPU, OS, rustc version, profile, feature flags
//! - **Variance envelope**: standard deviation and stability classification
//!
//! # Design principles
//!
//! 1. **Reproducibility**: Every baseline records enough metadata to explain
//!    why two runs differ (environment, seed, fixture, profile).
//! 2. **Variance awareness**: Unstable baselines are flagged, not silently used.
//! 3. **Traceability**: Every optimization must point back to a baseline record.
//! 4. **Dual coverage**: Baselines cover both canonical regression suites and
//!    adversarial challenge suites.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_harness::baseline_capture::*;
//!
//! let mut capture = BaselineCapture::new("render_diff_80x24", FixtureFamily::Render);
//! capture.record_sample(Sample::latency_us("buffer_diff", 142));
//! capture.record_sample(Sample::latency_us("buffer_diff", 138));
//! capture.record_sample(Sample::latency_us("buffer_diff", 145));
//! capture.record_sample(Sample::output_cost("ansi_bytes", 2048));
//!
//! let baseline = capture.finalize();
//! assert!(baseline.is_stable());
//! let json = baseline.to_json();
//! ```

use std::collections::BTreeMap;

use ftui_core::terminal_capabilities::ColorDepth;

/// A raw measurement sample.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Metric name (e.g., "buffer_diff", "presenter_emit").
    pub metric: String,
    /// Measurement category.
    pub category: MetricCategory,
    /// Raw value.
    pub value: f64,
    /// Unit of measurement.
    pub unit: String,
}

impl Sample {
    /// Create a latency sample in microseconds.
    #[must_use]
    pub fn latency_us(metric: &str, value_us: u64) -> Self {
        Self {
            metric: metric.to_string(),
            category: MetricCategory::Latency,
            value: value_us as f64,
            unit: "us".to_string(),
        }
    }

    /// Create a throughput sample in operations per second.
    #[must_use]
    pub fn throughput_ops(metric: &str, ops_per_sec: f64) -> Self {
        Self {
            metric: metric.to_string(),
            category: MetricCategory::Throughput,
            value: ops_per_sec,
            unit: "ops/s".to_string(),
        }
    }

    /// Create an output cost sample (cells, bytes, etc.).
    #[must_use]
    pub fn output_cost(metric: &str, count: u64) -> Self {
        Self {
            metric: metric.to_string(),
            category: MetricCategory::OutputCost,
            value: count as f64,
            unit: "count".to_string(),
        }
    }

    /// Create a memory sample in bytes.
    #[must_use]
    pub fn memory_bytes(metric: &str, bytes: u64) -> Self {
        Self {
            metric: metric.to_string(),
            category: MetricCategory::Memory,
            value: bytes as f64,
            unit: "bytes".to_string(),
        }
    }
}

/// Category of a performance metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetricCategory {
    /// Wall-clock or stage timing.
    Latency,
    /// Operations per second.
    Throughput,
    /// Cells touched, ANSI bytes emitted, etc.
    OutputCost,
    /// Heap allocation, RSS, etc.
    Memory,
}

impl MetricCategory {
    /// Whether wall-clock timing alone is sufficient for this category,
    /// or stage-level breakdowns are mandatory.
    #[must_use]
    pub const fn requires_stage_breakdown(&self) -> bool {
        match self {
            Self::Latency => true,
            Self::Throughput => false,
            Self::OutputCost => false,
            Self::Memory => false,
        }
    }
}

/// Which fixture family a baseline belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixtureFamily {
    /// Render pipeline benchmarks (buffer, diff, presenter).
    Render,
    /// Runtime benchmarks (event loop, subscriptions, effects).
    Runtime,
    /// Doctor workflow benchmarks (capture, seed, suite).
    Doctor,
    /// Adversarial/challenge benchmarks (stress, edge cases).
    Challenge,
}

impl FixtureFamily {
    /// Human-readable label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Render => "render",
            Self::Runtime => "runtime",
            Self::Doctor => "doctor",
            Self::Challenge => "challenge",
        }
    }
}

/// Stability classification for a baseline metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StabilityClass {
    /// Coefficient of variation < 5%: highly stable, safe for tight gates.
    Stable,
    /// CV 5-15%: moderate variance, use with wider tolerance.
    Moderate,
    /// CV > 15%: high variance, not suitable for regression gating.
    Unstable,
}

/// Latency percentiles computed from samples.
#[derive(Debug, Clone, Copy)]
pub struct Percentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub p999: f64,
    pub min: f64,
    pub max: f64,
}

/// Computed statistics for a single metric across all samples.
#[derive(Debug, Clone)]
pub struct MetricBaseline {
    /// Metric name.
    pub metric: String,
    /// Category.
    pub category: MetricCategory,
    /// Unit.
    pub unit: String,
    /// Number of samples.
    pub sample_count: usize,
    /// Arithmetic mean.
    pub mean: f64,
    /// Standard deviation.
    pub stddev: f64,
    /// Coefficient of variation (stddev / mean).
    pub cv: f64,
    /// Stability classification.
    pub stability: StabilityClass,
    /// Latency percentiles (only meaningful for Latency category).
    pub percentiles: Percentiles,
}

/// Environment fingerprint for baseline reproducibility.
#[derive(Debug, Clone)]
pub struct EnvironmentFingerprint {
    /// Rust compiler version.
    pub rustc_version: String,
    /// Cargo profile (debug/release).
    pub profile: String,
    /// Effective terminal color depth for presenter workloads.
    pub terminal_color_depth: Option<ColorDepth>,
    /// Target triple.
    pub target: String,
    /// Feature flags enabled.
    pub features: Vec<String>,
    /// CPU model (from /proc/cpuinfo or equivalent).
    pub cpu_model: String,
    /// Number of logical CPUs.
    pub cpu_count: u32,
    /// OS description.
    pub os: String,
}

impl EnvironmentFingerprint {
    /// Capture the current environment.
    ///
    /// Fills in what can be determined at runtime; callers should
    /// override fields that require build-time information.
    #[must_use]
    pub fn capture() -> Self {
        Self {
            rustc_version: String::new(), // must be filled by caller
            profile: if cfg!(debug_assertions) {
                "debug".to_string()
            } else {
                "release".to_string()
            },
            terminal_color_depth: None,
            target: std::env::consts::ARCH.to_string(),
            features: Vec::new(),
            cpu_model: String::new(),
            cpu_count: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1),
            os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        }
    }
}

/// A complete baseline record for one fixture.
#[derive(Debug, Clone)]
pub struct BaselineRecord {
    /// Fixture/scenario name.
    pub fixture: String,
    /// Fixture family.
    pub family: FixtureFamily,
    /// Per-metric baselines.
    pub metrics: Vec<MetricBaseline>,
    /// Environment fingerprint.
    pub environment: EnvironmentFingerprint,
    /// Random seed used (for deterministic replay).
    pub seed: u64,
    /// Whether all metrics are stable enough for regression gating.
    pub stable: bool,
    /// Schema version for forward compatibility.
    pub schema_version: u32,
}

impl BaselineRecord {
    /// Whether all metrics are stable (CV < 15%).
    #[must_use]
    pub fn is_stable(&self) -> bool {
        self.stable
    }

    /// Metrics that are unstable (CV > 15%).
    #[must_use]
    pub fn unstable_metrics(&self) -> Vec<&MetricBaseline> {
        self.metrics
            .iter()
            .filter(|m| m.stability == StabilityClass::Unstable)
            .collect()
    }

    /// Serialize to JSON for storage as a baseline artifact.
    #[must_use]
    pub fn to_json(&self) -> String {
        let terminal_color_depth = self.environment.terminal_color_depth.map_or_else(
            || "null".to_string(),
            |depth| format!(r#""{}""#, depth.as_str()),
        );
        let metrics_json: Vec<String> = self
            .metrics
            .iter()
            .map(|m| {
                format!(
                    r#"    {{
      "metric": "{}",
      "category": "{}",
      "unit": "{}",
      "sample_count": {},
      "mean": {:.2},
      "stddev": {:.2},
      "cv": {:.4},
      "stability": "{}",
      "p50": {:.2},
      "p95": {:.2},
      "p99": {:.2},
      "p999": {:.2},
      "min": {:.2},
      "max": {:.2}
    }}"#,
                    m.metric,
                    match m.category {
                        MetricCategory::Latency => "latency",
                        MetricCategory::Throughput => "throughput",
                        MetricCategory::OutputCost => "output_cost",
                        MetricCategory::Memory => "memory",
                    },
                    m.unit,
                    m.sample_count,
                    m.mean,
                    m.stddev,
                    m.cv,
                    match m.stability {
                        StabilityClass::Stable => "stable",
                        StabilityClass::Moderate => "moderate",
                        StabilityClass::Unstable => "unstable",
                    },
                    m.percentiles.p50,
                    m.percentiles.p95,
                    m.percentiles.p99,
                    m.percentiles.p999,
                    m.percentiles.min,
                    m.percentiles.max,
                )
            })
            .collect();

        format!(
            r#"{{
  "schema_version": {},
  "fixture": "{}",
  "family": "{}",
  "seed": {},
  "stable": {},
  "environment": {{
    "profile": "{}",
    "terminal_color_depth": {},
    "target": "{}",
    "cpu_count": {},
    "os": "{}"
  }},
  "metrics": [
{}
  ]
}}"#,
            self.schema_version,
            self.fixture,
            self.family.label(),
            self.seed,
            self.stable,
            self.environment.profile,
            terminal_color_depth,
            self.environment.target,
            self.environment.cpu_count,
            self.environment.os,
            metrics_json.join(",\n"),
        )
    }
}

/// Builder for capturing baseline samples and computing statistics.
pub struct BaselineCapture {
    fixture: String,
    family: FixtureFamily,
    seed: u64,
    samples: BTreeMap<String, Vec<Sample>>,
    environment: EnvironmentFingerprint,
}

impl BaselineCapture {
    /// Create a new baseline capture session.
    #[must_use]
    pub fn new(fixture: &str, family: FixtureFamily) -> Self {
        Self {
            fixture: fixture.to_string(),
            family,
            seed: 0,
            samples: BTreeMap::new(),
            environment: EnvironmentFingerprint::capture(),
        }
    }

    /// Set the random seed for reproducibility.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Override the environment fingerprint.
    #[must_use]
    pub fn with_environment(mut self, env: EnvironmentFingerprint) -> Self {
        self.environment = env;
        self
    }

    /// Record the effective terminal color depth for presenter measurements.
    #[must_use]
    pub fn with_terminal_color_depth(mut self, depth: ColorDepth) -> Self {
        self.environment.terminal_color_depth = Some(depth);
        self
    }

    /// Record a measurement sample.
    pub fn record_sample(&mut self, sample: Sample) {
        self.samples
            .entry(sample.metric.clone())
            .or_default()
            .push(sample);
    }

    /// Finalize the capture and compute statistics.
    #[must_use]
    pub fn finalize(self) -> BaselineRecord {
        let mut metrics = Vec::new();
        let mut all_stable = true;

        for (metric_name, samples) in &self.samples {
            if samples.is_empty() {
                continue;
            }

            let category = samples[0].category;
            let unit = samples[0].unit.clone();
            let values: Vec<f64> = samples.iter().map(|s| s.value).collect();
            let n = values.len();

            let mean = values.iter().sum::<f64>() / n as f64;
            let variance = if n > 1 {
                values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64
            } else {
                0.0
            };
            let stddev = variance.sqrt();
            let cv = if mean > 0.0 { stddev / mean } else { 0.0 };

            let stability = if cv < 0.05 {
                StabilityClass::Stable
            } else if cv < 0.15 {
                StabilityClass::Moderate
            } else {
                StabilityClass::Unstable
            };

            if stability == StabilityClass::Unstable {
                all_stable = false;
            }

            let percentiles = compute_percentiles(&values);

            metrics.push(MetricBaseline {
                metric: metric_name.clone(),
                category,
                unit,
                sample_count: n,
                mean,
                stddev,
                cv,
                stability,
                percentiles,
            });
        }

        BaselineRecord {
            fixture: self.fixture,
            family: self.family,
            metrics,
            environment: self.environment,
            seed: self.seed,
            stable: all_stable,
            schema_version: 1,
        }
    }
}

/// Compute latency percentiles from a slice of values.
fn compute_percentiles(values: &[f64]) -> Percentiles {
    if values.is_empty() {
        return Percentiles {
            p50: 0.0,
            p95: 0.0,
            p99: 0.0,
            p999: 0.0,
            min: 0.0,
            max: 0.0,
        };
    }

    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = sorted.len();
    let percentile = |p: f64| -> f64 {
        if n == 1 {
            return sorted[0];
        }
        let rank = p / 100.0 * (n - 1) as f64;
        let lower = rank.floor() as usize;
        let upper = rank.ceil() as usize;
        let frac = rank - lower as f64;
        if upper >= n {
            sorted[n - 1]
        } else {
            sorted[lower] * (1.0 - frac) + sorted[upper] * frac
        }
    };

    Percentiles {
        p50: percentile(50.0),
        p95: percentile(95.0),
        p99: percentile(99.0),
        p999: percentile(99.9),
        min: sorted[0],
        max: sorted[n - 1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_constructors() {
        let lat = Sample::latency_us("diff", 100);
        assert_eq!(lat.metric, "diff");
        assert_eq!(lat.category, MetricCategory::Latency);
        assert_eq!(lat.value, 100.0);
        assert_eq!(lat.unit, "us");

        let tp = Sample::throughput_ops("frames", 60.0);
        assert_eq!(tp.category, MetricCategory::Throughput);

        let oc = Sample::output_cost("ansi_bytes", 2048);
        assert_eq!(oc.category, MetricCategory::OutputCost);
        assert_eq!(oc.value, 2048.0);

        let mem = Sample::memory_bytes("rss", 1024 * 1024);
        assert_eq!(mem.category, MetricCategory::Memory);
    }

    #[test]
    fn percentiles_single_value() {
        let p = compute_percentiles(&[42.0]);
        assert_eq!(p.p50, 42.0);
        assert_eq!(p.p95, 42.0);
        assert_eq!(p.min, 42.0);
        assert_eq!(p.max, 42.0);
    }

    #[test]
    fn percentiles_empty() {
        let p = compute_percentiles(&[]);
        assert_eq!(p.p50, 0.0);
        assert_eq!(p.min, 0.0);
    }

    #[test]
    fn percentiles_ordered_values() {
        let values: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let p = compute_percentiles(&values);
        assert_eq!(p.min, 1.0);
        assert_eq!(p.max, 100.0);
        assert!(
            (p.p50 - 50.0).abs() < 1.0,
            "p50 should be ~50, got {}",
            p.p50
        );
        assert!(p.p95 > 90.0, "p95 should be > 90, got {}", p.p95);
        assert!(p.p99 > 95.0, "p99 should be > 95, got {}", p.p99);
    }

    #[test]
    fn baseline_capture_stable_metrics() {
        let mut cap = BaselineCapture::new("test_fixture", FixtureFamily::Render).with_seed(42);
        // Low-variance samples (all close to 100)
        for v in [98, 100, 102, 99, 101, 100, 98, 101, 99, 100] {
            cap.record_sample(Sample::latency_us("diff", v));
        }

        let baseline = cap.finalize();
        assert_eq!(baseline.fixture, "test_fixture");
        assert_eq!(baseline.family, FixtureFamily::Render);
        assert_eq!(baseline.seed, 42);
        assert_eq!(baseline.metrics.len(), 1);

        let m = &baseline.metrics[0];
        assert_eq!(m.metric, "diff");
        assert_eq!(m.sample_count, 10);
        assert!(
            m.cv < 0.05,
            "cv should be < 5% for stable data, got {}",
            m.cv
        );
        assert_eq!(m.stability, StabilityClass::Stable);
        assert!(baseline.is_stable());
    }

    #[test]
    fn baseline_capture_unstable_metrics() {
        let mut cap = BaselineCapture::new("noisy_fixture", FixtureFamily::Challenge);
        // High-variance samples
        for v in [10, 100, 500, 20, 300, 50, 400, 15, 200, 600] {
            cap.record_sample(Sample::latency_us("jittery_op", v));
        }

        let baseline = cap.finalize();
        let m = &baseline.metrics[0];
        assert_eq!(m.stability, StabilityClass::Unstable);
        assert!(!baseline.is_stable());
        assert_eq!(baseline.unstable_metrics().len(), 1);
    }

    #[test]
    fn baseline_capture_multiple_metrics() {
        let mut cap = BaselineCapture::new("multi", FixtureFamily::Runtime);
        for v in [100, 101, 99] {
            cap.record_sample(Sample::latency_us("event_loop", v));
        }
        for v in [2048, 2050, 2047] {
            cap.record_sample(Sample::output_cost("ansi_bytes", v));
        }

        let baseline = cap.finalize();
        assert_eq!(baseline.metrics.len(), 2);

        let names: Vec<&str> = baseline.metrics.iter().map(|m| m.metric.as_str()).collect();
        assert!(names.contains(&"event_loop"));
        assert!(names.contains(&"ansi_bytes"));
    }

    #[test]
    fn baseline_to_json_valid() {
        let mut cap = BaselineCapture::new("json_test", FixtureFamily::Render).with_seed(42);
        cap.record_sample(Sample::latency_us("diff", 100));
        cap.record_sample(Sample::latency_us("diff", 105));

        let baseline = cap.finalize();
        let json = baseline.to_json();

        assert!(json.starts_with('{'));
        assert!(json.ends_with('}'));
        assert!(json.contains("\"schema_version\": 1"));
        assert!(json.contains("\"fixture\": \"json_test\""));
        assert!(json.contains("\"family\": \"render\""));
        assert!(json.contains("\"seed\": 42"));
        assert!(json.contains("\"terminal_color_depth\": null"));
        assert!(json.contains("\"metric\": \"diff\""));
        assert!(json.contains("\"p50\":"));
        assert!(json.contains("\"p95\":"));
        assert!(json.contains("\"cv\":"));
    }

    #[test]
    fn environment_fingerprint_capture() {
        let env = EnvironmentFingerprint::capture();
        assert!(env.cpu_count > 0);
        assert!(!env.os.is_empty());
        assert!(!env.target.is_empty());
    }

    #[test]
    fn stability_thresholds() {
        // CV < 5% = Stable
        let mut cap = BaselineCapture::new("s", FixtureFamily::Render);
        for v in [100, 101, 99, 100, 100] {
            cap.record_sample(Sample::latency_us("a", v));
        }
        let b = cap.finalize();
        assert_eq!(b.metrics[0].stability, StabilityClass::Stable);

        // CV 5-15% = Moderate
        let mut cap = BaselineCapture::new("m", FixtureFamily::Render);
        for v in [100, 110, 90, 105, 95] {
            cap.record_sample(Sample::latency_us("a", v));
        }
        let b = cap.finalize();
        assert_eq!(b.metrics[0].stability, StabilityClass::Moderate);

        // CV > 15% = Unstable
        let mut cap = BaselineCapture::new("u", FixtureFamily::Render);
        for v in [50, 200, 30, 180, 100] {
            cap.record_sample(Sample::latency_us("a", v));
        }
        let b = cap.finalize();
        assert_eq!(b.metrics[0].stability, StabilityClass::Unstable);
    }

    #[test]
    fn latency_requires_stage_breakdown() {
        assert!(MetricCategory::Latency.requires_stage_breakdown());
        assert!(!MetricCategory::Throughput.requires_stage_breakdown());
        assert!(!MetricCategory::OutputCost.requires_stage_breakdown());
        assert!(!MetricCategory::Memory.requires_stage_breakdown());
    }

    #[test]
    fn fixture_family_labels() {
        assert_eq!(FixtureFamily::Render.label(), "render");
        assert_eq!(FixtureFamily::Runtime.label(), "runtime");
        assert_eq!(FixtureFamily::Doctor.label(), "doctor");
        assert_eq!(FixtureFamily::Challenge.label(), "challenge");
    }

    #[test]
    fn schema_version_is_set() {
        let cap = BaselineCapture::new("v", FixtureFamily::Render);
        let b = cap.finalize();
        assert_eq!(b.schema_version, 1);
    }

    #[test]
    fn empty_capture_produces_empty_baseline() {
        let cap = BaselineCapture::new("empty", FixtureFamily::Render);
        let b = cap.finalize();
        assert!(b.metrics.is_empty());
        assert!(b.is_stable(), "empty baseline should be considered stable");
    }

    #[test]
    fn percentiles_two_values() {
        let p = compute_percentiles(&[10.0, 20.0]);
        assert_eq!(p.min, 10.0);
        assert_eq!(p.max, 20.0);
        assert!((p.p50 - 15.0).abs() < 0.01, "p50 of [10,20] should be 15");
    }

    #[test]
    fn mean_and_stddev_correct() {
        let mut cap = BaselineCapture::new("stats", FixtureFamily::Render);
        for v in [10, 20, 30, 40, 50] {
            cap.record_sample(Sample::latency_us("x", v));
        }
        let b = cap.finalize();
        let m = &b.metrics[0];

        // Mean of [10,20,30,40,50] = 30
        assert!(
            (m.mean - 30.0).abs() < 0.01,
            "mean should be 30, got {}",
            m.mean
        );

        // Sample stddev of [10,20,30,40,50] = sqrt(250) ≈ 15.81
        assert!(
            (m.stddev - 15.81).abs() < 0.1,
            "stddev should be ~15.81, got {}",
            m.stddev
        );
    }
}

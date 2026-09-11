#![forbid(unsafe_code)]

//! Benchmark gate enforcement with structured evidence.
//!
//! Loads baseline performance thresholds, compares measured values, and emits
//! pass/fail evidence in JSONL format. This module provides the programmatic
//! backbone for CI performance regression gating.
//!
//! # Design
//!
//! A [`BenchmarkGate`] is configured with a set of [`Threshold`]s (metric name,
//! budget, tolerance). After collecting [`Measurement`]s, calling
//! [`evaluate`](BenchmarkGate::evaluate) produces a [`GateResult`] with
//! per-metric verdicts and an overall pass/fail.
//!
//! # Example
//!
//! ```ignore
//! use ftui_harness::benchmark_gate::{BenchmarkGate, Measurement, Threshold};
//!
//! let gate = BenchmarkGate::new("render_perf")
//!     .threshold(Threshold::new("frame_render_p99_us", 2000.0).tolerance_pct(10.0))
//!     .threshold(Threshold::new("diff_compute_p99_us", 500.0).tolerance_pct(15.0));
//!
//! let measurements = vec![
//!     Measurement::new("frame_render_p99_us", 1850.0),
//!     Measurement::new("diff_compute_p99_us", 480.0),
//! ];
//!
//! let result = gate.evaluate(&measurements);
//! assert!(result.passed());
//! ```
//!
//! # Baseline JSON Format
//!
//! Thresholds can be loaded from a JSON file matching the format used by
//! `scripts/perf_regression_gate.sh`:
//!
//! ```json
//! {
//!   "frame_render_p99_us": { "budget": 2000.0, "tolerance_pct": 10.0 },
//!   "diff_compute_p99_us": { "budget": 500.0 }
//! }
//! ```

use std::collections::{BTreeMap, BTreeSet};

use crate::determinism::{JsonValue, TestJsonlLogger};

// ============================================================================
// Threshold
// ============================================================================

/// A single performance threshold for gating.
#[derive(Debug, Clone)]
pub struct Threshold {
    /// Metric name (must match a [`Measurement`] name).
    pub metric: String,
    /// Budget value (upper bound for the metric).
    pub budget: f64,
    /// Tolerance as a percentage (0.0–100.0). A measurement is allowed to
    /// exceed `budget` by up to `budget * tolerance_pct / 100`.
    pub tolerance_pct: f64,
}

impl Threshold {
    /// Create a threshold with zero tolerance.
    pub fn new(metric: &str, budget: f64) -> Self {
        Self {
            metric: metric.to_string(),
            budget,
            tolerance_pct: 0.0,
        }
    }

    /// Set the tolerance percentage.
    #[must_use]
    pub fn tolerance_pct(mut self, pct: f64) -> Self {
        self.tolerance_pct = pct;
        self
    }

    /// Effective ceiling = budget × (1 + tolerance / 100).
    #[must_use]
    pub fn ceiling(&self) -> f64 {
        self.budget * (1.0 + self.tolerance_pct / 100.0)
    }

    fn is_valid(&self) -> bool {
        !self.metric.trim().is_empty()
            && self.budget.is_finite()
            && self.budget >= 0.0
            && self.tolerance_pct.is_finite()
            && (0.0..=100.0).contains(&self.tolerance_pct)
            && self.ceiling().is_finite()
    }
}

// ============================================================================
// Measurement
// ============================================================================

/// A single performance measurement to check against a threshold.
#[derive(Debug, Clone)]
pub struct Measurement {
    /// Metric name (should match a [`Threshold`] metric).
    pub metric: String,
    /// Measured value.
    pub value: f64,
    /// Optional unit label for evidence output (e.g., "μs", "bytes").
    pub unit: Option<String>,
}

impl Measurement {
    /// Create a measurement.
    pub fn new(metric: &str, value: f64) -> Self {
        Self {
            metric: metric.to_string(),
            value,
            unit: None,
        }
    }

    /// Set the unit label.
    #[must_use]
    pub fn unit(mut self, unit: &str) -> Self {
        self.unit = Some(unit.to_string());
        self
    }
}

// ============================================================================
// MetricVerdict
// ============================================================================

/// Verdict for a single metric check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricVerdict {
    /// Measured value is within budget (including tolerance).
    Pass,
    /// Measured value exceeds budget + tolerance, or its input is invalid.
    Fail,
    /// No threshold defined for this metric (informational only).
    Unchecked,
}

/// Detailed result for a single metric evaluation.
#[derive(Debug, Clone)]
pub struct MetricResult {
    /// Metric name.
    pub metric: String,
    /// Measured value.
    pub value: f64,
    /// Budget (if a threshold was defined).
    pub budget: Option<f64>,
    /// Effective ceiling (budget + tolerance).
    pub ceiling: Option<f64>,
    /// Tolerance percentage applied.
    pub tolerance_pct: Option<f64>,
    /// How much the value exceeds the budget as a percentage.
    /// Negative means under budget.
    pub overshoot_pct: Option<f64>,
    /// Per-metric verdict.
    pub verdict: MetricVerdict,
    /// Unit label (if provided).
    pub unit: Option<String>,
}

// ============================================================================
// GateResult
// ============================================================================

/// Overall result of a benchmark gate evaluation.
#[derive(Debug, Clone)]
pub struct GateResult {
    /// Gate name.
    pub gate_name: String,
    /// Per-metric results (sorted by metric name).
    pub metrics: Vec<MetricResult>,
    /// Number of metrics that passed.
    pub pass_count: usize,
    /// Number of metrics that failed.
    pub fail_count: usize,
    /// Number of metrics with no threshold (unchecked).
    pub unchecked_count: usize,
    /// Invalid configuration or measurement input, including missing required metrics.
    /// These errors fail the gate independently of the per-measurement counts.
    pub validation_errors: Vec<String>,
}

impl GateResult {
    /// True only for a nonempty set of passing required metrics with no failures
    /// or validation errors. Unchecked metrics never establish coverage.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.pass_count > 0 && self.fail_count == 0 && self.validation_errors.is_empty()
    }

    /// Return failed measurements. Missing measurements and configuration errors
    /// are reported separately in [`Self::validation_errors`].
    pub fn failures(&self) -> Vec<&MetricResult> {
        self.metrics
            .iter()
            .filter(|m| m.verdict == MetricVerdict::Fail)
            .collect()
    }

    /// Format a human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let status = if self.passed() { "PASS" } else { "FAIL" };
        let mut out = format!(
            "Gate '{}': {} ({} passed, {} failed, {} unchecked)\n",
            self.gate_name, status, self.pass_count, self.fail_count, self.unchecked_count
        );
        for m in &self.metrics {
            let icon = match m.verdict {
                MetricVerdict::Pass => "  ok",
                MetricVerdict::Fail => "FAIL",
                MetricVerdict::Unchecked => "  --",
            };
            let unit = m.unit.as_deref().unwrap_or("");
            if let Some(budget) = m.budget {
                let overshoot = m
                    .overshoot_pct
                    .map(|value| format!("{value:+.1}%"))
                    .unwrap_or_else(|| "n/a".to_string());
                out.push_str(&format!(
                    "  [{icon}] {}: {:.1}{unit} (budget: {:.1}{unit}, overshoot: {overshoot})\n",
                    m.metric, m.value, budget
                ));
            } else {
                out.push_str(&format!(
                    "  [{icon}] {}: {:.1}{unit} (no threshold)\n",
                    m.metric, m.value
                ));
            }
        }
        for error in &self.validation_errors {
            out.push_str(&format!("  [FAIL] {error}\n"));
        }
        out
    }
}

// ============================================================================
// BenchmarkGate
// ============================================================================

/// Benchmark gate that compares measurements against thresholds.
#[derive(Debug, Clone)]
pub struct BenchmarkGate {
    /// Gate name for evidence output.
    gate_name: String,
    /// Thresholds keyed by metric name.
    thresholds: BTreeMap<String, Threshold>,
    /// Repeated builder keys are ambiguous even when the values agree.
    duplicate_thresholds: BTreeSet<String>,
}

impl BenchmarkGate {
    /// Create a new benchmark gate.
    pub fn new(gate_name: &str) -> Self {
        Self {
            gate_name: gate_name.to_string(),
            thresholds: BTreeMap::new(),
            duplicate_thresholds: BTreeSet::new(),
        }
    }

    /// Add a threshold. Duplicate metric names invalidate the gate instead of
    /// allowing the last threshold to silently determine acceptance.
    #[must_use]
    pub fn threshold(mut self, threshold: Threshold) -> Self {
        let metric = threshold.metric.clone();
        if self.thresholds.insert(metric.clone(), threshold).is_some() {
            self.duplicate_thresholds.insert(metric);
        }
        self
    }

    /// Load thresholds from a simple JSON map.
    ///
    /// Expected format:
    /// ```json
    /// {
    ///   "metric_name": { "budget": 123.0, "tolerance_pct": 10.0 }
    /// }
    /// ```
    ///
    /// Returns `None` for malformed, duplicate, empty, or invalid thresholds.
    #[must_use]
    pub fn load_json(gate_name: &str, json: &str) -> Option<Self> {
        let parsed = parse_unique_json(json)?;
        let obj = parsed.as_object()?;
        let mut gate = Self::new(gate_name);
        for (metric, value) in obj {
            let budget = value.get("budget")?.as_f64()?;
            let tolerance_pct = match value.get("tolerance_pct") {
                Some(value) => value.as_f64()?,
                None => 0.0,
            };
            let threshold = Threshold::new(metric, budget).tolerance_pct(tolerance_pct);
            if !threshold.is_valid() {
                return None;
            }
            gate = gate.threshold(threshold);
        }
        (!gate.thresholds.is_empty()).then_some(gate)
    }

    /// Load thresholds from FrankenTUI's `tests/baseline.json` format.
    ///
    /// This format uses percentile budgets (`p99_ns`) and `threshold_pct`:
    /// ```json
    /// {
    ///   "frame_render": {
    ///     "p99_ns": 2000000,
    ///     "threshold_pct": 10
    ///   }
    /// }
    /// ```
    ///
    /// Entries whose keys start with `_` are skipped (metadata comments).
    /// The `percentile` parameter selects which budget to use (e.g., `"p99_ns"`).
    ///
    /// Returns `None` for malformed, duplicate, empty, or invalid thresholds.
    /// Metadata-only documents are empty gates and are rejected.
    #[must_use]
    pub fn load_baseline_json(gate_name: &str, json: &str, percentile: &str) -> Option<Self> {
        let parsed = parse_unique_json(json)?;
        let obj = parsed.as_object()?;
        let mut gate = Self::new(gate_name);
        for (metric, value) in obj {
            // Skip metadata keys (e.g., _comment, _format)
            if metric.starts_with('_') {
                continue;
            }
            let budget = value.get(percentile).and_then(|v| v.as_f64())?;
            let tolerance_pct = match value.get("threshold_pct") {
                Some(value) => value.as_f64()?,
                None => 0.0,
            };
            let threshold = Threshold::new(metric, budget).tolerance_pct(tolerance_pct);
            if !threshold.is_valid() {
                return None;
            }
            gate = gate.threshold(threshold);
        }
        (!gate.thresholds.is_empty()).then_some(gate)
    }

    /// Evaluate measurements against thresholds.
    ///
    /// Each configured threshold requires exactly one finite, nonnegative
    /// measurement. Empty gates, duplicate names, and invalid numeric inputs
    /// fail validation. Valid metrics with no matching threshold get
    /// [`MetricVerdict::Unchecked`] and cannot replace required measurements.
    /// Emits structured JSONL evidence via [`TestJsonlLogger`].
    pub fn evaluate(&self, measurements: &[Measurement]) -> GateResult {
        let mut logger = TestJsonlLogger::new_with(&format!("{}_gate", self.gate_name), 0, true, 0);
        logger.add_context_str("gate_name", &self.gate_name);

        logger.log(
            "gate.start",
            &[
                ("gate_name", JsonValue::str(&self.gate_name)),
                (
                    "threshold_count",
                    JsonValue::u64(self.thresholds.len() as u64),
                ),
                (
                    "measurement_count",
                    JsonValue::u64(measurements.len() as u64),
                ),
            ],
        );

        let mut metrics = Vec::new();
        let mut pass_count = 0usize;
        let mut fail_count = 0usize;
        let mut unchecked_count = 0usize;
        let mut validation_errors = Vec::new();
        let mut measurement_counts = BTreeMap::new();
        for measurement in measurements {
            *measurement_counts
                .entry(measurement.metric.as_str())
                .or_insert(0usize) += 1;
        }
        if self.thresholds.is_empty() {
            validation_errors.push("no thresholds configured".to_string());
        }
        for (metric, threshold) in &self.thresholds {
            if !threshold.is_valid() {
                validation_errors.push(format!("invalid threshold for '{metric}'"));
            }
            if self.duplicate_thresholds.contains(metric) {
                validation_errors.push(format!("duplicate threshold for '{metric}'"));
            }
            if !measurement_counts.contains_key(metric.as_str()) {
                validation_errors.push(format!("missing measurement for '{metric}'"));
            }
        }
        for (metric, count) in &measurement_counts {
            if *count > 1 {
                validation_errors.push(format!("duplicate measurements for '{metric}'"));
            }
        }

        for measurement in measurements {
            let valid_measurement = !measurement.metric.trim().is_empty()
                && measurement.value.is_finite()
                && measurement.value >= 0.0;
            if !valid_measurement {
                validation_errors.push(format!("invalid measurement for '{}'", measurement.metric));
            }
            let unique_measurement = measurement_counts[measurement.metric.as_str()] == 1;
            let result = if let Some(threshold) = self.thresholds.get(&measurement.metric) {
                let ceiling = threshold.ceiling();
                let valid_threshold = threshold.is_valid()
                    && !self.duplicate_thresholds.contains(&measurement.metric);
                let overshoot_pct =
                    if valid_measurement && valid_threshold && threshold.budget > 0.0 {
                        let overshoot =
                            (measurement.value - threshold.budget) / threshold.budget * 100.0;
                        overshoot.is_finite().then_some(overshoot)
                    } else if valid_measurement && valid_threshold && measurement.value == 0.0 {
                        Some(0.0)
                    } else {
                        None
                    };
                let verdict = if valid_measurement
                    && unique_measurement
                    && valid_threshold
                    && measurement.value <= ceiling
                {
                    MetricVerdict::Pass
                } else {
                    MetricVerdict::Fail
                };
                MetricResult {
                    metric: measurement.metric.clone(),
                    value: measurement.value,
                    budget: Some(threshold.budget),
                    ceiling: Some(ceiling),
                    tolerance_pct: Some(threshold.tolerance_pct),
                    overshoot_pct,
                    verdict,
                    unit: measurement.unit.clone(),
                }
            } else {
                MetricResult {
                    metric: measurement.metric.clone(),
                    value: measurement.value,
                    budget: None,
                    ceiling: None,
                    tolerance_pct: None,
                    overshoot_pct: None,
                    verdict: if valid_measurement && unique_measurement {
                        MetricVerdict::Unchecked
                    } else {
                        MetricVerdict::Fail
                    },
                    unit: measurement.unit.clone(),
                }
            };

            // Log per-metric evidence
            let verdict_str = match result.verdict {
                MetricVerdict::Pass => "pass",
                MetricVerdict::Fail => "fail",
                MetricVerdict::Unchecked => "unchecked",
            };

            let mut fields: Vec<(&str, JsonValue)> = vec![
                ("metric", JsonValue::str(&result.metric)),
                ("value", json_number(result.value)),
                ("verdict", JsonValue::str(verdict_str)),
            ];
            if let Some(budget) = result.budget {
                fields.push(("budget", json_number(budget)));
            }
            if let Some(ceiling) = result.ceiling {
                fields.push(("ceiling", json_number(ceiling)));
            }
            if let Some(overshoot) = result.overshoot_pct {
                fields.push(("overshoot_pct", json_number(overshoot)));
            }
            logger.log("gate.metric", &fields);

            match result.verdict {
                MetricVerdict::Pass => pass_count += 1,
                MetricVerdict::Fail => fail_count += 1,
                MetricVerdict::Unchecked => unchecked_count += 1,
            }

            metrics.push(result);
        }

        // Sort by metric name for stable output
        metrics.sort_by(|a, b| a.metric.cmp(&b.metric));
        validation_errors.sort();
        validation_errors.dedup();
        for error in &validation_errors {
            logger.log("gate.invalid", &[("reason", JsonValue::str(error))]);
        }

        let result = GateResult {
            gate_name: self.gate_name.clone(),
            metrics,
            pass_count,
            fail_count,
            unchecked_count,
            validation_errors,
        };
        let overall = if result.passed() { "pass" } else { "fail" };
        logger.log(
            "gate.result",
            &[
                ("gate_name", JsonValue::str(&self.gate_name)),
                ("verdict", JsonValue::str(overall)),
                ("pass_count", JsonValue::u64(pass_count as u64)),
                ("fail_count", JsonValue::u64(fail_count as u64)),
                ("unchecked_count", JsonValue::u64(unchecked_count as u64)),
                (
                    "validation_error_count",
                    JsonValue::u64(result.validation_errors.len() as u64),
                ),
            ],
        );

        result
    }
}

/// JSON numbers must remain valid even when the rejected input was NaN or infinity.
fn json_number(value: f64) -> JsonValue {
    JsonValue::raw(
        serde_json::Number::from_f64(value)
            .map(|number| number.to_string())
            .unwrap_or_else(|| "null".to_string()),
    )
}

/// `Value` retains only the last duplicate key. After validating JSON syntax,
/// inspect the original object keys as well so an ambiguous budget cannot pass.
fn parse_unique_json(json: &str) -> Option<serde_json::Value> {
    let parsed = serde_json::from_str(json).ok()?;
    let bytes = json.as_bytes();
    let mut objects: Vec<BTreeSet<String>> = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b'{' => objects.push(BTreeSet::new()),
            b'}' => {
                objects.pop()?;
            }
            b'"' => {
                let start = pos;
                pos += 1;
                while *bytes.get(pos)? != b'"' {
                    if bytes[pos] == b'\\' {
                        pos += 1;
                    }
                    pos += 1;
                }
                let end = pos + 1;
                let mut next = end;
                while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                    next += 1;
                }
                if bytes.get(next) == Some(&b':') {
                    let key = serde_json::from_str(&json[start..end]).ok()?;
                    if !objects.last_mut()?.insert(key) {
                        return None;
                    }
                }
            }
            _ => {}
        }
        pos += 1;
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_ceiling_with_tolerance() {
        let t = Threshold::new("render_p99", 2000.0).tolerance_pct(10.0);
        assert!((t.ceiling() - 2200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn threshold_ceiling_zero_tolerance() {
        let t = Threshold::new("render_p99", 1000.0);
        assert!((t.ceiling() - 1000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn gate_pass_within_budget() {
        let gate = BenchmarkGate::new("test_gate")
            .threshold(Threshold::new("metric_a", 100.0).tolerance_pct(10.0));

        let result = gate.evaluate(&[Measurement::new("metric_a", 95.0)]);
        assert!(result.passed());
        assert_eq!(result.pass_count, 1);
        assert_eq!(result.fail_count, 0);
    }

    #[test]
    fn gate_pass_within_tolerance() {
        let gate = BenchmarkGate::new("test_gate")
            .threshold(Threshold::new("metric_a", 100.0).tolerance_pct(10.0));

        // 105 is above budget (100) but within tolerance (110)
        let result = gate.evaluate(&[Measurement::new("metric_a", 105.0)]);
        assert!(result.passed());
    }

    #[test]
    fn gate_fail_exceeds_tolerance() {
        let gate = BenchmarkGate::new("test_gate")
            .threshold(Threshold::new("metric_a", 100.0).tolerance_pct(10.0));

        // 115 exceeds ceiling of 110
        let result = gate.evaluate(&[Measurement::new("metric_a", 115.0)]);
        assert!(!result.passed());
        assert_eq!(result.fail_count, 1);
    }

    #[test]
    fn gate_unchecked_metric() {
        let gate = BenchmarkGate::new("test_gate").threshold(Threshold::new("metric_a", 100.0));

        let result = gate.evaluate(&[
            Measurement::new("metric_a", 90.0),
            Measurement::new("metric_b", 999.0),
        ]);
        assert!(result.passed());
        assert_eq!(result.unchecked_count, 1);
    }

    #[test]
    fn gate_multiple_metrics_mixed() {
        let gate = BenchmarkGate::new("test_gate")
            .threshold(Threshold::new("fast", 100.0))
            .threshold(Threshold::new("slow", 200.0).tolerance_pct(5.0));

        let result = gate.evaluate(&[
            Measurement::new("fast", 80.0),
            Measurement::new("slow", 250.0), // exceeds 210 ceiling
        ]);
        assert!(!result.passed());
        assert_eq!(result.pass_count, 1);
        assert_eq!(result.fail_count, 1);

        let failures = result.failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].metric, "slow");
    }

    #[test]
    fn gate_load_json() {
        let json = r#"{
            "render_p99": { "budget": 2000.0, "tolerance_pct": 10.0 },
            "diff_p99": { "budget": 500.0 }
        }"#;
        let gate = BenchmarkGate::load_json("perf_gate", json).expect("valid JSON");
        let result = gate.evaluate(&[
            Measurement::new("render_p99", 1800.0),
            Measurement::new("diff_p99", 480.0),
        ]);
        assert!(result.passed());
    }

    #[test]
    fn gate_load_json_invalid() {
        assert!(BenchmarkGate::load_json("bad", "not json").is_none());
    }

    #[test]
    fn gate_load_baseline_json_format() {
        let json = r#"{
            "_comment": "Performance baseline",
            "_format": "p50/p95/p99/p999 in nanoseconds",
            "frame_render": {
                "p50_ns": 500000,
                "p95_ns": 1000000,
                "p99_ns": 2000000,
                "p999_ns": 5000000,
                "threshold_pct": 10
            },
            "diff_strategy": {
                "p50_ns": 50000,
                "p99_ns": 200000,
                "threshold_pct": 10
            }
        }"#;
        let gate = BenchmarkGate::load_baseline_json("perf_gate", json, "p99_ns")
            .expect("baseline JSON should parse");

        // Under budget
        let result = gate.evaluate(&[
            Measurement::new("frame_render", 1_800_000.0).unit("ns"),
            Measurement::new("diff_strategy", 190_000.0).unit("ns"),
        ]);
        assert!(result.passed(), "gate should pass: {}", result.summary());

        // Over budget + tolerance
        let result = gate.evaluate(&[
            Measurement::new("frame_render", 2_500_000.0).unit("ns"), // >2.2M ceiling
            Measurement::new("diff_strategy", 190_000.0).unit("ns"),
        ]);
        assert!(!result.passed(), "gate should fail on regression");
    }

    #[test]
    fn gate_load_baseline_json_skips_metadata() {
        let json = r#"{
            "_comment": "ignored",
            "metric_a": { "p99_ns": 100.0, "threshold_pct": 5 }
        }"#;
        let gate =
            BenchmarkGate::load_baseline_json("meta_test", json, "p99_ns").expect("should parse");
        let result = gate.evaluate(&[Measurement::new("metric_a", 95.0)]);
        assert!(result.passed());
        // The _comment entry should not appear as a threshold
        assert_eq!(result.metrics.len(), 1);
    }

    #[test]
    fn gate_summary_format() {
        let gate = BenchmarkGate::new("summary_test").threshold(Threshold::new("metric_a", 100.0));
        let result = gate.evaluate(&[Measurement::new("metric_a", 90.0).unit("μs")]);
        let summary = result.summary();
        assert!(summary.contains("PASS"));
        assert!(summary.contains("metric_a"));
        assert!(summary.contains("μs"));
    }

    #[test]
    fn gate_overshoot_pct_negative_when_under_budget() {
        let gate =
            BenchmarkGate::new("overshoot_test").threshold(Threshold::new("metric_a", 100.0));
        let result = gate.evaluate(&[Measurement::new("metric_a", 80.0)]);
        let m = &result.metrics[0];
        assert!(m.overshoot_pct.unwrap() < 0.0);
    }

    #[test]
    fn gate_empty_measurements() {
        let gate = BenchmarkGate::new("empty_test").threshold(Threshold::new("metric_a", 100.0));
        let result = gate.evaluate(&[]);
        assert!(!result.passed());
        assert_eq!(result.pass_count, 0);
        assert_eq!(result.fail_count, 0);
        assert_eq!(
            result.validation_errors,
            ["missing measurement for 'metric_a'"]
        );
        assert!(result.summary().contains("FAIL"));
    }

    #[test]
    fn gate_rejects_empty_configuration_even_with_diagnostics() {
        let gate = BenchmarkGate::new("no_thresholds");
        for measurements in [vec![], vec![Measurement::new("diagnostic", 1.0)]] {
            let result = gate.evaluate(&measurements);
            assert!(!result.passed());
            assert_eq!(result.pass_count, 0);
            assert_eq!(result.validation_errors, ["no thresholds configured"]);
        }
    }

    #[test]
    fn gate_requires_every_metric_and_diagnostics_do_not_supply_coverage() {
        let gate = BenchmarkGate::new("coverage")
            .threshold(Threshold::new("fast", 100.0))
            .threshold(Threshold::new("slow", 200.0));
        let result = gate.evaluate(&[
            Measurement::new("fast", 80.0),
            Measurement::new("diagnostic", 1.0),
        ]);
        assert!(!result.passed());
        assert_eq!(result.pass_count, 1);
        assert_eq!(result.unchecked_count, 1);
        assert_eq!(result.validation_errors, ["missing measurement for 'slow'"]);

        let complete = gate.evaluate(&[
            Measurement::new("slow", 200.0),
            Measurement::new("diagnostic", 1.0),
            Measurement::new("fast", 0.0),
        ]);
        assert!(complete.passed(), "{}", complete.summary());
        assert_eq!(complete.pass_count, 2);
        assert_eq!(complete.unchecked_count, 1);
        assert!(complete.validation_errors.is_empty());
    }

    #[test]
    fn gate_rejects_duplicate_thresholds_even_if_identical_or_relaxed() {
        for second_budget in [100.0, 10_000.0] {
            let gate = BenchmarkGate::new("duplicate_threshold")
                .threshold(Threshold::new("metric", 100.0))
                .threshold(Threshold::new("metric", second_budget));
            let result = gate.evaluate(&[Measurement::new("metric", 1.0)]);
            assert!(!result.passed());
            assert_eq!(result.fail_count, 1);
            assert_eq!(
                result.validation_errors,
                ["duplicate threshold for 'metric'"]
            );
        }
    }

    #[test]
    fn gate_rejects_duplicate_measurements_and_diagnostics() {
        let gate =
            BenchmarkGate::new("duplicate_measurement").threshold(Threshold::new("metric", 100.0));
        for metric in ["metric", "diagnostic"] {
            let mut measurements = vec![Measurement::new("metric", 1.0)];
            if metric == "diagnostic" {
                measurements.push(Measurement::new(metric, 1.0));
            }
            measurements.push(Measurement::new(metric, 1.0));
            let result = gate.evaluate(&measurements);
            assert!(!result.passed());
            assert_eq!(result.fail_count, 2);
            assert_eq!(
                result.validation_errors,
                [format!("duplicate measurements for '{metric}'")]
            );
        }
    }

    #[test]
    fn gate_rejects_invalid_values_including_unknown_metrics() {
        let gate = BenchmarkGate::new("invalid_values").threshold(Threshold::new("metric", 100.0));
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01] {
            for metric in ["metric", "diagnostic"] {
                let mut measurements = vec![Measurement::new(metric, value)];
                if metric == "diagnostic" {
                    measurements.push(Measurement::new("metric", 1.0));
                }
                let result = gate.evaluate(&measurements);
                assert!(!result.passed(), "{metric}: {value}");
                assert_eq!(result.fail_count, 1);
                assert_eq!(
                    result.validation_errors,
                    [format!("invalid measurement for '{metric}'")]
                );
            }
        }
    }

    #[test]
    fn gate_rejects_invalid_threshold_numbers_and_overflowed_ceilings() {
        let invalid = [
            Threshold::new("metric", f64::NAN),
            Threshold::new("metric", f64::INFINITY),
            Threshold::new("metric", f64::NEG_INFINITY),
            Threshold::new("metric", -1.0),
            Threshold::new("metric", 1.0).tolerance_pct(f64::NAN),
            Threshold::new("metric", 1.0).tolerance_pct(f64::INFINITY),
            Threshold::new("metric", 1.0).tolerance_pct(-1.0),
            Threshold::new("metric", 1.0).tolerance_pct(101.0),
            Threshold::new("metric", f64::MAX).tolerance_pct(100.0),
        ];
        for threshold in invalid {
            let gate = BenchmarkGate::new("invalid_threshold").threshold(threshold);
            let result = gate.evaluate(&[Measurement::new("metric", 0.0)]);
            assert!(!result.passed(), "{}", result.summary());
            assert_eq!(result.fail_count, 1);
            assert_eq!(result.validation_errors, ["invalid threshold for 'metric'"]);
        }
    }

    #[test]
    fn gate_rejects_blank_metric_names() {
        for metric in ["", " \t\n"] {
            let gate = BenchmarkGate::new("blank_name").threshold(Threshold::new(metric, 100.0));
            let result = gate.evaluate(&[Measurement::new(metric, 0.0)]);
            assert!(!result.passed());
            assert_eq!(result.fail_count, 1);
            assert_eq!(result.validation_errors.len(), 2);
        }
    }

    #[test]
    fn gate_accepts_zero_budget_and_inclusive_tolerance_boundaries() {
        let gate = BenchmarkGate::new("boundaries")
            .threshold(Threshold::new("zero", 0.0).tolerance_pct(100.0))
            .threshold(Threshold::new("doubled", 1.0).tolerance_pct(100.0));
        let result = gate.evaluate(&[
            Measurement::new("zero", 0.0),
            Measurement::new("doubled", 2.0),
        ]);
        assert!(result.passed());
        assert_eq!(result.pass_count, 2);
        let result = gate.evaluate(&[
            Measurement::new("zero", f64::MIN_POSITIVE),
            Measurement::new("doubled", 2.1),
        ]);
        assert!(!result.passed());
        assert_eq!(result.fail_count, 2);
        assert!(result.validation_errors.is_empty());
        assert!(
            result
                .failures()
                .iter()
                .any(|metric| metric.metric == "zero" && metric.overshoot_pct.is_none())
        );
    }

    #[test]
    fn rejected_nonfinite_values_produce_valid_json_evidence() {
        let logger = TestJsonlLogger::new_with("invalid_numeric_json", 0, true, 0);
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let line = logger.emit_line("gate.metric", &[("value", json_number(value))]);
            let parsed: serde_json::Value =
                serde_json::from_str(&line).expect("valid JSON evidence");
            assert!(parsed["value"].is_null());
        }
        for value in [0.0, f64::MIN_POSITIVE, 1.0, f64::MAX] {
            let line = logger.emit_line("gate.metric", &[("value", json_number(value))]);
            let parsed: serde_json::Value =
                serde_json::from_str(&line).expect("valid JSON evidence");
            assert_eq!(parsed["value"].as_f64(), Some(value));
        }
    }

    #[test]
    fn loaders_reject_ambiguous_and_invalid_documents() {
        let invalid_json = [
            "{}",
            "[]",
            "null",
            r#"{"metric":{}}"#,
            r#"{"metric":{"budget":"100"}}"#,
            r#"{"metric":{"budget":null}}"#,
            r#"{"metric":{"budget":-1}}"#,
            r#"{"metric":{"budget":1e400}}"#,
            r#"{"metric":{"budget":1,"tolerance_pct":null}}"#,
            r#"{"metric":{"budget":1,"tolerance_pct":"10"}}"#,
            r#"{"metric":{"budget":1,"tolerance_pct":-1}}"#,
            r#"{"metric":{"budget":1,"tolerance_pct":101}}"#,
            r#"{"metric":{"budget":1e308,"tolerance_pct":100}}"#,
            r#"{" ":{"budget":1}}"#,
            r#"{"metric":{"budget":1},"metric":{"budget":1000}}"#,
            r#"{"metric":{"budget":1},"\u006detric":{"budget":1000}}"#,
            r#"{"metric":{"budget":1,"budget":1000}}"#,
            r#"{"metric":{"budget":1,"tolerance_pct":0,"tolerance_pct":100}}"#,
        ];
        for json in invalid_json {
            assert!(
                BenchmarkGate::load_json("invalid", json).is_none(),
                "{json}"
            );
        }

        let invalid_baselines = [
            "{}",
            r#"{"_comment":"no thresholds"}"#,
            r#"{"metric":{"p50_ns":1}}"#,
            r#"{"metric":{"p99_ns":-1}}"#,
            r#"{"metric":{"p99_ns":1,"threshold_pct":null}}"#,
            r#"{"metric":{"p99_ns":1,"threshold_pct":"10"}}"#,
            r#"{"metric":{"p99_ns":1,"threshold_pct":101}}"#,
            r#"{"metric":{"p99_ns":1,"p99_ns":1000}}"#,
            r#"{"metric":{"p99_ns":1},"metric":{"p99_ns":1000}}"#,
        ];
        for json in invalid_baselines {
            assert!(
                BenchmarkGate::load_baseline_json("invalid", json, "p99_ns").is_none(),
                "{json}"
            );
        }
    }

    #[test]
    fn baseline_loader_accepts_escaped_names_and_nested_metadata() {
        let json = r#"{
            "_metadata": [{"text":"braces { } and escaped quote \""}, {"text":"ok"}],
            "m\u00e9tric": {"p99_ns":100,"threshold_pct":0},
            "other": {"p99_ns":0}
        }"#;
        let gate = BenchmarkGate::load_baseline_json("escaped", json, "p99_ns").unwrap();
        let result = gate.evaluate(&[
            Measurement::new("métric", 100.0),
            Measurement::new("other", 0.0),
        ]);
        assert!(result.passed(), "{}", result.summary());
        assert_eq!(result.pass_count, 2);
    }

    // =========================================================================
    // Runtime benchmark gate tests (bd-1vb19)
    // =========================================================================

    /// Synthetic contract inputs covering every configured baseline metric.
    /// These validate gate behavior; they are not measured performance evidence.
    fn baseline_measurements() -> Vec<Measurement> {
        vec![
            Measurement::new("frame_render", 1_000_000.0).unit("ns"),
            Measurement::new("layout_computation", 10_000.0).unit("ns"),
            Measurement::new("diff_strategy", 100_000.0).unit("ns"),
            Measurement::new("diff_strategy_large", 400_000.0).unit("ns"),
            Measurement::new("widget_render_block", 25_000.0).unit("ns"),
            Measurement::new("widget_render_table", 60_000.0).unit("ns"),
            Measurement::new("ansi_emit", 200_000.0).unit("ns"),
            Measurement::new("buffer_new_80x24", 10_000.0).unit("ns"),
            Measurement::new("buffer_new_200x60", 40_000.0).unit("ns"),
            Measurement::new("cell_bits_eq", 5.0).unit("ns"),
            Measurement::new("runtime_shutdown_latency", 1_000_000.0).unit("ns"),
            Measurement::new("runtime_first_frame", 5_000_000.0).unit("ns"),
            Measurement::new("runtime_command_roundtrip", 100_000.0).unit("ns"),
            Measurement::new("runtime_effect_queue_drain", 500_000.0).unit("ns"),
        ]
    }

    #[test]
    fn load_baseline_includes_runtime_benchmarks() {
        let json = include_str!("../../../tests/baseline.json");
        let gate = BenchmarkGate::load_baseline_json("runtime_gate", json, "p99_ns")
            .expect("baseline.json should parse");

        // Verify runtime benchmarks were loaded
        let metrics: Vec<&str> = gate
            .thresholds
            .keys()
            .filter(|k| k.starts_with("runtime_"))
            .map(|k| k.as_str())
            .collect();
        assert!(
            metrics.contains(&"runtime_shutdown_latency"),
            "shutdown_latency baseline should be loaded"
        );
        assert!(
            metrics.contains(&"runtime_first_frame"),
            "first_frame baseline should be loaded"
        );
        assert!(
            metrics.contains(&"runtime_command_roundtrip"),
            "command_roundtrip baseline should be loaded"
        );
        assert!(
            metrics.contains(&"runtime_effect_queue_drain"),
            "effect_queue_drain baseline should be loaded"
        );
    }

    #[test]
    fn runtime_gate_passes_within_budget() {
        let json = include_str!("../../../tests/baseline.json");
        let gate = BenchmarkGate::load_baseline_json("runtime_gate", json, "p99_ns")
            .expect("baseline.json should parse");

        // Simulate all required measurements well within budget.
        let measurements = baseline_measurements();
        let result = gate.evaluate(&measurements);
        assert!(
            result.passed(),
            "all runtime metrics should pass: {}",
            result.summary()
        );
        assert_eq!(result.pass_count, 14);
        assert!(result.validation_errors.is_empty());
    }

    #[test]
    fn runtime_gate_fails_on_regression() {
        let json = include_str!("../../../tests/baseline.json");
        let gate = BenchmarkGate::load_baseline_json("runtime_gate", json, "p99_ns")
            .expect("baseline.json should parse");

        // Simulate a severe regression on shutdown latency
        let mut measurements = baseline_measurements();
        measurements
            .iter_mut()
            .find(|measurement| measurement.metric == "runtime_shutdown_latency")
            .unwrap()
            .value = 100_000_000.0; // 100ms, way over 5ms budget
        let result = gate.evaluate(&measurements);
        assert!(!result.passed(), "regression should fail the gate");
        assert_eq!(result.fail_count, 1);
        assert_eq!(result.pass_count, 13);
        assert!(result.validation_errors.is_empty());

        let failures = result.failures();
        assert!(
            failures
                .iter()
                .any(|f| f.metric == "runtime_shutdown_latency"),
            "shutdown latency should be the failing metric"
        );
    }

    #[test]
    fn runtime_gate_summary_readable() {
        let json = include_str!("../../../tests/baseline.json");
        let gate = BenchmarkGate::load_baseline_json("runtime_gate", json, "p99_ns")
            .expect("baseline.json should parse");

        let measurements = baseline_measurements();
        let result = gate.evaluate(&measurements);
        let summary = result.summary();
        assert!(summary.contains("runtime_shutdown_latency"));
        assert!(summary.contains("PASS"));
    }
}

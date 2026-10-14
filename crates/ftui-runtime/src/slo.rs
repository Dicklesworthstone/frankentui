#![forbid(unsafe_code)]

//! SLO schema, breach detection, and safe-mode enforcement (bd-2xj.2).
//!
//! Provides machine-readable SLO definitions with data-plane and decision-plane
//! budgets. Integrates with the metrics registry (`BuiltinCounter::SloBreachesTotal`)
//! and structured tracing (`slo.check` span).
//!
//! # Architecture
//!
//! ```text
//! slo.yaml  ──parse──▶  SloSchema
//!                          │
//!         observations ──▶ check_breach() ──▶ BreachResult
//!                                               │
//!                          batch ──▶ check_safe_mode() ──▶ SafeModeDecision
//!                                                            │
//!                                   emit_slo_check() ──▶ tracing span + event
//! ```
//!
//! # Tracing contract
//!
//! - Span: `slo.check` with fields `metric_name`, `metric_type`, `baseline`,
//!   `current`, `ratio`, `severity`.
//! - WARN event for breach, ERROR event for safe-mode trigger.
//! - METRICS: `ftui_slo_breaches_total` counter incremented on breach.

use std::collections::HashMap;

// ============================================================================
// Types
// ============================================================================

/// Metric type for SLO enforcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricType {
    /// Latency in microseconds (p50, p95, p99, p999).
    Latency,
    /// Memory usage in bytes or counts.
    Memory,
    /// Error rate as a fraction (0.0-1.0).
    ErrorRate,
}

/// Per-metric SLO definition.
#[derive(Debug, Clone)]
pub struct MetricSlo {
    /// What kind of metric this is.
    pub metric_type: MetricType,
    /// Maximum absolute value allowed (breach if exceeded).
    pub max_value: Option<f64>,
    /// Maximum ratio vs baseline before breach.
    pub max_ratio: Option<f64>,
    /// If true, breaching this metric triggers safe-mode immediately.
    pub safe_mode_trigger: bool,
}

/// Validated SLO configuration parsed from slo.yaml.
#[derive(Debug, Clone)]
pub struct SloSchema {
    /// Global regression threshold as fraction (e.g., 0.10 = 10%).
    pub regression_threshold: f64,
    /// Global noise tolerance as fraction.
    pub noise_tolerance: f64,
    /// Per-metric SLO definitions.
    pub metrics: HashMap<String, MetricSlo>,
    /// Number of simultaneous breaches that triggers safe-mode.
    pub safe_mode_breach_count: usize,
    /// Error rate above which safe-mode is triggered regardless.
    pub safe_mode_error_rate: f64,
}

/// Schema validation error.
#[derive(Debug, Clone, PartialEq)]
pub enum SloSchemaError {
    /// A threshold value is out of range.
    InvalidThreshold { field: String, value: f64 },
    /// A required field is missing.
    MissingField(String),
    /// A value failed to parse.
    ParseError { field: String, reason: String },
    /// Unknown metric type.
    UnknownMetricType(String),
    /// Duplicate metric definition.
    DuplicateMetric(String),
    /// General malformed structure.
    MalformedStructure(String),
}

impl std::fmt::Display for SloSchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidThreshold { field, value } => {
                write!(f, "invalid threshold for '{field}': {value}")
            }
            Self::MissingField(field) => write!(f, "missing required field: '{field}'"),
            Self::ParseError { field, reason } => {
                write!(f, "parse error for '{field}': {reason}")
            }
            Self::UnknownMetricType(t) => write!(f, "unknown metric type: '{t}'"),
            Self::DuplicateMetric(name) => write!(f, "duplicate metric: '{name}'"),
            Self::MalformedStructure(msg) => write!(f, "malformed structure: {msg}"),
        }
    }
}

impl std::error::Error for SloSchemaError {}

/// Result of a breach check for a single metric.
#[derive(Debug, Clone)]
pub struct BreachResult {
    pub metric_name: String,
    pub metric_type: MetricType,
    pub baseline: f64,
    pub current: f64,
    pub ratio: f64,
    pub severity: BreachSeverity,
    pub safe_mode_trigger: bool,
}

/// Severity of a detected breach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreachSeverity {
    /// No breach detected.
    None,
    /// Within noise tolerance.
    Noise,
    /// Exceeded regression threshold.
    Breach,
    /// Absolute SLO value exceeded.
    AbsoluteBreach,
}

/// Safe-mode decision from batch breach evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeModeDecision {
    /// Normal operation continues.
    Normal,
    /// Safe-mode triggered with reason.
    Triggered(String),
}

// ============================================================================
// Defaults
// ============================================================================

impl Default for SloSchema {
    fn default() -> Self {
        Self {
            regression_threshold: 0.10,
            noise_tolerance: 0.05,
            metrics: HashMap::new(),
            safe_mode_breach_count: 3,
            safe_mode_error_rate: 0.10,
        }
    }
}

// ============================================================================
// Schema Parsing
// ============================================================================

/// Parse and validate slo.yaml content.
///
/// Returns a validated `SloSchema` or a list of validation errors.
pub fn parse_slo_yaml(yaml: &str) -> Result<SloSchema, Vec<SloSchemaError>> {
    let mut schema = SloSchema::default();
    let mut errors = Vec::new();
    let mut in_metrics = false;
    let mut current_metric: Option<String> = None;
    let mut current_slo = MetricSlo {
        metric_type: MetricType::Latency,
        max_value: None,
        max_ratio: None,
        safe_mode_trigger: false,
    };
    let mut seen_metrics = std::collections::HashSet::new();

    for (line_num, line) in yaml.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.contains('\t') {
            errors.push(SloSchemaError::MalformedStructure(format!(
                "line {}: tabs not allowed, use spaces",
                line_num + 1
            )));
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("regression_threshold:") {
            parse_threshold(
                value.trim(),
                "regression_threshold",
                &mut schema.regression_threshold,
                &mut errors,
            );
        } else if let Some(value) = trimmed.strip_prefix("noise_tolerance:") {
            parse_threshold(
                value.trim(),
                "noise_tolerance",
                &mut schema.noise_tolerance,
                &mut errors,
            );
        } else if let Some(value) = trimmed.strip_prefix("safe_mode_breach_count:") {
            match value.trim().parse::<usize>() {
                Ok(v) if v > 0 => schema.safe_mode_breach_count = v,
                Ok(_) => errors.push(SloSchemaError::InvalidThreshold {
                    field: "safe_mode_breach_count".into(),
                    value: 0.0,
                }),
                Err(e) => errors.push(SloSchemaError::ParseError {
                    field: "safe_mode_breach_count".into(),
                    reason: e.to_string(),
                }),
            }
        } else if let Some(value) = trimmed.strip_prefix("safe_mode_error_rate:") {
            parse_threshold(
                value.trim(),
                "safe_mode_error_rate",
                &mut schema.safe_mode_error_rate,
                &mut errors,
            );
        } else if trimmed == "metrics:" {
            in_metrics = true;
        } else if in_metrics
            && trimmed.ends_with(':')
            && !trimmed.starts_with("max_")
            && !trimmed.starts_with("metric_type:")
            && !trimmed.starts_with("safe_mode")
        {
            // Flush previous metric
            if let Some(ref name) = current_metric {
                schema.metrics.insert(name.clone(), current_slo.clone());
            }
            let metric_name = trimmed.trim_end_matches(':').to_string();
            if !seen_metrics.insert(metric_name.clone()) {
                errors.push(SloSchemaError::DuplicateMetric(metric_name.clone()));
            }
            current_metric = Some(metric_name);
            current_slo = MetricSlo {
                metric_type: MetricType::Latency,
                max_value: None,
                max_ratio: None,
                safe_mode_trigger: false,
            };
        } else if let Some(value) = trimmed.strip_prefix("metric_type:") {
            match value.trim() {
                "latency" => current_slo.metric_type = MetricType::Latency,
                "memory" => current_slo.metric_type = MetricType::Memory,
                "error_rate" => current_slo.metric_type = MetricType::ErrorRate,
                other => errors.push(SloSchemaError::UnknownMetricType(other.to_string())),
            }
        } else if let Some(value) = trimmed.strip_prefix("max_value:") {
            match value.trim().parse::<f64>() {
                Ok(v) => current_slo.max_value = Some(v),
                Err(e) => errors.push(SloSchemaError::ParseError {
                    field: "max_value".into(),
                    reason: e.to_string(),
                }),
            }
        } else if let Some(value) = trimmed.strip_prefix("max_ratio:") {
            match value.trim().parse::<f64>() {
                Ok(v) => current_slo.max_ratio = Some(v),
                Err(e) => errors.push(SloSchemaError::ParseError {
                    field: "max_ratio".into(),
                    reason: e.to_string(),
                }),
            }
        } else if let Some(value) = trimmed.strip_prefix("safe_mode_trigger:") {
            match value.trim() {
                "true" => current_slo.safe_mode_trigger = true,
                "false" => current_slo.safe_mode_trigger = false,
                other => errors.push(SloSchemaError::ParseError {
                    field: "safe_mode_trigger".into(),
                    reason: format!("expected 'true' or 'false', got '{other}'"),
                }),
            }
        }
    }

    // Flush last metric
    if let Some(ref name) = current_metric {
        schema.metrics.insert(name.clone(), current_slo);
    }

    // Cross-field validation
    if schema.noise_tolerance >= schema.regression_threshold {
        errors.push(SloSchemaError::InvalidThreshold {
            field: "noise_tolerance".into(),
            value: schema.noise_tolerance,
        });
    }

    if errors.is_empty() {
        Ok(schema)
    } else {
        Err(errors)
    }
}

fn parse_threshold(value: &str, field: &str, target: &mut f64, errors: &mut Vec<SloSchemaError>) {
    match value.parse::<f64>() {
        Ok(v) if (0.0..=1.0).contains(&v) => *target = v,
        Ok(v) => errors.push(SloSchemaError::InvalidThreshold {
            field: field.into(),
            value: v,
        }),
        Err(e) => errors.push(SloSchemaError::ParseError {
            field: field.into(),
            reason: e.to_string(),
        }),
    }
}

// ============================================================================
// Breach Detection
// ============================================================================

/// Check a single metric observation against its SLO.
pub fn check_breach(
    metric_name: &str,
    baseline: f64,
    current: f64,
    schema: &SloSchema,
) -> BreachResult {
    let ratio = if baseline > 0.0 {
        current / baseline
    } else {
        1.0
    };

    let metric_slo = schema.metrics.get(metric_name);
    let metric_type = metric_slo
        .map(|s| s.metric_type.clone())
        .unwrap_or(MetricType::Latency);
    let safe_mode_trigger = metric_slo.map(|s| s.safe_mode_trigger).unwrap_or(false);

    // Check absolute threshold first
    if let Some(slo) = metric_slo {
        if let Some(max_val) = slo.max_value
            && current > max_val
        {
            return BreachResult {
                metric_name: metric_name.to_string(),
                metric_type,
                baseline,
                current,
                ratio,
                severity: BreachSeverity::AbsoluteBreach,
                safe_mode_trigger,
            };
        }
        if let Some(max_ratio) = slo.max_ratio
            && ratio > max_ratio
        {
            return BreachResult {
                metric_name: metric_name.to_string(),
                metric_type,
                baseline,
                current,
                ratio,
                severity: BreachSeverity::Breach,
                safe_mode_trigger,
            };
        }
    }

    // Global threshold check
    let change_pct = ratio - 1.0;
    let severity = if change_pct > schema.regression_threshold {
        BreachSeverity::Breach
    } else if change_pct > schema.noise_tolerance {
        BreachSeverity::Noise
    } else {
        BreachSeverity::None
    };

    BreachResult {
        metric_name: metric_name.to_string(),
        metric_type,
        baseline,
        current,
        ratio,
        severity,
        safe_mode_trigger,
    }
}

/// Evaluate safe-mode trigger from a batch of breach results.
pub fn check_safe_mode(breaches: &[BreachResult], schema: &SloSchema) -> SafeModeDecision {
    // Check for explicit safe-mode triggers
    for b in breaches {
        if b.safe_mode_trigger
            && (b.severity == BreachSeverity::Breach
                || b.severity == BreachSeverity::AbsoluteBreach)
        {
            return SafeModeDecision::Triggered(format!(
                "metric '{}' breached with safe_mode_trigger=true (ratio={:.3})",
                b.metric_name, b.ratio
            ));
        }
    }

    // Check error rate threshold
    for b in breaches {
        if b.metric_type == MetricType::ErrorRate && b.current > schema.safe_mode_error_rate {
            return SafeModeDecision::Triggered(format!(
                "error rate '{}' at {:.3} exceeds safe_mode_error_rate {:.3}",
                b.metric_name, b.current, schema.safe_mode_error_rate
            ));
        }
    }

    // Check simultaneous breach count
    let breach_count = breaches
        .iter()
        .filter(|b| {
            b.severity == BreachSeverity::Breach || b.severity == BreachSeverity::AbsoluteBreach
        })
        .count();

    if breach_count >= schema.safe_mode_breach_count {
        return SafeModeDecision::Triggered(format!(
            "{breach_count} simultaneous breaches (threshold: {})",
            schema.safe_mode_breach_count
        ));
    }

    SafeModeDecision::Normal
}

/// Emit a tracing span for SLO check and log appropriate events.
///
/// Creates an `slo.check` span with fields: `metric_name`, `metric_type`,
/// `baseline`, `current`, `ratio`, `severity`.
///
/// Emits:
/// - ERROR for safe-mode trigger
/// - WARN for breach
/// - DEBUG for noise
/// - TRACE for within-SLO
pub fn emit_slo_check(breach: &BreachResult, safe_mode: &SafeModeDecision) {
    let span = tracing::info_span!(
        "slo.check",
        metric_name = breach.metric_name.as_str(),
        metric_type = ?breach.metric_type,
        baseline = breach.baseline,
        current = breach.current,
        ratio = breach.ratio,
        severity = ?breach.severity,
    );
    let _guard = span.enter();

    match safe_mode {
        SafeModeDecision::Triggered(reason) => {
            tracing::error!(
                target: crate::telemetry_schema::TARGET_RUNTIME,
                metric = breach.metric_name.as_str(),
                ratio = breach.ratio,
                reason = reason.as_str(),
                "safe-mode triggered"
            );
        }
        SafeModeDecision::Normal => match breach.severity {
            BreachSeverity::Breach | BreachSeverity::AbsoluteBreach => {
                tracing::warn!(
                    target: crate::telemetry_schema::TARGET_RUNTIME,
                    metric = breach.metric_name.as_str(),
                    baseline = breach.baseline,
                    current = breach.current,
                    ratio = breach.ratio,
                    severity = ?breach.severity,
                    "SLO breach detected"
                );
            }
            BreachSeverity::Noise => {
                tracing::debug!(
                    target: crate::telemetry_schema::TARGET_RUNTIME,
                    metric = breach.metric_name.as_str(),
                    ratio = breach.ratio,
                    "noise-level change within tolerance"
                );
            }
            BreachSeverity::None => {
                tracing::trace!(
                    target: crate::telemetry_schema::TARGET_RUNTIME,
                    metric = breach.metric_name.as_str(),
                    ratio = breach.ratio,
                    "metric within SLO"
                );
            }
        },
    }
}

/// Run a full SLO check batch: parse schema, check all metrics, evaluate safe-mode.
///
/// Returns `(Vec<BreachResult>, SafeModeDecision)`.
pub fn run_slo_check(
    schema: &SloSchema,
    observations: &[(&str, f64, f64)], // (metric_name, baseline, current)
) -> (Vec<BreachResult>, SafeModeDecision) {
    let breaches: Vec<BreachResult> = observations
        .iter()
        .map(|(name, baseline, current)| check_breach(name, *baseline, *current, schema))
        .collect();

    let safe_mode = check_safe_mode(&breaches, schema);

    // Emit tracing spans
    for b in &breaches {
        emit_slo_check(b, &safe_mode);
    }

    (breaches, safe_mode)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_valid_yaml() {
        let yaml = r#"
regression_threshold: 0.10
noise_tolerance: 0.05
metrics:
  render_p99:
    metric_type: latency
    max_value: 4000.0
    max_ratio: 1.25
    safe_mode_trigger: true
"#;
        let schema = parse_slo_yaml(yaml).expect("should parse");
        assert_eq!(schema.metrics.len(), 1);
        let m = schema.metrics.get("render_p99").unwrap();
        assert_eq!(m.metric_type, MetricType::Latency);
        assert_eq!(m.max_value, Some(4000.0));
        assert!(m.safe_mode_trigger);
    }

    #[test]
    fn parse_empty_uses_defaults() {
        let schema = parse_slo_yaml("").expect("empty should use defaults");
        assert!((schema.regression_threshold - 0.10).abs() < f64::EPSILON);
        assert!((schema.noise_tolerance - 0.05).abs() < f64::EPSILON);
        assert_eq!(schema.safe_mode_breach_count, 3);
        assert!(schema.metrics.is_empty());
    }

    #[test]
    fn reject_invalid_threshold() {
        let yaml = "regression_threshold: 1.5\nnoise_tolerance: 0.05\n";
        let errors = parse_slo_yaml(yaml).unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            SloSchemaError::InvalidThreshold { field, .. } if field == "regression_threshold"
        )));
    }

    #[test]
    fn reject_noise_gte_regression() {
        let yaml = "regression_threshold: 0.05\nnoise_tolerance: 0.10\n";
        let errors = parse_slo_yaml(yaml).unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            SloSchemaError::InvalidThreshold { field, .. } if field == "noise_tolerance"
        )));
    }

    #[test]
    fn reject_unknown_metric_type() {
        let yaml = "regression_threshold: 0.10\nnoise_tolerance: 0.05\nmetrics:\n  m:\n    metric_type: throughput\n";
        let errors = parse_slo_yaml(yaml).unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            SloSchemaError::UnknownMetricType(t) if t == "throughput"
        )));
    }

    #[test]
    fn reject_duplicate_metric() {
        let yaml = "regression_threshold: 0.10\nnoise_tolerance: 0.05\nmetrics:\n  m:\n    metric_type: latency\n  m:\n    metric_type: latency\n";
        let errors = parse_slo_yaml(yaml).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, SloSchemaError::DuplicateMetric(_)))
        );
    }

    #[test]
    fn breach_absolute_threshold() {
        let schema = SloSchema {
            metrics: {
                let mut m = HashMap::new();
                m.insert(
                    "p99".into(),
                    MetricSlo {
                        metric_type: MetricType::Latency,
                        max_value: Some(500.0),
                        max_ratio: Some(1.15),
                        safe_mode_trigger: false,
                    },
                );
                m
            },
            ..SloSchema::default()
        };
        let result = check_breach("p99", 400.0, 520.0, &schema);
        assert_eq!(result.severity, BreachSeverity::AbsoluteBreach);
    }

    #[test]
    fn breach_ratio_threshold() {
        let schema = SloSchema {
            metrics: {
                let mut m = HashMap::new();
                m.insert(
                    "p99".into(),
                    MetricSlo {
                        metric_type: MetricType::Latency,
                        max_value: Some(1000.0),
                        max_ratio: Some(1.10),
                        safe_mode_trigger: false,
                    },
                );
                m
            },
            ..SloSchema::default()
        };
        let result = check_breach("p99", 400.0, 480.0, &schema);
        assert_eq!(result.severity, BreachSeverity::Breach);
    }

    #[test]
    fn within_slo_no_breach() {
        let schema = SloSchema {
            metrics: {
                let mut m = HashMap::new();
                m.insert(
                    "p99".into(),
                    MetricSlo {
                        metric_type: MetricType::Latency,
                        max_value: Some(500.0),
                        max_ratio: Some(1.15),
                        safe_mode_trigger: false,
                    },
                );
                m
            },
            ..SloSchema::default()
        };
        let result = check_breach("p99", 400.0, 404.0, &schema);
        assert_eq!(result.severity, BreachSeverity::None);
    }

    #[test]
    fn safe_mode_triggered_by_flag() {
        let schema = SloSchema::default();
        let breaches = vec![BreachResult {
            metric_name: "critical".into(),
            metric_type: MetricType::Latency,
            baseline: 200.0,
            current: 600.0,
            ratio: 3.0,
            severity: BreachSeverity::Breach,
            safe_mode_trigger: true,
        }];
        let decision = check_safe_mode(&breaches, &schema);
        assert!(matches!(decision, SafeModeDecision::Triggered(_)));
    }

    #[test]
    fn safe_mode_triggered_by_error_rate() {
        let schema = SloSchema {
            safe_mode_error_rate: 0.10,
            ..SloSchema::default()
        };
        let breaches = vec![BreachResult {
            metric_name: "errors".into(),
            metric_type: MetricType::ErrorRate,
            baseline: 0.02,
            current: 0.15,
            ratio: 7.5,
            severity: BreachSeverity::Breach,
            safe_mode_trigger: false,
        }];
        let decision = check_safe_mode(&breaches, &schema);
        assert!(matches!(decision, SafeModeDecision::Triggered(ref r) if r.contains("error rate")));
    }

    #[test]
    fn safe_mode_triggered_by_breach_count() {
        let schema = SloSchema {
            safe_mode_breach_count: 2,
            ..SloSchema::default()
        };
        let breaches = vec![
            BreachResult {
                metric_name: "a".into(),
                metric_type: MetricType::Latency,
                baseline: 100.0,
                current: 200.0,
                ratio: 2.0,
                severity: BreachSeverity::Breach,
                safe_mode_trigger: false,
            },
            BreachResult {
                metric_name: "b".into(),
                metric_type: MetricType::Memory,
                baseline: 1000.0,
                current: 3000.0,
                ratio: 3.0,
                severity: BreachSeverity::AbsoluteBreach,
                safe_mode_trigger: false,
            },
        ];
        let decision = check_safe_mode(&breaches, &schema);
        assert!(
            matches!(decision, SafeModeDecision::Triggered(ref r) if r.contains("simultaneous"))
        );
    }

    #[test]
    fn safe_mode_not_triggered_below_thresholds() {
        let schema = SloSchema::default();
        let breaches = vec![BreachResult {
            metric_name: "ok".into(),
            metric_type: MetricType::Latency,
            baseline: 100.0,
            current: 115.0,
            ratio: 1.15,
            severity: BreachSeverity::Breach,
            safe_mode_trigger: false,
        }];
        let decision = check_safe_mode(&breaches, &schema);
        assert_eq!(decision, SafeModeDecision::Normal);
    }

    #[test]
    fn zero_baseline_no_panic() {
        let schema = SloSchema::default();
        let result = check_breach("zero", 0.0, 5.0, &schema);
        assert!((result.ratio - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn improvement_not_flagged() {
        let schema = SloSchema::default();
        let result = check_breach("improving", 200.0, 150.0, &schema);
        assert_eq!(result.severity, BreachSeverity::None);
    }

    #[test]
    fn run_slo_check_batch_normal() {
        let schema = SloSchema {
            metrics: {
                let mut m = HashMap::new();
                m.insert(
                    "p99".into(),
                    MetricSlo {
                        metric_type: MetricType::Latency,
                        max_value: Some(500.0),
                        max_ratio: Some(1.15),
                        safe_mode_trigger: false,
                    },
                );
                m
            },
            ..SloSchema::default()
        };
        let observations = vec![("p99", 400.0, 404.0)];
        let (breaches, decision) = run_slo_check(&schema, &observations);
        assert_eq!(breaches.len(), 1);
        assert_eq!(decision, SafeModeDecision::Normal);
    }

    #[test]
    fn schema_error_display() {
        let err = SloSchemaError::InvalidThreshold {
            field: "regression_threshold".into(),
            value: 1.5,
        };
        let msg = err.to_string();
        assert!(msg.contains("regression_threshold"));
        assert!(msg.contains("1.5"));
    }

    #[test]
    fn parse_all_three_metric_types() {
        let yaml = r#"
regression_threshold: 0.10
noise_tolerance: 0.05
metrics:
  lat:
    metric_type: latency
    max_value: 100.0
  mem:
    metric_type: memory
    max_value: 1000.0
  err:
    metric_type: error_rate
    max_value: 0.01
"#;
        let schema = parse_slo_yaml(yaml).unwrap();
        assert_eq!(
            schema.metrics.get("lat").unwrap().metric_type,
            MetricType::Latency
        );
        assert_eq!(
            schema.metrics.get("mem").unwrap().metric_type,
            MetricType::Memory
        );
        assert_eq!(
            schema.metrics.get("err").unwrap().metric_type,
            MetricType::ErrorRate
        );
    }

    #[test]
    fn comments_and_blanks_ignored() {
        let yaml = "# comment\nregression_threshold: 0.12\n\n# another\nnoise_tolerance: 0.03\n";
        let schema = parse_slo_yaml(yaml).unwrap();
        assert!((schema.regression_threshold - 0.12).abs() < f64::EPSILON);
    }
}

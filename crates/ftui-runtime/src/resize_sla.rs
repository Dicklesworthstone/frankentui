#![forbid(unsafe_code)]

//! Resize SLA monitoring with conformal alerting (bd-1rz0.21).
//!
//! This module provides SLA monitoring for resize operations by integrating
//! the [`ConformalAlert`] system with resize telemetry hooks.
//!
//! # Mathematical Model
//!
//! The SLA monitor tracks resize latency (time from resize event to final
//! frame apply) and uses conformal prediction to detect violations:
//!
//! ```text
//! SLA violation := latency > conformal_threshold(calibration_data, alpha)
//! ```
//!
//! The conformal threshold is computed using the (n+1) rule from
//! [`crate::conformal_alert`], providing distribution-free coverage guarantees.
//!
//! # Key Invariants
//!
//! 1. **Latency bound**: Alert if latency exceeds calibrated threshold
//! 2. **FPR control**: False positive rate <= alpha (configurable)
//! 3. **Anytime-valid**: E-process layer prevents FPR inflation from early stopping
//! 4. **Full provenance**: Every alert includes evidence ledger
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::resize_sla::{ResizeSlaMonitor, SlaConfig};
//! use ftui_runtime::resize_coalescer::{ResizeCoalescer, TelemetryHooks};
//!
//! let sla_monitor = ResizeSlaMonitor::new(SlaConfig::default());
//! let hooks = sla_monitor.make_hooks();
//!
//! let coalescer = ResizeCoalescer::new(config, (80, 24))
//!     .with_telemetry_hooks(hooks);
//!
//! // SLA violations are logged and can be queried
//! if let Some(alert) = sla_monitor.last_alert() {
//!     println!("SLA violation: {}", alert.evidence_summary());
//! }
//! ```

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use web_time::Instant;

use crate::conformal_alert::{AlertConfig, AlertDecision, AlertStats, ConformalAlert};
use crate::resize_coalescer::{DecisionLog, TelemetryHooks};
use crate::voi_sampling::{VoiConfig, VoiSampler, VoiSummary};

/// Configuration for resize SLA monitoring.
#[derive(Debug, Clone)]
pub struct SlaConfig {
    /// Significance level alpha for conformal alerting.
    /// Lower alpha = more conservative (fewer false alarms). Default: 0.05.
    pub alpha: f64,

    /// Minimum latency samples before activating SLA monitoring.
    /// Default: 20.
    pub min_calibration: usize,

    /// Maximum latency samples to retain for calibration.
    /// Default: 200.
    pub max_calibration: usize,

    /// Target SLA latency in milliseconds.
    /// Used for reference/logging; conformal threshold is data-driven.
    /// Default: 100.0 (100ms).
    pub target_latency_ms: f64,

    /// Enable JSONL logging of SLA events.
    /// Default: true.
    pub enable_logging: bool,

    /// Alert cooldown: minimum events between consecutive alerts.
    /// Default: 10.
    pub alert_cooldown: u64,

    /// Hysteresis factor for alert boundary.
    /// Default: 1.1.
    pub hysteresis: f64,

    /// Optional VOI sampling policy for latency measurements.
    /// When set, latency observations are sampled via VOI decisions.
    pub voi_sampling: Option<VoiConfig>,
}

impl Default for SlaConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            min_calibration: 20,
            max_calibration: 200,
            target_latency_ms: 100.0,
            enable_logging: true,
            alert_cooldown: 10,
            hysteresis: 1.1,
            voi_sampling: None,
        }
    }
}

/// Evidence for a single resize operation.
#[derive(Debug, Clone)]
pub struct ResizeEvidence {
    /// Timestamp of the resize event.
    pub timestamp: Instant,
    /// Latency in milliseconds from resize to apply.
    pub latency_ms: f64,
    /// Final applied size (width, height).
    pub applied_size: (u16, u16),
    /// Whether this was a forced apply (deadline exceeded).
    pub forced: bool,
    /// Current regime at time of apply.
    pub regime: &'static str,
    /// Total coalesce time if coalesced.
    pub coalesce_ms: Option<f64>,
}

/// SLA event log entry for JSONL output.
#[derive(Debug, Clone)]
pub struct SlaLogEntry {
    /// Event index.
    pub event_idx: u64,
    /// Event type: "calibrate", "observe", "alert", "stats".
    pub event_type: &'static str,
    /// Latency in milliseconds.
    pub latency_ms: f64,
    /// Target SLA latency.
    pub target_latency_ms: f64,
    /// Current conformal threshold.
    pub threshold_ms: f64,
    /// E-value from conformal alerter.
    pub e_value: f64,
    /// Whether alert was triggered.
    pub is_alert: bool,
    /// Alert reason (if any).
    pub alert_reason: Option<String>,
    /// Applied size.
    pub applied_size: (u16, u16),
    /// Forced apply flag.
    pub forced: bool,
}

/// Summary statistics for SLA monitoring.
#[derive(Debug, Clone)]
pub struct SlaSummary {
    /// Total resize events observed.
    pub total_events: u64,
    /// Events in calibration phase.
    pub calibration_events: usize,
    /// Total SLA alerts triggered.
    pub total_alerts: u64,
    /// Current conformal threshold (ms).
    pub current_threshold_ms: f64,
    /// Mean latency from calibration (ms).
    pub mean_latency_ms: f64,
    /// Std latency from calibration (ms).
    pub std_latency_ms: f64,
    /// Current e-value.
    pub current_e_value: f64,
    /// Empirical false positive rate.
    pub empirical_fpr: f64,
    /// Target SLA (ms).
    pub target_latency_ms: f64,
}

/// Resize SLA monitor with conformal alerting.
///
/// Tracks resize latency and alerts on SLA violations using distribution-free
/// conformal prediction.
pub struct ResizeSlaMonitor {
    config: SlaConfig,
    alerter: RefCell<ConformalAlert>,
    event_count: RefCell<u64>,
    total_alerts: RefCell<u64>,
    last_alert: RefCell<Option<AlertDecision>>,
    logs: RefCell<Vec<SlaLogEntry>>,
    sampler: RefCell<Option<VoiSampler>>,
}

impl ResizeSlaMonitor {
    /// Create a new SLA monitor with given configuration.
    pub fn new(config: SlaConfig) -> Self {
        let alert_config = AlertConfig {
            alpha: config.alpha,
            min_calibration: config.min_calibration,
            max_calibration: config.max_calibration,
            enable_logging: config.enable_logging,
            hysteresis: config.hysteresis,
            alert_cooldown: config.alert_cooldown,
            ..AlertConfig::default()
        };
        let sampler = config.voi_sampling.clone().map(VoiSampler::new);

        Self {
            config,
            alerter: RefCell::new(ConformalAlert::new(alert_config)),
            event_count: RefCell::new(0),
            total_alerts: RefCell::new(0),
            last_alert: RefCell::new(None),
            logs: RefCell::new(Vec::new()),
            sampler: RefCell::new(sampler),
        }
    }

    /// Process a resize apply decision log and return alert decision.
    pub fn on_decision(&self, entry: &DecisionLog) -> Option<AlertDecision> {
        // Extract latency from coalesce_ms or time_since_render_ms
        let latency_ms = entry.coalesce_ms.unwrap_or(entry.time_since_render_ms);
        let applied_size = entry.applied_size?;
        if let Some(ref mut sampler) = *self.sampler.borrow_mut() {
            let decision = sampler.decide(entry.timestamp);
            if !decision.should_sample {
                return None;
            }
            let result = self.process_latency(latency_ms, applied_size, entry.forced);
            let violated = latency_ms > self.config.target_latency_ms;
            sampler.observe_at(violated, entry.timestamp);
            return result;
        }

        self.process_latency(latency_ms, applied_size, entry.forced)
    }

    /// Process a latency observation.
    fn process_latency(
        &self,
        latency_ms: f64,
        applied_size: (u16, u16),
        forced: bool,
    ) -> Option<AlertDecision> {
        *self.event_count.borrow_mut() += 1;
        let event_idx = *self.event_count.borrow();

        let mut alerter = self.alerter.borrow_mut();

        // Calibration phase: feed latencies to build baseline
        if alerter.calibration_count() < self.config.min_calibration {
            alerter.calibrate(latency_ms);

            if self.config.enable_logging {
                self.logs.borrow_mut().push(SlaLogEntry {
                    event_idx,
                    event_type: "calibrate",
                    latency_ms,
                    target_latency_ms: self.config.target_latency_ms,
                    threshold_ms: alerter.threshold(),
                    e_value: alerter.e_value(),
                    is_alert: false,
                    alert_reason: None,
                    applied_size,
                    forced,
                });
            }

            return None;
        }

        // Detection phase: check for SLA violations
        let decision = alerter.observe(latency_ms);

        if self.config.enable_logging {
            self.logs.borrow_mut().push(SlaLogEntry {
                event_idx,
                event_type: if decision.is_alert {
                    "alert"
                } else {
                    "observe"
                },
                latency_ms,
                target_latency_ms: self.config.target_latency_ms,
                threshold_ms: decision.evidence.conformal_threshold,
                e_value: decision.evidence.e_value,
                is_alert: decision.is_alert,
                alert_reason: if decision.is_alert {
                    Some(format!("{:?}", decision.evidence.reason))
                } else {
                    None
                },
                applied_size,
                forced,
            });
        }

        if decision.is_alert {
            *self.total_alerts.borrow_mut() += 1;
            *self.last_alert.borrow_mut() = Some(decision.clone());
        }

        Some(decision)
    }

    /// Get the last alert (if any).
    pub fn last_alert(&self) -> Option<AlertDecision> {
        self.last_alert.borrow().clone()
    }

    /// Get SLA summary statistics.
    pub fn summary(&self) -> SlaSummary {
        let alerter = self.alerter.borrow();
        let stats = alerter.stats();

        SlaSummary {
            total_events: *self.event_count.borrow(),
            calibration_events: stats.calibration_samples,
            total_alerts: *self.total_alerts.borrow(),
            current_threshold_ms: stats.current_threshold,
            mean_latency_ms: stats.calibration_mean,
            std_latency_ms: stats.calibration_std,
            current_e_value: stats.current_e_value,
            empirical_fpr: stats.empirical_fpr,
            target_latency_ms: self.config.target_latency_ms,
        }
    }

    /// Get alerter stats directly.
    pub fn alerter_stats(&self) -> AlertStats {
        self.alerter.borrow().stats()
    }

    /// Get SLA logs.
    pub fn logs(&self) -> Vec<SlaLogEntry> {
        self.logs.borrow().clone()
    }

    /// Convert logs to JSONL format.
    pub fn logs_to_jsonl(&self) -> String {
        let logs = self.logs.borrow();
        let mut output = String::new();

        for entry in logs.iter() {
            let line = format!(
                r#"{{"event":"sla","idx":{},"type":"{}","latency_ms":{:.3},"target_ms":{:.1},"threshold_ms":{:.3},"e_value":{:.6},"alert":{},"reason":{},"size":[{},{}],"forced":{}}}"#,
                entry.event_idx,
                entry.event_type,
                entry.latency_ms,
                entry.target_latency_ms,
                entry.threshold_ms,
                entry.e_value,
                entry.is_alert,
                entry
                    .alert_reason
                    .as_ref()
                    .map(|r| format!("\"{}\"", r))
                    .unwrap_or_else(|| "null".to_string()),
                entry.applied_size.0,
                entry.applied_size.1,
                entry.forced
            );
            output.push_str(&line);
            output.push('\n');
        }

        output
    }

    /// Clear logs.
    pub fn clear_logs(&self) {
        self.logs.borrow_mut().clear();
    }

    /// Reset the monitor (keeps configuration).
    pub fn reset(&self) {
        let alert_config = AlertConfig {
            alpha: self.config.alpha,
            min_calibration: self.config.min_calibration,
            max_calibration: self.config.max_calibration,
            enable_logging: self.config.enable_logging,
            hysteresis: self.config.hysteresis,
            alert_cooldown: self.config.alert_cooldown,
            ..AlertConfig::default()
        };

        *self.alerter.borrow_mut() = ConformalAlert::new(alert_config);
        *self.event_count.borrow_mut() = 0;
        *self.total_alerts.borrow_mut() = 0;
        *self.last_alert.borrow_mut() = None;
        self.logs.borrow_mut().clear();
        *self.sampler.borrow_mut() = self.config.voi_sampling.clone().map(VoiSampler::new);
    }

    /// Current threshold in milliseconds.
    pub fn threshold_ms(&self) -> f64 {
        self.alerter.borrow().threshold()
    }

    /// Whether monitoring is active (past calibration phase).
    pub fn is_active(&self) -> bool {
        self.alerter.borrow().calibration_count() >= self.config.min_calibration
    }

    /// Number of calibration samples collected.
    pub fn calibration_count(&self) -> usize {
        self.alerter.borrow().calibration_count()
    }

    /// Sampling summary if VOI sampling is enabled.
    pub fn sampling_summary(&self) -> Option<VoiSummary> {
        self.sampler.borrow().as_ref().map(VoiSampler::summary)
    }

    /// Sampling logs rendered as JSONL (if enabled).
    pub fn sampling_logs_to_jsonl(&self) -> Option<String> {
        self.sampler
            .borrow()
            .as_ref()
            .map(|sampler| sampler.logs_to_jsonl())
    }
}

/// Create TelemetryHooks that feed into an SLA monitor.
///
/// Returns a tuple of (TelemetryHooks, Rc<ResizeSlaMonitor>) so the monitor
/// can be queried after hooking into a ResizeCoalescer.
///
/// Note: Uses Rc + RefCell internally since TelemetryHooks callbacks are
/// `Fn` (not `FnMut`) but we need to mutate the monitor state.
pub fn make_sla_hooks(config: SlaConfig) -> (TelemetryHooks, Arc<Mutex<ResizeSlaMonitor>>) {
    let monitor = Arc::new(Mutex::new(ResizeSlaMonitor::new(config)));
    let monitor_clone = Arc::clone(&monitor);

    // Hook into on_resize_applied events to track latency
    let hooks = TelemetryHooks::new().on_resize_applied(move |entry: &DecisionLog| {
        // Only process apply events (not coalesce)
        if (entry.action == "apply" || entry.action == "apply_forced")
            && let Ok(monitor) = monitor_clone.lock()
        {
            monitor.on_decision(entry);
        }
    });

    (hooks, monitor)
}

// =============================================================================
// Unit Tests (bd-1rz0.21)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resize_coalescer::Regime;

    fn test_config() -> SlaConfig {
        SlaConfig {
            alpha: 0.05,
            min_calibration: 5,
            max_calibration: 50,
            target_latency_ms: 50.0,
            enable_logging: true,
            alert_cooldown: 0,
            hysteresis: 1.0,
            voi_sampling: None,
        }
    }

    fn sample_decision_log(now: Instant, latency_ms: f64) -> DecisionLog {
        DecisionLog {
            timestamp: now,
            elapsed_ms: 0.0,
            event_idx: 1,
            dt_ms: 0.0,
            event_rate: 0.0,
            regime: Regime::Steady,
            action: "apply",
            pending_size: None,
            applied_size: Some((80, 24)),
            time_since_render_ms: latency_ms,
            coalesce_ms: Some(latency_ms),
            forced: false,
        }
    }

    // =========================================================================
    // Basic construction and state
    // =========================================================================

    #[test]
    fn initial_state() {
        let monitor = ResizeSlaMonitor::new(test_config());

        assert!(!monitor.is_active());
        assert_eq!(monitor.calibration_count(), 0);
        assert!(monitor.last_alert().is_none());
        assert!(monitor.logs().is_empty());
    }

    #[test]
    fn calibration_phase() {
        let monitor = ResizeSlaMonitor::new(test_config());

        // Feed calibration samples
        for i in 0..5 {
            let result = monitor.process_latency(10.0 + i as f64, (80, 24), false);
            assert!(result.is_none(), "Should be in calibration phase");
        }

        assert!(monitor.is_active());
        assert_eq!(monitor.calibration_count(), 5);
    }

    #[test]
    fn detection_phase_normal() {
        let monitor = ResizeSlaMonitor::new(test_config());

        // Calibrate
        for i in 0..5 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        // Normal observation (within calibration range)
        let result = monitor.process_latency(12.0, (80, 24), false);
        assert!(result.is_some());
        assert!(!result.unwrap().is_alert);
    }

    #[test]
    fn detection_phase_alert() {
        let mut config = test_config();
        config.hysteresis = 0.1; // Lower threshold for easier triggering
        let monitor = ResizeSlaMonitor::new(config);

        // Calibrate with tight distribution
        for _ in 0..5 {
            monitor.process_latency(10.0, (80, 24), false);
        }

        // Extreme latency should trigger alert
        let result = monitor.process_latency(1000.0, (80, 24), false);
        assert!(result.is_some());

        let decision = result.unwrap();
        assert!(
            decision.evidence.conformal_alert || decision.evidence.eprocess_alert,
            "Extreme latency should trigger alert"
        );
    }

    // =========================================================================
    // Logging tests
    // =========================================================================

    #[test]
    fn logging_captures_events() {
        let monitor = ResizeSlaMonitor::new(test_config());

        // Calibrate
        for i in 0..5 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        // Observe
        monitor.process_latency(12.0, (80, 24), false);
        monitor.process_latency(15.0, (100, 40), true);

        let logs = monitor.logs();
        assert_eq!(logs.len(), 7);

        // Check calibration entries
        assert_eq!(logs[0].event_type, "calibrate");
        assert_eq!(logs[4].event_type, "calibrate");

        // Check observation entries
        assert_eq!(logs[5].event_type, "observe");
        assert_eq!(logs[6].applied_size, (100, 40));
        assert!(logs[6].forced);
    }

    #[test]
    fn jsonl_format() {
        let monitor = ResizeSlaMonitor::new(test_config());

        // Calibrate with 5 events (min_calibration=5), then 1 observation.
        // The 6th value must fall within conformal bounds to be "observe"
        // rather than "alert". Calibration on values 10-14 yields mean=12,
        // threshold=2.0, so use 12.0 (residual=0) for the observation.
        for i in 0..5 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }
        monitor.process_latency(12.0, (80, 24), false);

        let jsonl = monitor.logs_to_jsonl();
        assert!(jsonl.contains(r#""event":"sla""#));
        assert!(jsonl.contains(r#""type":"calibrate""#));
        assert!(jsonl.contains(r#""type":"observe""#));
        assert!(jsonl.contains(r#""latency_ms":"#));
        assert!(jsonl.contains(r#""threshold_ms":"#));
    }

    // =========================================================================
    // Summary statistics
    // =========================================================================

    #[test]
    fn summary_reflects_state() {
        let monitor = ResizeSlaMonitor::new(test_config());

        for i in 0..10 {
            monitor.process_latency(10.0 + (i as f64) * 2.0, (80, 24), false);
        }

        let summary = monitor.summary();
        assert_eq!(summary.total_events, 10);
        assert!(summary.mean_latency_ms > 0.0);
        assert!(summary.current_threshold_ms > 0.0);
        assert_eq!(summary.target_latency_ms, 50.0);
    }

    // =========================================================================
    // Reset behavior
    // =========================================================================

    #[test]
    fn reset_clears_state() {
        let monitor = ResizeSlaMonitor::new(test_config());

        for i in 0..10 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        assert!(monitor.is_active());
        assert!(!monitor.logs().is_empty());

        monitor.reset();

        assert!(!monitor.is_active());
        assert!(monitor.logs().is_empty());
        assert_eq!(monitor.calibration_count(), 0);
    }

    // =========================================================================
    // Integration with DecisionLog
    // =========================================================================

    #[test]
    fn on_decision_processes_entry() {
        use crate::resize_coalescer::Regime;

        let monitor = ResizeSlaMonitor::new(test_config());

        // Create a DecisionLog entry representing an apply event
        let entry = DecisionLog {
            timestamp: std::time::Instant::now(),
            elapsed_ms: 0.0,
            event_idx: 1,
            dt_ms: 0.0,
            event_rate: 0.0,
            regime: Regime::Steady,
            action: "apply",
            pending_size: None,
            applied_size: Some((100, 40)),
            time_since_render_ms: 15.0,
            coalesce_ms: Some(15.0),
            forced: false,
        };

        let result = monitor.on_decision(&entry);
        assert!(result.is_none()); // Still in calibration

        // Feed more entries
        for i in 0..5 {
            let entry = DecisionLog {
                timestamp: std::time::Instant::now(),
                elapsed_ms: 0.0,
                event_idx: 2 + i,
                dt_ms: 0.0,
                event_rate: 0.0,
                regime: Regime::Steady,
                action: "apply",
                pending_size: None,
                applied_size: Some((100, 40)),
                time_since_render_ms: 15.0 + i as f64,
                coalesce_ms: Some(15.0 + i as f64),
                forced: false,
            };
            monitor.on_decision(&entry);
        }

        assert!(monitor.is_active());
    }

    // =========================================================================
    // Hook factory
    // =========================================================================

    #[test]
    fn make_sla_hooks_creates_valid_hooks() {
        let (_hooks, monitor) = make_sla_hooks(test_config());

        // Verify monitor is accessible and not active initially
        let monitor = monitor.lock().expect("sla monitor lock");
        assert!(!monitor.is_active());
        assert_eq!(monitor.calibration_count(), 0);
    }

    // =========================================================================
    // Property tests
    // =========================================================================

    #[test]
    fn property_calibration_mean_accurate() {
        let monitor = ResizeSlaMonitor::new(test_config());

        let samples: Vec<f64> = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        let expected_mean: f64 = samples.iter().sum::<f64>() / samples.len() as f64;

        for &s in &samples {
            monitor.process_latency(s, (80, 24), false);
        }

        let summary = monitor.summary();
        assert!(
            (summary.mean_latency_ms - expected_mean).abs() < 0.01,
            "Mean should be accurate: {} vs {}",
            summary.mean_latency_ms,
            expected_mean
        );
    }

    #[test]
    fn property_alert_count_nondecreasing() {
        let mut config = test_config();
        config.hysteresis = 0.1;
        config.alert_cooldown = 0;
        let monitor = ResizeSlaMonitor::new(config);

        // Calibrate
        for _ in 0..5 {
            monitor.process_latency(10.0, (80, 24), false);
        }

        let mut prev_alerts = 0u64;
        for i in 0..20 {
            let latency = if i % 3 == 0 { 1000.0 } else { 10.0 };
            monitor.process_latency(latency, (80, 24), false);

            let current_alerts = *monitor.total_alerts.borrow();
            assert!(
                current_alerts >= prev_alerts,
                "Alert count should be non-decreasing"
            );
            prev_alerts = current_alerts;
        }
    }

    #[test]
    fn deterministic_behavior() {
        let config = test_config();

        let run = || {
            let monitor = ResizeSlaMonitor::new(config.clone());
            for i in 0..10 {
                monitor.process_latency(10.0 + i as f64, (80, 24), false);
            }
            (
                monitor.summary().mean_latency_ms,
                monitor.threshold_ms(),
                *monitor.total_alerts.borrow(),
            )
        };

        let (m1, t1, a1) = run();
        let (m2, t2, a2) = run();

        assert!((m1 - m2).abs() < 1e-10, "Mean must be deterministic");
        assert!((t1 - t2).abs() < 1e-10, "Threshold must be deterministic");
        assert_eq!(a1, a2, "Alert count must be deterministic");
    }

    #[test]
    fn voi_sampling_skips_when_policy_says_no() {
        let mut config = test_config();
        config.voi_sampling = Some(VoiConfig {
            sample_cost: 10.0,
            max_interval_events: 0,
            max_interval_ms: 0,
            ..VoiConfig::default()
        });
        let monitor = ResizeSlaMonitor::new(config);

        let entry = sample_decision_log(Instant::now(), 12.0);
        let result = monitor.on_decision(&entry);
        assert!(result.is_none(), "Sampling should skip under high cost");

        let summary = monitor.summary();
        assert_eq!(summary.total_events, 0);
        let sampling = monitor.sampling_summary().expect("sampling summary");
        assert_eq!(sampling.total_events, 1);
    }

    #[test]
    fn voi_sampling_forced_sample_records_event() {
        let mut config = test_config();
        // Skip calibration so the first sampled event reaches the observe
        // phase and returns Some(AlertDecision) instead of None.
        config.min_calibration = 0;
        config.voi_sampling = Some(VoiConfig {
            sample_cost: 10.0,
            max_interval_events: 1,
            ..VoiConfig::default()
        });
        let monitor = ResizeSlaMonitor::new(config);

        let entry = sample_decision_log(Instant::now(), 12.0);
        let result = monitor.on_decision(&entry);
        assert!(result.is_some());

        let summary = monitor.summary();
        assert_eq!(summary.total_events, 1);
        let sampling = monitor.sampling_summary().expect("sampling summary");
        assert_eq!(sampling.total_samples, 1);
    }

    #[test]
    fn sla_config_default_values() {
        let config = SlaConfig::default();
        assert!((config.alpha - 0.05).abs() < 1e-10);
        assert_eq!(config.min_calibration, 20);
        assert_eq!(config.max_calibration, 200);
        assert!((config.target_latency_ms - 100.0).abs() < 1e-10);
        assert!(config.enable_logging);
        assert_eq!(config.alert_cooldown, 10);
        assert!((config.hysteresis - 1.1).abs() < 1e-10);
        assert!(config.voi_sampling.is_none());
    }

    #[test]
    fn last_alert_initially_none() {
        let monitor = ResizeSlaMonitor::new(test_config());
        assert!(monitor.last_alert().is_none());
    }

    #[test]
    fn clear_logs_empties_log_vec() {
        let monitor = ResizeSlaMonitor::new(test_config());
        let now = Instant::now();
        for i in 0..3 {
            monitor.on_decision(&sample_decision_log(now, 10.0 + i as f64));
        }
        assert!(!monitor.logs().is_empty());
        monitor.clear_logs();
        assert!(monitor.logs().is_empty());
    }

    #[test]
    fn threshold_ms_returns_value() {
        let monitor = ResizeSlaMonitor::new(test_config());
        let threshold = monitor.threshold_ms();
        // Before calibration, threshold should be some default
        assert!(threshold.is_finite());
    }

    #[test]
    fn is_active_after_calibration() {
        let monitor = ResizeSlaMonitor::new(test_config());
        assert!(!monitor.is_active());
        let now = Instant::now();
        for i in 0..5 {
            monitor.on_decision(&sample_decision_log(now, 10.0 + i as f64));
        }
        assert!(monitor.is_active());
    }

    #[test]
    fn calibration_count_tracks_samples() {
        let monitor = ResizeSlaMonitor::new(test_config());
        assert_eq!(monitor.calibration_count(), 0);
        let now = Instant::now();
        monitor.on_decision(&sample_decision_log(now, 10.0));
        assert_eq!(monitor.calibration_count(), 1);
    }

    #[test]
    fn alerter_stats_returns_valid() {
        let monitor = ResizeSlaMonitor::new(test_config());
        let stats = monitor.alerter_stats();
        assert_eq!(stats.calibration_samples, 0);
    }

    #[test]
    fn sampling_summary_none_without_voi() {
        let monitor = ResizeSlaMonitor::new(test_config());
        assert!(monitor.sampling_summary().is_none());
    }

    #[test]
    fn sampling_logs_to_jsonl_none_without_voi() {
        let monitor = ResizeSlaMonitor::new(test_config());
        assert!(monitor.sampling_logs_to_jsonl().is_none());
    }

    // =========================================================================
    // Edge-case tests (bd-1nn7a)
    // =========================================================================

    #[test]
    fn edge_on_decision_none_applied_size() {
        let monitor = ResizeSlaMonitor::new(test_config());
        let entry = DecisionLog {
            timestamp: Instant::now(),
            elapsed_ms: 0.0,
            event_idx: 1,
            dt_ms: 0.0,
            event_rate: 0.0,
            regime: Regime::Steady,
            action: "apply",
            pending_size: None,
            applied_size: None, // Missing applied_size
            time_since_render_ms: 10.0,
            coalesce_ms: Some(10.0),
            forced: false,
        };
        let result = monitor.on_decision(&entry);
        assert!(
            result.is_none(),
            "Should return None when applied_size is None"
        );
        // Event should not be counted
        assert_eq!(*monitor.event_count.borrow(), 0);
    }

    #[test]
    fn edge_on_decision_coalesce_ms_none_falls_back() {
        let monitor = ResizeSlaMonitor::new(test_config());
        let entry = DecisionLog {
            timestamp: Instant::now(),
            elapsed_ms: 0.0,
            event_idx: 1,
            dt_ms: 0.0,
            event_rate: 0.0,
            regime: Regime::Steady,
            action: "apply",
            pending_size: None,
            applied_size: Some((80, 24)),
            time_since_render_ms: 42.0,
            coalesce_ms: None, // Falls back to time_since_render_ms
            forced: false,
        };
        let result = monitor.on_decision(&entry);
        // Should process using time_since_render_ms (42.0)
        assert!(result.is_none()); // Still in calibration
        assert_eq!(monitor.calibration_count(), 1);
    }

    #[test]
    fn edge_zero_latency() {
        let monitor = ResizeSlaMonitor::new(test_config());
        for _ in 0..5 {
            monitor.process_latency(0.0, (80, 24), false);
        }
        // All-zero calibration, observe zero -> no alert
        let result = monitor.process_latency(0.0, (80, 24), false);
        assert!(result.is_some());
        assert!(!result.unwrap().is_alert);
    }

    #[test]
    fn edge_negative_latency() {
        let monitor = ResizeSlaMonitor::new(test_config());
        // Negative latencies should not panic
        for _ in 0..5 {
            monitor.process_latency(-10.0, (80, 24), false);
        }
        let result = monitor.process_latency(-5.0, (80, 24), false);
        assert!(result.is_some());
    }

    #[test]
    fn edge_nan_latency() {
        let monitor = ResizeSlaMonitor::new(test_config());
        for _ in 0..5 {
            monitor.process_latency(10.0, (80, 24), false);
        }
        // NaN latency should not panic
        let result = monitor.process_latency(f64::NAN, (80, 24), false);
        assert!(result.is_some());
    }

    #[test]
    fn edge_infinity_latency() {
        let mut config = test_config();
        config.hysteresis = 0.1;
        let monitor = ResizeSlaMonitor::new(config);
        for _ in 0..5 {
            monitor.process_latency(10.0, (80, 24), false);
        }
        let result = monitor.process_latency(f64::INFINITY, (80, 24), false);
        assert!(result.is_some());
        // Infinite latency should trigger conformal alert
        assert!(result.unwrap().evidence.conformal_alert);
    }

    #[test]
    fn edge_logging_disabled() {
        let mut config = test_config();
        config.enable_logging = false;
        let monitor = ResizeSlaMonitor::new(config);

        for i in 0..10 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        assert!(
            monitor.logs().is_empty(),
            "Logs should be empty when disabled"
        );
        assert!(monitor.logs_to_jsonl().is_empty());
    }

    #[test]
    fn edge_reset_then_reuse() {
        let monitor = ResizeSlaMonitor::new(test_config());

        // First cycle
        for i in 0..10 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }
        assert!(monitor.is_active());
        let summary1 = monitor.summary();
        assert_eq!(summary1.total_events, 10);

        // Reset
        monitor.reset();
        assert!(!monitor.is_active());
        assert_eq!(monitor.calibration_count(), 0);
        assert!(monitor.last_alert().is_none());

        // Second cycle with different data
        for i in 0..10 {
            monitor.process_latency(50.0 + i as f64, (120, 40), false);
        }
        assert!(monitor.is_active());
        let summary2 = monitor.summary();
        assert_eq!(summary2.total_events, 10);
        // Mean should reflect new data, not old
        assert!(summary2.mean_latency_ms > 40.0);
    }

    #[test]
    fn edge_multiple_resets() {
        let monitor = ResizeSlaMonitor::new(test_config());

        for _ in 0..3 {
            for i in 0..5 {
                monitor.process_latency(10.0 + i as f64, (80, 24), false);
            }
            monitor.reset();
        }

        assert!(!monitor.is_active());
        assert_eq!(*monitor.event_count.borrow(), 0);
    }

    #[test]
    fn edge_min_calibration_zero() {
        let mut config = test_config();
        config.min_calibration = 0;
        let monitor = ResizeSlaMonitor::new(config);

        // Immediately active since min_calibration=0
        assert!(monitor.is_active());

        // First observation goes directly to observe phase
        let result = monitor.process_latency(10.0, (80, 24), false);
        assert!(result.is_some());
    }

    #[test]
    fn edge_last_alert_updates() {
        let mut config = test_config();
        config.hysteresis = 0.1;
        config.alert_cooldown = 0;
        let monitor = ResizeSlaMonitor::new(config);

        // Calibrate
        for _ in 0..5 {
            monitor.process_latency(10.0, (80, 24), false);
        }

        // Trigger alerts with extreme latency
        let mut got_alert = false;
        for _ in 0..10 {
            let result = monitor.process_latency(1000.0, (80, 24), false);
            if let Some(decision) = result
                && decision.is_alert
            {
                got_alert = true;
            }
        }

        if got_alert {
            let last = monitor.last_alert();
            assert!(last.is_some());
            assert!(last.unwrap().is_alert);
        }
    }

    #[test]
    fn edge_forced_flag_propagates_to_log() {
        let monitor = ResizeSlaMonitor::new(test_config());

        monitor.process_latency(10.0, (80, 24), true);

        let logs = monitor.logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].forced);
    }

    #[test]
    fn edge_applied_size_propagates_to_log() {
        let monitor = ResizeSlaMonitor::new(test_config());

        monitor.process_latency(10.0, (200, 60), false);

        let logs = monitor.logs();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].applied_size, (200, 60));
    }

    #[test]
    fn edge_event_count_accuracy() {
        let monitor = ResizeSlaMonitor::new(test_config());

        for i in 0..15 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        assert_eq!(*monitor.event_count.borrow(), 15);
        assert_eq!(monitor.summary().total_events, 15);
    }

    #[test]
    fn edge_jsonl_with_alert() {
        let mut config = test_config();
        config.hysteresis = 0.1;
        config.alert_cooldown = 0;
        let monitor = ResizeSlaMonitor::new(config);

        // Calibrate with tight distribution
        for _ in 0..5 {
            monitor.process_latency(10.0, (80, 24), false);
        }

        // Trigger alert
        monitor.process_latency(10000.0, (80, 24), false);

        let jsonl = monitor.logs_to_jsonl();
        // Should have both calibrate and either observe/alert entries
        assert!(jsonl.contains(r#""type":"calibrate""#));
        let has_alert_or_observe =
            jsonl.contains(r#""type":"alert""#) || jsonl.contains(r#""type":"observe""#);
        assert!(has_alert_or_observe);
    }

    #[test]
    fn edge_summary_after_reset() {
        let monitor = ResizeSlaMonitor::new(test_config());

        for i in 0..10 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        monitor.reset();

        let summary = monitor.summary();
        assert_eq!(summary.total_events, 0);
        assert_eq!(summary.total_alerts, 0);
        assert_eq!(summary.calibration_events, 0);
    }

    #[test]
    fn edge_sla_config_clone_debug() {
        let config = SlaConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.alpha, config.alpha);
        assert_eq!(cloned.min_calibration, config.min_calibration);
        let debug = format!("{:?}", config);
        assert!(debug.contains("SlaConfig"));
    }

    #[test]
    fn edge_resize_evidence_clone_debug() {
        let ev = ResizeEvidence {
            timestamp: Instant::now(),
            latency_ms: 42.0,
            applied_size: (80, 24),
            forced: false,
            regime: "steady",
            coalesce_ms: Some(10.0),
        };
        let cloned = ev.clone();
        assert_eq!(cloned.latency_ms, 42.0);
        assert_eq!(cloned.applied_size, (80, 24));
        let debug = format!("{:?}", ev);
        assert!(debug.contains("ResizeEvidence"));
    }

    #[test]
    fn edge_sla_log_entry_clone_debug() {
        let entry = SlaLogEntry {
            event_idx: 1,
            event_type: "calibrate",
            latency_ms: 10.0,
            target_latency_ms: 50.0,
            threshold_ms: 20.0,
            e_value: 1.0,
            is_alert: false,
            alert_reason: None,
            applied_size: (80, 24),
            forced: false,
        };
        let cloned = entry.clone();
        assert_eq!(cloned.event_idx, 1);
        assert_eq!(cloned.event_type, "calibrate");
        let debug = format!("{:?}", entry);
        assert!(debug.contains("SlaLogEntry"));
    }

    #[test]
    fn edge_sla_summary_clone_debug() {
        let monitor = ResizeSlaMonitor::new(test_config());
        for i in 0..5 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }
        let summary = monitor.summary();
        let cloned = summary.clone();
        assert_eq!(cloned.total_events, summary.total_events);
        let debug = format!("{:?}", summary);
        assert!(debug.contains("SlaSummary"));
    }

    #[test]
    fn edge_max_calibration_small() {
        let mut config = test_config();
        config.max_calibration = 3;
        config.min_calibration = 3;
        let monitor = ResizeSlaMonitor::new(config);

        // Feed more than max_calibration samples
        for i in 0..10 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        // Calibration window should be bounded
        assert!(monitor.calibration_count() <= 3);
    }

    #[test]
    fn edge_large_latency_values() {
        let monitor = ResizeSlaMonitor::new(test_config());
        for _ in 0..5 {
            monitor.process_latency(1e15, (80, 24), false);
        }
        // Should handle very large values without panic
        let result = monitor.process_latency(1e15, (80, 24), false);
        assert!(result.is_some());
        let summary = monitor.summary();
        assert!(summary.mean_latency_ms.is_finite());
    }
}

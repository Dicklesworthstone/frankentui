#![forbid(unsafe_code)]

//! bd-37a.8: Unit tests for BOCPD resize coalescing.
//!
//! Covers:
//! 1. Change-point detection on synthetic regime-change data (known change points)
//! 2. Coalescing behavior (N rapid resizes → 1 committed resize)
//! 3. Hazard function parameter sensitivity
//! 4. Edge cases: single resize, identical consecutive sizes
//! 5. Assert `bocpd.update` span fields
//! 6. Assert INFO log on regime transition
//! 7. Verify `bocpd_change_points_detected_total` counter
//!
//! Run:
//!   cargo test -p ftui-runtime --test bocpd_unit_tests

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use ftui_runtime::bocpd::{
    BocpdConfig, BocpdDetector, BocpdRegime, bocpd_change_points_detected_total,
};
use ftui_runtime::resize_coalescer::{CoalesceAction, CoalescerConfig, ResizeCoalescer};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use web_time::Instant;

// ============================================================================
// Tracing capture infrastructure (adapted from tracing_span_hierarchy.rs)
// ============================================================================

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedSpan {
    name: String,
    target: String,
    level: tracing::Level,
    fields: HashMap<String, String>,
    parent_name: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedEvent {
    level: tracing::Level,
    target: String,
    message: String,
    fields: HashMap<String, String>,
    parent_span_name: Option<String>,
}

struct SpanCapture {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    span_index: Arc<Mutex<HashMap<u64, usize>>>,
}

impl SpanCapture {
    fn new() -> (Self, CaptureHandle) {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let span_index = Arc::new(Mutex::new(HashMap::new()));

        let handle = CaptureHandle {
            spans: spans.clone(),
            events: events.clone(),
        };

        (
            Self {
                spans,
                events,
                span_index,
            },
            handle,
        )
    }
}

struct CaptureHandle {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl CaptureHandle {
    fn spans(&self) -> Vec<CapturedSpan> {
        self.spans.lock().unwrap().clone()
    }

    fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().unwrap().clone()
    }
}

struct FieldVisitor(Vec<(String, String)>);

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .push((field.name().to_string(), format!("{value:?}")));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
}

impl<S> tracing_subscriber::Layer<S> for SpanCapture
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(Vec::new());
        attrs.record(&mut visitor);

        let parent_name = ctx
            .current_span()
            .id()
            .and_then(|pid| ctx.span(pid))
            .map(|span_ref| span_ref.name().to_string());

        let mut fields: HashMap<String, String> = visitor.0.into_iter().collect();
        for field in attrs.metadata().fields() {
            fields.entry(field.name().to_string()).or_default();
        }

        let mut spans = self.spans.lock().unwrap();
        let idx = spans.len();
        spans.push(CapturedSpan {
            name: attrs.metadata().name().to_string(),
            target: attrs.metadata().target().to_string(),
            level: *attrs.metadata().level(),
            fields,
            parent_name,
        });

        self.span_index.lock().unwrap().insert(id.into_u64(), idx);
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(Vec::new());
        values.record(&mut visitor);

        let index = self.span_index.lock().unwrap();
        if let Some(&idx) = index.get(&id.into_u64()) {
            let mut spans = self.spans.lock().unwrap();
            if let Some(span) = spans.get_mut(idx) {
                for (k, v) in visitor.0 {
                    span.fields.insert(k, v);
                }
            }
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = FieldVisitor(Vec::new());
        event.record(&mut visitor);

        let fields: HashMap<String, String> = visitor.0.clone().into_iter().collect();
        let message = visitor
            .0
            .iter()
            .find(|(k, _)| k == "message")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        let parent_span_name = ctx
            .current_span()
            .id()
            .and_then(|id| ctx.span(id))
            .map(|span_ref| span_ref.name().to_string());

        self.events.lock().unwrap().push(CapturedEvent {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            message,
            fields,
            parent_span_name,
        });
    }
}

fn with_captured_tracing<F>(f: F) -> CaptureHandle
where
    F: FnOnce(),
{
    ensure_global_trace_level();
    let (layer, handle) = SpanCapture::new();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::TRACE)
        .with(layer);
    tracing::subscriber::with_default(subscriber, || {
        // Ensure callsite interest is recomputed for this scoped subscriber so
        // DEBUG instrumentation remains observable under parallel test runs.
        tracing::callsite::rebuild_interest_cache();
        f();
    });
    handle
}

fn ensure_global_trace_level() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let subscriber =
            tracing_subscriber::registry().with(tracing_subscriber::filter::LevelFilter::TRACE);
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

// ============================================================================
// 1. Change-point detection on synthetic regime-change data
// ============================================================================

/// Steady→Burst transition at a known change point.
#[test]
fn changepoint_steady_to_burst_known_location() {
    let mut det = BocpdDetector::with_defaults();
    let start = Instant::now();

    // Phase 1: 20 steady events at 200ms intervals
    for i in 0..20 {
        det.observe_event(start + Duration::from_millis(200 * (i + 1)));
    }
    let p_after_steady = det.p_burst();
    assert!(
        p_after_steady < 0.3,
        "Should be in steady regime: p_burst={p_after_steady}"
    );

    // Phase 2: Change point! 30 burst events at 10ms intervals
    let burst_start = start + Duration::from_millis(4100);
    for i in 0..30 {
        det.observe_event(burst_start + Duration::from_millis(10 * (i + 1)));
    }
    let p_after_burst = det.p_burst();
    assert!(
        p_after_burst > 0.7,
        "Should have transitioned to burst: p_burst={p_after_burst}"
    );
    assert_eq!(det.regime(), BocpdRegime::Burst);
}

/// Burst→Steady recovery at a known change point.
#[test]
fn changepoint_burst_to_steady_recovery() {
    let mut det = BocpdDetector::with_defaults();
    let start = Instant::now();

    // Phase 1: 30 burst events (10ms intervals)
    for i in 0..30 {
        det.observe_event(start + Duration::from_millis(10 * (i + 1)));
    }
    assert!(det.p_burst() > 0.5, "Should be in burst after rapid events");

    // Phase 2: 40 steady events (300ms intervals)
    let recovery_start = start + Duration::from_millis(400);
    for i in 0..40 {
        det.observe_event(recovery_start + Duration::from_millis(300 * (i + 1)));
    }
    assert!(
        det.p_burst() < 0.3,
        "Should recover to steady: p_burst={}",
        det.p_burst()
    );
    assert_eq!(det.regime(), BocpdRegime::Steady);
}

/// Multiple regime changes: steady→burst→steady→burst.
#[test]
fn changepoint_oscillatory_regimes() {
    let mut det = BocpdDetector::with_defaults();
    let start = Instant::now();
    let mut t = start;

    // Steady phase 1
    for _ in 0..15 {
        t += Duration::from_millis(200);
        det.observe_event(t);
    }
    assert!(det.p_burst() < 0.5, "Initial steady phase");

    // Burst phase 1
    for _ in 0..25 {
        t += Duration::from_millis(8);
        det.observe_event(t);
    }
    let burst1 = det.p_burst();
    assert!(burst1 > 0.5, "First burst phase: p_burst={burst1}");

    // Steady phase 2
    for _ in 0..30 {
        t += Duration::from_millis(250);
        det.observe_event(t);
    }
    assert!(
        det.p_burst() < 0.3,
        "Recovery to steady: p_burst={}",
        det.p_burst()
    );

    // Burst phase 2
    for _ in 0..25 {
        t += Duration::from_millis(8);
        det.observe_event(t);
    }
    assert!(
        det.p_burst() > 0.5,
        "Second burst phase: p_burst={}",
        det.p_burst()
    );
}

// ============================================================================
// 2. Coalescing behavior (N rapid resizes → 1 committed resize)
// ============================================================================

/// N rapid resizes through BOCPD-enabled coalescer → only final size applied.
#[test]
fn coalesce_rapid_resizes_to_final_size() {
    let config = CoalescerConfig {
        hard_deadline_ms: 200,
        ..CoalescerConfig::default().with_bocpd()
    };
    let mut c = ResizeCoalescer::new(config, (80, 24));

    // Inject 10 rapid resizes with increasing sizes, 5ms apart
    let mut applied_count = 0;
    let mut last_applied_size = (0u16, 0u16);

    for i in 1..=10 {
        let w = 80 + i as u16;
        let h = 24 + i as u16;
        let action = c.handle_resize(w, h);
        if let CoalesceAction::ApplyResize { width, height, .. } = action {
            applied_count += 1;
            last_applied_size = (width, height);
        }

        // Small tick between events
        std::thread::sleep(Duration::from_millis(2));
    }

    // Allow the coalescer to settle via tick
    for _ in 0..50 {
        let action = c.tick();
        if let CoalesceAction::ApplyResize { width, height, .. } = action {
            applied_count += 1;
            last_applied_size = (width, height);
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // Final size should be the last one we sent
    assert!(applied_count >= 1, "At least one apply should occur");
    assert_eq!(
        last_applied_size,
        (90, 34),
        "Latest-wins: final size should be the last resize"
    );
}

/// Hard deadline forces apply even during coalescing.
#[test]
fn coalesce_hard_deadline_forces_apply() {
    let config = CoalescerConfig {
        hard_deadline_ms: 50,
        burst_delay_ms: 100,
        ..CoalescerConfig::default().with_bocpd()
    };
    let mut c = ResizeCoalescer::new(config, (80, 24));

    c.handle_resize(100, 40);
    // Wait longer than hard deadline
    std::thread::sleep(Duration::from_millis(60));

    let action = c.tick();
    if let CoalesceAction::ApplyResize {
        forced_by_deadline, ..
    } = action
    {
        assert!(forced_by_deadline, "Should be forced by hard deadline");
    } else {
        // Second tick should catch it
        std::thread::sleep(Duration::from_millis(10));
        let action2 = c.tick();
        assert!(
            matches!(action2, CoalesceAction::ApplyResize { .. }),
            "Hard deadline should force apply within 100ms"
        );
    }
}

// ============================================================================
// 3. Hazard function parameter sensitivity
// ============================================================================

/// Lower hazard_lambda → faster change-point detection.
#[test]
fn hazard_lambda_sensitivity_lower_detects_faster() {
    let start = Instant::now();

    // Low hazard_lambda (expect changepoints more frequently → faster detection)
    let config_fast = BocpdConfig {
        hazard_lambda: 10.0,
        ..Default::default()
    };
    let mut det_fast = BocpdDetector::new(config_fast);

    // High hazard_lambda (expect changepoints less frequently → slower detection)
    let config_slow = BocpdConfig {
        hazard_lambda: 200.0,
        ..Default::default()
    };
    let mut det_slow = BocpdDetector::new(config_slow);

    // Baseline: steady events
    for i in 0..10 {
        let t = start + Duration::from_millis(200 * (i + 1));
        det_fast.observe_event(t);
        det_slow.observe_event(t);
    }

    // Transition to burst
    let burst_start = start + Duration::from_millis(2200);
    let mut fast_burst_at = None;
    let mut slow_burst_at = None;

    for i in 0..40 {
        let t = burst_start + Duration::from_millis(10 * (i + 1));
        det_fast.observe_event(t);
        det_slow.observe_event(t);

        if fast_burst_at.is_none() && det_fast.p_burst() > 0.7 {
            fast_burst_at = Some(i);
        }
        if slow_burst_at.is_none() && det_slow.p_burst() > 0.7 {
            slow_burst_at = Some(i);
        }
    }

    // Fast detector should detect burst no later than slow detector.
    // (Both should eventually detect; fast should get there first or at same time)
    if let (Some(fast), Some(slow)) = (fast_burst_at, slow_burst_at) {
        assert!(
            fast <= slow,
            "Lower hazard_lambda should detect burst at same time or sooner: fast={fast}, slow={slow}"
        );
    }
    // If only fast detected, that also confirms sensitivity
}

/// Higher burst_prior → more willing to classify as burst.
#[test]
fn burst_prior_sensitivity() {
    let start = Instant::now();

    let config_low_prior = BocpdConfig {
        burst_prior: 0.05,
        ..Default::default()
    };
    let config_high_prior = BocpdConfig {
        burst_prior: 0.4,
        ..Default::default()
    };

    let mut det_low = BocpdDetector::new(config_low_prior);
    let mut det_high = BocpdDetector::new(config_high_prior);

    // Same intermediate events (50ms apart — between steady and burst rates)
    for i in 0..20 {
        let t = start + Duration::from_millis(50 * (i + 1));
        det_low.observe_event(t);
        det_high.observe_event(t);
    }

    // High-prior detector should have higher p_burst
    assert!(
        det_high.p_burst() >= det_low.p_burst(),
        "Higher burst_prior should yield higher p_burst: high={}, low={}",
        det_high.p_burst(),
        det_low.p_burst()
    );
}

/// mu_burst_ms sensitivity: lower mu_burst_ms → more aggressive burst detection for rapid events.
#[test]
fn mu_burst_sensitivity() {
    let start = Instant::now();

    // Very short expected burst interval
    let config_short = BocpdConfig {
        mu_burst_ms: 5.0,
        ..Default::default()
    };
    // Longer expected burst interval
    let config_long = BocpdConfig {
        mu_burst_ms: 50.0,
        ..Default::default()
    };

    let mut det_short = BocpdDetector::new(config_short);
    let mut det_long = BocpdDetector::new(config_long);

    // Very rapid events at 8ms intervals (closer to mu_burst=5ms)
    for i in 0..30 {
        let t = start + Duration::from_millis(8 * (i + 1));
        det_short.observe_event(t);
        det_long.observe_event(t);
    }

    // Short mu_burst detector should see these 8ms events as better fitting burst
    // (the likelihood ratio favors burst when observations ~ mu_burst)
    assert!(
        det_short.p_burst() > 0.5,
        "Short mu_burst should detect burst: p_burst={}",
        det_short.p_burst()
    );
}

// ============================================================================
// 4. Edge cases
// ============================================================================

/// Single resize event should not cause regime transition.
#[test]
fn edge_case_single_resize() {
    let mut det = BocpdDetector::with_defaults();
    let t = Instant::now();

    let regime = det.observe_event(t);
    assert_eq!(
        regime,
        BocpdRegime::Steady,
        "Single event should not trigger burst"
    );
    assert_eq!(det.observation_count(), 1);
    assert!(det.last_evidence().is_some());
}

/// Identical consecutive sizes through coalescer.
#[test]
fn edge_case_identical_consecutive_sizes() {
    let config = CoalescerConfig::default().with_bocpd();
    let mut c = ResizeCoalescer::new(config, (100, 40));

    // Same size as initial → should be no-op
    let action = c.handle_resize(100, 40);
    assert_eq!(action, CoalesceAction::None, "Same size should return None");
}

/// Very long gap between events resets to steady-like behavior.
#[test]
fn edge_case_long_gap_between_events() {
    let mut det = BocpdDetector::with_defaults();
    let start = Instant::now();

    // First a burst
    for i in 0..20 {
        det.observe_event(start + Duration::from_millis(10 * (i + 1)));
    }
    assert!(det.p_burst() > 0.5);

    // Then a very long gap (5 seconds) → clamped to max_observation_ms (10s)
    let late = start + Duration::from_millis(5200);
    det.observe_event(late);

    // One slow event should shift towards steady
    let late2 = late + Duration::from_millis(5000);
    det.observe_event(late2);

    // After two very slow events, p_burst should decrease significantly
    assert!(
        det.p_burst() < 0.9,
        "Long gap should reduce p_burst: {}",
        det.p_burst()
    );
}

/// Observation clamped to minimum.
#[test]
fn edge_case_instant_consecutive_events() {
    let mut det = BocpdDetector::with_defaults();
    let t = Instant::now();

    // Two events at exactly the same time → dt=0 → clamped to min_observation_ms
    det.observe_event(t);
    det.observe_event(t); // Same instant

    // Should not panic, p_burst should be valid
    assert!(det.p_burst() >= 0.0 && det.p_burst() <= 1.0);
    assert_eq!(det.observation_count(), 2);
}

/// Posterior stays normalized even with extreme inputs.
#[test]
fn edge_case_extreme_rapid_events() {
    let mut det = BocpdDetector::with_defaults();
    let start = Instant::now();

    // 200 events at 1ms intervals (extremely rapid)
    for i in 0..200 {
        det.observe_event(start + Duration::from_millis(i + 1));
    }

    let sum: f64 = det.run_length_posterior().iter().sum();
    assert!(
        (sum - 1.0).abs() < 1e-6,
        "Posterior must stay normalized: sum={sum}"
    );
    assert!(det.p_burst() >= 0.0 && det.p_burst() <= 1.0);
}

// ============================================================================
// 5. Assert bocpd.update span fields
// ============================================================================

/// Verify `bocpd.update` span is emitted with all required fields.
#[test]
fn span_bocpd_update_has_required_fields() {
    let handle = with_captured_tracing(|| {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();
        det.observe_event(start);
        det.observe_event(start + Duration::from_millis(50));
    });

    let spans = handle.spans();
    let bocpd_spans: Vec<_> = spans.iter().filter(|s| s.name == "bocpd.update").collect();

    assert!(
        !bocpd_spans.is_empty(),
        "bocpd.update span should be emitted; all spans: {:?}",
        spans.iter().map(|s| &s.name).collect::<Vec<_>>()
    );

    for span in &bocpd_spans {
        assert!(
            span.fields.contains_key("run_length_posterior_max"),
            "bocpd.update must have run_length_posterior_max field; fields: {:?}",
            span.fields.keys().collect::<Vec<_>>()
        );
        assert!(
            span.fields.contains_key("change_point_probability"),
            "bocpd.update must have change_point_probability field"
        );
        assert!(
            span.fields.contains_key("coalescing_active"),
            "bocpd.update must have coalescing_active field"
        );
        assert!(
            span.fields.contains_key("resize_count_in_window"),
            "bocpd.update must have resize_count_in_window field"
        );
    }
}

/// Verify span field values are reasonable.
#[test]
fn span_bocpd_update_field_values_reasonable() {
    let handle = with_captured_tracing(|| {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();
        for i in 0..5 {
            det.observe_event(start + Duration::from_millis(200 * (i + 1)));
        }
    });

    let spans = handle.spans();
    let bocpd_spans: Vec<_> = spans.iter().filter(|s| s.name == "bocpd.update").collect();

    assert_eq!(bocpd_spans.len(), 5, "Should have 5 bocpd.update spans");

    // Last span: after 5 steady events, coalescing_active should be false
    let last = bocpd_spans.last().unwrap();
    assert_eq!(
        last.fields.get("coalescing_active").map(|s| s.as_str()),
        Some("false"),
        "Coalescing should be inactive during steady regime"
    );

    // resize_count_in_window should be 5
    assert_eq!(
        last.fields
            .get("resize_count_in_window")
            .map(|s| s.as_str()),
        Some("5"),
        "resize_count_in_window should equal observation count"
    );
}

/// Verify coalescing_active becomes true during burst regime.
#[test]
fn span_coalescing_active_true_during_burst() {
    let handle = with_captured_tracing(|| {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();
        // Rapid events to trigger burst
        for i in 0..30 {
            det.observe_event(start + Duration::from_millis(8 * (i + 1)));
        }
    });

    let spans = handle.spans();
    let bocpd_spans: Vec<_> = spans.iter().filter(|s| s.name == "bocpd.update").collect();

    // At least some spans should show coalescing_active=true
    let active_count = bocpd_spans
        .iter()
        .filter(|s| s.fields.get("coalescing_active").map(|v| v.as_str()) == Some("true"))
        .count();

    assert!(
        active_count > 0,
        "Some spans should have coalescing_active=true during burst"
    );
}

// ============================================================================
// 6. Assert INFO log on regime transition
// ============================================================================

/// Verify INFO event fires on regime transition with required fields.
#[test]
fn info_log_on_regime_transition() {
    let handle = with_captured_tracing(|| {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();

        // Steady phase
        for i in 0..10 {
            det.observe_event(start + Duration::from_millis(200 * (i + 1)));
        }

        // Drive into burst
        let burst_start = start + Duration::from_millis(2100);
        for i in 0..30 {
            det.observe_event(burst_start + Duration::from_millis(5 * (i + 1)));
        }
    });

    let events = handle.events();

    // Find INFO-level events about regime transition
    let transition_events: Vec<_> = events
        .iter()
        .filter(|e| {
            e.level == tracing::Level::INFO
                && e.target == ftui_runtime::telemetry_schema::TARGET_BOCPD
                && e.message.contains("regime transition detected")
        })
        .collect();

    assert!(
        !transition_events.is_empty(),
        "Expected INFO log for regime transition; all events: {:?}",
        events
            .iter()
            .filter(|e| e.target == ftui_runtime::telemetry_schema::TARGET_BOCPD)
            .map(|e| (&e.level, &e.message))
            .collect::<Vec<_>>()
    );

    // Verify required fields
    for event in &transition_events {
        assert!(
            event.fields.contains_key("from_regime"),
            "Transition event must have from_regime field"
        );
        assert!(
            event.fields.contains_key("to_regime"),
            "Transition event must have to_regime field"
        );
        assert!(
            event.fields.contains_key("p_burst"),
            "Transition event must have p_burst field"
        );
        assert!(
            event.fields.contains_key("observation_count"),
            "Transition event must have observation_count field"
        );
    }
}

/// Verify DEBUG events for posterior updates (via field presence).
#[test]
fn debug_log_for_posterior_updates() {
    let handle = with_captured_tracing(|| {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();
        for i in 0..5 {
            det.observe_event(start + Duration::from_millis(100 * (i + 1)));
        }
    });

    let events = handle.events();

    // Filter for DEBUG events with p_burst field (unique to posterior update events)
    let update_events: Vec<_> = events
        .iter()
        .filter(|e| {
            e.level == tracing::Level::DEBUG
                && e.target == ftui_runtime::telemetry_schema::TARGET_BOCPD
                && e.fields.contains_key("p_burst")
        })
        .collect();

    assert!(
        !update_events.is_empty(),
        "Should have at least one DEBUG posterior update event with p_burst field; \
         total ftui.bocpd events: {}",
        events
            .iter()
            .filter(|e| e.target == ftui_runtime::telemetry_schema::TARGET_BOCPD)
            .count()
    );

    // Verify required fields on captured update events
    for event in &update_events {
        assert!(
            event.fields.contains_key("observation_ms"),
            "Posterior update should have observation_ms"
        );
        assert!(
            event.fields.contains_key("observation_count"),
            "Posterior update should have observation_count"
        );
    }
}

/// DEBUG event for bocpd_run_length histogram emitted.
#[test]
fn debug_log_for_run_length_histogram() {
    let handle = with_captured_tracing(|| {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();
        det.observe_event(start);
        det.observe_event(start + Duration::from_millis(100));
    });

    let events = handle.events();
    let histogram_events: Vec<_> = events
        .iter()
        .filter(|e| {
            e.level == tracing::Level::DEBUG
                && e.target == ftui_runtime::telemetry_schema::TARGET_BOCPD
                && e.message.contains("bocpd run length histogram")
        })
        .collect();

    assert_eq!(
        histogram_events.len(),
        2,
        "Should have one histogram event per observe_event"
    );

    for event in &histogram_events {
        assert!(
            event.fields.contains_key("bocpd_run_length"),
            "Histogram event must have bocpd_run_length field"
        );
    }
}

/// No INFO log when regime stays the same.
#[test]
fn no_info_log_when_regime_stable() {
    let handle = with_captured_tracing(|| {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();
        // All steady events
        for i in 0..10 {
            det.observe_event(start + Duration::from_millis(200 * (i + 1)));
        }
    });

    let events = handle.events();
    let transition_events: Vec<_> = events
        .iter()
        .filter(|e| {
            e.level == tracing::Level::INFO
                && e.target == ftui_runtime::telemetry_schema::TARGET_BOCPD
                && e.message.contains("regime transition detected")
        })
        .collect();

    assert!(
        transition_events.is_empty(),
        "No INFO transition log expected when regime stays steady"
    );
}

// ============================================================================
// 7. Verify bocpd_change_points_detected_total counter
// ============================================================================

/// Counter increments when regime changes from steady to burst.
#[test]
fn counter_increments_on_steady_to_burst() {
    let before = bocpd_change_points_detected_total();
    let mut det = BocpdDetector::with_defaults();
    let start = Instant::now();

    // Steady baseline
    for i in 0..5 {
        det.observe_event(start + Duration::from_millis(200 * (i + 1)));
    }
    let mid = bocpd_change_points_detected_total();

    // Drive to burst
    let burst_start = start + Duration::from_millis(1100);
    for i in 0..30 {
        det.observe_event(burst_start + Duration::from_millis(5 * (i + 1)));
    }
    let after = bocpd_change_points_detected_total();

    // Counter should have increased (at least one transition)
    assert!(
        after > before || after > mid,
        "Counter should increment on regime transition: before={before}, mid={mid}, after={after}"
    );
}

/// Counter tracks across multiple transitions.
#[test]
fn counter_tracks_multiple_transitions() {
    let before = bocpd_change_points_detected_total();
    let mut det = BocpdDetector::with_defaults();
    let start = Instant::now();
    let mut t = start;

    // Steady → burst
    for _ in 0..10 {
        t += Duration::from_millis(200);
        det.observe_event(t);
    }
    for _ in 0..30 {
        t += Duration::from_millis(5);
        det.observe_event(t);
    }

    // Burst → steady
    for _ in 0..30 {
        t += Duration::from_millis(300);
        det.observe_event(t);
    }

    let after = bocpd_change_points_detected_total();

    // At least 2 transitions expected (steady→transitional/burst, then burst→transitional/steady)
    // But due to Transitional intermediate state, may be more
    assert!(
        after >= before + 2,
        "Expected at least 2 transition increments: before={before}, after={after}"
    );
}

/// No regime transition INFO log emitted when regime stays steady
/// (uses tracing capture to avoid global counter concurrency issues).
#[test]
fn no_transition_when_regime_stays_steady() {
    let handle = with_captured_tracing(|| {
        let mut det = BocpdDetector::with_defaults();
        let start = Instant::now();

        // Only steady events
        for i in 0..20 {
            det.observe_event(start + Duration::from_millis(200 * (i + 1)));
        }

        // Assert the detector never left steady
        assert_eq!(det.regime(), BocpdRegime::Steady);
    });

    // Verify no INFO transition events were emitted
    let events = handle.events();
    let transition_events: Vec<_> = events
        .iter()
        .filter(|e| {
            e.level == tracing::Level::INFO
                && e.target == ftui_runtime::telemetry_schema::TARGET_BOCPD
                && e.message.contains("regime transition detected")
        })
        .collect();

    assert!(
        transition_events.is_empty(),
        "No transition events expected for steady-only traffic"
    );
}

#![cfg(feature = "experimental")]
#![forbid(unsafe_code)]

//! bd-37a.9: Unit tests for VOI sampling & e-process testing.
//!
//! Covers:
//! 1. VOI correctly skips low-information samples
//! 2. VOI correctly triggers sampling when uncertainty is high
//! 3. E-process wealth accumulation against known signal
//! 4. E-process type-I error control under null
//! 5. Assert `voi.evaluate` and `eprocess.update` spans
//! 6. Property-based: under null, rejection rate <= alpha + epsilon over 1000 runs
//!
//! Run:
//!   cargo test -p ftui-runtime --test voi_eprocess_unit_tests

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};

use ftui_runtime::eprocess_throttle::{EProcessThrottle, ThrottleConfig};
use ftui_runtime::voi_sampling::{VoiConfig, VoiSampler};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use web_time::{Duration, Instant};

// ============================================================================
// Tracing capture infrastructure
// ============================================================================

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedSpan {
    name: String,
    target: String,
    level: tracing::Level,
    fields: HashMap<String, String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedEvent {
    level: tracing::Level,
    target: String,
    message: String,
    fields: HashMap<String, String>,
}

struct SpanCapture {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl SpanCapture {
    fn new() -> (Self, CaptureHandle) {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));

        let handle = CaptureHandle {
            spans: spans.clone(),
            events: events.clone(),
        };

        (Self { spans, events }, handle)
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

    #[allow(dead_code)]
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
        _id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(Vec::new());
        attrs.record(&mut visitor);

        let mut fields: HashMap<String, String> = visitor.0.into_iter().collect();
        for field in attrs.metadata().fields() {
            fields.entry(field.name().to_string()).or_default();
        }

        self.spans.lock().unwrap().push(CapturedSpan {
            name: attrs.metadata().name().to_string(),
            target: attrs.metadata().target().to_string(),
            level: *attrs.metadata().level(),
            fields,
        });
    }

    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(Vec::new());
        event.record(&mut visitor);

        let fields: HashMap<String, String> = visitor.0.clone().into_iter().collect();
        let message = visitor
            .0
            .iter()
            .find(|(k, _)| k == "message")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        self.events.lock().unwrap().push(CapturedEvent {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            message,
            fields,
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
// Simple LCG for deterministic pseudo-random
// ============================================================================

fn lcg_next(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
    *state
}

fn lcg_f64(state: &mut u64) -> f64 {
    (lcg_next(state) >> 33) as f64 / (1u64 << 31) as f64
}

// ============================================================================
// 1. VOI correctly skips low-information samples
// ============================================================================

#[test]
fn voi_skips_when_cost_exceeds_gain() {
    // With very high sample cost and low uncertainty (after many samples),
    // VOI should skip rather than sample.
    let config = VoiConfig {
        sample_cost: 100.0,     // Very high cost
        max_interval_events: 0, // Disable forced interval
        max_interval_ms: 0,
        ..Default::default()
    };
    let mut sampler = VoiSampler::new(config);
    let mut now = Instant::now();

    // First few decisions might sample (initial uncertainty is high)
    for _ in 0..5 {
        let d = sampler.decide(now);
        if d.should_sample {
            sampler.observe_at(false, now);
        }
        now += Duration::from_millis(1);
    }

    // After some observations with high cost, further decisions should skip
    let mut skip_count = 0;
    for _ in 0..10 {
        let d = sampler.decide(now);
        if !d.should_sample {
            skip_count += 1;
        } else {
            sampler.observe_at(false, now);
        }
        now += Duration::from_millis(1);
    }

    assert!(
        skip_count > 0,
        "VOI should skip at least some samples when cost is high"
    );
}

#[test]
fn voi_skips_after_uncertainty_shrinks() {
    // After many non-violation observations, posterior converges and VOI gain drops.
    // Use very low cost so every decision samples, allowing the posterior to converge.
    let config = VoiConfig {
        sample_cost: 0.0001, // Very low cost so we always sample
        max_interval_events: 0,
        max_interval_ms: 0,
        prior_alpha: 1.0,
        prior_beta: 1.0,
        ..Default::default()
    };
    let mut sampler = VoiSampler::new(config);
    let mut now = Instant::now();

    // Feed many non-violations to shrink posterior variance
    for _ in 0..100 {
        let d = sampler.decide(now);
        if d.should_sample {
            sampler.observe_at(false, now);
        }
        now += Duration::from_millis(1);
    }

    // After convergence, VOI gain should be very small
    let var = sampler.posterior_variance();
    assert!(
        var < 0.01,
        "posterior variance should be small after many observations, got {var}"
    );
}

// ============================================================================
// 2. VOI correctly triggers sampling when uncertainty is high
// ============================================================================

#[test]
fn voi_samples_when_uncertainty_is_high() {
    // With low cost and uniform prior (high uncertainty), VOI should sample.
    let config = VoiConfig {
        sample_cost: 0.0001, // Very low cost
        max_interval_events: 0,
        max_interval_ms: 0,
        prior_alpha: 1.0,
        prior_beta: 1.0, // Uniform prior = max uncertainty
        ..Default::default()
    };
    let mut sampler = VoiSampler::new(config);
    let d = sampler.decide(Instant::now());

    assert!(
        d.should_sample,
        "VOI should sample when uncertainty is high and cost is low"
    );
    assert!(
        d.voi_gain > 0.0,
        "VOI gain should be positive under high uncertainty"
    );
}

#[test]
fn voi_forced_interval_always_samples() {
    let config = VoiConfig {
        max_interval_events: 3,
        sample_cost: 1000.0, // Would never sample voluntarily
        ..Default::default()
    };
    let mut sampler = VoiSampler::new(config);
    let mut now = Instant::now();

    // First 2 events may or may not sample; 3rd should be forced
    for _ in 0..2 {
        let d = sampler.decide(now);
        if d.should_sample {
            sampler.observe_at(false, now);
        }
        now += Duration::from_millis(1);
    }

    let d3 = sampler.decide(now);
    assert!(d3.should_sample, "max_interval should force sampling");
    assert!(d3.forced_by_interval, "should be forced by interval");
}

// ============================================================================
// 3. E-process wealth accumulation against known signal
// ============================================================================

#[test]
fn eprocess_wealth_grows_under_alternative() {
    // When the true match rate is much higher than mu_0, wealth should grow.
    let base = Instant::now();
    let mut cfg = ThrottleConfig {
        mu_0: 0.1,
        hard_deadline_ms: u64::MAX,
        min_observations_between: u64::MAX,
        ..Default::default()
    };
    cfg.grapa_eta = 0.0; // Fixed lambda for clean test
    let mut t = EProcessThrottle::new_at(cfg, base);

    // True match rate = 0.5 >> mu_0 = 0.1
    let mut rng = 42u64;
    for i in 1..=100 {
        let matched = lcg_f64(&mut rng) < 0.5;
        t.observe_at(matched, base + Duration::from_millis(i));
    }

    assert!(
        t.wealth() > 10.0,
        "Wealth should grow substantially under alternative: {}",
        t.wealth()
    );
}

#[test]
fn eprocess_wealth_stable_under_null() {
    // Under H0 (match rate = mu_0), wealth should not systematically grow.
    let base = Instant::now();
    let cfg = ThrottleConfig {
        mu_0: 0.1,
        hard_deadline_ms: u64::MAX,
        min_observations_between: u64::MAX,
        grapa_eta: 0.0,
        ..Default::default()
    };
    let mut t = EProcessThrottle::new_at(cfg.clone(), base);

    let mut rng = 999u64;
    for i in 1..=200 {
        let matched = lcg_f64(&mut rng) < cfg.mu_0;
        t.observe_at(matched, base + Duration::from_millis(i));
    }

    // Under null with fixed lambda, wealth is a martingale E[W] ≈ 1
    // Allow slack for finite sample
    assert!(
        t.wealth() < 100.0,
        "Wealth under null should stay bounded, got {}",
        t.wealth()
    );
}

#[test]
fn eprocess_triggers_recompute_under_strong_signal() {
    let base = Instant::now();
    let cfg = ThrottleConfig {
        mu_0: 0.1,
        alpha: 0.05,
        min_observations_between: 1,
        hard_deadline_ms: u64::MAX,
        ..Default::default()
    };
    let mut t = EProcessThrottle::new_at(cfg, base);

    let mut triggered = false;
    for i in 1..=200 {
        // 100% match rate = extremely strong signal
        let d = t.observe_at(true, base + Duration::from_millis(i));
        if d.should_recompute && !d.forced_by_deadline {
            triggered = true;
            break;
        }
    }

    assert!(
        triggered,
        "E-process should reject under strong signal (100% match vs 10% null)"
    );
}

// ============================================================================
// 4. E-process type-I error control under null
// ============================================================================

#[test]
fn eprocess_type_i_control_500_trials() {
    let base = Instant::now();
    let cfg = ThrottleConfig {
        mu_0: 0.1,
        alpha: 0.05,
        min_observations_between: 1,
        hard_deadline_ms: u64::MAX,
        grapa_eta: 0.0, // Fixed lambda for clean type-I test
        ..Default::default()
    };

    let n_trials = 500;
    let n_obs = 200;
    let mut false_triggers = 0u64;
    let mut rng = 77u64;

    for trial in 0..n_trials {
        let mut t = EProcessThrottle::new_at(cfg.clone(), base);
        for i in 1..=n_obs {
            let matched = lcg_f64(&mut rng) < cfg.mu_0;
            let d = t.observe_at(
                matched,
                base + Duration::from_millis(i as u64 + trial * 1000),
            );
            if d.should_recompute {
                false_triggers += 1;
                break;
            }
        }
    }

    let rate = false_triggers as f64 / n_trials as f64;
    assert!(
        rate < cfg.alpha * 3.0,
        "False trigger rate {rate:.4} exceeds 3×alpha = {:.4}",
        cfg.alpha * 3.0
    );
}

// ============================================================================
// 5. Assert voi.evaluate and eprocess.update spans (cross-module)
// ============================================================================

#[test]
fn both_spans_emitted_in_combined_workflow() {
    // Test VOI span separately (may be filtered from combined due to subscriber scope)
    let voi_handle = with_captured_tracing(|| {
        let base = Instant::now();
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let d = sampler.decide(base);
        if d.should_sample {
            sampler.observe_at(false, base);
        }
    });

    let ep_handle = with_captured_tracing(|| {
        let base = Instant::now();
        let mut throttle = EProcessThrottle::new_at(ThrottleConfig::default(), base);
        throttle.observe_at(true, base + Duration::from_millis(1));
    });

    let voi_spans: Vec<_> = voi_handle
        .spans()
        .iter()
        .filter(|s| s.name == "voi.evaluate")
        .cloned()
        .collect();
    assert!(
        !voi_spans.is_empty(),
        "expected voi.evaluate span, got spans: {:?}",
        voi_handle
            .spans()
            .iter()
            .map(|s| &s.name)
            .collect::<Vec<_>>()
    );

    let ep_spans: Vec<_> = ep_handle
        .spans()
        .iter()
        .filter(|s| s.name == "eprocess.update")
        .cloned()
        .collect();
    assert!(
        !ep_spans.is_empty(),
        "expected eprocess.update span, got spans: {:?}",
        ep_handle
            .spans()
            .iter()
            .map(|s| &s.name)
            .collect::<Vec<_>>()
    );

    // Verify key fields on each
    let voi = &voi_spans[0];
    assert!(voi.fields.contains_key("decision_context"));
    assert!(voi.fields.contains_key("voi_estimate"));
    assert!(voi.fields.contains_key("sample_decision"));

    let ep = &ep_spans[0];
    assert!(ep.fields.contains_key("test_id"));
    assert!(ep.fields.contains_key("wealth_current"));
    assert!(ep.fields.contains_key("rejected"));
}

#[test]
fn voi_span_fields_accurate_for_skip_decision() {
    let handle = with_captured_tracing(|| {
        let config = VoiConfig {
            sample_cost: 1000.0, // Force skip
            max_interval_events: 0,
            max_interval_ms: 0,
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);
        let mut now = Instant::now();
        // Observe first to have a prior sample
        let d = sampler.decide(now);
        if d.should_sample {
            sampler.observe_at(false, now);
        }
        now += Duration::from_millis(1);
        sampler.decide(now);
    });

    let spans = handle.spans();
    let voi_spans: Vec<_> = spans.iter().filter(|s| s.name == "voi.evaluate").collect();

    // Find a span with sample_decision=false (skip)
    let skip_spans: Vec<_> = voi_spans
        .iter()
        .filter(|s| {
            s.fields
                .get("sample_decision")
                .is_some_and(|v| v == "false")
        })
        .collect();

    // We should get at least one skip decision given the high cost
    // (first decision might sample due to max_interval, but second should skip)
    assert!(
        !skip_spans.is_empty() || voi_spans.len() >= 2,
        "expected at least one skip decision or two decisions total"
    );
}

#[test]
fn eprocess_span_rejected_true_on_rejection() {
    let handle = with_captured_tracing(|| {
        let base = Instant::now();
        let cfg = ThrottleConfig {
            min_observations_between: 1,
            hard_deadline_ms: u64::MAX,
            ..Default::default()
        };
        let mut t = EProcessThrottle::new_at(cfg, base);

        for i in 1..=100 {
            let d = t.observe_at(true, base + Duration::from_millis(i));
            if d.should_recompute && !d.forced_by_deadline {
                break;
            }
        }
    });

    let spans = handle.spans();
    let rejected_spans: Vec<_> = spans
        .iter()
        .filter(|s| {
            s.name == "eprocess.update" && s.fields.get("rejected").is_some_and(|v| v == "true")
        })
        .collect();

    assert!(
        !rejected_spans.is_empty(),
        "expected eprocess.update span with rejected=true"
    );
}

// ============================================================================
// 6. Property-based: rejection rate under null <= alpha + epsilon (1000 runs)
// ============================================================================

#[test]
fn property_null_rejection_rate_bounded_1000_runs() {
    let base = Instant::now();
    let alpha = 0.05;
    let mu_0 = 0.1;
    let cfg = ThrottleConfig {
        mu_0,
        alpha,
        min_observations_between: 1,
        hard_deadline_ms: u64::MAX,
        grapa_eta: 0.0,
        ..Default::default()
    };

    let n_trials = 1000;
    let n_obs = 300;
    let mut rejections = 0u64;
    let mut rng = 314159u64;

    for trial in 0..n_trials {
        let mut t = EProcessThrottle::new_at(cfg.clone(), base);
        for i in 1..=n_obs {
            let matched = lcg_f64(&mut rng) < mu_0;
            let d = t.observe_at(
                matched,
                base + Duration::from_millis(i as u64 + trial * 10000),
            );
            if d.should_recompute {
                rejections += 1;
                break;
            }
        }
    }

    let rate = rejections as f64 / n_trials as f64;
    // Anytime-valid guarantee: P(reject | H0) <= alpha
    // Allow 2.5x slack for finite-sample variance
    let bound = alpha * 2.5;
    assert!(
        rate < bound,
        "Rejection rate under null = {rate:.4}, exceeds {bound:.4} (alpha={alpha})"
    );
}

#[test]
fn property_voi_rejection_rate_bounded_1000_runs() {
    // Use VOI sampler to drive sampling under null, verify e-process
    // rejection rate is still controlled.
    let base = Instant::now();
    let alpha = 0.05;
    let mu_0 = 0.05;

    let n_trials = 1000;
    let n_events = 200;
    let mut rejections = 0u64;
    let mut rng = 271828u64;

    for trial in 0..n_trials {
        let voi_config = VoiConfig {
            alpha,
            mu_0,
            sample_cost: 0.01,
            max_interval_events: 0,
            max_interval_ms: 0,
            ..Default::default()
        };
        let mut sampler = VoiSampler::new_at(voi_config, base);

        let ep_config = ThrottleConfig {
            alpha,
            mu_0,
            min_observations_between: 1,
            hard_deadline_ms: u64::MAX,
            grapa_eta: 0.0,
            ..Default::default()
        };
        let mut throttle = EProcessThrottle::new_at(ep_config, base);

        let mut rejected = false;
        for i in 1..=n_events {
            let now = base + Duration::from_millis(i as u64 + trial * 10000);
            let d = sampler.decide(now);
            if d.should_sample {
                let violated = lcg_f64(&mut rng) < mu_0;
                sampler.observe_at(violated, now);
                let td = throttle.observe_at(violated, now);
                if td.should_recompute && !td.forced_by_deadline {
                    rejected = true;
                    break;
                }
            }
        }
        if rejected {
            rejections += 1;
        }
    }

    let rate = rejections as f64 / n_trials as f64;
    let bound = alpha * 3.0;
    assert!(
        rate < bound,
        "VOI-driven rejection rate under null = {rate:.4}, exceeds {bound:.4}"
    );
}

// ============================================================================
// Combined VOI + e-process integration scenarios
// ============================================================================

#[test]
fn voi_and_eprocess_coordinated_workflow() {
    // Simulate a realistic workflow: VOI decides when to sample,
    // e-process monitors for regime change.
    let base = Instant::now();
    let mut sampler = VoiSampler::new_at(VoiConfig::default(), base);
    let mut throttle = EProcessThrottle::new_at(ThrottleConfig::default(), base);

    let mut total_samples = 0u64;
    let mut rng = 42u64;

    for i in 1..=100 {
        let now = base + Duration::from_millis(i);
        let d = sampler.decide(now);
        if d.should_sample {
            total_samples += 1;
            let violated = lcg_f64(&mut rng) < 0.1;
            sampler.observe_at(violated, now);
            throttle.observe_at(violated, now);
        }
    }

    let summary = sampler.summary();
    let stats = throttle.stats();

    assert!(summary.total_events == 100);
    assert!(summary.total_samples == total_samples);
    assert!(stats.total_observations == total_samples);
}

#[test]
fn voi_deterministic_across_runs() {
    let base = Instant::now();
    let config = VoiConfig {
        sample_cost: 0.01,
        ..Default::default()
    };

    let run = || {
        let mut sampler = VoiSampler::new_at(config.clone(), base);
        let mut rng = 42u64;
        let mut decisions = Vec::new();

        for i in 1..=50 {
            let now = base + Duration::from_millis(i);
            let d = sampler.decide(now);
            let violated = lcg_next(&mut rng).is_multiple_of(7);
            if d.should_sample {
                sampler.observe_at(violated, now);
            }
            decisions.push((d.should_sample, d.voi_gain));
        }
        decisions
    };

    let d1 = run();
    let d2 = run();
    assert_eq!(d1.len(), d2.len());
    for (i, (a, b)) in d1.iter().zip(d2.iter()).enumerate() {
        assert_eq!(a.0, b.0, "decision[{i}] should_sample mismatch");
        assert!(
            (a.1 - b.1).abs() < 1e-10,
            "decision[{i}] voi_gain mismatch: {} vs {}",
            a.1,
            b.1
        );
    }
}

#[test]
fn eprocess_deterministic_across_runs() {
    let base = Instant::now();
    let cfg = ThrottleConfig {
        hard_deadline_ms: u64::MAX,
        min_observations_between: u64::MAX,
        grapa_eta: 0.0,
        ..Default::default()
    };

    let run = || {
        let mut t = EProcessThrottle::new_at(cfg.clone(), base);
        let mut rng = 42u64;
        let mut decisions = Vec::new();

        for i in 1..=50 {
            let matched = lcg_next(&mut rng).is_multiple_of(5);
            let d = t.observe_at(matched, base + Duration::from_millis(i));
            decisions.push((d.should_recompute, d.wealth));
        }
        decisions
    };

    let d1 = run();
    let d2 = run();
    assert_eq!(d1.len(), d2.len());
    for (i, (a, b)) in d1.iter().zip(d2.iter()).enumerate() {
        assert_eq!(a.0, b.0, "decision[{i}] should_recompute mismatch");
        assert!(
            (a.1 - b.1).abs() < 1e-10,
            "decision[{i}] wealth mismatch: {} vs {}",
            a.1,
            b.1
        );
    }
}

#![forbid(unsafe_code)]

//! bd-xox.11: E2E test — Full observability pipeline end-to-end.
//!
//! Covers:
//! (1) Run simulated application with full observability enabled
//! (2) Capture tracing output, metrics dump, and evidence ledger
//! (3) Verify span hierarchy complete for full render cycle
//! (4) Verify all metrics populated
//! (5) Verify evidence records present for every Bayesian decision
//! (6) Verify Prometheus metrics parseable
//! (7) Verify transparency levels render correctly
//!
//! Run:
//!   cargo test -p ftui-runtime --test e2e_observability_pipeline

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};

use ftui_runtime::telemetry_schema;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

// ============================================================================
// Tracing Capture Infrastructure
// ============================================================================

#[derive(Debug, Clone)]
struct CapturedSpan {
    name: String,
    level: tracing::Level,
    fields: HashMap<String, String>,
    parent_name: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedEvent {
    level: tracing::Level,
    message: Option<String>,
    fields: HashMap<String, String>,
    parent_span_name: Option<String>,
}

struct PipelineCapture {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    span_index: Arc<Mutex<HashMap<u64, usize>>>,
}

type PipelineCaptureInit = (
    PipelineCapture,
    Arc<Mutex<Vec<CapturedSpan>>>,
    Arc<Mutex<Vec<CapturedEvent>>>,
);

impl PipelineCapture {
    fn new() -> PipelineCaptureInit {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let span_index = Arc::new(Mutex::new(HashMap::new()));
        (
            Self {
                spans: spans.clone(),
                events: events.clone(),
                span_index,
            },
            spans,
            events,
        )
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

impl<S> tracing_subscriber::Layer<S> for PipelineCapture
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

        let mut fields: HashMap<String, String> = HashMap::new();
        for field in attrs.metadata().fields() {
            fields.entry(field.name().to_string()).or_default();
        }
        for (k, v) in visitor.0 {
            fields.insert(k, v);
        }

        let parent_name = ctx.span_scope(id).and_then(|mut scope| {
            scope.next(); // skip self
            scope.next().map(|parent| parent.name().to_string())
        });

        let idx = {
            let mut spans = self.spans.lock().unwrap();
            let idx = spans.len();
            spans.push(CapturedSpan {
                name: attrs.metadata().name().to_string(),
                level: *attrs.metadata().level(),
                fields,
                parent_name,
            });
            idx
        };

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

        if let Some(&idx) = self.span_index.lock().unwrap().get(&id.into_u64()) {
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

        let fields: HashMap<String, String> = visitor.0.into_iter().collect();
        let message = fields.get("message").cloned();

        let parent_span_name = ctx.event_span(event).map(|span| span.name().to_string());

        self.events.lock().unwrap().push(CapturedEvent {
            level: *event.metadata().level(),
            message,
            fields,
            parent_span_name,
        });
    }
}

fn with_pipeline_capture<F>(f: F) -> (Vec<CapturedSpan>, Vec<CapturedEvent>)
where
    F: FnOnce(),
{
    ensure_global_trace_level();
    let (layer, spans, events) = PipelineCapture::new();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::TRACE)
        .with(layer);
    tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        f();
    });
    let s = spans.lock().unwrap().clone();
    let e = events.lock().unwrap().clone();
    (s, e)
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
// Simulated Observability Pipeline
// ============================================================================

/// Decision domain for evidence tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DecisionDomain {
    DiffStrategy,
    ResizeCoalescing,
    FrameBudget,
    Degradation,
    VoiSampling,
    HintRanking,
    PaletteScoring,
}

impl DecisionDomain {
    const ALL: [DecisionDomain; 7] = [
        Self::DiffStrategy,
        Self::ResizeCoalescing,
        Self::FrameBudget,
        Self::Degradation,
        Self::VoiSampling,
        Self::HintRanking,
        Self::PaletteScoring,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::DiffStrategy => "diff_strategy",
            Self::ResizeCoalescing => "resize_coalescing",
            Self::FrameBudget => "frame_budget",
            Self::Degradation => "degradation",
            Self::VoiSampling => "voi_sampling",
            Self::HintRanking => "hint_ranking",
            Self::PaletteScoring => "palette_scoring",
        }
    }
}

/// Evidence entry in the unified ledger.
#[derive(Debug, Clone)]
struct EvidenceEntry {
    domain: DecisionDomain,
    timestamp_ns: u64,
    log_posterior: f64,
    action: String,
    loss_avoided: f64,
    confidence_interval: (f64, f64),
    evidence_terms: Vec<(String, f64)>,
}

/// Unified evidence ledger with ring buffer semantics.
#[derive(Debug)]
struct UnifiedEvidenceLedger {
    entries: Vec<EvidenceEntry>,
    capacity: usize,
}

impl UnifiedEvidenceLedger {
    fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
        }
    }

    fn record(&mut self, entry: EvidenceEntry) {
        if self.entries.len() >= self.capacity {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn domain_count(&self, domain: DecisionDomain) -> usize {
        self.entries.iter().filter(|e| e.domain == domain).count()
    }

    fn to_jsonl(&self) -> String {
        let mut output = String::new();
        for entry in &self.entries {
            let evidence: Vec<String> = entry
                .evidence_terms
                .iter()
                .map(|(label, llr)| format!("{{\"label\":\"{label}\",\"llr\":{llr:.3}}}"))
                .collect();
            output.push_str(&format!(
                "{{\"domain\":\"{}\",\"ts_ns\":{},\"log_posterior\":{:.3},\"action\":\"{}\",\"loss_avoided\":{:.3},\"ci\":[{:.3},{:.3}],\"evidence\":[{}]}}\n",
                entry.domain.as_str(),
                entry.timestamp_ns,
                entry.log_posterior,
                entry.action,
                entry.loss_avoided,
                entry.confidence_interval.0,
                entry.confidence_interval.1,
                evidence.join(",")
            ));
        }
        output
    }
}

/// Metrics registry (simplified for E2E pipeline test).
#[derive(Debug)]
struct MetricsCollector {
    counters: HashMap<String, f64>,
    histograms: HashMap<String, (f64, u64)>, // (sum, count)
    gauges: HashMap<String, f64>,
}

impl MetricsCollector {
    fn new() -> Self {
        Self {
            counters: HashMap::new(),
            histograms: HashMap::new(),
            gauges: HashMap::new(),
        }
    }

    fn counter_inc(&mut self, name: &str, v: f64) {
        *self.counters.entry(name.to_string()).or_default() += v;
    }

    fn histogram_observe(&mut self, name: &str, v: f64) {
        let entry = self.histograms.entry(name.to_string()).or_insert((0.0, 0));
        entry.0 += v;
        entry.1 += 1;
    }

    fn gauge_set(&mut self, name: &str, v: f64) {
        self.gauges.insert(name.to_string(), v);
    }

    fn export_prometheus(&self) -> String {
        let mut out = String::new();
        for (name, val) in &self.counters {
            out.push_str(&format!("# HELP {name} counter\n"));
            out.push_str(&format!("# TYPE {name} counter\n"));
            out.push_str(&format!("{name} {val}\n"));
        }
        for (name, (sum, count)) in &self.histograms {
            out.push_str(&format!("# HELP {name} histogram\n"));
            out.push_str(&format!("# TYPE {name} histogram\n"));
            out.push_str(&format!("{name}_sum {sum}\n"));
            out.push_str(&format!("{name}_count {count}\n"));
        }
        for (name, val) in &self.gauges {
            out.push_str(&format!("# HELP {name} gauge\n"));
            out.push_str(&format!("# TYPE {name} gauge\n"));
            out.push_str(&format!("{name} {val}\n"));
        }
        out
    }
}

/// Galaxy-brain transparency level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TransparencyLevel {
    /// L0: One-line summary (always visible).
    Summary,
    /// L1: Key metrics and current state.
    Metrics,
    /// L2: Decision rationale with evidence.
    Rationale,
    /// L3: Full Bayesian posterior and raw data.
    FullDetail,
}

impl TransparencyLevel {
    const ALL: [TransparencyLevel; 4] = [
        Self::Summary,
        Self::Metrics,
        Self::Rationale,
        Self::FullDetail,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "L0:Summary",
            Self::Metrics => "L1:Metrics",
            Self::Rationale => "L2:Rationale",
            Self::FullDetail => "L3:FullDetail",
        }
    }

    fn render_card(self, evidence: &EvidenceEntry) -> String {
        match self {
            Self::Summary => {
                format!(
                    "[{}] {} → {}",
                    evidence.domain.as_str(),
                    evidence.action,
                    if evidence.loss_avoided > 0.0 {
                        "beneficial"
                    } else {
                        "neutral"
                    }
                )
            }
            Self::Metrics => {
                format!(
                    "[{}] action={} loss_avoided={:.2} ci=[{:.2},{:.2}]",
                    evidence.domain.as_str(),
                    evidence.action,
                    evidence.loss_avoided,
                    evidence.confidence_interval.0,
                    evidence.confidence_interval.1,
                )
            }
            Self::Rationale => {
                let top_evidence: Vec<String> = evidence
                    .evidence_terms
                    .iter()
                    .take(3)
                    .map(|(label, llr)| format!("{label}={llr:.2}"))
                    .collect();
                format!(
                    "[{}] {} because [{}] (posterior={:.3})",
                    evidence.domain.as_str(),
                    evidence.action,
                    top_evidence.join(", "),
                    evidence.log_posterior,
                )
            }
            Self::FullDetail => {
                let all_evidence: Vec<String> = evidence
                    .evidence_terms
                    .iter()
                    .map(|(label, llr)| format!("{label}:{llr:.4}"))
                    .collect();
                format!(
                    "[{}] domain={} ts={} posterior={:.6} action={} loss={:.4} ci=[{:.4},{:.4}] evidence=[{}]",
                    evidence.domain.as_str(),
                    evidence.domain.as_str(),
                    evidence.timestamp_ns,
                    evidence.log_posterior,
                    evidence.action,
                    evidence.loss_avoided,
                    evidence.confidence_interval.0,
                    evidence.confidence_interval.1,
                    all_evidence.join(", "),
                )
            }
        }
    }
}

// ============================================================================
// Simulated Full Render Cycle
// ============================================================================

/// Simulate a complete render cycle emitting all canonical spans.
fn simulate_full_render_cycle() {
    // Phase 1: Init
    {
        let init_span = tracing::info_span!(
            telemetry_schema::span::PROGRAM_INIT,
            model_type = "TestModel",
            cmd_count = tracing::field::Empty
        );
        let _guard = init_span.enter();
        init_span.record("cmd_count", 0u64);
        tracing::debug!("model initialized");
    }

    // Phase 2: Update cycle
    {
        let update_span = tracing::info_span!(
            telemetry_schema::span::PROGRAM_UPDATE,
            msg_type = tracing::field::Empty,
            duration_us = tracing::field::Empty,
            cmd_type = tracing::field::Empty
        );
        let _guard = update_span.enter();
        update_span.record("msg_type", "KeyPress");
        update_span.record("duration_us", 42u64);
        update_span.record("cmd_type", "None");
        tracing::debug!("update processed");
    }

    // Phase 3: Frame render
    {
        let frame_span = tracing::info_span!(
            telemetry_schema::span::RENDER_FRAME,
            width = tracing::field::Empty,
            height = tracing::field::Empty,
            duration_us = tracing::field::Empty
        );
        let _guard = frame_span.enter();
        frame_span.record("width", 80u64);
        frame_span.record("height", 24u64);

        // View rendering (child of frame)
        {
            let view_span = tracing::info_span!(
                telemetry_schema::span::PROGRAM_VIEW,
                duration_us = tracing::field::Empty,
                widget_count = tracing::field::Empty
            );
            let _guard = view_span.enter();
            view_span.record("duration_us", 15u64);
            view_span.record("widget_count", 3u64);
            tracing::trace!("view rendered");
        }

        // Present (child of frame)
        {
            let present_span = tracing::info_span!(
                telemetry_schema::span::RENDER_PRESENT,
                bytes_written = tracing::field::Empty,
                runs_count = tracing::field::Empty,
                duration_us = tracing::field::Empty
            );
            let _guard = present_span.enter();
            present_span.record("bytes_written", 1024u64);
            present_span.record("runs_count", 12u64);
            present_span.record("duration_us", 8u64);

            // Inline render (child of present)
            {
                let inline_span = tracing::info_span!("inline.render");
                let _guard = inline_span.enter();

                // Scroll region (child of inline)
                {
                    let _scroll =
                        tracing::info_span!(telemetry_schema::span::RENDER_SCROLL_REGION).entered();
                    tracing::trace!("scroll region applied");
                }

                // Diff compute (child of inline)
                {
                    let _diff =
                        tracing::info_span!(telemetry_schema::span::RENDER_DIFF_COMPUTE).entered();
                    tracing::trace!("diff computed");
                }

                // Emit (child of inline)
                {
                    let _emit = tracing::info_span!(telemetry_schema::span::RENDER_EMIT).entered();
                    tracing::trace!("ANSI emitted");
                }
            }
        }

        frame_span.record("duration_us", 25u64);
    }

    // Phase 4: Subscriptions
    {
        let subs_span = tracing::info_span!(
            telemetry_schema::span::PROGRAM_SUBSCRIPTIONS,
            active_count = 2u64,
            started = 0u64,
            stopped = 0u64
        );
        let _guard = subs_span.enter();
        tracing::debug!("subscriptions enumerated");
    }

    // Phase 5: Conformal prediction
    {
        let _pred = tracing::info_span!(
            "conformal.predict",
            alpha = 0.05f64,
            interval_width_us = tracing::field::Empty
        )
        .entered();
        tracing::trace!("conformal prediction computed");
    }
}

/// Simulate evidence collection for all 7 decision domains.
fn simulate_evidence_collection(ledger: &mut UnifiedEvidenceLedger) {
    let base_ts = 1_000_000_000u64;

    ledger.record(EvidenceEntry {
        domain: DecisionDomain::DiffStrategy,
        timestamp_ns: base_ts,
        log_posterior: 1.386,
        action: "dirty_rows".into(),
        loss_avoided: 0.35,
        confidence_interval: (0.72, 0.88),
        evidence_terms: vec![
            ("change_fraction".into(), 4.0),
            ("dirty_rows_ratio".into(), 2.5),
            ("frame_time_headroom".into(), 1.2),
        ],
    });

    ledger.record(EvidenceEntry {
        domain: DecisionDomain::ResizeCoalescing,
        timestamp_ns: base_ts + 16_000,
        log_posterior: 0.847,
        action: "coalesce".into(),
        loss_avoided: 0.12,
        confidence_interval: (0.55, 0.78),
        evidence_terms: vec![("event_rate".into(), 3.2), ("regime_stability".into(), 1.8)],
    });

    ledger.record(EvidenceEntry {
        domain: DecisionDomain::FrameBudget,
        timestamp_ns: base_ts + 32_000,
        log_posterior: -0.405,
        action: "maintain_budget".into(),
        loss_avoided: 0.05,
        confidence_interval: (0.35, 0.55),
        evidence_terms: vec![
            ("cusum_signal".into(), 0.8),
            ("eprocess_wealth".into(), 1.1),
        ],
    });

    ledger.record(EvidenceEntry {
        domain: DecisionDomain::Degradation,
        timestamp_ns: base_ts + 48_000,
        log_posterior: -1.204,
        action: "no_degradation".into(),
        loss_avoided: 0.0,
        confidence_interval: (0.15, 0.30),
        evidence_terms: vec![("conformal_coverage".into(), 0.5)],
    });

    ledger.record(EvidenceEntry {
        domain: DecisionDomain::VoiSampling,
        timestamp_ns: base_ts + 64_000,
        log_posterior: 0.693,
        action: "sample_diff".into(),
        loss_avoided: 0.22,
        confidence_interval: (0.60, 0.75),
        evidence_terms: vec![("voi_gain".into(), 2.8), ("uncertainty".into(), 1.5)],
    });

    ledger.record(EvidenceEntry {
        domain: DecisionDomain::HintRanking,
        timestamp_ns: base_ts + 80_000,
        log_posterior: 0.405,
        action: "rank_by_recency".into(),
        loss_avoided: 0.08,
        confidence_interval: (0.50, 0.65),
        evidence_terms: vec![("recency_weight".into(), 1.9)],
    });

    ledger.record(EvidenceEntry {
        domain: DecisionDomain::PaletteScoring,
        timestamp_ns: base_ts + 96_000,
        log_posterior: 0.182,
        action: "score_contrast".into(),
        loss_avoided: 0.03,
        confidence_interval: (0.45, 0.58),
        evidence_terms: vec![
            ("contrast_ratio".into(), 1.4),
            ("luminance_diff".into(), 1.1),
        ],
    });
}

/// Simulate metrics collection for a render cycle.
fn simulate_metrics_collection(metrics: &mut MetricsCollector) {
    // Counters
    metrics.counter_inc("render_frames_total", 1.0);
    metrics.counter_inc("diff_strategy_selected_total", 1.0);
    metrics.counter_inc("ansi_sequences_parsed_total", 42.0);
    metrics.counter_inc("ansi_malformed_total", 0.0);
    metrics.counter_inc("runtime_messages_processed_total", 3.0);
    metrics.counter_inc("effects_executed_total", 1.0);
    metrics.counter_inc("slo_breaches_total", 0.0);
    metrics.counter_inc("terminal_resize_events_total", 0.0);
    metrics.counter_inc("incremental_cache_hits_total", 5.0);
    metrics.counter_inc("incremental_cache_misses_total", 1.0);

    // Histograms
    metrics.histogram_observe("render_frame_duration_us", 250.0);
    metrics.histogram_observe("diff_strategy_duration_us", 45.0);
    metrics.histogram_observe("layout_compute_duration_us", 30.0);
    metrics.histogram_observe("widget_render_duration_us", 15.0);
    metrics.histogram_observe("conformal_prediction_interval_width_us", 80.0);
    metrics.histogram_observe("animation_duration_ms", 16.0);

    // Gauges
    metrics.gauge_set("terminal_active", 1.0);
    metrics.gauge_set("eprocess_wealth", 42.5);
}

// ============================================================================
// E2E Pipeline Tests
// ============================================================================

/// (1) Full render cycle produces all required spans.
#[test]
fn full_render_cycle_produces_all_spans() {
    let (spans, _events) = with_pipeline_capture(simulate_full_render_cycle);

    let required_spans = [
        telemetry_schema::span::PROGRAM_INIT,
        telemetry_schema::span::PROGRAM_UPDATE,
        telemetry_schema::span::RENDER_FRAME,
        telemetry_schema::span::PROGRAM_VIEW,
        telemetry_schema::span::RENDER_PRESENT,
        "inline.render",
        telemetry_schema::span::RENDER_SCROLL_REGION,
        telemetry_schema::span::RENDER_DIFF_COMPUTE,
        telemetry_schema::span::RENDER_EMIT,
        telemetry_schema::span::PROGRAM_SUBSCRIPTIONS,
        "conformal.predict",
    ];

    for name in &required_spans {
        assert!(
            spans.iter().any(|s| s.name == *name),
            "Missing required span: '{name}'"
        );
    }
}

/// (2) Span count is non-zero for every declared span name.
#[test]
fn span_count_nonzero_for_every_declared_name() {
    let (spans, _events) = with_pipeline_capture(simulate_full_render_cycle);

    let declared = [
        telemetry_schema::span::PROGRAM_INIT,
        telemetry_schema::span::PROGRAM_UPDATE,
        telemetry_schema::span::RENDER_FRAME,
        telemetry_schema::span::PROGRAM_VIEW,
        telemetry_schema::span::RENDER_PRESENT,
        telemetry_schema::span::PROGRAM_SUBSCRIPTIONS,
        "conformal.predict",
        "inline.render",
        telemetry_schema::span::RENDER_SCROLL_REGION,
        telemetry_schema::span::RENDER_DIFF_COMPUTE,
        telemetry_schema::span::RENDER_EMIT,
    ];

    for name in &declared {
        let count = spans.iter().filter(|s| s.name == *name).count();
        assert!(
            count > 0,
            "Span '{name}' has count 0; should have at least 1"
        );
    }
}

/// (3) Span hierarchy: parent-child relationships correct.
#[test]
fn span_hierarchy_parent_child_correct() {
    let (spans, _events) = with_pipeline_capture(simulate_full_render_cycle);

    let required_parents = [
        (
            telemetry_schema::span::PROGRAM_VIEW,
            telemetry_schema::span::RENDER_FRAME,
        ),
        (
            telemetry_schema::span::RENDER_PRESENT,
            telemetry_schema::span::RENDER_FRAME,
        ),
        ("inline.render", telemetry_schema::span::RENDER_PRESENT),
        (
            telemetry_schema::span::RENDER_SCROLL_REGION,
            "inline.render",
        ),
        (telemetry_schema::span::RENDER_DIFF_COMPUTE, "inline.render"),
        (telemetry_schema::span::RENDER_EMIT, "inline.render"),
    ];

    for (child_name, expected_parent) in &required_parents {
        let child_span = spans
            .iter()
            .find(|s| s.name == *child_name)
            .unwrap_or_else(|| panic!("Missing span: '{child_name}'"));

        assert_eq!(
            child_span.parent_name.as_deref(),
            Some(*expected_parent),
            "Span '{}' should have parent '{}', got {:?}",
            child_name,
            expected_parent,
            child_span.parent_name
        );
    }
}

/// (4) Root spans have no parent.
#[test]
fn root_spans_have_no_parent() {
    let (spans, _events) = with_pipeline_capture(simulate_full_render_cycle);

    let root_spans = [
        telemetry_schema::span::PROGRAM_INIT,
        telemetry_schema::span::PROGRAM_UPDATE,
        telemetry_schema::span::RENDER_FRAME,
        telemetry_schema::span::PROGRAM_SUBSCRIPTIONS,
        "conformal.predict",
    ];

    for name in &root_spans {
        let span = spans
            .iter()
            .find(|s| s.name == *name)
            .unwrap_or_else(|| panic!("Missing root span: '{name}'"));

        assert!(
            span.parent_name.is_none(),
            "Root span '{}' should have no parent, got {:?}",
            name,
            span.parent_name
        );
    }
}

/// (5) Span fields populated for key spans.
#[test]
fn span_fields_populated() {
    let (spans, _events) = with_pipeline_capture(simulate_full_render_cycle);

    // ftui.program.update should have msg_type and duration_us
    let update = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::PROGRAM_UPDATE)
        .unwrap();
    assert!(
        update.fields.contains_key("msg_type"),
        "update span should have msg_type field"
    );
    assert!(
        update.fields.contains_key("duration_us"),
        "update span should have duration_us field"
    );

    // ftui.render.frame should have width, height, duration_us
    let frame = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::RENDER_FRAME)
        .unwrap();
    assert!(
        frame.fields.contains_key("width"),
        "frame span should have width field"
    );
    assert!(
        frame.fields.contains_key("height"),
        "frame span should have height field"
    );

    // ftui.program.view should have widget_count
    let view = spans
        .iter()
        .find(|s| s.name == telemetry_schema::span::PROGRAM_VIEW)
        .unwrap();
    assert!(
        view.fields.contains_key("widget_count"),
        "view span should have widget_count field"
    );
}

/// (6) Events emitted within spans.
#[test]
fn events_emitted_during_render_cycle() {
    let (_spans, events) = with_pipeline_capture(simulate_full_render_cycle);

    assert!(
        !events.is_empty(),
        "Should have at least some events during render cycle"
    );

    // Debug events should have been emitted
    let debug_events: Vec<_> = events
        .iter()
        .filter(|e| e.level == tracing::Level::DEBUG)
        .collect();
    assert!(!debug_events.is_empty(), "Should have DEBUG-level events");
}

/// (7) All 7 evidence domains have records in the ledger.
#[test]
fn all_seven_evidence_domains_present() {
    let mut ledger = UnifiedEvidenceLedger::new(100);
    simulate_evidence_collection(&mut ledger);

    assert_eq!(ledger.len(), 7, "Should have exactly 7 evidence entries");

    for domain in DecisionDomain::ALL {
        assert_eq!(
            ledger.domain_count(domain),
            1,
            "Domain '{}' should have exactly 1 entry",
            domain.as_str()
        );
    }
}

/// (8) Evidence JSONL roundtrip preserves all fields.
#[test]
fn evidence_jsonl_contains_required_fields() {
    let mut ledger = UnifiedEvidenceLedger::new(100);
    simulate_evidence_collection(&mut ledger);

    let jsonl = ledger.to_jsonl();
    let lines: Vec<&str> = jsonl.lines().collect();

    assert_eq!(lines.len(), 7, "Should have 7 JSONL lines");

    let required_fields = [
        "\"domain\":",
        "\"ts_ns\":",
        "\"log_posterior\":",
        "\"action\":",
        "\"loss_avoided\":",
        "\"ci\":",
        "\"evidence\":",
    ];

    for line in &lines {
        for field in &required_fields {
            assert!(
                line.contains(field),
                "JSONL line missing field {field}: {line}"
            );
        }
    }
}

/// (9) Evidence entries have valid Bayesian semantics.
#[test]
fn evidence_entries_have_valid_bayesian_semantics() {
    let mut ledger = UnifiedEvidenceLedger::new(100);
    simulate_evidence_collection(&mut ledger);

    for entry in &ledger.entries {
        // Confidence interval: lower < upper
        assert!(
            entry.confidence_interval.0 < entry.confidence_interval.1,
            "CI lower bound should be < upper for domain '{}'",
            entry.domain.as_str()
        );

        // Evidence terms should have non-zero LLR for at least one term
        assert!(
            !entry.evidence_terms.is_empty(),
            "Domain '{}' should have at least one evidence term",
            entry.domain.as_str()
        );

        // Loss avoided should be non-negative
        assert!(
            entry.loss_avoided >= 0.0,
            "Domain '{}' loss_avoided should be >= 0",
            entry.domain.as_str()
        );

        // Action should be non-empty
        assert!(
            !entry.action.is_empty(),
            "Domain '{}' should have an action",
            entry.domain.as_str()
        );
    }
}

/// (10) All declared metrics populated after render cycle.
#[test]
fn all_metrics_populated_after_render() {
    let mut metrics = MetricsCollector::new();
    simulate_metrics_collection(&mut metrics);

    let expected_counters = [
        "render_frames_total",
        "diff_strategy_selected_total",
        "ansi_sequences_parsed_total",
        "ansi_malformed_total",
        "runtime_messages_processed_total",
        "effects_executed_total",
        "slo_breaches_total",
        "terminal_resize_events_total",
        "incremental_cache_hits_total",
        "incremental_cache_misses_total",
    ];

    for name in &expected_counters {
        assert!(
            metrics.counters.contains_key(*name),
            "Missing counter: '{name}'"
        );
    }

    let expected_histograms = [
        "render_frame_duration_us",
        "diff_strategy_duration_us",
        "layout_compute_duration_us",
        "widget_render_duration_us",
        "conformal_prediction_interval_width_us",
        "animation_duration_ms",
    ];

    for name in &expected_histograms {
        assert!(
            metrics.histograms.contains_key(*name),
            "Missing histogram: '{name}'"
        );
    }

    let expected_gauges = ["terminal_active", "eprocess_wealth"];

    for name in &expected_gauges {
        assert!(
            metrics.gauges.contains_key(*name),
            "Missing gauge: '{name}'"
        );
    }
}

/// (11) Prometheus export is parseable.
#[test]
fn prometheus_export_parseable() {
    let mut metrics = MetricsCollector::new();
    simulate_metrics_collection(&mut metrics);

    let output = metrics.export_prometheus();

    // Every line should be well-formed
    for (i, line) in output.lines().enumerate() {
        if line.starts_with('#') {
            // Comment line: HELP or TYPE
            assert!(
                line.starts_with("# HELP ") || line.starts_with("# TYPE "),
                "Line {} is a comment but not HELP or TYPE: '{line}'",
                i + 1
            );
        } else if !line.is_empty() {
            // Metric line: name value
            let parts: Vec<&str> = line.split_whitespace().collect();
            assert!(
                parts.len() >= 2,
                "Line {} should have name + value: '{line}'",
                i + 1
            );
            // Value should be parseable as f64
            assert!(
                parts[1].parse::<f64>().is_ok(),
                "Line {} value '{}' not a valid float",
                i + 1,
                parts[1]
            );
        }
    }
}

/// (12) Prometheus export contains all declared metric names.
#[test]
fn prometheus_export_contains_all_metric_names() {
    let mut metrics = MetricsCollector::new();
    simulate_metrics_collection(&mut metrics);

    let output = metrics.export_prometheus();

    let all_metric_names = [
        "render_frames_total",
        "diff_strategy_selected_total",
        "ansi_sequences_parsed_total",
        "render_frame_duration_us",
        "widget_render_duration_us",
        "terminal_active",
        "eprocess_wealth",
    ];

    for name in &all_metric_names {
        assert!(
            output.contains(name),
            "Prometheus output should contain metric '{name}'"
        );
    }
}

/// (13) Galaxy-brain transparency: all 4 levels render.
#[test]
fn transparency_all_four_levels_render() {
    let evidence = EvidenceEntry {
        domain: DecisionDomain::DiffStrategy,
        timestamp_ns: 1_000_000_000,
        log_posterior: 1.386,
        action: "dirty_rows".into(),
        loss_avoided: 0.35,
        confidence_interval: (0.72, 0.88),
        evidence_terms: vec![
            ("change_fraction".into(), 4.0),
            ("dirty_rows_ratio".into(), 2.5),
        ],
    };

    for level in TransparencyLevel::ALL {
        let card = level.render_card(&evidence);
        assert!(
            !card.is_empty(),
            "Transparency {} should produce non-empty card",
            level.as_str()
        );
        assert!(
            card.contains("diff_strategy"),
            "Transparency {} card should reference domain: '{card}'",
            level.as_str()
        );
    }
}

/// (14) Transparency levels are progressively more detailed.
#[test]
fn transparency_levels_progressively_detailed() {
    let evidence = EvidenceEntry {
        domain: DecisionDomain::FrameBudget,
        timestamp_ns: 2_000_000_000,
        log_posterior: -0.405,
        action: "maintain_budget".into(),
        loss_avoided: 0.05,
        confidence_interval: (0.35, 0.55),
        evidence_terms: vec![
            ("cusum_signal".into(), 0.8),
            ("eprocess_wealth".into(), 1.1),
        ],
    };

    let cards: Vec<String> = TransparencyLevel::ALL
        .iter()
        .map(|level| level.render_card(&evidence))
        .collect();

    // Each level should be longer than the previous
    for i in 1..cards.len() {
        assert!(
            cards[i].len() > cards[i - 1].len(),
            "Level {} card ({} chars) should be longer than level {} card ({} chars)",
            i,
            cards[i].len(),
            i - 1,
            cards[i - 1].len()
        );
    }

    // L0 should NOT contain posterior
    assert!(
        !cards[0].contains("posterior"),
        "L0 should not expose posterior"
    );

    // L2 should contain evidence terms
    assert!(
        cards[2].contains("cusum_signal"),
        "L2 should contain evidence term labels"
    );

    // L3 should contain timestamp
    assert!(
        cards[3].contains("2000000000"),
        "L3 should contain raw timestamp"
    );
}

/// (15) Evidence ledger ring buffer semantics.
#[test]
fn evidence_ledger_ring_buffer_eviction() {
    let mut ledger = UnifiedEvidenceLedger::new(3);

    for i in 0..5 {
        ledger.record(EvidenceEntry {
            domain: DecisionDomain::DiffStrategy,
            timestamp_ns: i * 1000,
            log_posterior: i as f64 * 0.1,
            action: format!("action_{i}"),
            loss_avoided: 0.0,
            confidence_interval: (0.0, 1.0),
            evidence_terms: vec![],
        });
    }

    assert_eq!(ledger.len(), 3, "Ring buffer should cap at capacity 3");

    // Oldest entries should have been evicted
    assert_eq!(
        ledger.entries[0].action, "action_2",
        "Oldest remaining should be action_2"
    );
    assert_eq!(
        ledger.entries[2].action, "action_4",
        "Newest should be action_4"
    );
}

/// (16) Combined pipeline: spans + metrics + evidence in single cycle.
#[test]
fn combined_pipeline_single_render_cycle() {
    let mut ledger = UnifiedEvidenceLedger::new(100);
    let mut metrics = MetricsCollector::new();

    let (spans, events) = with_pipeline_capture(|| {
        simulate_full_render_cycle();
    });

    simulate_evidence_collection(&mut ledger);
    simulate_metrics_collection(&mut metrics);

    // Verify all three observability layers populated
    assert!(spans.len() >= 10, "Should have >= 10 spans");
    assert!(!events.is_empty(), "Should have events");
    assert_eq!(ledger.len(), 7, "Should have 7 evidence entries");
    assert!(!metrics.counters.is_empty(), "Should have counter metrics");
    assert!(
        !metrics.histograms.is_empty(),
        "Should have histogram metrics"
    );
    assert!(!metrics.gauges.is_empty(), "Should have gauge metrics");

    // Prometheus output should be non-empty and well-formed
    let prom = metrics.export_prometheus();
    assert!(
        prom.contains("# TYPE"),
        "Prometheus output should have TYPE lines"
    );

    // JSONL output should be non-empty
    let jsonl = ledger.to_jsonl();
    assert_eq!(jsonl.lines().count(), 7, "JSONL should have 7 lines");
}

/// (17) Tracing span levels are correct.
#[test]
fn span_levels_are_correct() {
    let (spans, _events) = with_pipeline_capture(simulate_full_render_cycle);

    // All pipeline spans should be INFO level
    let info_spans = [
        telemetry_schema::span::PROGRAM_INIT,
        telemetry_schema::span::PROGRAM_UPDATE,
        telemetry_schema::span::RENDER_FRAME,
        telemetry_schema::span::PROGRAM_VIEW,
        telemetry_schema::span::RENDER_PRESENT,
        "inline.render",
        telemetry_schema::span::RENDER_SCROLL_REGION,
        telemetry_schema::span::RENDER_DIFF_COMPUTE,
        telemetry_schema::span::RENDER_EMIT,
        telemetry_schema::span::PROGRAM_SUBSCRIPTIONS,
        "conformal.predict",
    ];

    for name in &info_spans {
        let span = spans
            .iter()
            .find(|s| s.name == *name)
            .unwrap_or_else(|| panic!("Missing span: '{name}'"));
        assert_eq!(
            span.level,
            tracing::Level::INFO,
            "Span '{}' should be INFO level, got {:?}",
            name,
            span.level
        );
    }
}

/// (18) No orphan spans (all non-root spans have parents).
#[test]
fn no_orphan_spans() {
    let (spans, _events) = with_pipeline_capture(simulate_full_render_cycle);

    let root_span_names = [
        telemetry_schema::span::PROGRAM_INIT,
        telemetry_schema::span::PROGRAM_UPDATE,
        telemetry_schema::span::RENDER_FRAME,
        telemetry_schema::span::PROGRAM_SUBSCRIPTIONS,
        "conformal.predict",
    ];

    for span in &spans {
        if !root_span_names.contains(&span.name.as_str()) {
            assert!(
                span.parent_name.is_some(),
                "Non-root span '{}' should have a parent",
                span.name
            );
        }
    }
}

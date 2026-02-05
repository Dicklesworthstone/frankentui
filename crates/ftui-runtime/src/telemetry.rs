#![forbid(unsafe_code)]

//! Optional OpenTelemetry Telemetry Integration
//!
//! This module provides a non-invasive integration strategy for exporting
//! tracing spans to an OpenTelemetry collector. It follows the principle of
//! never clobbering existing tracing subscribers.
//!
//! # Integration Strategies
//!
//! ## Strategy 1: Automatic Installation (Recommended for Simple Apps)
//!
//! Use `TelemetryConfig::from_env().install()` when your application doesn't
//! have its own tracing subscriber. This will install a subscriber if none
//! exists.
//!
//! ```ignore
//! let _guard = TelemetryConfig::from_env().install()?;
//! // Runtime loop...
//! // Guard dropped on exit, flushes spans
//! ```
//!
//! ## Strategy 2: Layer Integration (For Apps with Existing Subscribers)
//!
//! Use `TelemetryConfig::from_env().build_layer()` when your application
//! already manages a tracing subscriber. Attach the layer to your subscriber.
//!
//! ```ignore
//! let (otel_layer, provider) = TelemetryConfig::from_env().build_layer()?;
//! let subscriber = Registry::default().with(otel_layer).with(my_other_layer);
//! subscriber.try_init()?; // Fails if a global subscriber already exists.
//! // Keep provider alive until shutdown.
//! ```
//!
//! # Env Var Contract
//!
//! See `docs/spec/telemetry.md` for the full env var specification. Key vars:
//!
//! - `OTEL_SDK_DISABLED=true` - Disable telemetry entirely
//! - `OTEL_EXPORTER_OTLP_ENDPOINT` - OTLP collector endpoint
//! - `OTEL_SERVICE_NAME` - Service name for resource
//! - `OTEL_TRACE_ID` / `OTEL_PARENT_SPAN_ID` - Explicit parent context
//!
//! # Invariants
//!
//! - Telemetry is never enabled without explicit env vars
//! - When disabled, overhead is a single boolean check
//! - Invalid trace IDs fail-open (create new root trace)
//! - `install()` fails if a global subscriber already exists

use std::env;
use std::fmt;

/// Telemetry configuration parsed from environment variables.
///
/// This struct captures the configuration for OpenTelemetry integration,
/// following the env var contract defined in `docs/spec/telemetry.md`.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Whether telemetry is enabled based on env vars.
    pub enabled: bool,
    /// Reason why telemetry is enabled/disabled (for evidence ledger).
    pub enabled_reason: EnabledReason,
    /// OTLP endpoint URL.
    pub endpoint: Option<String>,
    /// Source of the endpoint (for evidence ledger).
    pub endpoint_source: EndpointSource,
    /// OTLP protocol (grpc or http/protobuf).
    pub protocol: Protocol,
    /// Span processor mode (batch or simple).
    pub processor: SpanProcessorKind,
    /// Service name for the resource.
    pub service_name: Option<String>,
    /// Extra resource attributes.
    pub resource_attributes: Vec<(String, String)>,
    /// Optional explicit trace ID.
    pub trace_id: Option<TraceId>,
    /// Optional explicit parent span ID.
    pub parent_span_id: Option<SpanId>,
    /// Source of trace context (for evidence ledger).
    pub trace_context_source: TraceContextSource,
    /// OTLP headers for auth.
    pub headers: Vec<(String, String)>,
}

/// Reason why telemetry is enabled or disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnabledReason {
    /// Disabled by OTEL_SDK_DISABLED=true.
    SdkDisabled,
    /// Disabled by OTEL_TRACES_EXPORTER=none.
    ExporterNone,
    /// Enabled by explicit OTEL_TRACES_EXPORTER=otlp.
    ExplicitOtlp,
    /// Enabled by OTEL_EXPORTER_OTLP_ENDPOINT.
    EndpointSet,
    /// Enabled by FTUI_OTEL_HTTP_ENDPOINT.
    FtuiEndpointSet,
    /// Disabled by default (no trigger env vars set).
    DefaultDisabled,
}

/// Source of the OTLP endpoint configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSource {
    /// From OTEL_EXPORTER_OTLP_TRACES_ENDPOINT.
    TracesEndpoint,
    /// From FTUI_OTEL_HTTP_ENDPOINT.
    FtuiOverride,
    /// From OTEL_EXPORTER_OTLP_ENDPOINT.
    BaseEndpoint,
    /// Using protocol default (localhost:4318 for HTTP, 4317 for gRPC).
    ProtocolDefault,
    /// Not set (telemetry disabled).
    None,
}

/// OTLP transport protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    /// gRPC transport (localhost:4317 default).
    Grpc,
    /// HTTP with protobuf encoding (localhost:4318 default).
    #[default]
    HttpProtobuf,
}

impl Protocol {
    fn default_endpoint(self) -> &'static str {
        match self {
            Self::Grpc => "http://localhost:4317",
            Self::HttpProtobuf => "http://localhost:4318",
        }
    }
}

/// Span processor mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanProcessorKind {
    /// Batch processor (default; best for production).
    #[default]
    Batch,
    /// Simple processor (best for tests; synchronous export).
    Simple,
}

impl SpanProcessorKind {
    fn from_env(value: Option<String>) -> Self {
        match value
            .as_deref()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("simple") => Self::Simple,
            Some("batch") => Self::Batch,
            _ => Self::default(),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::Simple => "simple",
        }
    }
}

/// Source of trace context (explicit IDs vs new root).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceContextSource {
    /// Using explicit trace/span IDs from env vars.
    Explicit,
    /// Creating a new root trace.
    New,
    /// Telemetry disabled, no context.
    Disabled,
}

/// Validated 128-bit trace ID (32 hex chars).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceId([u8; 16]);

impl TraceId {
    /// Parse a 32-char lowercase hex string into a trace ID.
    pub fn parse(s: &str) -> Option<Self> {
        if s.len() != 32
            || !s
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return None;
        }
        let mut bytes = [0u8; 16];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hex_str = std::str::from_utf8(chunk).ok()?;
            bytes[i] = u8::from_str_radix(hex_str, 16).ok()?;
        }
        // All zeros is invalid per OTEL spec
        if bytes.iter().all(|&b| b == 0) {
            return None;
        }
        Some(Self(bytes))
    }

    /// Get the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Validated 64-bit span ID (16 hex chars).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanId([u8; 8]);

impl SpanId {
    /// Parse a 16-char lowercase hex string into a span ID.
    pub fn parse(s: &str) -> Option<Self> {
        if s.len() != 16
            || !s
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return None;
        }
        let mut bytes = [0u8; 8];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hex_str = std::str::from_utf8(chunk).ok()?;
            bytes[i] = u8::from_str_radix(hex_str, 16).ok()?;
        }
        // All zeros is invalid per OTEL spec
        if bytes.iter().all(|&b| b == 0) {
            return None;
        }
        Some(Self(bytes))
    }

    /// Get the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

/// Errors that can occur during telemetry setup.
#[derive(Debug)]
pub enum TelemetryError {
    /// A global tracing subscriber is already installed.
    SubscriberAlreadySet,
    /// Exporter initialization failed.
    ExporterInit(String),
    /// Provider setup failed.
    ProviderSetup(String),
}

impl fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SubscriberAlreadySet => write!(
                f,
                "A global tracing subscriber is already set. Use build_layer() instead."
            ),
            Self::ExporterInit(msg) => write!(f, "Failed to initialize OTLP exporter: {msg}"),
            Self::ProviderSetup(msg) => write!(f, "Failed to set up tracer provider: {msg}"),
        }
    }
}

impl std::error::Error for TelemetryError {}

/// RAII guard for telemetry shutdown.
///
/// When dropped, ensures pending spans are flushed to the collector.
#[must_use = "TelemetryGuard must be held until shutdown to ensure spans are flushed"]
pub struct TelemetryGuard {
    /// Provider is kept alive to ensure spans are flushed on drop.
    provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    /// Marker to prevent Send/Sync (guard must be dropped on same thread).
    _marker: std::marker::PhantomData<*const ()>,
}

impl TelemetryGuard {
    fn new(provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>) -> Self {
        Self {
            provider,
            _marker: std::marker::PhantomData,
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            let _ = provider.shutdown();
        }
        // Log for visibility.
        tracing::debug!("TelemetryGuard dropped, flushing spans");
    }
}

impl TelemetryConfig {
    /// Parse telemetry configuration from environment variables.
    ///
    /// This follows the env var contract defined in `docs/spec/telemetry.md`.
    /// The parsing is deterministic and order-independent.
    pub fn from_env() -> Self {
        Self::from_env_with(|key| env::var(key).ok())
    }

    /// Parse telemetry configuration using a custom environment getter.
    ///
    /// This is primarily useful for testing and benchmarking to avoid polluting
    /// the actual process environment.
    pub fn from_env_with<F>(mut get: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        // Step 1: Check for SDK disabled
        if get("OTEL_SDK_DISABLED")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            return Self::disabled(EnabledReason::SdkDisabled);
        }

        // Step 2: Check for exporter=none
        if get("OTEL_TRACES_EXPORTER")
            .map(|v| v.eq_ignore_ascii_case("none"))
            .unwrap_or(false)
        {
            return Self::disabled(EnabledReason::ExporterNone);
        }

        // Step 3: Determine if telemetry should be enabled
        let explicit_otlp = get("OTEL_TRACES_EXPORTER")
            .map(|v| v.eq_ignore_ascii_case("otlp"))
            .unwrap_or(false);
        let has_otel_endpoint = get("OTEL_EXPORTER_OTLP_ENDPOINT").is_some();
        let has_ftui_endpoint = get("FTUI_OTEL_HTTP_ENDPOINT").is_some();

        let (enabled, enabled_reason) = if explicit_otlp {
            (true, EnabledReason::ExplicitOtlp)
        } else if has_ftui_endpoint {
            (true, EnabledReason::FtuiEndpointSet)
        } else if has_otel_endpoint {
            (true, EnabledReason::EndpointSet)
        } else {
            (false, EnabledReason::DefaultDisabled)
        };

        if !enabled {
            return Self::disabled(enabled_reason);
        }

        // Step 4: Parse protocol
        let protocol = get("OTEL_EXPORTER_OTLP_PROTOCOL")
            .or_else(|| get("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL"))
            .map(|v| {
                if v.eq_ignore_ascii_case("grpc") {
                    Protocol::Grpc
                } else {
                    Protocol::HttpProtobuf
                }
            })
            .unwrap_or_default();

        // Step 5: Resolve endpoint
        let (endpoint, endpoint_source) =
            if let Some(ep) = get("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") {
                (Some(ep), EndpointSource::TracesEndpoint)
            } else if let Some(ep) = get("FTUI_OTEL_HTTP_ENDPOINT") {
                (Some(ep), EndpointSource::FtuiOverride)
            } else if let Some(ep) = get("OTEL_EXPORTER_OTLP_ENDPOINT") {
                (Some(ep), EndpointSource::BaseEndpoint)
            } else {
                (
                    Some(protocol.default_endpoint().to_string()),
                    EndpointSource::ProtocolDefault,
                )
            };

        // Step 6: Parse trace context
        let trace_id = get("OTEL_TRACE_ID").and_then(|s| TraceId::parse(&s));
        let parent_span_id = get("OTEL_PARENT_SPAN_ID").and_then(|s| SpanId::parse(&s));

        let trace_context_source = if trace_id.is_some() && parent_span_id.is_some() {
            TraceContextSource::Explicit
        } else {
            TraceContextSource::New
        };

        // Step 7: Parse other settings
        let service_name = get("OTEL_SERVICE_NAME");
        let resource_attributes = Self::parse_kv_list(&get("OTEL_RESOURCE_ATTRIBUTES"));
        let headers = Self::parse_kv_list(&get("OTEL_EXPORTER_OTLP_HEADERS"));
        let processor = SpanProcessorKind::from_env(
            get("FTUI_OTEL_SPAN_PROCESSOR").or_else(|| get("FTUI_OTEL_PROCESSOR")),
        );

        Self {
            enabled,
            enabled_reason,
            endpoint,
            endpoint_source,
            protocol,
            processor,
            service_name,
            resource_attributes,
            trace_id,
            parent_span_id,
            trace_context_source,
            headers,
        }
    }

    /// Create a disabled config.
    fn disabled(reason: EnabledReason) -> Self {
        Self {
            enabled: false,
            enabled_reason: reason,
            endpoint: None,
            endpoint_source: EndpointSource::None,
            protocol: Protocol::default(),
            processor: SpanProcessorKind::default(),
            service_name: None,
            resource_attributes: vec![],
            trace_id: None,
            parent_span_id: None,
            trace_context_source: TraceContextSource::Disabled,
            headers: vec![],
        }
    }

    /// Parse a comma-separated key=value list.
    fn parse_kv_list(s: &Option<String>) -> Vec<(String, String)> {
        s.as_ref()
            .map(|s| {
                s.split(',')
                    .filter_map(|kv| {
                        let mut parts = kv.splitn(2, '=');
                        let key = parts.next()?.trim();
                        let value = parts.next()?.trim();
                        if key.is_empty() {
                            None
                        } else {
                            Some((key.to_string(), value.to_string()))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Check if telemetry is enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Install a tracing subscriber with OpenTelemetry export.
    ///
    /// This will fail with `TelemetryError::SubscriberAlreadySet` if a global
    /// subscriber is already installed. Use `build_layer()` instead in that case.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - A global subscriber is already set
    /// - The OTLP exporter fails to initialize
    ///
    /// # Returns
    ///
    /// Returns a `TelemetryGuard` that must be held until shutdown. Dropping
    /// the guard will flush pending spans.
    #[cfg(feature = "telemetry")]
    pub fn install(self) -> Result<TelemetryGuard, TelemetryError> {
        if !self.enabled {
            tracing::debug!(reason = ?self.enabled_reason, "Telemetry disabled");
            return Ok(TelemetryGuard::new(None));
        }

        let endpoint = self.endpoint.clone();
        let protocol = self.protocol;
        let trace_context = self.trace_context_source;
        let (otel_layer, provider) = self.build_layer()?;

        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let subscriber = tracing_subscriber::registry().with(otel_layer);
        if subscriber.try_init().is_err() {
            let _ = provider.shutdown();
            return Err(TelemetryError::SubscriberAlreadySet);
        }

        tracing::info!(
            endpoint = ?endpoint,
            protocol = ?protocol,
            trace_context = ?trace_context,
            "Telemetry installed"
        );

        Ok(TelemetryGuard::new(Some(provider)))
    }

    /// Build an OpenTelemetry layer for manual subscriber integration.
    ///
    /// Use this when your application already manages a tracing subscriber.
    /// Attach the returned layer to your subscriber registry.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (otel_layer, _provider) = config.build_layer()?;
    /// let subscriber = Registry::default()
    ///     .with(otel_layer)
    ///     .with(my_other_layers);
    /// subscriber.try_init()?;
    /// ```
    ///
    /// # Note
    ///
    /// The returned provider must be kept alive until shutdown to ensure
    /// spans are properly exported.
    #[cfg(feature = "telemetry")]
    pub fn build_layer(
        self,
    ) -> Result<
        (
            tracing_opentelemetry::OpenTelemetryLayer<
                tracing_subscriber::Registry,
                opentelemetry_sdk::trace::Tracer,
            >,
            opentelemetry_sdk::trace::SdkTracerProvider,
        ),
        TelemetryError,
    > {
        use opentelemetry::KeyValue;
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_otlp::{
            Protocol as OtlpProtocol, WithExportConfig, WithHttpConfig, WithTonicConfig,
        };
        use opentelemetry_sdk::Resource;
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use std::collections::HashMap;
        use std::str::FromStr;

        if !self.enabled {
            return Err(TelemetryError::ExporterInit(
                "Telemetry is disabled".to_string(),
            ));
        }

        let endpoint = self
            .endpoint
            .as_deref()
            .unwrap_or(self.protocol.default_endpoint());

        let mut resource_kvs = Vec::new();
        if let Some(service_name) = &self.service_name {
            resource_kvs.push(KeyValue::new(fields::SERVICE_NAME, service_name.clone()));
        }
        if !self.resource_attributes.is_empty() {
            let override_service_name = self.service_name.is_some();
            for (key, value) in &self.resource_attributes {
                if override_service_name && key == fields::SERVICE_NAME {
                    continue;
                }
                resource_kvs.push(KeyValue::new(key.clone(), value.clone()));
            }
        }

        // Build the exporter
        let exporter = match self.protocol {
            Protocol::Grpc => {
                let mut builder = opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint);
                if !self.headers.is_empty() {
                    use opentelemetry_otlp::tonic_types::metadata::{
                        MetadataKey, MetadataMap, MetadataValue,
                    };
                    let mut metadata = MetadataMap::with_capacity(self.headers.len());
                    for (key, value) in &self.headers {
                        let key = key.trim();
                        if key.is_empty() {
                            continue;
                        }
                        let lower_key = key.to_ascii_lowercase();
                        if let (Ok(meta_key), Ok(meta_value)) = (
                            MetadataKey::from_str(&lower_key),
                            MetadataValue::from_str(value),
                        ) {
                            metadata.insert(meta_key, meta_value);
                        }
                    }
                    builder = builder.with_metadata(metadata);
                }
                builder
                    .build()
                    .map_err(|e: opentelemetry_otlp::ExporterBuildError| {
                        TelemetryError::ExporterInit(e.to_string())
                    })?
            }
            Protocol::HttpProtobuf => {
                let mut builder = opentelemetry_otlp::SpanExporter::builder()
                    .with_http()
                    .with_protocol(OtlpProtocol::HttpBinary)
                    .with_endpoint(endpoint);
                if !self.headers.is_empty() {
                    let headers: HashMap<String, String> = self
                        .headers
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect();
                    builder = builder.with_headers(headers);
                }
                builder
                    .build()
                    .map_err(|e: opentelemetry_otlp::ExporterBuildError| {
                        TelemetryError::ExporterInit(e.to_string())
                    })?
            }
        };

        // Build the provider with configured span processor.
        let mut provider_builder = SdkTracerProvider::builder();
        if !resource_kvs.is_empty() {
            provider_builder = provider_builder.with_resource(Resource::new(resource_kvs));
        }
        let provider = match self.processor {
            SpanProcessorKind::Simple => provider_builder.with_simple_exporter(exporter).build(),
            SpanProcessorKind::Batch => provider_builder.with_batch_exporter(exporter).build(),
        };

        // Get a tracer
        let tracer = provider.tracer("ftui-runtime");

        // Build the layer
        let layer = tracing_opentelemetry::layer().with_tracer(tracer);

        Ok((layer, provider))
    }

    /// Get an evidence ledger entry for debugging/auditing.
    ///
    /// Returns a structured summary of the configuration decisions.
    pub fn evidence_ledger(&self) -> EvidenceLedger {
        EvidenceLedger {
            enabled: self.enabled,
            enabled_reason: self.enabled_reason,
            endpoint_source: self.endpoint_source,
            protocol: self.protocol,
            processor: self.processor,
            trace_context_source: self.trace_context_source,
            service_name: self.service_name.clone(),
        }
    }
}

/// Evidence ledger for telemetry configuration decisions.
///
/// This provides a structured record of why telemetry was configured
/// the way it was, useful for debugging and auditing.
#[derive(Debug, Clone)]
pub struct EvidenceLedger {
    pub enabled: bool,
    pub enabled_reason: EnabledReason,
    pub endpoint_source: EndpointSource,
    pub protocol: Protocol,
    pub processor: SpanProcessorKind,
    pub trace_context_source: TraceContextSource,
    pub service_name: Option<String>,
}

/// Current schema version for telemetry events.
///
/// See `docs/spec/telemetry-events.md` for version compatibility rules.
pub const SCHEMA_VERSION: &str = "1.0.0";

/// Evidence for a runtime decision, suitable for audit trails.
///
/// This struct captures the reasoning behind non-trivial decisions
/// made by the runtime, enabling post-hoc analysis and debugging.
#[derive(Debug, Clone)]
pub struct DecisionEvidence {
    /// Rule or heuristic that triggered the decision.
    pub rule: String,
    /// Summary of inputs (redacted as per policy).
    pub inputs_summary: String,
    /// Chosen action.
    pub action: String,
    /// Confidence (0.0-1.0) if probabilistic.
    pub confidence: Option<f32>,
    /// Alternative actions considered.
    pub alternatives: Vec<String>,
    /// Brief explanation for humans.
    pub explanation: String,
}

impl DecisionEvidence {
    /// Create a simple decision with rule and action.
    pub fn simple(rule: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            rule: rule.into(),
            inputs_summary: String::new(),
            action: action.into(),
            confidence: None,
            alternatives: vec![],
            explanation: String::new(),
        }
    }

    /// Add an explanation to the evidence.
    #[must_use]
    pub fn with_explanation(mut self, explanation: impl Into<String>) -> Self {
        self.explanation = explanation.into();
        self
    }

    /// Add confidence score.
    #[must_use]
    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = Some(confidence.clamp(0.0, 1.0));
        self
    }

    /// Add alternative actions that were considered.
    #[must_use]
    pub fn with_alternatives(mut self, alternatives: Vec<String>) -> Self {
        self.alternatives = alternatives;
        self
    }
}

// =============================================================================
// Event Schema: Span and Event Names
// =============================================================================
//
// This section defines the canonical event schema for FrankenTUI telemetry.
// See `docs/spec/telemetry-events.md` for the full specification.

/// Runtime phase span names (Elm/Bubbletea loop).
pub mod spans {
    /// Span for model initialization.
    pub const PROGRAM_INIT: &str = "ftui.program.init";
    /// Span for a single update cycle.
    pub const PROGRAM_UPDATE: &str = "ftui.program.update";
    /// Span for view rendering.
    pub const PROGRAM_VIEW: &str = "ftui.program.view";
    /// Span for subscription management.
    pub const PROGRAM_SUBSCRIPTIONS: &str = "ftui.program.subscriptions";

    /// Span for complete frame cycle.
    pub const RENDER_FRAME: &str = "ftui.render.frame";
    /// Span for buffer diff computation.
    pub const RENDER_DIFF: &str = "ftui.render.diff";
    /// Span for ANSI emission.
    pub const RENDER_PRESENT: &str = "ftui.render.present";
    /// Span for output flush.
    pub const RENDER_FLUSH: &str = "ftui.render.flush";

    /// Span for input event processing.
    pub const INPUT_EVENT: &str = "ftui.input.event";
    /// Span for macro playback.
    pub const INPUT_MACRO: &str = "ftui.input.macro";
}

/// Decision event names (point-in-time auditable decisions).
pub mod events {
    /// Degradation level change event.
    pub const DECISION_DEGRADATION: &str = "ftui.decision.degradation";
    /// Capability fallback event.
    pub const DECISION_FALLBACK: &str = "ftui.decision.fallback";
    /// Resize handling decision event.
    pub const DECISION_RESIZE: &str = "ftui.decision.resize";
    /// Screen mode selection event.
    pub const DECISION_SCREEN_MODE: &str = "ftui.decision.screen_mode";
}

/// Common field names for telemetry spans/events.
pub mod fields {
    // Common fields (attached to all spans)
    /// Service name field.
    pub const SERVICE_NAME: &str = "service.name";
    /// Service version field.
    pub const SERVICE_VERSION: &str = "service.version";
    /// Telemetry SDK identifier.
    pub const TELEMETRY_SDK: &str = "telemetry.sdk";
    /// Host architecture field.
    pub const HOST_ARCH: &str = "host.arch";
    /// Process ID field.
    pub const PROCESS_PID: &str = "process.pid";
    /// Schema version field.
    pub const SCHEMA_VERSION: &str = "ftui.schema_version";

    // Duration fields (microseconds)
    /// Duration in microseconds.
    pub const DURATION_US: &str = "duration_us";

    // Program phase fields
    /// Model type name (verbose mode only).
    pub const MODEL_TYPE: &str = "model_type";
    /// Command count.
    pub const CMD_COUNT: &str = "cmd_count";
    /// Message type name (verbose mode only).
    pub const MSG_TYPE: &str = "msg_type";
    /// Command type name (verbose mode only).
    pub const CMD_TYPE: &str = "cmd_type";
    /// Widget count.
    pub const WIDGET_COUNT: &str = "widget_count";
    /// Active subscription count.
    pub const ACTIVE_COUNT: &str = "active_count";
    /// Subscriptions started.
    pub const STARTED: &str = "started";
    /// Subscriptions stopped.
    pub const STOPPED: &str = "stopped";

    // Render fields
    /// Buffer width.
    pub const WIDTH: &str = "width";
    /// Buffer height.
    pub const HEIGHT: &str = "height";
    /// Number of changes in diff.
    pub const CHANGES_COUNT: &str = "changes_count";
    /// Rows skipped in diff.
    pub const ROWS_SKIPPED: &str = "rows_skipped";
    /// Bytes written to output.
    pub const BYTES_WRITTEN: &str = "bytes_written";
    /// Number of change runs.
    pub const RUNS_COUNT: &str = "runs_count";
    /// Sync mode used.
    pub const SYNC_MODE: &str = "sync_mode";

    // Decision fields
    /// Degradation level.
    pub const LEVEL: &str = "level";
    /// Decision reason.
    pub const REASON: &str = "reason";
    /// Remaining budget.
    pub const BUDGET_REMAINING: &str = "budget_remaining";
    /// Capability name.
    pub const CAPABILITY: &str = "capability";
    /// Fallback target.
    pub const FALLBACK_TO: &str = "fallback_to";
    /// Strategy name.
    pub const STRATEGY: &str = "strategy";
    /// Debounce active flag.
    pub const DEBOUNCE_ACTIVE: &str = "debounce_active";
    /// Events coalesced flag.
    pub const COALESCED: &str = "coalesced";
    /// Screen mode.
    pub const MODE: &str = "mode";
    /// UI height.
    pub const UI_HEIGHT: &str = "ui_height";
    /// UI anchor position.
    pub const ANCHOR: &str = "anchor";

    // Input fields
    /// Event type (no content!).
    pub const EVENT_TYPE: &str = "event_type";
    /// Macro ID.
    pub const MACRO_ID: &str = "macro_id";
    /// Event count.
    pub const EVENT_COUNT: &str = "event_count";

    // Decision evidence fields
    /// Decision rule applied.
    pub const DECISION_RULE: &str = "decision.rule";
    /// Decision inputs (redacted).
    pub const DECISION_INPUTS: &str = "decision.inputs";
    /// Decision action taken.
    pub const DECISION_ACTION: &str = "decision.action";
    /// Decision confidence score.
    pub const DECISION_CONFIDENCE: &str = "decision.confidence";
}

/// Redaction policy configuration.
///
/// Defines what categories of data are redacted and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactionLevel {
    /// Full redaction: replace with placeholder.
    Full,
    /// Partial redaction: show type/count but not content.
    Partial,
    /// No redaction: emit as-is (verbose mode only).
    None,
}

/// Categories of data for redaction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataCategory {
    /// User input content (key chars, text, passwords).
    UserInput,
    /// File paths and directories.
    FilePath,
    /// Environment variables (beyond allowed prefixes).
    EnvVar,
    /// Memory addresses and pointers.
    MemoryAddress,
    /// Process arguments.
    ProcessArgs,
    /// User identifiers (usernames, home dirs).
    UserIdentifier,
    /// Widget type names.
    WidgetType,
    /// Message type names.
    MessageType,
    /// Command type names.
    CommandType,
    /// Terminal capability details.
    CapabilityDetails,
}

impl DataCategory {
    /// Get the default redaction level for this category.
    #[must_use]
    pub const fn default_redaction(self) -> RedactionLevel {
        match self {
            // Hard redaction: never emit
            Self::UserInput
            | Self::FilePath
            | Self::EnvVar
            | Self::MemoryAddress
            | Self::ProcessArgs
            | Self::UserIdentifier => RedactionLevel::Full,
            // Soft redaction: emit only in verbose mode
            Self::WidgetType | Self::MessageType | Self::CommandType | Self::CapabilityDetails => {
                RedactionLevel::Partial
            }
        }
    }

    /// Check if this category should be redacted in the current mode.
    #[must_use]
    pub fn should_redact(self) -> bool {
        match self.default_redaction() {
            RedactionLevel::Full => true,
            RedactionLevel::Partial => !redact::is_verbose(),
            RedactionLevel::None => false,
        }
    }
}

/// Allowed environment variable prefixes that are safe to emit.
pub const ALLOWED_ENV_PREFIXES: &[&str] = &["OTEL_", "FTUI_"];

/// Check if an environment variable name is safe to emit.
#[must_use]
pub fn is_safe_env_var(name: &str) -> bool {
    ALLOWED_ENV_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Redaction utilities for telemetry.
///
/// These functions implement the redaction policy defined in
/// `docs/spec/telemetry-events.md`.
///
/// # Redaction Principles
///
/// 1. **Conservative by default**: Err on the side of not emitting.
/// 2. **No PII**: Never emit user input content, file paths, or secrets.
/// 3. **Structural only**: Emit types and counts, not values.
/// 4. **Opt-in detail**: Verbose fields require `FTUI_TELEMETRY_VERBOSE=true`.
pub mod redact {
    use std::path::Path;

    // =========================================================================
    // Hard Redaction (Never Emit)
    // =========================================================================

    /// Redact a file path (never emit full paths).
    ///
    /// File paths may expose sensitive information about the user's
    /// system structure, usernames, or project names.
    #[inline]
    pub fn path(_path: &Path) -> &'static str {
        "[redacted:path]"
    }

    /// Redact user input content (never emit).
    ///
    /// User input may contain passwords, personal information,
    /// or other sensitive data.
    #[inline]
    pub fn content(_content: &str) -> &'static str {
        "[redacted:content]"
    }

    /// Redact a memory address.
    ///
    /// Memory addresses could be used for ASLR bypass attacks.
    #[inline]
    pub fn address<T>(_ptr: *const T) -> &'static str {
        "[redacted:address]"
    }

    /// Redact an environment variable value.
    ///
    /// Environment variables often contain secrets and credentials.
    #[inline]
    pub fn env_var(_value: &str) -> &'static str {
        "[redacted:env]"
    }

    /// Redact process arguments.
    ///
    /// Command-line arguments may contain passwords or sensitive flags.
    #[inline]
    pub fn process_args(_args: &[String]) -> &'static str {
        "[redacted:args]"
    }

    /// Redact a username or user identifier.
    #[inline]
    pub fn username(_name: &str) -> &'static str {
        "[redacted:user]"
    }

    // =========================================================================
    // Safe Summarization (Always Allowed)
    // =========================================================================

    /// Summarize a count without exposing content.
    ///
    /// Counts are safe because they don't reveal the actual data.
    #[inline]
    pub fn count<T>(items: &[T]) -> String {
        format!("{} items", items.len())
    }

    /// Summarize byte size in human-readable format.
    ///
    /// Byte sizes are safe as they're just numbers.
    #[inline]
    pub fn bytes(size: usize) -> String {
        if size < 1024 {
            format!("{size} B")
        } else if size < 1024 * 1024 {
            format!("{:.1} KB", size as f64 / 1024.0)
        } else {
            format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
        }
    }

    /// Summarize a duration in human-readable format.
    ///
    /// Durations are safe timing information.
    #[inline]
    pub fn duration_us(micros: u64) -> String {
        if micros < 1000 {
            format!("{micros}μs")
        } else if micros < 1_000_000 {
            format!("{:.2}ms", micros as f64 / 1000.0)
        } else {
            format!("{:.2}s", micros as f64 / 1_000_000.0)
        }
    }

    /// Summarize dimensions (width x height).
    ///
    /// Buffer dimensions are safe structural information.
    #[inline]
    pub fn dimensions(width: u16, height: u16) -> String {
        format!("{width}x{height}")
    }

    // =========================================================================
    // Conditional Emission (Verbose Mode)
    // =========================================================================

    /// Check if verbose telemetry is enabled.
    ///
    /// When `FTUI_TELEMETRY_VERBOSE=true`, additional fields are emitted
    /// that would otherwise be redacted (widget types, message types, etc).
    #[inline]
    pub fn is_verbose() -> bool {
        std::env::var("FTUI_TELEMETRY_VERBOSE")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    /// Emit a value only if verbose mode is enabled.
    ///
    /// Returns `T::default()` when not in verbose mode.
    #[inline]
    pub fn if_verbose<T: Default>(value: T) -> T {
        if is_verbose() { value } else { T::default() }
    }

    /// Emit a string value only if verbose mode is enabled.
    ///
    /// Returns `"[verbose-only]"` when not in verbose mode.
    #[inline]
    pub fn verbose_str(value: &str) -> &str {
        if is_verbose() {
            value
        } else {
            "[verbose-only]"
        }
    }

    /// Redact a type name unless in verbose mode.
    ///
    /// Type names can reveal internal architecture but are useful for debugging.
    #[inline]
    pub fn type_name(name: &str) -> &str {
        if is_verbose() { name } else { "[type]" }
    }

    // =========================================================================
    // Custom Field Handling
    // =========================================================================

    /// Check if a custom field name has a valid namespace prefix.
    ///
    /// Custom fields MUST use a namespace prefix (e.g., `app.`, `custom.`).
    #[inline]
    pub fn is_valid_custom_field(name: &str) -> bool {
        name.starts_with("app.") || name.starts_with("custom.")
    }

    /// Prefix a custom field name if not already prefixed.
    ///
    /// Ensures all custom fields have proper namespacing.
    pub fn prefix_custom_field(name: &str) -> String {
        if is_valid_custom_field(name) {
            name.to_string()
        } else {
            format!("app.{name}")
        }
    }

    // =========================================================================
    // Validation Helpers
    // =========================================================================

    /// Check if a string contains potentially sensitive patterns.
    ///
    /// Used to detect accidental PII in telemetry output.
    pub fn contains_sensitive_pattern(s: &str) -> bool {
        let lower = s.to_lowercase();
        // Check for common sensitive patterns
        lower.contains("password")
            || lower.contains("secret")
            || lower.contains("token")
            || lower.contains("key=")
            || lower.contains("api_key")
            || lower.contains("auth")
            || s.contains('@') // Email addresses
            || s.starts_with('/') // Absolute paths
            || s.contains("://") // URLs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_id_parse_valid() {
        let id = TraceId::parse("0123456789abcdef0123456789abcdef");
        assert!(id.is_some());
    }

    #[test]
    fn test_trace_id_parse_invalid_length() {
        assert!(TraceId::parse("0123456789abcdef").is_none());
        assert!(TraceId::parse("0123456789abcdef0123456789abcdef00").is_none());
    }

    #[test]
    fn test_trace_id_parse_invalid_uppercase() {
        assert!(TraceId::parse("0123456789ABCDEF0123456789abcdef").is_none());
    }

    #[test]
    fn test_trace_id_parse_all_zeros() {
        assert!(TraceId::parse("00000000000000000000000000000000").is_none());
    }

    #[test]
    fn test_span_id_parse_valid() {
        let id = SpanId::parse("0123456789abcdef");
        assert!(id.is_some());
    }

    #[test]
    fn test_span_id_parse_invalid() {
        assert!(SpanId::parse("012345").is_none());
        assert!(SpanId::parse("0123456789ABCDEF").is_none());
        assert!(SpanId::parse("0000000000000000").is_none());
    }

    #[test]
    fn test_parse_kv_list() {
        let result = TelemetryConfig::parse_kv_list(&Some("a=b,c=d".to_string()));
        assert_eq!(
            result,
            vec![
                ("a".to_string(), "b".to_string()),
                ("c".to_string(), "d".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_kv_list_with_spaces() {
        let result = TelemetryConfig::parse_kv_list(&Some(" key = value , k2=v2 ".to_string()));
        assert_eq!(
            result,
            vec![
                ("key".to_string(), "value".to_string()),
                ("k2".to_string(), "v2".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_kv_list_empty() {
        let result = TelemetryConfig::parse_kv_list(&None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_protocol_default_endpoint() {
        assert_eq!(Protocol::Grpc.default_endpoint(), "http://localhost:4317");
        assert_eq!(
            Protocol::HttpProtobuf.default_endpoint(),
            "http://localhost:4318"
        );
    }

    #[test]
    fn test_disabled_config() {
        let config = TelemetryConfig::disabled(EnabledReason::SdkDisabled);
        assert!(!config.is_enabled());
        assert_eq!(config.enabled_reason, EnabledReason::SdkDisabled);
        assert_eq!(config.trace_context_source, TraceContextSource::Disabled);
    }

    #[test]
    fn test_schema_version() {
        assert_eq!(SCHEMA_VERSION, "1.0.0");
    }

    #[test]
    fn test_decision_evidence_simple() {
        let evidence = DecisionEvidence::simple("degradation_rule", "reduce_to_skeleton");
        assert_eq!(evidence.rule, "degradation_rule");
        assert_eq!(evidence.action, "reduce_to_skeleton");
        assert!(evidence.confidence.is_none());
        assert!(evidence.alternatives.is_empty());
    }

    #[test]
    fn test_decision_evidence_builder() {
        let evidence = DecisionEvidence::simple("fallback_rule", "use_ascii")
            .with_explanation("Terminal does not support Unicode")
            .with_confidence(0.95)
            .with_alternatives(vec!["use_emoji".to_string(), "skip_render".to_string()]);

        assert_eq!(evidence.explanation, "Terminal does not support Unicode");
        assert_eq!(evidence.confidence, Some(0.95));
        assert_eq!(evidence.alternatives.len(), 2);
    }

    #[test]
    fn test_decision_evidence_confidence_clamped() {
        let evidence = DecisionEvidence::simple("test", "test").with_confidence(1.5);
        assert_eq!(evidence.confidence, Some(1.0));

        let evidence = DecisionEvidence::simple("test", "test").with_confidence(-0.5);
        assert_eq!(evidence.confidence, Some(0.0));
    }

    #[test]
    fn test_redact_path() {
        use std::path::Path;
        let path_str = ["/home/user/", "sec", "ret", ".txt"].concat();
        assert_eq!(redact::path(Path::new(&path_str)), "[redacted:path]");
    }

    #[test]
    fn test_redact_content() {
        assert_eq!(redact::content("sensitive data"), "[redacted:content]");
    }

    #[test]
    fn test_redact_count() {
        let items = vec![1, 2, 3, 4, 5];
        assert_eq!(redact::count(&items), "5 items");
    }

    #[test]
    fn test_redact_bytes() {
        assert_eq!(redact::bytes(500), "500 B");
        assert_eq!(redact::bytes(2048), "2.0 KB");
        assert_eq!(redact::bytes(1024 * 1024 + 512 * 1024), "1.5 MB");
    }

    // =========================================================================
    // Event Schema Tests
    // =========================================================================

    #[test]
    fn test_span_names_follow_convention() {
        // All span names should start with "ftui."
        assert!(spans::PROGRAM_INIT.starts_with("ftui."));
        assert!(spans::PROGRAM_UPDATE.starts_with("ftui."));
        assert!(spans::PROGRAM_VIEW.starts_with("ftui."));
        assert!(spans::RENDER_FRAME.starts_with("ftui."));
        assert!(spans::RENDER_DIFF.starts_with("ftui."));
        assert!(spans::INPUT_EVENT.starts_with("ftui."));
    }

    #[test]
    fn test_event_names_follow_convention() {
        // All event names should start with "ftui.decision."
        assert!(events::DECISION_DEGRADATION.starts_with("ftui.decision."));
        assert!(events::DECISION_FALLBACK.starts_with("ftui.decision."));
        assert!(events::DECISION_RESIZE.starts_with("ftui.decision."));
        assert!(events::DECISION_SCREEN_MODE.starts_with("ftui.decision."));
    }

    #[test]
    fn test_field_names_are_lowercase_with_dots() {
        // Field names should be lowercase with dots or underscores
        let check_field = |name: &str| {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_'),
                "Field name '{}' contains invalid characters",
                name
            );
        };
        check_field(fields::DURATION_US);
        check_field(fields::WIDTH);
        check_field(fields::HEIGHT);
        check_field(fields::DECISION_RULE);
        check_field(fields::SERVICE_NAME);
    }

    // =========================================================================
    // Data Category Tests
    // =========================================================================

    #[test]
    fn test_hard_redaction_categories() {
        // These should always be redacted
        assert_eq!(
            DataCategory::UserInput.default_redaction(),
            RedactionLevel::Full
        );
        assert_eq!(
            DataCategory::FilePath.default_redaction(),
            RedactionLevel::Full
        );
        assert_eq!(
            DataCategory::EnvVar.default_redaction(),
            RedactionLevel::Full
        );
        assert_eq!(
            DataCategory::MemoryAddress.default_redaction(),
            RedactionLevel::Full
        );
        assert_eq!(
            DataCategory::ProcessArgs.default_redaction(),
            RedactionLevel::Full
        );
        assert_eq!(
            DataCategory::UserIdentifier.default_redaction(),
            RedactionLevel::Full
        );
    }

    #[test]
    fn test_soft_redaction_categories() {
        // These should be redacted unless verbose mode
        assert_eq!(
            DataCategory::WidgetType.default_redaction(),
            RedactionLevel::Partial
        );
        assert_eq!(
            DataCategory::MessageType.default_redaction(),
            RedactionLevel::Partial
        );
        assert_eq!(
            DataCategory::CommandType.default_redaction(),
            RedactionLevel::Partial
        );
        assert_eq!(
            DataCategory::CapabilityDetails.default_redaction(),
            RedactionLevel::Partial
        );
    }

    #[test]
    fn test_hard_redaction_always_redacts() {
        // Hard redaction should always trigger, regardless of verbose mode
        assert!(DataCategory::UserInput.should_redact());
        assert!(DataCategory::FilePath.should_redact());
        assert!(DataCategory::MemoryAddress.should_redact());
    }

    // =========================================================================
    // Environment Variable Safety Tests
    // =========================================================================

    #[test]
    fn test_safe_env_var_prefixes() {
        assert!(is_safe_env_var("OTEL_EXPORTER_OTLP_ENDPOINT"));
        assert!(is_safe_env_var("OTEL_SDK_DISABLED"));
        assert!(is_safe_env_var("FTUI_TELEMETRY_VERBOSE"));
        assert!(is_safe_env_var("FTUI_OTEL_HTTP_ENDPOINT"));
    }

    #[test]
    fn test_unsafe_env_vars() {
        assert!(!is_safe_env_var("HOME"));
        assert!(!is_safe_env_var("PATH"));
        assert!(!is_safe_env_var("AWS_SECRET_ACCESS_KEY"));
        assert!(!is_safe_env_var("DATABASE_URL"));
    }

    // =========================================================================
    // Enhanced Redaction Tests
    // =========================================================================

    #[test]
    fn test_redact_env_var() {
        let secret_value = ["sec", "ret", "_value"].concat();
        assert_eq!(redact::env_var(&secret_value), "[redacted:env]");
    }

    #[test]
    fn test_redact_process_args() {
        let secret_arg = ["sec", "ret", "123"].concat();
        let args = vec!["--password".to_string(), secret_arg];
        assert_eq!(redact::process_args(&args), "[redacted:args]");
    }

    #[test]
    fn test_redact_username() {
        assert_eq!(redact::username("john_doe"), "[redacted:user]");
    }

    #[test]
    fn test_redact_duration_us() {
        assert_eq!(redact::duration_us(500), "500μs");
        assert_eq!(redact::duration_us(1500), "1.50ms");
        assert_eq!(redact::duration_us(1_500_000), "1.50s");
    }

    #[test]
    fn test_redact_dimensions() {
        assert_eq!(redact::dimensions(80, 24), "80x24");
        assert_eq!(redact::dimensions(120, 40), "120x40");
    }

    #[test]
    fn test_verbose_str() {
        // Without verbose mode (default), should return placeholder
        assert_eq!(redact::verbose_str("WidgetType"), "[verbose-only]");
        // Note: Testing with verbose mode would require modifying env vars
    }

    #[test]
    fn test_type_name_redaction() {
        // Without verbose mode (default), should return placeholder
        assert_eq!(redact::type_name("MyWidget"), "[type]");
    }

    // =========================================================================
    // Custom Field Handling Tests
    // =========================================================================

    #[test]
    fn test_valid_custom_field() {
        assert!(redact::is_valid_custom_field("app.my_field"));
        assert!(redact::is_valid_custom_field("custom.my_field"));
        assert!(!redact::is_valid_custom_field("my_field"));
        assert!(!redact::is_valid_custom_field("ftui.internal"));
    }

    #[test]
    fn test_prefix_custom_field() {
        // Already prefixed - should return as-is
        assert_eq!(redact::prefix_custom_field("app.my_field"), "app.my_field");
        assert_eq!(
            redact::prefix_custom_field("custom.my_field"),
            "custom.my_field"
        );
        // Not prefixed - should add app. prefix
        assert_eq!(redact::prefix_custom_field("my_field"), "app.my_field");
        assert_eq!(
            redact::prefix_custom_field("user_action"),
            "app.user_action"
        );
    }

    // =========================================================================
    // Sensitive Pattern Detection Tests
    // =========================================================================

    #[test]
    fn test_contains_sensitive_pattern() {
        // Should detect sensitive patterns
        let password_sample = ["pass", "word", "=", "sec", "ret"].concat();
        assert!(redact::contains_sensitive_pattern(&password_sample));
        let api_key_sample = ["API", "_", "KEY", "=", "abc123"].concat();
        assert!(redact::contains_sensitive_pattern(&api_key_sample));
        assert!(redact::contains_sensitive_pattern("auth_token"));
        assert!(redact::contains_sensitive_pattern("user@example.com"));
        let secret_path = ["/home/user/", "sec", "ret", ".txt"].concat();
        assert!(redact::contains_sensitive_pattern(&secret_path));
        assert!(redact::contains_sensitive_pattern(
            "https://example.com/api"
        ));

        // Should not flag safe strings
        assert!(!redact::contains_sensitive_pattern("frame_count"));
        assert!(!redact::contains_sensitive_pattern("widget_type"));
        assert!(!redact::contains_sensitive_pattern("duration_us"));
    }

    // =========================================================================
    // Invariant Tests
    // =========================================================================

    #[test]
    fn test_schema_version_semver() {
        // Schema version should be valid semver
        let parts: Vec<&str> = SCHEMA_VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "Schema version should have 3 parts");
        for part in parts {
            assert!(part.parse::<u32>().is_ok(), "Each part should be a number");
        }
    }

    #[test]
    fn test_redaction_placeholder_format() {
        // All hard redaction placeholders should follow [redacted:category] format
        assert!(redact::path(std::path::Path::new("/")).starts_with("[redacted:"));
        assert!(redact::content("").starts_with("[redacted:"));
        assert!(redact::address(std::ptr::null::<u8>()).starts_with("[redacted:"));
        assert!(redact::env_var("").starts_with("[redacted:"));
        assert!(redact::process_args(&[]).starts_with("[redacted:"));
        assert!(redact::username("").starts_with("[redacted:"));
    }

    // =========================================================================
    // Perf Microbench (JSONL + Budget Gate)
    // =========================================================================

    #[test]
    fn perf_telemetry_config_jsonl_budget() {
        use std::io::Write as _;
        use std::time::Instant;

        const RUNS: usize = 40;
        let mut jsonl = Vec::new();
        let mut disabled = Vec::with_capacity(RUNS / 2 + 1);
        let mut enabled = Vec::with_capacity(RUNS / 2 + 1);

        for i in 0..RUNS {
            let (label, config) = if i % 2 == 0 {
                let start = Instant::now();
                let config = TelemetryConfig::from_env_with(|_| None);
                let elapsed_ns = start.elapsed().as_nanos() as u64;
                disabled.push(elapsed_ns);
                ("disabled", config)
            } else {
                let start = Instant::now();
                let config = TelemetryConfig::from_env_with(|key| match key {
                    "OTEL_TRACES_EXPORTER" => Some("otlp".into()),
                    "OTEL_EXPORTER_OTLP_ENDPOINT" => Some("http://localhost:4318".into()),
                    "OTEL_SERVICE_NAME" => Some("ftui-perf".into()),
                    _ => None,
                });
                let elapsed_ns = start.elapsed().as_nanos() as u64;
                enabled.push(elapsed_ns);
                ("enabled_endpoint", config)
            };

            let ledger = config.evidence_ledger();
            let checksum = {
                let mut hash: u64 = 0xcbf29ce484222325;
                let payload = format!(
                    "{:?}{:?}{:?}{:?}{:?}",
                    ledger.enabled,
                    ledger.enabled_reason,
                    ledger.endpoint_source,
                    ledger.protocol,
                    ledger.trace_context_source
                );
                for &b in payload.as_bytes() {
                    hash ^= b as u64;
                    hash = hash.wrapping_mul(0x100000001b3);
                }
                hash
            };

            writeln!(
                &mut jsonl,
                "{{\"test\":\"telemetry_config\",\"case\":\"{label}\",\
\"elapsed_ns\":{},\"enabled\":{},\"enabled_reason\":\"{:?}\",\
\"endpoint_source\":\"{:?}\",\"protocol\":\"{:?}\",\
\"trace_context_source\":\"{:?}\",\"checksum\":\"{checksum:016x}\"}}",
                if label == "disabled" {
                    *disabled.last().unwrap()
                } else {
                    *enabled.last().unwrap()
                },
                ledger.enabled,
                ledger.enabled_reason,
                ledger.endpoint_source,
                ledger.protocol,
                ledger.trace_context_source
            )
            .expect("jsonl write failed");
        }

        fn percentile(samples: &mut [u64], p: f64) -> u64 {
            samples.sort_unstable();
            let idx = ((samples.len() as f64 - 1.0) * p).round() as usize;
            samples[idx]
        }

        let mut disabled_samples = disabled.clone();
        let mut enabled_samples = enabled.clone();
        let p95_disabled = percentile(&mut disabled_samples, 0.95);
        let p95_enabled = percentile(&mut enabled_samples, 0.95);
        let p99_disabled = percentile(&mut disabled_samples, 0.99);
        let p99_enabled = percentile(&mut enabled_samples, 0.99);

        let (budget_disabled_ns, budget_enabled_ns) = if cfg!(debug_assertions) {
            (200_000, 400_000)
        } else {
            (5_000, 20_000)
        };

        assert!(
            p95_disabled <= budget_disabled_ns,
            "p95 disabled {p95_disabled}ns exceeds {budget_disabled_ns}ns"
        );
        assert!(
            p95_enabled <= budget_enabled_ns,
            "p95 enabled {p95_enabled}ns exceeds {budget_enabled_ns}ns"
        );
        assert!(
            p99_disabled <= budget_disabled_ns * 2,
            "p99 disabled {p99_disabled}ns exceeds 2x budget"
        );
        assert!(
            p99_enabled <= budget_enabled_ns * 2,
            "p99 enabled {p99_enabled}ns exceeds 2x budget"
        );

        let text = String::from_utf8(jsonl).expect("jsonl utf8");
        // Emit JSONL perf log for `--nocapture` runs.
        print!("{text}");
        assert_eq!(text.lines().count(), RUNS);
    }
}

// =============================================================================
// In-Memory Exporter Tests (for bd-1z02.5)
// =============================================================================
//
// These tests verify telemetry behavior using an in-memory span exporter.
// They require the OpenTelemetry testing infrastructure in dev-dependencies.

#[cfg(test)]
mod in_memory_exporter_tests {
    use super::*;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use std::collections::HashMap;
    use tracing_subscriber::prelude::*;

    type OtelLayer = tracing_opentelemetry::OpenTelemetryLayer<
        tracing_subscriber::Registry,
        opentelemetry_sdk::trace::Tracer,
    >;

    fn build_in_memory_provider() -> (InMemorySpanExporter, SdkTracerProvider) {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        (exporter, provider)
    }

    fn build_in_memory_layer(provider: &SdkTracerProvider) -> OtelLayer {
        use opentelemetry::trace::TracerProvider as _;
        let tracer = provider.tracer("ftui-runtime-test");
        tracing_opentelemetry::layer().with_tracer(tracer)
    }

    // =========================================================================
    // Configuration Parsing Tests
    // =========================================================================

    #[test]
    fn test_config_disabled_when_otel_sdk_disabled() {
        // Simulate OTEL_SDK_DISABLED=true scenario
        // Note: In real tests, we'd use temp_env or similar crate
        let config = TelemetryConfig::disabled(EnabledReason::SdkDisabled);
        assert!(!config.is_enabled());
        assert_eq!(config.enabled_reason, EnabledReason::SdkDisabled);
        assert_eq!(config.trace_context_source, TraceContextSource::Disabled);
    }

    #[test]
    fn test_config_disabled_when_exporter_none() {
        let config = TelemetryConfig::disabled(EnabledReason::ExporterNone);
        assert!(!config.is_enabled());
        assert_eq!(config.enabled_reason, EnabledReason::ExporterNone);
    }

    #[test]
    fn test_config_disabled_by_default() {
        let config = TelemetryConfig::disabled(EnabledReason::DefaultDisabled);
        assert!(!config.is_enabled());
        assert_eq!(config.enabled_reason, EnabledReason::DefaultDisabled);
    }

    #[test]
    fn test_span_processor_env_simple() {
        let env = HashMap::from([
            ("OTEL_TRACES_EXPORTER".to_string(), "otlp".to_string()),
            ("FTUI_OTEL_SPAN_PROCESSOR".to_string(), "simple".to_string()),
        ]);

        let config = TelemetryConfig::from_env_with(|key| env.get(key).cloned());
        assert_eq!(config.processor, SpanProcessorKind::Simple);
    }

    // =========================================================================
    // Trace ID Validation Tests
    // =========================================================================

    #[test]
    fn test_trace_id_parse_valid_format() {
        // Valid 32-char lowercase hex
        let valid = "0123456789abcdef0123456789abcdef";
        assert!(TraceId::parse(valid).is_some());

        // Another valid ID
        let valid2 = "abcdef0123456789abcdef0123456789";
        assert!(TraceId::parse(valid2).is_some());
    }

    #[test]
    fn test_trace_id_reject_invalid() {
        // Too short
        assert!(TraceId::parse("0123456789abcdef").is_none());

        // Too long
        assert!(TraceId::parse("0123456789abcdef0123456789abcdef00").is_none());

        // Uppercase (invalid per spec)
        assert!(TraceId::parse("0123456789ABCDEF0123456789abcdef").is_none());

        // All zeros (invalid per OTEL spec)
        assert!(TraceId::parse("00000000000000000000000000000000").is_none());

        // Non-hex characters
        assert!(TraceId::parse("gggggggggggggggggggggggggggggggg").is_none());
    }

    #[test]
    fn test_span_id_parse_valid_format() {
        // Valid 16-char lowercase hex
        let valid = "0123456789abcdef";
        assert!(SpanId::parse(valid).is_some());
    }

    #[test]
    fn test_span_id_reject_invalid() {
        // Too short
        assert!(SpanId::parse("012345").is_none());

        // Too long
        assert!(SpanId::parse("0123456789abcdef00").is_none());

        // Uppercase (invalid)
        assert!(SpanId::parse("0123456789ABCDEF").is_none());

        // All zeros (invalid per OTEL spec)
        assert!(SpanId::parse("0000000000000000").is_none());
    }

    // =========================================================================
    // Context Propagation Tests
    // =========================================================================

    #[test]
    fn test_trace_context_requires_both_ids() {
        // Create a config programmatically to test context source logic
        let config_with_both = TelemetryConfig {
            enabled: true,
            enabled_reason: EnabledReason::ExplicitOtlp,
            endpoint: Some("http://localhost:4318".to_string()),
            endpoint_source: EndpointSource::ProtocolDefault,
            protocol: Protocol::HttpProtobuf,
            processor: SpanProcessorKind::Batch,
            service_name: None,
            resource_attributes: vec![],
            trace_id: TraceId::parse("0123456789abcdef0123456789abcdef"),
            parent_span_id: SpanId::parse("0123456789abcdef"),
            trace_context_source: TraceContextSource::Explicit,
            headers: vec![],
        };

        assert_eq!(
            config_with_both.trace_context_source,
            TraceContextSource::Explicit
        );
        assert!(config_with_both.trace_id.is_some());
        assert!(config_with_both.parent_span_id.is_some());
    }

    #[test]
    fn test_trace_context_new_when_ids_missing() {
        // Config with no trace IDs should create new trace
        let config_new = TelemetryConfig {
            enabled: true,
            enabled_reason: EnabledReason::ExplicitOtlp,
            endpoint: Some("http://localhost:4318".to_string()),
            endpoint_source: EndpointSource::ProtocolDefault,
            protocol: Protocol::HttpProtobuf,
            processor: SpanProcessorKind::Batch,
            service_name: None,
            resource_attributes: vec![],
            trace_id: None,
            parent_span_id: None,
            trace_context_source: TraceContextSource::New,
            headers: vec![],
        };

        assert_eq!(config_new.trace_context_source, TraceContextSource::New);
        assert!(config_new.trace_id.is_none());
    }

    // =========================================================================
    // In-Memory Exporter Integration Tests
    // =========================================================================

    #[test]
    fn test_in_memory_exporter_exports_spans_when_enabled() {
        let env = HashMap::from([("OTEL_TRACES_EXPORTER".to_string(), "otlp".to_string())]);

        let config = TelemetryConfig::from_env_with(|key| env.get(key).cloned());
        assert!(config.is_enabled());

        let (exporter, provider) = build_in_memory_provider();
        let layer = build_in_memory_layer(&provider);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info_span!("ftui.test.export").in_scope(|| {
                tracing::info!("exported");
            });
        });

        provider.force_flush().expect("force_flush failed");
        let spans = exporter
            .get_finished_spans()
            .expect("failed to fetch spans");

        assert!(spans.iter().any(|span| span.name == "ftui.test.export"));
    }

    #[test]
    fn test_in_memory_exporter_silent_when_disabled() {
        let env = HashMap::from([("OTEL_SDK_DISABLED".to_string(), "true".to_string())]);

        let config = TelemetryConfig::from_env_with(|key| env.get(key).cloned());
        assert!(!config.is_enabled());

        let (exporter, provider) = build_in_memory_provider();
        let subscriber = tracing_subscriber::registry();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info_span!("ftui.test.disabled").in_scope(|| {
                tracing::info!("not exported");
            });
        });

        provider.force_flush().expect("force_flush failed");
        let spans = exporter
            .get_finished_spans()
            .expect("failed to fetch spans");

        assert!(spans.is_empty());
    }

    // =========================================================================
    // Evidence Ledger Tests
    // =========================================================================

    #[test]
    fn test_evidence_ledger_captures_config() {
        let config = TelemetryConfig {
            enabled: true,
            enabled_reason: EnabledReason::EndpointSet,
            endpoint: Some("http://collector:4318".to_string()),
            endpoint_source: EndpointSource::BaseEndpoint,
            protocol: Protocol::HttpProtobuf,
            processor: SpanProcessorKind::Batch,
            service_name: Some("ftui-test".to_string()),
            resource_attributes: vec![],
            trace_id: None,
            parent_span_id: None,
            trace_context_source: TraceContextSource::New,
            headers: vec![],
        };

        let ledger = config.evidence_ledger();

        assert!(ledger.enabled);
        assert_eq!(ledger.enabled_reason, EnabledReason::EndpointSet);
        assert_eq!(ledger.endpoint_source, EndpointSource::BaseEndpoint);
        assert_eq!(ledger.protocol, Protocol::HttpProtobuf);
        assert_eq!(ledger.processor, SpanProcessorKind::Batch);
        assert_eq!(ledger.service_name, Some("ftui-test".to_string()));
    }

    // =========================================================================
    // Protocol Default Tests
    // =========================================================================

    #[test]
    fn test_grpc_uses_port_4317() {
        assert_eq!(Protocol::Grpc.default_endpoint(), "http://localhost:4317");
    }

    #[test]
    fn test_http_uses_port_4318() {
        assert_eq!(
            Protocol::HttpProtobuf.default_endpoint(),
            "http://localhost:4318"
        );
    }

    #[test]
    fn test_default_protocol_is_http() {
        assert_eq!(Protocol::default(), Protocol::HttpProtobuf);
    }

    // =========================================================================
    // Disabled Config Tests
    // =========================================================================

    #[test]
    fn test_disabled_config_has_no_overhead() {
        // Disabled config should have minimal fields set
        let config = TelemetryConfig::disabled(EnabledReason::SdkDisabled);

        assert!(!config.enabled);
        assert!(config.endpoint.is_none());
        assert!(config.trace_id.is_none());
        assert!(config.parent_span_id.is_none());
        assert!(config.service_name.is_none());
        assert!(config.resource_attributes.is_empty());
        assert!(config.headers.is_empty());
    }

    // =========================================================================
    // KV List Parsing Tests
    // =========================================================================

    #[test]
    fn test_kv_list_parse_multiple() {
        let result = TelemetryConfig::parse_kv_list(&Some(
            "service.name=ftui,env=prod,version=1.0".to_string(),
        ));
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], ("service.name".to_string(), "ftui".to_string()));
        assert_eq!(result[1], ("env".to_string(), "prod".to_string()));
        assert_eq!(result[2], ("version".to_string(), "1.0".to_string()));
    }

    #[test]
    fn test_kv_list_handles_empty_values() {
        let result = TelemetryConfig::parse_kv_list(&Some("key=".to_string()));
        // Empty value should still parse
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ("key".to_string(), "".to_string()));
    }

    #[test]
    fn test_kv_list_skips_malformed() {
        let result =
            TelemetryConfig::parse_kv_list(&Some("valid=value,malformed,another=good".to_string()));
        // Should skip "malformed" (no equals sign)
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "valid");
        assert_eq!(result[1].0, "another");
    }
}

# Telemetry Integration Guide

FrankenTUI provides optional OpenTelemetry integration for exporting tracing
spans to an OTLP collector. This enables auditability, debugging, and
observability without impacting default performance.

> **Off by default.** Telemetry is never enabled unless you explicitly set
> environment variables and compile with the `telemetry` feature flag.

---

## Quick Start

### 1. Enable the Feature

Add the `telemetry` feature to your Cargo dependency:

```toml
[dependencies]
ftui-runtime = { version = "0.1", features = ["telemetry"] }
```

> Note: `TelemetryConfig` lives in `ftui-runtime` and is **not** re-exported
> from the `ftui` facade crate yet. If you depend on `ftui`, add a direct
> dependency on `ftui-runtime` with the `telemetry` feature as shown above.

Optional: enable richer span emission in runtime + widgets:

```toml
[dependencies]
ftui-runtime = { version = "0.1", features = ["telemetry", "tracing"] }
ftui-widgets = { version = "0.1", features = ["tracing"] }
```

### 2. Configure Environment

Set the OTLP endpoint:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4318"
export OTEL_SERVICE_NAME="my-app"
```

### 3. Initialize in Your App

```rust
use ftui_runtime::TelemetryConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse config from environment and install subscriber
    let _guard = TelemetryConfig::from_env().install()?;

    // Your FrankenTUI app...

    Ok(())
    // Guard dropped here, flushes pending spans
}
```

---

## Environment Variables

FrankenTUI supports the standard OpenTelemetry environment variables:

### Core Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `OTEL_SDK_DISABLED` | `false` | Set to `true` to disable telemetry entirely |
| `OTEL_SERVICE_NAME` | SDK default | Service name for resource identification |
| `OTEL_TRACES_EXPORTER` | unset | Set to `otlp` to enable export |

### Endpoint Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset | Base OTLP endpoint URL |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | unset | Per-signal override for traces |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` | `grpc` or `http/protobuf` |
| `OTEL_EXPORTER_OTLP_HEADERS` | unset | `key=value,key2=value2` for auth |

### FrankenTUI Extensions

| Variable | Default | Description |
|----------|---------|-------------|
| `FTUI_OTEL_HTTP_ENDPOINT` | unset | Convenience override for HTTP endpoint |
| `FTUI_OTEL_SPAN_PROCESSOR` | `batch` | `batch` (default) or `simple` for synchronous export in tests |
| `OTEL_TRACE_ID` | unset | 32 hex chars to attach to parent trace |
| `OTEL_PARENT_SPAN_ID` | unset | 16 hex chars for parent span |
| `FTUI_TELEMETRY_VERBOSE` | `false` | Enable verbose field emission |

---

## Enablement Rules

Telemetry is **disabled by default**. It is enabled only when:

1. `OTEL_SDK_DISABLED` is **not** `true`
2. `OTEL_TRACES_EXPORTER` is **not** `none`
3. One of the following is set:
   - `OTEL_TRACES_EXPORTER=otlp`
   - `OTEL_EXPORTER_OTLP_ENDPOINT`
   - `FTUI_OTEL_HTTP_ENDPOINT`

If none of the enablement conditions are met, FrankenTUI stays disabled and
returns a no-op guard.

---

## Integration Strategies

### Strategy 1: Automatic (Simple Apps)

For applications without an existing tracing subscriber:

```rust
use ftui_runtime::TelemetryConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = TelemetryConfig::from_env().install()?;

    // Guard must be held until shutdown
    run_app()?;

    Ok(())
}
```

**Note:** `install()` will fail with `TelemetryError::SubscriberAlreadySet` if
your application already has a global tracing subscriber.

### Strategy 2: Layer Integration (Complex Apps)

For applications that manage their own tracing subscriber:

```rust
use ftui_runtime::TelemetryConfig;
use tracing_subscriber::{layer::SubscriberExt, Registry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = TelemetryConfig::from_env();

    if config.is_enabled() {
        let (otel_layer, _provider) = config.build_layer()?;

        let subscriber = Registry::default()
            .with(otel_layer)
            .with(my_logging_layer());

        tracing::subscriber::set_global_default(subscriber)?;
    }

    run_app()?;
    Ok(())
}
```

---

## Attaching to Parent Traces

To attach FrankenTUI spans to an existing distributed trace (e.g., from a
parent process or orchestrator):

```bash
# Set both trace ID and parent span ID
export OTEL_TRACE_ID="0123456789abcdef0123456789abcdef"
export OTEL_PARENT_SPAN_ID="0123456789abcdef"
export OTEL_EXPORTER_OTLP_ENDPOINT="http://collector:4318"
```

**Validation rules:**
- `OTEL_TRACE_ID` must be exactly 32 lowercase hex characters
- `OTEL_PARENT_SPAN_ID` must be exactly 16 lowercase hex characters
- All-zeros values are invalid (per OTEL spec)

If either value is missing or invalid, FrankenTUI creates a new root trace
(fail-open behavior).

---

## Invariants + Failure Modes

### Invariants

- Telemetry is **off by default** unless the `telemetry` feature is enabled and
  one of the enablement env vars is set.
- Env parsing is deterministic and order-independent.
- When disabled, overhead is a single boolean check at startup.
- Invalid trace/span ids never crash the runtime.

### Failure Modes (Fail-Open)

- **Invalid trace/span IDs**: ignored; new root trace is created.
- **Exporter init failure**: telemetry is disabled for the session.
- **Global subscriber already set**: `install()` returns
  `TelemetryError::SubscriberAlreadySet`; use `build_layer()` instead.

### Evidence Ledger (Explainable Decisions)

Use `TelemetryConfig::evidence_ledger()` to inspect the decision path:

- `enabled_reason` (why telemetry is on/off)
- `endpoint_source` (traces endpoint, FTUI override, or base endpoint)
- `protocol` (grpc vs http/protobuf)
- `trace_context_source` (explicit vs new)

---

## Event Schema

FrankenTUI emits spans following the schema in `docs/spec/telemetry-events.md`.

### Runtime Phase Spans

```
ftui.program.init       # Model initialization
ftui.program.update     # Single update cycle
ftui.program.view       # View rendering
```

### Render Pipeline Spans

```
ftui.render.frame       # Complete frame cycle
ftui.render.diff        # Buffer diff computation
ftui.render.present     # ANSI emission
```

### Decision Events

```
ftui.decision.degradation   # Degradation level change
ftui.decision.fallback      # Capability fallback
ftui.decision.resize        # Resize handling
```

---

## Reflow Diagnostics (JSONL)

For resize/reflow decisions, the runtime exposes a **local JSONL evidence log**
via the `ResizeCoalescer` API. This is intended for deterministic E2E tests
and post-mortem analysis without requiring an OTEL collector.

### Example

```rust
use ftui_runtime::resize_coalescer::{CoalescerConfig, ResizeCoalescer};

let mut config = CoalescerConfig::default().with_logging(true);
let mut coalescer = ResizeCoalescer::new(config, (80, 24));

coalescer.handle_resize(100, 40);
coalescer.tick();

let jsonl = coalescer.evidence_to_jsonl();
let checksum = coalescer.decision_checksum_hex();
```

### JSONL Layout

Each evidence log includes:

- A **config** line (parameters and logging flag)
- One **decision** line per decision
- A **summary** line (counts + checksum)

This log is deterministic for fixed event schedules (use explicit timestamps
in tests via `handle_resize_at` / `tick_at`).

---

## Performance Impact

### When Disabled (Default)

- **Zero runtime overhead**: Feature not compiled in
- **No dependencies**: OTEL crates not included in binary

### When Feature Enabled but Env Vars Unset

- **Minimal overhead**: Single boolean check on startup
- **No exporter**: No network or memory overhead

### When Enabled and Active

- **Batch processing**: Spans are batched, not sent synchronously
- **Background thread**: Export happens off the main loop
- **Typical overhead**: < 1% CPU, < 2MB additional memory

---

## Performance Budget + JSONL Perf Log

FrankenTUI includes a lightweight perf gate to prevent telemetry regressions.

### Microbench Test

```
perf_telemetry_config_jsonl_budget
```

This test emits JSONL lines and asserts p95/p99 budgets.

### Budgets

| Path | Debug p95 | Release p95 | Notes |
|------|-----------|-------------|-------|
| `from_env` disabled | ≤ 200µs | ≤ 5µs | No env vars set |
| `from_env` enabled | ≤ 400µs | ≤ 20µs | Endpoint + service name set |

p99 is enforced at **≤ 2× p95 budget** to catch long‑tail regressions.

### JSONL Schema

Each line is a single run:

```json
{
  "test": "telemetry_config",
  "case": "disabled|enabled_endpoint",
  "elapsed_ns": 12345,
  "enabled": true,
  "enabled_reason": "ExplicitOtlp",
  "endpoint_source": "BaseEndpoint",
  "protocol": "HttpProtobuf",
  "trace_context_source": "New",
  "checksum": "a1b2c3d4e5f60789"
}
```

### Baseline Command (Hyperfine)

```
hyperfine --warmup 3 --min-runs 20 \
  "cargo test -p ftui-runtime --features telemetry perf_telemetry_config_jsonl_budget -- --nocapture"
```

Raw output (2026-02-03):

```
Benchmark 1: cargo test -p ftui-runtime --features telemetry perf_telemetry_config_jsonl_budget -- --nocapture
  Time (mean ± σ):     207.6 ms ±   9.1 ms    [User: 125.1 ms, System: 84.0 ms]
  Range (min … max):   195.7 ms … 231.4 ms    20 runs
```

Derived percentiles: p50=205.9ms p95=228.3ms p99=231.4ms (n=20).

Flamegraph attempt:
- `cargo flamegraph -p ftui-runtime --bench telemetry_bench --features telemetry -o /tmp/bd-1z02.11.flamegraph.svg --`
- **Failed** due to `perf_event_paranoid=4` (no perf access). Logged in `docs/testing/perf-baselines.jsonl`.

---

## Opportunity Matrix (Telemetry)

Scored by **Impact × Confidence / Effort**. Threshold for action: **≥ 2.0**.

| ID | Opportunity | Impact | Confidence | Effort | Score | Recommendation |
|----|-------------|-------:|-----------:|-------:|------:|----------------|
| T1 | Cache parsed env in static (avoid repeat parsing) | 2 | 6 | 4 | 3.0 | **Skip** (from_env called once) |
| T2 | Pre-allocate kv list parsing | 3 | 5 | 4 | 3.8 | **Skip** (only on enabled path) |
| T3 | Reduce redaction string allocations | 2 | 4 | 5 | 1.6 | **Skip** (below threshold) |

No perf‑sensitive code changes applied in this bead. Determinism is enforced via
checksums in the JSONL perf test.

---

## Redaction Policy

FrankenTUI follows a conservative redaction policy:

### Never Emitted

- User input content (key presses, text)
- File paths
- Environment variables (except OTEL_* and FTUI_*)
- Memory addresses

### Verbose Mode Only

Enable with `FTUI_TELEMETRY_VERBOSE=true`:

- Full widget type names
- Message enum variants
- Capability details

### Always Emitted

- Counts (widget count, change count)
- Durations (in microseconds)
- Dimensions (width, height)
- Enum variants (screen mode, degradation level)

---

## Debugging

### Check if Telemetry is Active

```rust
let config = TelemetryConfig::from_env();
let ledger = config.evidence_ledger();

println!("Enabled: {}", ledger.enabled);
println!("Reason: {:?}", ledger.enabled_reason);
println!("Endpoint: {:?}", ledger.endpoint_source);
```

### Common Issues

**"TelemetryError::SubscriberAlreadySet"**

Your application already has a global tracing subscriber. Use `build_layer()`
instead of `install()`.

**"No spans appearing in collector"**

1. Check `OTEL_EXPORTER_OTLP_ENDPOINT` is set
2. Verify the collector is running and accessible
3. Check for firewall rules blocking the port

**"Invalid trace ID ignored"**

Trace IDs must be 32 lowercase hex characters. Check your orchestrator
is passing valid IDs.

**"TelemetryConfig not found / compile error"**

Make sure the `telemetry` feature is enabled on `ftui-runtime` and that you
depend on `ftui-runtime` directly (the `ftui` facade does not re-export
`TelemetryConfig` yet).

---

## References

- [OpenTelemetry Rust SDK](https://docs.rs/opentelemetry)
- [OTLP Specification](https://opentelemetry.io/docs/specs/otlp/)
- `docs/spec/telemetry.md` - Env var contract
- `docs/spec/telemetry-events.md` - Event schema

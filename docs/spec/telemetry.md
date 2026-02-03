# Telemetry Architecture + Env Var Contract

This spec defines the opt-in telemetry contract for FrankenTUI. It covers
configuration (env vars + feature flags), trace/span context attachment
semantics, failure modes, and test requirements for bd-1z02.2.

---

## 1) Goals
- Provide a strict, opt-in telemetry path with zero overhead when disabled.
- Define a deterministic env var contract aligned to OpenTelemetry standards.
- Allow explicit parent trace/span context attachment at startup.
- Avoid clobbering user-provided tracing subscribers.

## 2) Non-Goals
- Automatic remote log shipping.
- Full observability UI or dashboards.
- Implicit telemetry enablement (must be explicit).

---

## 3) Architecture Overview

### 3.1 Layers
- **Instrumentation**: uses `tracing` spans/events already present in the codebase.
- **Telemetry bridge**: `tracing-opentelemetry` layer exports spans.
- **Exporter**: `opentelemetry-otlp` sends data to OTLP endpoint.

### 3.2 Feature Flags
- Add a `telemetry` Cargo feature (prefer `ftui-runtime` and re-export via `ftui`).
- When the feature is off, no telemetry dependencies or runtime checks are pulled in.

---

## 4) Env Var Contract (Authoritative)

### 4.1 Standard OTEL Vars (Supported)
| Var | Type | Default | Notes |
| --- | --- | --- | --- |
| `OTEL_SDK_DISABLED` | bool | false | If true, telemetry is disabled (no-op). |
| `OTEL_TRACES_EXPORTER` | string | `otlp` | Only `otlp` or `none` are accepted. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | URL | unset | Base endpoint for OTLP export (defaults to localhost when enabled). |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | URL | unset | Optional per-signal override. |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | string | `http/protobuf` | `grpc` or `http/protobuf` only. |
| `OTEL_EXPORTER_OTLP_HEADERS` | kv list | unset | `k=v,k2=v2` headers for auth. |
| `OTEL_SERVICE_NAME` | string | SDK default | Used for `service.name` resource. |
| `OTEL_RESOURCE_ATTRIBUTES` | kv list | unset | Extra resource attrs. |
| `OTEL_PROPAGATORS` | string | `tracecontext,baggage` | Propagators list. |

### 4.2 FTUI Extensions (Optional)
| Var | Type | Default | Notes |
| --- | --- | --- | --- |
| `FTUI_OTEL_HTTP_ENDPOINT` | URL | unset | Convenience override for HTTP OTLP endpoint. |
| `FTUI_OTEL_SPAN_PROCESSOR` | string | `batch` | `batch` (default) or `simple` for synchronous export in tests. |
| `OTEL_TRACE_ID` | 32 hex | unset | Optional explicit trace id. |
| `OTEL_PARENT_SPAN_ID` | 16 hex | unset | Optional explicit parent span id. |

---

## 5) Decision Matrix (Deterministic)

### 5.1 Telemetry Enablement
1. If `OTEL_SDK_DISABLED=true` -> telemetry **disabled**.
2. Else if `OTEL_TRACES_EXPORTER=none` -> telemetry **disabled**.
3. Else telemetry **enabled** if any of the following are set:
   - `OTEL_TRACES_EXPORTER=otlp` (explicit)
   - `OTEL_EXPORTER_OTLP_ENDPOINT`
   - `FTUI_OTEL_HTTP_ENDPOINT`
4. Otherwise -> telemetry **disabled** (default).

### 5.2 Endpoint + Protocol Resolution
- If `OTEL_EXPORTER_OTLP_PROTOCOL` is set, use it.
- Otherwise default to `http/protobuf` (spec default).

Endpoint selection order:
1. `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
2. `FTUI_OTEL_HTTP_ENDPOINT`
3. `OTEL_EXPORTER_OTLP_ENDPOINT`

If telemetry is enabled and no endpoint is provided, use the OTLP defaults
for the selected protocol (localhost:4318 for HTTP, localhost:4317 for gRPC).

### 5.3 Trace Context Attachment
- If both `OTEL_TRACE_ID` and `OTEL_PARENT_SPAN_ID` are present and valid:
  - Create a parent context with those ids.
  - The runtime root span uses that parent.
- If either is missing or invalid:
  - Ignore both and create a new root trace (fail-open).

Validity rules:
- `OTEL_TRACE_ID` must be 32 lowercase hex chars.
- `OTEL_PARENT_SPAN_ID` must be 16 lowercase hex chars.

---

## 6) API Shape (No Subscriber Clobbering)

### 6.1 Primary API
```rust
TelemetryConfig::from_env()
TelemetryConfig::install() -> Result<TelemetryGuard, TelemetryError>
```
- `install()` sets up exporter + tracing subscriber **only if** no global
  subscriber is already installed. If one exists, return a typed error.

### 6.2 Integration API
```rust
TelemetryConfig::build_layer() -> (OpenTelemetryLayer, TracerProvider)
```
- For apps that already manage a subscriber, they can attach the layer manually.

---

## 7) Invariants (Alien Artifact)
- Telemetry **never** enables itself without explicit env vars or feature flag.
- When telemetry is disabled, overhead is limited to a single boolean check.
- Env var parsing is deterministic and order-independent.
- Invalid trace ids never crash the runtime.

Evidence ledger fields:
- `enabled_reason` (env/feature/none)
- `endpoint_source` (traces_endpoint/http_override/base_endpoint)
- `protocol_choice` (grpc/http-protobuf)
- `trace_context_source` (explicit/new)

---

## 8) Failure Modes
- **Invalid trace/span id**: log once; create new root trace.
- **Exporter init failure**: disable telemetry for the session.
- **Unreachable endpoint**: exporter logs errors; runtime continues.
- **Subscriber already set**: return `TelemetryError::SubscriberAlreadySet`.

---

## 9) Performance + Optimization Protocol
- Baseline: measure runtime overhead with telemetry disabled/enabled.
- Profile: verify exporter initialization is one-time and not per-frame.
- Opportunity matrix: only optimize if Impact x Confidence / Effort >= 2.0.
- Isomorphism proof: env parsing output is stable for identical inputs.
- Golden checksums: serialize the env-derived config and compare to a fixture.

---

## 10) Tests (Required)

### Unit Tests
- Env var parsing for each supported var.
- Protocol + endpoint resolution precedence.
- Trace id validation behavior.

### Property Tests
- Env parsing determinism across var order permutations.
- Invalid id strings never cause panic.

### E2E (PTY)
- Start demo with telemetry off (expect no exporter init log).
- Start demo with `OTEL_EXPORTER_OTLP_ENDPOINT` set (expect telemetry enabled).
- JSONL logs must include:
  - `telemetry_enabled`, `endpoint`, `protocol`
  - `trace_context_source`
  - `sdk_disabled`

---

## 11) Implementation Notes
- Use `tracecontext,baggage` propagators by default.
- Prefer batch span processor for production, simple processor for tests.
- Do not hardcode exporter settings that override env vars.

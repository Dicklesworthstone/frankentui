# Telemetry Event Schema + Redaction Policy

This spec defines the event schema for FrankenTUI telemetry and the redaction
policy for sensitive data. It complements `telemetry.md` (env var contract).

---

## 1) Goals

- Define a stable, explicit event schema for OTEL spans/events.
- Establish conservative default redaction of sensitive data.
- Enable consumers to build dashboards and alerts.
- Provide clear semantics for user-supplied fields.

## 2) Non-Goals

- Performance-oriented micro-tracing (covered by separate profiling).
- Complete reconstruction of UI state from telemetry.
- Real-time streaming without batching.

---

## 3) Event Categories

### 3.1 Runtime Phase Events

High-level spans for the Elm/Bubbletea runtime loop.

| Span Name | Description | Fields |
|-----------|-------------|--------|
| `ftui.program.init` | Model initialization | `model_type`, `cmd_count` |
| `ftui.program.update` | Single update cycle | `msg_type`, `duration_us`, `cmd_type` |
| `ftui.program.view` | View rendering | `duration_us`, `widget_count` |
| `ftui.program.subscriptions` | Subscription management | `active_count`, `started`, `stopped` |

### 3.2 Render Pipeline Events

Spans for the render kernel (buffer, diff, presenter).

| Span Name | Description | Fields |
|-----------|-------------|--------|
| `ftui.render.frame` | Complete frame cycle | `width`, `height`, `duration_us` |
| `ftui.render.diff` | Buffer diff computation | `changes_count`, `rows_skipped`, `duration_us` |
| `ftui.render.present` | ANSI emission | `bytes_written`, `runs_count`, `duration_us` |
| `ftui.render.flush` | Output flush | `duration_us`, `sync_mode` |
| `ftui.reflow.apply` | Resize application outcome | `width`, `height`, `debounce_ms`, `latency_ms`, `rate_hz` |
| `ftui.reflow.placeholder` | Resize placeholder shown | `width`, `height`, `rate_hz` |

### 3.3 Decision Events

Point-in-time events for auditable decisions.

| Event Name | Description | Fields |
|------------|-------------|--------|
| `ftui.decision.degradation` | Degradation level change | `degradation_level`, `reason_code`, `budget_remaining` |
| `ftui.decision.fallback` | Capability fallback | `capability`, `fallback_to`, `reason_code` |
| `ftui.decision.resize` | Resize handling decision | `strategy`, `debounce_active`, `coalesced`, `same_size`, `width`, `height`, `rate_hz` |
| `ftui.decision.screen_mode` | Screen mode selection | `mode`, `ui_height`, `anchor` |

#### 3.3.1 Runtime Mode Contract Extensions

For the runtime-performance lane, degradation and fallback events are not complete
unless they also explain the user-visible service mode:

- `ftui.decision.degradation`
  - must carry `runtime_mode`, `runtime_mode_before`,
    `runtime_mode_after`, `pressure_class`,
    `degradation_level`, `queue_depth`, `queue_capacity`,
    `queue_high_water`, `coalescing_state`, `coalesced_count`,
    `deferred_count`,
    `dropped_count`, `reason_code`, `strict_guarantees`,
    `degraded_behaviors`, `recovery_target`, `signal_surface`, and
    `work_disposition`; if it closes a degraded interval, it must also carry
    `recovery_completed`, `recovery_latency_ms`, and `degraded_interval_ms`
- `ftui.decision.fallback`
  - must carry `runtime_mode`, `rollback_required`, `operator_action`, and the
    `reason_code`; if the fallback changes active service quality, it must also
    carry `pressure_class`, any active `degradation_level`, and
    `work_disposition`

Recovery does not need a separate OTEL event name if it is represented as a
`ftui.decision.degradation` transition back toward `healthy`, but the closing
transition must still report recovery latency and degraded-interval duration.

### 3.4 Input Events

Spans for input processing (redacted by default).

| Span Name | Description | Fields |
|-----------|-------------|--------|
| `ftui.input.event` | Input event processing | `event_type` (no content!) |
| `ftui.input.macro` | Macro playback | `macro_id`, `event_count` |

---

### 3.5 Accessibility Events

Emitted on the `ftui.a11y` target only when `ProgramConfig::accessibility`
is set (`with_accessibility(ScreenReaderPolicy)`); the default config
collects no tree and emits nothing here.

| Event Name | Description | Fields |
|------------|-------------|--------|
| `ftui.a11y` (`a11y tree changed`, debug) | The frame's accessibility tree differs from the previous frame's | `frame_idx`, `nodes`, `added`, `removed`, `changed`, `focus_changed`, `announcements`, `dropped` |
| `ftui.a11y` (`screen reader announcement`, info) | One bounded screen-reader announcement derived from the tree diff | `frame_idx`, `node_id`, `urgency` (`polite` \| `assertive`), `reason`, `text` |

`text` is the announcement string built from the node's role, name,
description and state (bounded by `ScreenReaderPolicy::max_text_chars`).
It can carry user-visible widget text (input values are masked by the
widget when `TextInput` is in password mode); treat it like the
`ftui.input.event` payload under the redaction policy in Section 5 when
forwarding it beyond the local evidence sink.

## 4) Field Schema

### 4.1 Common Fields (All Spans)

These fields are attached to every span:

```
service.name      string   - From OTEL_SERVICE_NAME or "ftui-runtime"
service.version   string   - FrankenTUI version
telemetry.sdk     string   - "ftui-telemetry"
host.arch         string   - Target architecture
process.pid       int      - Process ID
```

### 4.2 Duration Fields

All duration fields use microseconds (us) as the unit for precision:

```
duration_us       u64      - Elapsed time in microseconds
```

### 4.3 Decision Evidence Fields

Decision events include structured evidence:

```
decision.rule      string   - Rule/heuristic applied
decision.inputs    string   - JSON-serialized input state (redacted)
decision.action    string   - Chosen action
decision.confidence f32     - Confidence score (0.0-1.0) if applicable
```

---

## 5) Redaction Policy

### 5.1 Principles

1. **Conservative by default**: Err on the side of not emitting.
2. **No PII**: Never emit user input content, file paths, or secrets.
3. **Structural only**: Emit types and counts, not values.
4. **Opt-in detail**: Verbose fields require explicit configuration.

### 5.2 Never Emit (Hard Redaction)

The following MUST never appear in telemetry:

| Category | Examples |
|----------|----------|
| **User input content** | Key characters, text buffer contents, passwords |
| **File paths** | Log files, config paths, temp files |
| **Environment variables** | Beyond OTEL_* and FTUI_* prefixes |
| **Memory addresses** | Pointer values, buffer addresses |
| **Process arguments** | Command-line arguments |
| **User identifiers** | Usernames, home directories |

### 5.3 Conditionally Emit (Soft Redaction)

These are omitted by default but can be enabled via `FTUI_TELEMETRY_VERBOSE=true`:

| Category | When Enabled |
|----------|--------------|
| **Widget types** | Full widget type names |
| **Message types** | Model::Message enum variants |
| **Command types** | Cmd enum variants |
| **Capability details** | Full terminal capability report |

### 5.4 Always Emit (No Redaction)

These are considered safe for all environments:

| Category | Examples |
|----------|----------|
| **Counts** | Widget count, change count, event count |
| **Durations** | All timing measurements |
| **Dimensions** | Buffer width/height, UI height |
| **Enum variants** | Screen mode, degradation level |
| **Boolean flags** | Mouse enabled, sync available |

---

## 6) User-Supplied Field Handling

### 6.1 Custom Span Attributes

Applications may attach custom attributes via tracing:

```rust
tracing::info_span!("my_operation", custom.field = "value");
```

**Policy:**
- Prefix requirement: Custom fields MUST use a namespace prefix (e.g., `app.`, `custom.`)
- No automatic redaction: Application is responsible for not emitting sensitive data
- Pass-through: Custom fields are passed to the OTEL exporter unchanged

### 6.2 Custom Events

Applications may emit custom events:

```rust
tracing::info!(target: "app.audit", action = "user_action");
```

**Policy:**
- Filtered by target: Only targets matching `app.*` or `custom.*` are exported
- Rate limiting: Custom events are subject to the same batching as built-in events
- Documentation: Applications should document their custom event schemas

---

## 7) Schema Versioning

### 7.1 Version Field

All telemetry includes a schema version:

```
ftui.schema_version   string   - Semantic version (e.g., "1.0.0")
```

### 7.2 Compatibility Rules

- **Patch versions** (1.0.x): Additive only, no breaking changes
- **Minor versions** (1.x.0): New fields, deprecated fields still emitted
- **Major versions** (x.0.0): Breaking changes, old fields may be removed

### 7.3 Current Schema Version

**Version: 1.0.0** (Initial stable schema)

---

## 8) Invariants (Alien Artifact)

1. **Redaction completeness**: No user input content escapes to telemetry.
2. **Schema stability**: Breaking changes require major version bump.
3. **Duration precision**: All durations use microseconds.
4. **Deterministic field set**: Same operation produces same field names.
5. **Bounded cardinality**: Enum-typed fields have known cardinality.

### Failure Modes

| Scenario | Behavior |
|----------|----------|
| Serialization error | Log warning, omit event |
| Field value overflow | Saturate to max value |
| Unknown field type | Stringify with `.to_string()` |
| Custom field collision | Prefix with `app.` |

---

## 9) Decision Evidence + Local Evidence Sink

For OTEL decision events, include structured evidence:

```rust
pub struct DecisionEvidence {
    /// Rule or heuristic that triggered the decision
    pub rule: String,
    /// Inputs to the decision (redacted as per policy)
    pub inputs_summary: String,
    /// Chosen action
    pub action: String,
    /// Confidence (0.0-1.0) if probabilistic
    pub confidence: Option<f32>,
    /// Alternative actions considered
    pub alternatives: Vec<String>,
    /// Brief explanation for humans
    pub explanation: String,
}
```

### 9.1 Evidence Sink Configuration (Runtime)

Deterministic decision logs are emitted via the **local JSONL evidence sink**.
This is distinct from OTEL spans and is intended for E2E capture + replay.

Configuration (see `crates/ftui-runtime/src/evidence_sink.rs`):

```rust
use ftui_runtime::{EvidenceSinkConfig, EvidenceSinkDestination, ProgramConfig};

let config = ProgramConfig::default()
    .with_evidence_sink(
        EvidenceSinkConfig::default()
            .with_enabled(true)
            .with_destination(EvidenceSinkDestination::file("evidence.jsonl"))
            .with_flush_on_write(true),
    );
```

Notes:
- Default is **disabled**.
- Destination: `Stdout` or `File(path)`.
- Flush-on-write defaults to **true** (recommended for tests).
- Resize decision JSONL requires `CoalescerConfig::with_logging(true)`.
- BOCPD JSONL requires `BocpdConfig::with_logging(true)`.

### 9.2 Evidence Sink JSONL (Current)

Each line is a single JSON object. Field sets are **event-specific**.
These are the current runtime events emitted by the evidence sink.

#### Event: `diff_decision`

Required fields:
- `run_id` (string)
- `event_idx` (u64)
- `strategy` (`Full` | `DirtyRows` | `FullRedraw`)
- `cost_full`, `cost_dirty`, `cost_redraw`
- `posterior_mean`, `posterior_variance`, `alpha`, `beta`
- `dirty_rows`, `total_rows`, `total_cells`
- `span_count`, `span_coverage_pct`, `max_span_len`
- `fallback_reason`, `scan_cost_estimate`
- `tile_used`, `tile_fallback`
- `tile_w`, `tile_h`, `tile_size`, `tiles_x`, `tiles_y`
- `dirty_tiles`, `dirty_tile_count`, `dirty_cells`, `dirty_tile_ratio`, `dirty_cell_ratio`
- `scanned_tiles`, `skipped_tiles`, `skipped_tile_count`
- `tile_scan_cells_estimate`, `sat_build_cost_est`
- `bayesian_enabled`, `dirty_rows_enabled`

Runtime defaults (`RuntimeDiffConfig`):
- `bayesian_enabled = true`
- `dirty_rows_enabled = true`
- `reset_on_resize = true`
- `reset_on_invalidation = true`

Example:

```json
{"event":"diff_decision","run_id":"diff-4242","event_idx":12,"strategy":"DirtyRows","cost_full":4800.000000,"cost_dirty":1200.000000,"cost_redraw":0.000000,"posterior_mean":0.036000,"posterior_variance":0.000340,"alpha":3.500000,"beta":92.500000,"dirty_rows":10,"total_rows":40,"total_cells":4800,"span_count":6,"span_coverage_pct":12.500000,"max_span_len":18,"fallback_reason":"none","scan_cost_estimate":600,"tile_used":true,"tile_fallback":"none","tile_w":16,"tile_h":16,"tile_size":256,"tiles_x":5,"tiles_y":3,"dirty_tiles":2,"dirty_tile_count":2,"dirty_cells":40,"dirty_tile_ratio":0.133333,"dirty_cell_ratio":0.008333,"scanned_tiles":2,"skipped_tiles":13,"skipped_tile_count":13,"tile_scan_cells_estimate":512,"sat_build_cost_est":4800,"bayesian_enabled":true,"dirty_rows_enabled":true}
```

Notes:
- `span_coverage_pct` is a 0–100 percentage of total cells covered by spans (including full rows).
- `fallback_reason` values: `none`, `no_spans`, `no_dirty_rows`, `span_overflow`, `full_strategy`, `full_redraw`.
- `scan_cost_estimate` reflects the cells scanned for posterior updates.

#### Event: `config` (resize coalescer)

Required fields:
- `steady_delay_ms`, `burst_delay_ms`, `hard_deadline_ms`
- `burst_enter_rate`, `burst_exit_rate`
- `cooldown_frames`, `rate_window_size`
- `logging_enabled`

#### Event: `decision` (resize coalescer)

Required fields:
- `idx`, `elapsed_ms`, `dt_ms`, `event_rate`
- `regime` (`steady` | `burst`)
- `action` (`apply` | `apply_forced` | `apply_immediate` | `coalesce` | `skip_same_size`)
- `pending_w`, `pending_h`, `applied_w`, `applied_h`
- `time_since_render_ms`, `coalesce_ms`, `forced`

#### Event: `decision_evidence` (resize coalescer)

Required fields:
- `log_bayes_factor` (float or `"inf"`)
- `regime_contribution`, `timing_contribution`, `rate_contribution`
- `explanation`

#### Event: `bocpd`

Schema versioned (`schema_version: "bocpd-v1"`).
Required fields:
- `p_burst`, `log_bf`, `obs_ms`, `regime`
- `ll_steady`, `ll_burst`
- `runlen_mean`, `runlen_var`, `runlen_mode`, `runlen_p95`, `runlen_tail`
- `delay_ms` (nullable), `forced_deadline` (nullable)
- `n_obs`

Example:

```json
{"schema_version":"bocpd-v1","event":"bocpd","p_burst":0.7321,"log_bf":1.204,"obs_ms":18.0,"regime":"burst","ll_steady":0.001234,"ll_burst":0.056789,"runlen_mean":12.4,"runlen_var":9.100,"runlen_mode":9,"runlen_p95":21,"runlen_tail":0.0420,"delay_ms":40,"forced_deadline":false,"n_obs":64}
```

#### Event: `allocation_budget_config`

Required fields:
- `alpha`, `mu_0`, `sigma_sq`, `cusum_k`, `cusum_h`
- `lambda`, `window_size`

#### Event: `allocation_budget_evidence`

Required fields:
- `frame`, `x`, `residual`
- `cusum_plus`, `cusum_minus`, `e_value`
- `alert` (bool)

#### Event: `a11y_tree`

Written once per rendered frame whose accessibility tree differs from the
previous frame's (requires `ProgramConfig::accessibility`).

Required fields:
- `frame_idx` (u64)
- `nodes` (usize, node count of the new tree)
- `added`, `removed`, `changed` (usize, diff counts)
- `focus` (u64 node id of the newly focused node, or `null`)
- `announcements` (usize, announcements kept), `dropped` (usize, cut by the policy cap)

Example:

```json
{"event":"a11y_tree","frame_idx":9,"nodes":27,"added":24,"removed":5,"changed":3,"focus":null,"announcements":1,"dropped":0}
```

#### Event: `a11y_announcement`

One row per screen-reader announcement derived from the frame's tree diff,
written right after that frame's `a11y_tree` row, assertive before polite.

Required fields:
- `frame_idx` (u64)
- `node_id` (u64 or `null`)
- `urgency` (`polite` | `assertive`)
- `reason` (`FocusChanged` | `LiveRegionAdded` | `LiveContentChanged` | `LiveRegionChanged`)
- `text` (string, JSON-escaped, bounded by `ScreenReaderPolicy::max_text_chars`)

Example:

```json
{"event":"a11y_announcement","frame_idx":9,"node_id":12185758060053980160,"urgency":"polite","reason":"FocusChanged","text":"textInput: Password. password input. focused"}
```

### 9.3 Runtime Mode Contract Additions

The current evidence sink already captures low-level decisions. For `bd-8vstx`
and downstream runtime-performance work, the following additions are mandatory
for any scenario that enters `stressed`, `degraded`, or `recovered` mode.

#### Event: `mode_transition`

Required fields:
- `run_id`
- `event_idx`
- `runtime_mode`
- `runtime_mode_before` / `runtime_mode_after`
  - `healthy`, `stressed`, `degraded`, `recovered`
- `pressure_class`
  - `steady_state`, `input_backpressure`, `mixed_workload`, `shutdown_pressure`, `capability_fallback`
- `degradation_level`
- `queue_depth`
- `queue_capacity`
- `queue_high_water`
- `coalescing_state`
- `coalesced_count`
- `deferred_count`
- `dropped_count`
- `reason_code`
- `strict_guarantees`
- `degraded_behaviors`
- `recovery_target`
- `signal_surface`
- `work_disposition`

When `mode_transition` closes a degraded interval, it must also include:
- `recovery_completed`
- `recovery_latency_ms`
- `degraded_interval_ms`

#### Event: `recovery_complete`

Required fields:
- all fields required by `mode_transition` when it closes a degraded interval
- `pending_work_drained`

These additions may be emitted via OTEL events, the local evidence sink, or both.
Once the runtime-performance lane adopts them, missing mode/recovery evidence is
a contract failure rather than optional telemetry.

---

## 10) Implementation Notes

### 10.1 Span Attributes

Use `tracing::Span::record()` for dynamic fields:

```rust
let span = tracing::info_span!("ftui.render.frame", width = ?width, height = ?height);
let _guard = span.enter();
// ... render ...
span.record("duration_us", elapsed.as_micros() as u64);
```

### 10.2 Redaction Helper

Implement a redaction utility for consistent handling:

```rust
pub fn redact_path(path: &Path) -> &'static str {
    "[redacted:path]"
}

pub fn redact_content(content: &str) -> &'static str {
    "[redacted:content]"
}

pub fn summarize_count(items: &[T]) -> String {
    format!("{} items", items.len())
}
```

### 10.3 Verbose Mode

Check `FTUI_TELEMETRY_VERBOSE` for conditional fields:

```rust
fn is_verbose() -> bool {
    std::env::var("FTUI_TELEMETRY_VERBOSE")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}
```

---

## 11) Tests

### Unit Tests

- Redaction functions return placeholder strings
- Schema version field is present
- Duration fields are u64 microseconds
- Custom field prefixing works correctly

### Property Tests

- No user input content appears in any telemetry output
- Field names are ASCII lowercase with dots
- All enum variants have known cardinality

### E2E Tests

- Capture OTEL export and verify schema compliance
- Verify redaction in verbose and non-verbose modes
- Check schema version in exported spans
- Drive input-flood and mixed-workload scenarios and verify `mode_transition` / recovery evidence contains strict-guarantee and work-disposition fields

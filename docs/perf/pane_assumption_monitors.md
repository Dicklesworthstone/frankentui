# Pane Latency-Envelope & Assumption Monitors (bd-1pvzq.2)

> Status: **implemented · deterministic · CI-enforced**
>
> Module: `crates/ftui-layout/src/pane_monitors.rs`
> Gates: `crates/ftui-layout/tests/pane_monitor_gates.rs`
> Runs under `cargo test --workspace --all-features` and
> `cargo clippy --workspace --all-targets -- -D warnings`.

A single scalar perf budget can't protect the advanced pane strategies, because
each rests on an *operating assumption* that fails in its own way. These monitors
turn every assumption into an explicit, structured, operator-readable verdict, so
a CI gate or a local triage session can say **which assumption failed, on which
scenario, under which strategy** — instead of leaving a regression as a
mysterious slowdown.

## The five monitors

| Assumption | What it guards | Healthy / Degraded / Violated |
|------------|----------------|-------------------------------|
| `ReplayDepthBound` | Checkpointed `replay()` walks ≤ one checkpoint interval | `replay_depth/interval` ≤ 1 / > 1 / > 2 |
| `RetentionPressure` | Bounded retention can hold the working set | `WithinBudget`·`PrunedToFit<90%` / `ConservativeHold`·`>90%` / `FloorReached` |
| `SelectorChurn` | Hysteresis keeps the chosen strategy stable | switch-rate ≤ 25% / > 25% / > 50% |
| `FallbackFrequency` | The workload isn't constantly forced to the safe path | conservative-fallback-rate ≤ 50% / > 50% / > 80% |
| `LatencyEnvelope` | Per-op latency stays within the strategy budget | `observed/envelope` ≤ 0.8 / > 0.8 / > 1.0 |

Thresholds live in `PaneMonitorThresholds` (with sensible `Default`s tuned to
flag ≈2× blowups and sustained thrash, not normal jitter) so a caller can tighten
them per environment.

## Verdicts and reports

Each monitor returns a `PaneMonitorVerdict`:

```rust
pub struct PaneMonitorVerdict {
    pub assumption: PaneAssumption,
    pub strategy: PaneMemoryStrategy,
    pub status: PaneMonitorStatus,   // Healthy | Degraded | Violated
    pub observed: f64,               // ratio / percentage / ns-per-op
    pub budget: f64,                 // value at which it becomes a violation
    pub headroom_pct: f64,           // remaining headroom (negative once over)
    pub explanation: String,         // operator-readable (AC4)
}
```

The `explanation` is plain language aimed at an operator, e.g.

> Checkpointed replay walks 40 step(s), 2.5x the 16-step checkpoint interval
> (checkpoint_hit=false) — undo/redo will feel sluggish; the checkpoint-spacing
> assumption is violated.

`PaneMonitorReport` aggregates verdicts for a scenario and exposes:

- `worst_status()` / `has_violations()` — the **CI fail condition**.
- `violations()` — iterate the failed assumptions.
- `to_json()` — deterministic single-object **structured log** (byte-identical
  for identical telemetry, safe to attach to CI artifacts).
- `summary_log()` — operator-facing one-line-per-verdict summary.

## Inputs (real telemetry)

The monitors read the telemetry the substrates already emit — no new
instrumentation:

| Monitor | Input |
|---------|-------|
| `monitor_replay_depth` | `PaneInteractionTimeline::replay_diagnostics()` |
| `monitor_retention_pressure` | `apply_retention_to_{version_store,timeline}(…)` decision |
| `monitor_selector_churn` / `monitor_fallback_frequency` | a slice of `PaneExecutionDecision` (successive `select`/`reselect`) |
| `monitor_latency_envelope` | observed ns/op (e.g. `replay.ns_per_iteration` from the bd-1pvzq.1 replay artifact) vs a per-strategy envelope |

## CI fail criteria

`crates/ftui-layout/tests/pane_monitor_gates.rs` drives the monitors with telemetry
from the actual substrates and the selector, asserting:

- a representative **healthy** resize session produces **no violations**, and
- pathological regimes — a coarse-checkpoint replay blowup, an impossible
  retention budget (`FloorReached`), and a thrashing selector — are each flagged
  as a **violation** with the right assumption and an operator-readable message.

If a change makes a healthy session start violating an assumption, the suite
fails; if a genuinely degraded regime stops being flagged, the pathological cases
fail.

## Reuse by the rest of the lane

- **bd-1pvzq.1** supplies the `replay.ns_per_iteration` / `allocation` /
  `retention` telemetry the latency and retention monitors consume.
- **bd-1pvzq.5** (E2E soak / rollback) uses `report.has_violations()` as a
  programmatic rollback trigger and `summary_log()` / `to_json()` as the
  operator-grade soak log.

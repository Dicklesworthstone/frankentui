# Diff Strategy Decision Contract (bd-3jlw5.6)

Formal specification for the Bayesian diff strategy selector's behavior
in each operating regime. All decisions are recorded in the
`DiffEvidenceLedger` for post-hoc audit.

## State Machine

```
                  change_fraction > 0.5
  StableFrame ──────────────────────────── BurstyChange
      │  ▲                                     │  ▲
      │  │ 3 consecutive low-change frames     │  │
      │  └─────────────────────────────────────┘  │
      │                                           │
      │  resize event          resize event       │
      │  ┌────────────┐        ┌────────────┐     │
      ▼  │            ▼        ▼            │     │
      ResizeRegime ────────────────────────────────┘
          │ 1 frame, return to previous regime
          │
      latency > 10ms
      ┌────────────┐
      ▼            │
  DegradedTerminal ┘
      │
      │ latency < 5ms
      ▼
  StableFrame (or previous non-degraded regime)
```

## Regime Specifications

### StableFrame

Applies when >90% of recent frames have <5% cell change rate.

| Property | Value |
|----------|-------|
| Primary strategy | `DirtyRows` (scan only dirty rows) |
| Confidence threshold | 0.7 |
| Fallback | `Full` diff if confidence < 0.7 |
| Evidence condition | `change_fraction < 0.05` for last N frames |
| SLA target | p99 < 500us |
| Beta prior | alpha=1.0, beta=19.0 (prior expectation: 5% change) |

**Entry conditions:**
- Default regime on startup
- 3 consecutive frames with change_fraction < 0.05 after BurstyChange
- Latency drop below 5ms after DegradedTerminal

**Exit conditions:**
- change_fraction > 0.5 for current frame -> BurstyChange
- Terminal resize event -> ResizeRegime
- Terminal write latency > 10ms -> DegradedTerminal

### BurstyChange

Applies when a sudden large change is detected (e.g., page scroll, tab switch).

| Property | Value |
|----------|-------|
| Primary strategy | `Full` (full row-major scan) |
| Trigger | `change_fraction > 0.5` for current frame |
| Recovery | Return to StableFrame after 3 low-change frames |
| SLA target | p99 < 2ms |

**Entry conditions:**
- change_fraction > 0.5 detected in current frame
- Posterior probability of high-change regime > 0.7

**Exit conditions:**
- 3 consecutive frames with change_fraction < 0.05 -> StableFrame
- Terminal resize event -> ResizeRegime

### ResizeRegime

Applies for exactly 1 frame after a terminal resize event.

| Property | Value |
|----------|-------|
| Primary strategy | `FullRedraw` (skip diff, emit all cells) |
| Trigger | Terminal size changed |
| Duration | 1 frame |
| SLA target | p99 < 5ms |

**Entry conditions:**
- Terminal size change detected

**Exit conditions:**
- After 1 frame, return to previous regime (StableFrame or BurstyChange)

### DegradedTerminal

Applies when the terminal is slow or high-latency.

| Property | Value |
|----------|-------|
| Primary strategy | `DirtyRows` with significance filter |
| Trigger | Measured terminal write latency > 10ms |
| Significance filter | Skip cells that only changed attributes (not content) |
| Recovery | Return to normal when latency < 5ms |
| SLA target | Best effort (prioritize perceived responsiveness) |

**Entry conditions:**
- Terminal write latency exceeds 10ms

**Exit conditions:**
- Terminal write latency drops below 5ms -> StableFrame

## Decision Recording

Every decision is recorded in the `DiffEvidenceLedger` ring buffer:

```json
{
  "type": "diff_decision",
  "frame": 42,
  "regime": "stable_frame",
  "strategy": "DirtyRows",
  "confidence": 0.850000,
  "fallback": false,
  "posterior_mean": 0.030000,
  "posterior_var": 0.000500,
  "cost_full": 1.2000,
  "cost_dirty": 0.3000,
  "cost_redraw": 2.4000,
  "alpha": 3.0000,
  "beta": 57.0000,
  "obs": [
    {"m": "change_fraction", "v": 0.030000, "c": 0.400000},
    {"m": "dirty_rows", "v": 2.000000, "c": 0.150000}
  ]
}
```

Regime transitions are recorded separately:

```json
{
  "type": "regime_transition",
  "frame": 100,
  "from": "stable_frame",
  "to": "bursty_change",
  "trigger": "confidence=0.600 strategy=Full",
  "confidence": 0.600000
}
```

## Invariants

1. **Determinism**: Same sequence of observations produces same regime transitions
2. **Monotonic confidence**: Confidence increases with consistent evidence
3. **Hysteresis**: Regime switches require sustained evidence (no single-frame flapping)
4. **Fallback safety**: Fallback strategy always produces correct output (never skips cells)
5. **Ring buffer**: Last 10K decisions always available for audit

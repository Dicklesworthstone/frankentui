# Pane Execution-Policy Selector (bd-1k7ek.6)

> Status: **selector and live engine implemented; adoption validation in progress**
>
> Library: `crates/ftui-layout/src/pane_execution.rs`
> Builds on: persistence spike (bd-1k7ek.5), retention policy (bd-25wj7.2),
> memory telemetry (bd-25wj7.1)
> Tests: `pane_execution::tests`, `tests/pane_persistent_equivalence.rs`,
> and the terminal/browser host tests. Prototype telemetry:
> `benches/pane_memory_telemetry.rs`.

## Why

The selector compares a no-history baseline, checkpointed history, and persistent
versions. The live `PaneExecutionEngine` supports the two history substrates and
a conservative canonical execution mode. Differential tests compare operation
outcomes, errors, complete snapshots, and retained history; the no-history
baseline cannot supply undo/redo and is rejected by the live engine.

Per the bead's explicit intent, this is **not opaque adaptive magic**: selection
is a pure deterministic function of an observed profile and documented thresholds,
every decision is explained, and a conservative fallback is always one call away.

## Selection

`PaneExecutionPolicy::select(profile)` is a pure function of a
`PaneWorkloadProfile` (operation count, local-op fraction, peak ops/sec, whether
history is required) and explicit thresholds. The decision tree, in order:

| Condition | Strategy | Reason |
|-----------|----------|--------|
| `forced_strategy` set | that strategy | `forced_override` |
| `conservative` set | checkpointed | `conservative_fallback` |
| `!history_required` | baseline | `no_history_required` |
| deep **and** resize-dominated **and** bursty | persistent | `resize_dominated_burst` |
| otherwise | **checkpointed** | `general_default` |

The checkpointed timeline is the **conservative default and fallback** — it is the
certified production path and the differential oracle for the persistent spike, so
it is chosen whenever the persistent criteria are not *all* met.

Default thresholds are explicit and tunable. They reflect the persistence
prototype and memory telemetry, which excluded live conversion and maintenance;
they are not validated adoption thresholds for the live engine:

| Threshold | Default | Rationale |
|-----------|---------|-----------|
| `persistent_min_operations` | 64 | the spike's bounded-window depth |
| `persistent_local_fraction_pct` | 80 | resize-dominated (`SetSplitRatio` is the `Local` family) |
| `persistent_burst_ops_per_sec` | 60 | a live drag-resize burst |
| `hysteresis_pct` | 10 | anti-thrash margin (see below) |

## Overrides & fallback (rollback ergonomics)

- `PaneExecutionPolicy::forcing(strategy)` forces any specific strategy — for
  A/B testing, debugging a single substrate, or staged rollout.
- `PaneExecutionPolicy::conservative()` forces the certified checkpointed path —
  the one-call rollback switch.

Both bypass the adaptive logic entirely and are reported as `forced` in the
decision, so an operator always knows when a choice was imposed vs inferred.

## Behavior comparison

`strategy_choice_never_diverges_behavior` compares final hashes for its selector
corpus. The live engine tests additionally compare complete operation payloads,
rejected edits, allocator IDs, coalesced gestures, undo/redo, migration with a
retained redo tail, and retention under pressure. These are bounded regression
corpora, not a universal equivalence or performance proof.

## Anti-thrash (hysteresis)

`reselect(profile, previous)` adds hysteresis so the selector does not flip-flop
when the workload jitters near a threshold:

- **Entering** persistent requires the local fraction to clear the threshold *by
  the margin* (≥ 90% with defaults).
- **Leaving** persistent requires either a failed hard gate (depth/burst) or the
  local fraction to drop *below* the threshold by the margin (< 70% with
  defaults).
- The history requirement is a hard functional flag (no hysteresis) — entering or
  leaving the baseline path is immediate.

`hysteresis_prevents_thrashing_near_threshold` proves a profile oscillating in the
70–90% local band holds its strategy, while a decisive move (≥ 90% / < 70%)
switches. Overrides ignore hysteresis.

## Decision traces & evidence

Every decision carries a `PaneStrategyReason` and a human-readable `log`, and the
whole `PaneExecutionDecision` is `serde::Serialize` for manifests. From
`cargo bench -p ftui-layout --bench pane_memory_telemetry`:

```
selector/execution[persistent] resize_dominated_burst: ops=512 local=100% burst=240/s history=true
  (thresholds: min_ops=64 local>=80% burst>=60/s hysteresis=10%); retention budget bytes=476917 units=0
selector/execution[checkpointed] general_default: ops=384 local=48% burst=240/s history=true
  (thresholds: min_ops=64 local>=80% burst>=60/s hysteresis=10%); retention budget bytes=219626 units=0
```

In that prototype telemetry, the selector chose persistent for the resize storm
and checkpointed for the mixed session. Moving a persistent root cursor is O(1);
live navigation also flattens and validates a canonical tree, so its full cost is
larger. Prototype structural-sharing results likewise do not include the live
journal and render projection. The decision carries a retention budget for the
selected substrate.

## For downstream beads

- **bd-1pvzq.2/.4 (latency monitors / determinism matrix):** the
  `PaneExecutionDecision` records are the observable signal to assert deterministic
  selection and surface rollback events.
- **bd-1pvzq.3/.5 (replay-oracle gates / E2E soak):** the no-divergence property is
  the certification hook; the decision logs feed operator-grade soak diagnostics.
- **Live integration (G47):** Layout Lab and the WASM `RunnerCore` now use
  `PaneExecutionEngine` through the shared `PaneHistory` interaction adapter.
  Actual PTY/browser execution and the complete rendering cost remain separate
  adoption evidence; native host tests and prototype benchmarks do not establish them.

## Live engine contract

The engine owns one journal and the selected history substrate. Persistent
`SetSplitRatio` operations execute through path copying and produce a validated
canonical tree for rendering. Structural edits execute the canonical operation
once. Conservative mode uses the full-validation canonical operation path.
`status()` counts actual applies and transitions by execution mode.

Call `observe(tree, sample)` after each successful operation, supplying measured
execution-plus-recording latency and a monotonic timestamp. The shared host
adapter does this using `web_time::Instant`. An observation failure leaves the
accepted edit accepted and records `last_maintenance_error`. Samples exclude
subsequent maintenance; performance comparisons must include that work separately
or time the whole caller boundary.

Migration rebuilds every retained journal entry, including redo, and checks the
complete snapshot at the current cursor before publication. Import validates
checkpoints and journal hashes. `begin_gesture` pins history until `end_gesture`;
hosts restore the gesture-start cursor before ending a cancellation. Layout Lab
defers cancellation requested from a read-only hidden-view render until its next
mutable event.

Live retention counts edits, with one additional baseline version in a persistent
store. The engine prunes both representations together and includes the journal,
store, and live canonical projection in its modeled byte total. These totals are
structural estimates, not allocator measurements. Protected redo and the current
state survive an impossible budget; the pressure is reported and selects
conservative execution. Default retention is 4096 edits with no byte ceiling.
The constructor currently forces checkpointed execution. Adaptive and persistent
policies are explicit opt-ins: the full-wrapper diagnostic comparison exposed
regressions on unbounded histories, so prototype timings do not justify automatic
adoption by default.

The default measured-operation envelope is 8 ms, configurable through
`set_latency_envelope_ns`; zero disables that monitor. A violation latches
conservative execution until an explicit `set_policy` reset. The live engine
gives an operator's conservative override precedence over forced persistent
selection. `set_policy` stages migration and retention atomically; during a
gesture it defers execution-mode changes until the gesture ends.

The WASM `ShowcaseRunner` exposes `paneExecutionStrategy()` for the active
substrate and `paneExecutionStatusJson()` for counters and diagnostics.
`paneSetExecutionPolicy(mode, maxRetainedBytes, maxRetainedEdits)` accepts
`0=checkpointed`, `1=persistent`, `2=conservative`, or `3=adaptive`; each ceiling
uses zero for unbounded. Arguments must be finite integer JavaScript numbers in
the unsigned 32-bit range. Invalid arguments and requests while a pane pointer is
active throw without changing state. Finish the gesture, or call
`panePointerCancel` and handle its capture command, before changing policy.
The status JSON uses Rust integer counters, which can exceed JavaScript's safe
integer range.

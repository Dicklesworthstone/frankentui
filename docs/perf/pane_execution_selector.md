# Pane Execution-Policy Selector (bd-1k7ek.6)

> Status: **specified · implemented · regression-tested · evidence captured**
>
> Library: `crates/ftui-layout/src/pane_execution.rs`
> Builds on: persistence spike (bd-1k7ek.5), retention policy (bd-25wj7.2),
> memory telemetry (bd-25wj7.1)
> Tests: `pane_execution::tests` (10) · Evidence: `benches/pane_memory_telemetry.rs`

## Why

Three semantically-**equivalent** ways to drive pane undo/redo now exist —
**baseline** (no history), the **checkpointed** `PaneInteractionTimeline`, and the
**persistent** `PaneVersionStore` — proven byte-identical over lockstep histories
(`tests/pane_persistent_equivalence.rs`). Once equivalent implementations exist,
the system should pick the cheapest *safe* one for the observed workload rather
than hard-coding one globally.

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

Default thresholds (explicit and tunable on the policy), derived from the
persistence spike and memory telemetry — the persistent store earns its keep only
on resize storms deep enough that O(1) navigation and structural sharing pay off:

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

## No behavior divergence

Because the candidate strategies are **proven equivalent**, selecting among them
changes cost, never observable behavior. The test
`strategy_choice_never_diverges_behavior` re-asserts this at the selector boundary:
the same operation stream driven through all three substrates yields an identical
final state hash.

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

The selector routes the **resize storm** (100% local) to the persistent store —
where the memory telemetry shows O(1) navigation and ~70% structural sharing — and
falls the **mixed session** (48% local) back to the checkpointed default. The
decision carries the retention budget that the bounded-retention policy
(bd-25wj7.2) applies to the selected substrate.

## For downstream beads

- **bd-1pvzq.2/.4 (latency monitors / determinism matrix):** the
  `PaneExecutionDecision` records are the observable signal to assert deterministic
  selection and surface rollback events.
- **bd-1pvzq.3/.5 (replay-oracle gates / E2E soak):** the no-divergence property is
  the certification hook; the decision logs feed operator-grade soak diagnostics.
- **Live integration:** a future execution engine holding all three substrates
  calls `select`/`reselect` per window and `apply_retention_to_*` on the chosen
  substrate. The persistent store remains a prototype on no production path today.

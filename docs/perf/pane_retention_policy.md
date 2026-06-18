# Pane Retention & Pruning Policy (bd-25wj7.2)

> Status: **specified · implemented · regression-tested · evidence captured**
>
> Library: `crates/ftui-layout/src/pane_retention.rs`
> Substrate hooks: `PaneVersionStore::set_max_versions`, `PaneInteractionTimeline::set_max_entries`
> Telemetry baseline: [`pane_memory_telemetry.md`](pane_memory_telemetry.md) (bd-25wj7.1)
> Tests: `pane_retention::tests` (8) · Evidence bench: `benches/pane_memory_telemetry.rs`

## Why

The asymptotic replay-speed work turns pane history into *retained state*:
persistent versions, operation-log entries, and checkpoint snapshots. Unbounded,
that state grows without limit — the [memory telemetry](pane_memory_telemetry.md)
shows the persistent store at **3.3×** the checkpointed timeline and the timeline
at **24–37×** the live tree. Architectural speedups are only operationally safe
with an **explicit, deterministic, observable** memory ceiling. This policy is
that guardrail: "memory bounded by policy, not by hope."

## The policy

`PaneRetentionPolicy` is an explicit ceiling on two axes (`0` = unbounded):

| Axis | Field | Meaning |
|------|-------|---------|
| Bytes | `max_retained_bytes` | modeled-byte budget (same byte model as `pane_memory`) |
| Units | `max_retained_units` | hard cap on versions (store) / entries (timeline) |

Application (`apply_to_version_store`, `apply_to_timeline`) is a pure function of
the policy and current retained state:

1. Install the **unit cap** (if set), pruning oldest history.
2. Prune oldest history one unit at a time until the **byte budget** is met —
   always keeping the newest unit.

The newest unit is never pruned, so **the current state is never discarded**.
Every application returns a `PaneRetentionDecision`: a serializable telemetry
record (strategy, budget, units/bytes before-after, units pruned, outcome) plus a
human-readable `log` line, and it carries the **preserved current-state hash** as
proof that only history — never the live state — was dropped.

### Retained-state classes capped

| Substrate | Retained-state classes | Pruning mechanism |
|-----------|------------------------|-------------------|
| Persistent store | `Arc`-shared versions | drop oldest version roots (newest kept) |
| Checkpointed timeline | op-log entries + checkpoint snapshots | advance replay baseline, re-base checkpoints |

There is no separate standalone "cache" today; the timeline's checkpoint
snapshots *are* the replay cache and are pruned with the entries. (A persistent
positional index is future work — `bd-25wj7.3`.)

## Determinism & fallback

Two fallbacks are explicit and observable via `PaneRetentionOutcome`:

| Outcome | When | Behavior |
|---------|------|----------|
| `WithinBudget` | already under budget | no-op |
| `PrunedToFit` | over budget, prunable | pruned oldest to fit |
| `ConservativeHold` | over budget **and** `conservative_debug` | held everything; nothing discarded |
| `FloorReached` | even one retained unit exceeds the byte budget | pruned to the single live unit; never below |

**Conservative debugging** (`PaneRetentionPolicy::conservative()`) disables
pruning entirely so an operator can inspect full history; the decision still
reports what *would* be discarded. **Floor** is the deterministic safety stop:
the timeline's irreducible cost is its baseline snapshot (pruning entries advances
the baseline), so a byte budget below that reports `FloorReached` rather than
sacrificing the head.

## Memory before/after evidence

From `cargo bench -p ftui-layout --bench pane_memory_telemetry` — a representative
byte budget applied to each substrate (versions = ¼ of unbounded footprint,
entries = ½). `head_hash` is **identical before and after**, proving state
preservation:

### `resize_storm` (512 ops)

| Substrate | units before→after | bytes before→after | budget | outcome |
|-----------|--------------------|--------------------|--------|---------|
| persistent store | 513 → 125 (−388) | 1 907 668 → 475 084 | 476 917 | `pruned_to_fit` |
| checkpointed timeline | 512 → 240 (−272) | 575 636 → 278 272 | 287 818 | `pruned_to_fit` |

### `mixed_session` (384 ops)

| Substrate | units before→after | bytes before→after | budget | outcome |
|-----------|--------------------|--------------------|--------|---------|
| persistent store | 385 → 89 (−296) | 878 506 → 209 203 | 219 626 | `pruned_to_fit` |
| checkpointed timeline | 384 → 160 (−224) | 294 513 → 140 544 | 147 256 | `pruned_to_fit` |

Both substrates land **under budget** while preserving the head state (same
`head_hash` for persistent and checkpointed within a scenario, since they
represent the same final tree — a built-in cross-check).

## Tests (`pane_retention::tests`)

`within_budget_is_a_noop`, `unit_budget_caps_versions_and_preserves_head`,
`byte_budget_prunes_to_fit_and_preserves_head`,
`conservative_debug_holds_over_budget_state`, `floor_is_never_breached`,
`timeline_policy_prunes_entries_and_preserves_head`, `decisions_are_deterministic`,
`unbounded_policy_never_prunes` — covering pruning edge cases, deterministic budget
enforcement, conservative/floor fallback, and state-hash preservation under
retention pressure.

## For downstream beads

- **`bd-1k7ek.6` (execution-policy selector):** a selected strategy
  (baseline/checkpointed/persistent) carries a `PaneRetentionPolicy`; the selector
  calls `apply_to_*` after recording to keep memory bounded.
- **`bd-1pvzq.2/.4` (latency-envelope monitors / determinism matrix):** the
  `PaneRetentionDecision` records are the observable signal to assert bounded
  growth and deterministic pruning in CI.

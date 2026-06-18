# Pane Soak + Rollback E2E (bd-1pvzq.5)

> Status: **implemented · deterministic · CI-enforced**
>
> Driver: `crates/ftui-layout/tests/pane_soak_rollback.rs`
> Runner: `scripts/pane_soak_rollback.sh`
> Runs under `cargo test --workspace --all-features` and the
> `pane-perf-artifacts` CI job.

Unit and integration tests prove the optimized pane strategies are *correct*.
This layer proves the user-visible *workflow* is **safe under sustained
interaction** and that, when an operating assumption breaks, the engine **rolls
back to the conservative path** and keeps producing the correct state — emitting
operator-grade JSONL the whole way.

## The scenario

A deterministic soak (seeded SplitMix64):

1. Many rounds of resize interaction drive a checkpointed timeline **and** a
   persistent version store in lockstep (the optimized path), against a canonical
   conservative oracle.
2. Each round the bd-1pvzq.2 assumption monitors are evaluated on real telemetry
   (replay depth, retention pressure, selector churn, fallback frequency,
   latency envelope).
3. At a designated **pressure round** a retention spike (a tiny budget standing
   in for accumulated memory pressure) makes the retention monitor *violate*. The
   controller treats `PaneMonitorReport::has_violations()` as the **rollback
   trigger** and switches to the conservative (checkpointed) strategy with a safe
   budget.
4. Post-rollback rounds run conservative and the **violation clears** (an
   elevated *fallback-frequency* reading is the expected degraded mode, not a
   failure).

## Asserted invariants (CI-deterministic)

No wall-clock is used in assertions, so the driver is reproducible:

- a rollback fires **exactly** at the pressure round,
- the **final state hash equals the canonical baseline** — behavior is preserved
  *across* the rollback (the rollback changes representation/strategy, not the
  resulting state); checkpointed `replay()` and the persistent store agree too,
- every post-rollback round **clears the violation** (recovery is real),
- the emitted JSONL is well-formed and carries the rollback + summary events.

A companion test (`pane_soak_without_pressure_never_rolls_back`) proves the
rollback is driven by *real* violations, not fired spuriously.

## Operator-grade JSONL

The driver writes one JSON object per line to `$PANE_SOAK_LOG` (default
`target/pane-soak/pane_soak_rollback.jsonl`):

| event | fields |
|-------|--------|
| `pane_soak_round` | `round`, `strategy`, `reason`, `replay_depth`, `checkpoint_interval`, `retention_outcome`, `monitor_worst`, `violations`, `rolled_back`, `state_hash`, `ops_applied` |
| `pane_soak_rollback` | `round`, `from_strategy`, `to_strategy`, `trigger`, `monitor_summary` (the operator-readable verdict text), `state_hash` |
| `pane_soak_summary` | `rounds`, `rollbacks`, `rollback_round`, `final_strategy`, `final_state_hash`, `canonical_state_hash`, `replay_state_hash`, `store_state_hash`, `certified`, `seed` |

The log shows, at a glance, the **active strategy**, **fallback/rollback
events**, **timings/telemetry**, **state hashes**, and the final **semantic
outcome** — exactly what AC3 requires to diagnose a timing regression, semantic
drift, or bad fallback behavior without reproducing from scratch.

## Running it

```bash
# Default soak (12 rounds, pressure at round 6):
./scripts/pane_soak_rollback.sh

# Tune the soak:
./scripts/pane_soak_rollback.sh --rounds 20 --pressure-round 8 --seed 42

# Add terminal+web drag/resize smoke coverage:
./scripts/pane_soak_rollback.sh --with-smoke

# CI-style fixed location:
./scripts/pane_soak_rollback.sh --out-dir target/pane-soak/ci
```

The runner:

- runs the soak driver (rch-aware), capturing `driver.log`,
- validates the JSONL contract (round/rollback/summary events present, exactly
  one rollback, `certified == true`),
- optionally runs `scripts/pane_e2e.sh --mode smoke` for cross-host drag/resize,
- writes `manifest.json` (config + verdict + the summary event + a repro
  command),
- on failure, writes a **self-contained** `failure/` bundle (logs + manifest +
  `failure.txt` with the exact repro command) for local triage and CI upload.

## Relationship to the rest of the lane

This is the workflow-level capstone of bd-1pvzq:

- it **reuses** the bd-1pvzq.2 monitors as the rollback trigger,
- its state-hash certification mirrors the bd-1pvzq.3 golden oracle,
- the bundle is uploaded alongside the bd-1pvzq.1 replay artifacts,
- and its substrate equivalence rests on the bd-1pvzq.4 determinism matrix.

Together they let a release prove both *"the strategies agree"* and *"the
user-visible workflow stays safe and recovers"* before pane optimizations roll
out (bd-2dy6m → bd-a46q1 → bd-1w0w4).

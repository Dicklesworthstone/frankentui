# Pane Soak / Stress / Chaos — Flake Triage Workflow

This document describes the long-run reliability suite for the pane split-tree
engine (bd-a46q1.6) and the process for triaging and quarantining any
intermittent failure it surfaces.

## What it is

`crates/ftui-layout/tests/pane_soak_stress.rs` exercises the pane engine for
*reliability over time and under load*, complementing the bounded single-pass
property check in `pane_invariant_fuzz.rs`:

- **Soak** — prolonged random-but-valid operation streams (default 1,500 ops,
  memory-bounded) asserting structural + layout invariants at every step, plus a
  full-journal replay to an identical state hash and snapshot.
- **Stress** — viewport resize storms (thousands of rapid `solve_layout` calls
  with a non-drifting probe), repeated split/close churn, and deep/wide tree
  construction.
- **Chaos** — transaction rollback as a true no-op, invalid-operation rejection
  without mutation, and idempotent recovery (`repair_safe`) over valid state —
  the core-level analogues of host interruption events.

## How to run

```bash
# Default (fast, ~1-2s) — part of `cargo test -p ftui-layout`.
cargo test -p ftui-layout --test pane_soak_stress

# Scaled long run (CI nightly / pre-release).
PANE_SOAK_STEPS=20000 PANE_STRESS_RESIZES=20000 PANE_STRESS_CHURN=8000 \
PANE_STRESS_DEPTH=256 PANE_STRESS_WIDTH=512 \
cargo test -p ftui-layout --test pane_soak_stress -- --nocapture

# Via the pane E2E runner (cross-host harness; soak runs in full/stress modes).
./scripts/pane_e2e.sh --mode full
./scripts/pane_e2e.sh --mode stress --stress-iterations 5
```

### Knobs

| Env var | Default | Scenario |
|---|---|---|
| `PANE_SOAK_STEPS` | 1500 | soak stream length |
| `PANE_SOAK_SEED` | `0xA46A1` | soak primary seed |
| `PANE_SOAK_MAX_LEAVES` | 24 | soak memory bound |
| `PANE_STRESS_RESIZES` | 3000 | resize-storm solve count |
| `PANE_STRESS_CHURN` | 1000 | split/close churn cycles |
| `PANE_STRESS_DEPTH` | 48 | deep-tree depth |
| `PANE_STRESS_WIDTH` | 128 | wide-tree leaf count |
| `PANE_SOAK_ARTIFACT_DIR` | _(unset)_ | when set, persist evidence + repro bundles |

## Pass / fail criteria

A run **passes** iff, for every scenario and every step:

1. `PaneTree::validate()` succeeds and `invariant_report()` has no errors,
2. `solve_layout` is deterministic and yields in-bounds, positive-size rects for
   every leaf,
3. the soak journal replays to a byte-identical snapshot and state hash
   (determinism),
4. rejected/invalid operations leave the tree's state hash unchanged, and
   rolled-back transactions are exact no-ops,
5. the soak tree stays within its configured leaf bound (no unbounded growth).

Any violation **fails** the run. There is no "warn" tier — these are hard
invariants.

## Reproducibility (the foundation of triage)

Every scenario is driven by a **fixed-seed deterministic LCG** with no
wall-clock, floating-point, or `any::<u64>()` randomness. Therefore:

- A failure is **byte-for-byte reproducible from the seed alone**.
- On the first invariant violation the soak loop writes a **minimized repro
  bundle** (when `PANE_SOAK_ARTIFACT_DIR` is set) and panics with a message that
  contains the seed, the failing step, the exact violation, and a ready-to-paste
  reproduction command:

  ```
  reproduce: PANE_SOAK_SEED=<seed> cargo test -p ftui-layout \
      --test pane_soak_stress <scenario>
  ```

### Artifact bundle format

When `PANE_SOAK_ARTIFACT_DIR` is set:

| File | Contents |
|---|---|
| `evidence_<scenario>_seed<seed>.json` | success evidence: `{scenario, seed, steps, final_hash, status}` (schema `pane-soak-evidence-v1`) |
| `repro_<scenario>_seed<seed>.json` | failure repro metadata: `{scenario, seed, failing_step, applied_ops, message}` (schema `pane-soak-repro-v1`) |
| `repro_<scenario>_seed<seed>.journal` | the full ordered operation journal up to (and including) the failing step |

The journal + seed are a complete, deterministic replay of the failure — no
ad-hoc rerun or bisection is required.

## Triage & quarantine process

1. **Reproduce from the seed first.** Run the printed `reproduce:` command. A
   genuine engine bug reproduces deterministically every time.
2. **If it does NOT reproduce deterministically**, the bug is in the *test
   environment*, not the engine — a real non-determinism source (parallelism,
   shared mutable state, env leakage). Do not quarantine; file a bug against the
   harness and fix the non-determinism. The suite is designed to have none, so
   this is itself a defect.
3. **If it reproduces**, you have a minimal deterministic repro (seed +
   journal). File a `br` bug with:
   - the seed, scenario, failing step, and violation message,
   - the attached `repro_*.json` + `repro_*.journal` artifacts,
   - the offending `PaneOperation` (last line of the journal).
4. **Quarantine only with a tracking bead.** If a failing scenario must be
   temporarily de-gated to unblock CI, mark the specific test `#[ignore =
   "bd-XXXXX: <one-line reason>"]` — never delete it, never broaden the ignore.
   The bead owns the fix; the ignore is removed in the same PR that closes it.
5. **Ownership.** Soak/stress/chaos failures are owned by the pane-validation
   track (`bd-a46q1`). Cross-host parity failures route to the parity runner
   (`pane_cross_host_parity.rs`, bd-a46q1.5); pure core-engine failures route to
   pane-core (`bd-1qkzq`).

## Relationship to the rest of the validation pyramid

- `pane_invariant_fuzz.rs` — bounded single-pass property check (fast gate).
- `pane_soak_stress.rs` — **this suite**: long-run reliability + stress + chaos.
- `pane_cross_host_parity.rs` — terminal↔web semantic parity (bd-a46q1.5).
- `scripts/pane_e2e.sh` — orchestrates terminal PTY + web + parity + soak with
  structured JSONL evidence and per-step artifact bundles.

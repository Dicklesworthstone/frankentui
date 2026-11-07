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

- The generated operation sequence is **byte-for-byte reproducible from the
  seed alone**.
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

The journal + seed reconstruct the operation sequence. For failures involving
rendering or scheduling, also retain the relevant settings and execution
conditions described below.

## Triage & quarantine process

1. **Reproduce from the seed first.** Run the printed `reproduce:` command.
   Keep the operation history fixed while recording execution and rendering
   inputs; a scheduling-dependent engine defect may still need a controlled
   interleaving to reproduce.
2. **If it does NOT reproduce deterministically**, investigate the engine and
   its inputs before attributing the failure to the test environment. Shared
   mutable state, clocks, environment-dependent settings, and incomplete
   checkpoint restoration can all produce intermittent failures. Use a
   controlled schedule to distinguish changed inputs from different outputs
   for identical inputs. Do not quarantine or weaken the failing assertion.
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

## Showcase frame and ANSI parity (#115)

The full Layout Lab parity trace compares checkpointed, persistent,
conservative, and adaptive execution through real input, undo/redo,
mid-history migration, branch replacement, retention, and rollback. It is the
rendering evidence cited by `bd-g00-root-epic-ewths.44.2`. Keep its state-hash,
snapshot, journal, cursor, frame-checksum, ANSI-byte, and execution-counter
assertions together: passing the state comparisons does not establish frame
equivalence.

The [kernel plan](../planning/plan-to-create-frankentui-codex.md) requires the
same **model state, size, and theme** to produce the same frame. Layout Lab
resolves the process-wide theme repeatedly while rendering, and its style
helpers also read accessibility settings. Theme setters synchronize with
`ScopedThemeLock`; ordinary color-token reads do not acquire that lock.
Therefore, another showcase test can change colors or attributes between
equivalent policy lanes, or within a single unguarded frame, without changing
the pane tree or journal.

`assert_pane_showcase_parity` pins these inputs with the existing
`ScopedRenderLock` **before constructing any lane**, and holds that guard
through the entire trace and presentation. Only users of the shared theme and
accessibility state contend for these locks; the Rust suite keeps its normal
parallel execution. Presenter capabilities and viewport dimensions remain
explicit. Mismatches retain the original checksum assertion and include the
first differing cell and pinned input settings, followed by exact cell and
ANSI comparisons.

The focused regressions distinguish separate questions:

- `pane_render_interference_from_concurrent_theme_changes` uses acknowledged
  channel messages to change the theme and large-text setting from another
  thread while equivalent, off-head migrated pane states remain unchanged.
  It checks that cell content/geometry and semantic state match, that changed
  rendering inputs alter styles/checksums/ANSI, and that restoring the inputs
  restores the exact original frame and bytes. There are no sleeps or
  scheduler-dependent success conditions.
- `pane_showcase_parity_with_concurrent_theme_writers` starts competing
  full-trace workers with different pinned themes and retains all original
  assertions, including migration, pruning, rollback, and subsequent input.
- `pane_render_and_ansi_parity_survives_latency_fallback` injects a latency
  violation into one engine and a non-violating observation into its control.
  It proves that conservative fallback really occurs, that persistent and
  conservative execution remain semantically and visually equivalent with
  diagnostic overlays hidden, and that real edit/undo/redo input still works.

Run the focused regressions and vary the libtest thread count when checking
the full showcase suite. Use the pinned toolchain and the repository's DSR
verification workflow; these are the underlying Cargo invocations:

```bash
cargo test -p ftui-demo-showcase --lib --locked pane_render_ -- --nocapture
cargo test -p ftui-demo-showcase --lib --locked pane_showcase_parity_with_concurrent_theme_writers
cargo test -p ftui-demo-showcase --lib --locked -- --exact \
  screens::layout_lab::tests::pane_full_showcase_render_and_ansi_present_parity_across_policies_and_rollback
cargo test -p ftui-demo-showcase --lib --locked -- --test-threads=1
cargo test -p ftui-demo-showcase --lib --locked -- --test-threads=16
cargo test -p ftui-demo-showcase --lib --locked -- --test-threads=64
```

Record available CPUs separately from the requested test-thread count. A
64-thread run on a smaller host exercises concurrency but does not reproduce
the reporter's 64-core hardware. A passing focused run alone does not establish
that the original full-suite scheduling failure has been reproduced or fixed
on that hardware.

## Relationship to the rest of the validation pyramid

- `pane_invariant_fuzz.rs` — bounded single-pass property check (fast gate).
- `pane_soak_stress.rs` — **this suite**: long-run reliability + stress + chaos.
- `pane_cross_host_parity.rs` — terminal↔web semantic parity (bd-a46q1.5).
- `scripts/pane_e2e.sh` — orchestrates terminal PTY + web + parity + soak with
  structured JSONL evidence and per-step artifact bundles.

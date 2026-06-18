# Pane Golden Replay-Oracle & Differential Certification (bd-1pvzq.3)

> Status: **implemented · CI-enforced**
>
> Golden: `scripts/pane_replay_golden.json`
> Tool: `scripts/pane_replay_artifacts.py certify` (+ `selftest`)
> Proof: `crates/ftui-layout/tests/pane_determinism_matrix.rs` (bd-1pvzq.4)
> CI: `.github/workflows/ci.yml` → `pane-perf-artifacts`

Checkpointing, local fast paths, persistence, and policy selection each increase
the risk of subtle semantic drift. A perf gate that only checks timing can pass
green while the optimized pane execution quietly stops reproducing the correct
state. This layer makes that impossible: every perf-gate run carries
**behavior-preservation evidence** alongside the timing data.

## Two pieces of evidence

1. **Golden replay-oracle** — `scripts/pane_replay_golden.json` pins the
   deterministic replay state-hashes the `pane_profile_harness` produces for each
   scenario (`baseline_hash`, `final_hash`). The harness verifies `replay() ==
   applied tree` every iteration, so these are replay-certified hashes. A drift
   means the optimized execution no longer reproduces the certified state — a
   **behavior** regression, not a timing one.

   `aggregate_hash` is intentionally **not** pinned: it XOR-mixes over iterations
   and so depends on `--iterations` (full vs `--test` mode). `baseline_hash` and
   `final_hash` are iteration-independent and stable.

2. **Differential proof** — the bd-1pvzq.4 determinism matrix
   (`pane_determinism_matrix`) proves the four execution substrates (adaptive
   baseline, conservative oracle, checkpointed replay, persistent versions) plus
   the policy-selected path are observationally identical, with a structured
   first-divergence report. The certification **consumes that matrix result**
   rather than inventing a weaker ad-hoc check (bd-1pvzq.3 AC2).

## The certification artifact

`pane_replay_artifacts.py certify` compares a run's `replay_artifact_index.json`
against the golden and writes `differential_certification.json` into the bundle:

```json
{
  "schema": "ftui.pane.differential_certification",
  "scenario": "timeline-ratios-32x2000",
  "classification": "certified",
  "golden_oracle": {
    "matched": true,
    "checks": [{"hash": "baseline_hash", "expected": …, "actual": …, "match": true}, …],
    "first_mismatch": null
  },
  "differential_matrix": {
    "test": "pane_determinism_matrix",
    "substrates": ["baseline", "conservative", "checkpointed_replay", "persistent", "policy_selected"],
    "passed": true
  },
  "timing": {"ns_per_iteration": 507158},
  "summary": "…operator-readable sentence…"
}
```

### Classification (AC4 — tell the failure modes apart)

| `classification` | Meaning | Exit (with `--require-match`) |
|------------------|---------|-------------------------------|
| `certified` | replay hashes match golden **and** the differential matrix passed | 0 |
| `semantic_drift` | a replay hash changed — the optimized execution reproduces a *different* state | 1 |
| `differential_matrix_failed` | hashes match golden but the strategies disagree (matrix failed) | 1 |
| `scenario_not_in_golden` | the golden does not cover this scenario | 1 |

Each carries an operator-readable `summary` that distinguishes **semantic drift**
from a **timing/budget regression** (handled by `bench_budget.sh` /
`perf_regression_gate.sh`) from an **assumption/retention violation** (handled by
the bd-1pvzq.2 monitors). The failure report names the scenario, the first
mismatching hash with expected-vs-actual, the certified substrates, and the
ns/op — replay-friendly enough to reproduce locally (AC3).

## Where it runs

- **Locally**, `scripts/pane_profile.sh` runs `certify --require-match` against
  the committed golden after emitting the bundle (the matrix result is recorded
  as run-separately). A golden drift fails the profiling run loudly.
- **In CI**, the `pane-perf-artifacts` job runs the `pane_determinism_matrix`
  differential proof, then `certify --differential-matrix-passed true
  --require-match`. Reaching the certify step means the matrix passed, so the
  differential half is recorded as passed; a golden drift fails the gate. The
  `differential_certification.json` is uploaded with the bundle.

## Updating the golden

After an **intended** behavior change, regenerate the golden from a trusted run:

```bash
./scripts/pane_profile.sh --test
python3 scripts/pane_replay_artifacts.py certify \
  --index target/pane-profiling/bd-1y0ph/replay_artifact_index.json \
  --golden scripts/pane_replay_golden.json --update-golden
```

Review the diff to `scripts/pane_replay_golden.json` like any other golden — a
changed hash there is a recorded, reviewed behavior change, not an accident.

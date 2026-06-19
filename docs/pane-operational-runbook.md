# Pane Workspace Operational Runbook

Operator-grade incident response for the pane workspace in production and CI.
Every playbook is grounded in a concrete diagnostic artifact and a concrete
remediation. Companion: the
[release gate policy](pane-release-gate-policy.md) and the
[evidence manifest](spec/pane-release-evidence-manifest.md).

> **Golden lever:** when the adaptive pane execution path misbehaves, force the
> **conservative certified checkpointed path** —
> `PaneExecutionPolicy::adaptive(retention).conservative()` (reason
> `PaneStrategyReason::ConservativeFallback`). It is the path the soak/rollback
> driver falls back to and the one the golden oracle certifies. When in doubt,
> roll back to conservative; behavior is preserved across the rollback (proven
> by `pane_soak_rollback`).

---

## Diagnostic artifacts at a glance

| Symptom domain | Artifact | Produced by |
|----------------|----------|-------------|
| Behavioral / perf drift | `differential_certification.json` (`classification`) | `scripts/pane_replay_artifacts.py certify` |
| Replay/symbolization provenance | `replay_artifact_index.json` | `scripts/pane_profile.sh` + `pane_replay_artifacts.py` |
| Sustained-load safety | `pane_soak_rollback.jsonl` + manifest | `scripts/pane_soak_rollback.sh` |
| Cross-host divergence | parity first-divergence JSON diff | `pane_cross_host_parity` test |
| Aggregate ship decision | `pane_release_gate.json` | `scripts/pane_release_gate.py` |
| Whole-feature evidence | `pane_release_evidence.json` | `scripts/pane_release_evidence.py` |
| Assumption health | `PaneMonitorReport` (in soak JSONL) | the five `monitor_*` functions |

---

## Incident 1 — Pane assumption violation (latency / churn / retention / fallback / replay-depth)

**Signal.** A `monitor_*` function reports a non-`Ok` `PaneMonitorStatus` (e.g.
`monitor_latency_envelope`, `monitor_selector_churn`,
`monitor_retention_pressure`, `monitor_fallback_frequency`,
`monitor_replay_depth`); in CI it surfaces in the soak JSONL as a violation
round.

**Triage.**
1. Identify *which* `PaneAssumption` fired and the round it fired on (soak JSONL
   `assumption` + round fields).
2. Latency envelope → compare against the `monitor_latency_envelope`
   `PaneMonitorThresholds` (and the perf gate's configured present-time
   thresholds in `ci.yml`); is this a real regression or a noisy host?
3. Churn/fallback frequency → the execution selector is thrashing; check whether
   the workload regime changed (resize storm vs mixed).

**Remediation.**
- Immediate: pin the certified checkpointed path with
  `PaneExecutionPolicy::adaptive(retention).conservative()` — stops adaptation.
- Then: file/triage a perf bead; re-baseline only with evidence, never by
  loosening a threshold silently (that requires a logged override per the
  [gate policy](pane-release-gate-policy.md#override-process)).

---

## Incident 2 — Soak / rollback failure

**Signal.** `pane_soak_rollback.sh` fails, or the JSONL shows the rollback did
**not** fire at the pressure round, or behavior was **not** preserved across the
rollback.

**Triage.**
1. Read `pane_soak_rollback.jsonl`: each line carries the round, the active
   strategy, any assumption violation, the rollback event, and the state hash.
2. Confirm the rollback round equals the configured `--pressure-round`.
3. Compare the pre- and post-rollback state hashes — they must match
   (behavior-preserving rollback).

**Remediation.**
- Rollback didn't fire → the monitor wiring or threshold regressed; bisect
  against the last green soak run.
- Behavior diverged across rollback → a real correctness bug in the conservative
  path; this blocks release. Reproduce locally with
  `./scripts/pane_soak_rollback.sh --rounds 20 --pressure-round 8` and minimize.

---

## Incident 3 — Cross-host parity divergence

**Signal.** `pane_cross_host_parity` fails and emits a first-divergence JSON diff
(op / hash / cursor / strategy).

**Triage.**
1. Read the diff: which operation index first diverged, and on which host?
2. **Terminal is canonical.** If terminal and web disagree, the terminal
   semantics are authoritative.

**Remediation.**
- Fix the **web adapter** (coordinate normalization, pointer-capture lifecycle,
  cancel-reason mapping) to match terminal — do **not** fork the model.
- If the divergence is an *intentional* host difference, it must be added to the
  runner's normalization set with a comment, not left unexplained.
- See [`docs/spec/pane-parity-contract-and-program.md`](spec/pane-parity-contract-and-program.md).

---

## Incident 4 — Persisted-workspace / snapshot recovery

**Signal.** Loading a `VersionedPaneTree` snapshot returns a
`PersistentApplyError`, or a restored workspace fails `tree.validate()`.

**Triage.**
1. Check the snapshot's schema version against `PANE_TREE_SCHEMA_VERSION`.
2. A version your build doesn't understand is *rejected*, not corrupted — that
   is the designed behavior.

**Remediation.**
- Never partially apply. On `PersistentApplyError`, log it and fall back to
  `PaneTree::singleton(..)` (a fresh default workspace); the screen must not
  crash.
- For a schema bump, follow the migration path in the
  [migration guide](migration/flex-to-pane-and-versioning.md#persisted-workspace-versioning)
  and extend the migration test corpus.

---

## Incident 5 — Release gate NO-GO

**Signal.** `pane_release_gate.py` exits non-zero; `pane_release_gate.json` lists
`blocking_failures`.

**Triage.** Read the blocking clause:
- `perf_certified` → a semantic drift (see Incident 1/2). **Not waivable** — fix
  or revert.
- `perf_runtime_artifacts` → the perf job didn't produce the replay index /
  certification; the run is incomplete, re-run the perf job.
- `correctness` / `parity` / `accessibility` / `observability` → a recorded red
  suite; jump to that suite's own CI job for the failure.
- `all_dimensions_present` → the evidence bundle is malformed; re-collect.

**Remediation.** Fix the root cause. Overrides are logged with owner + expiry +
follow-up bead (gate policy §Override). `perf_certified` is never overridden.

---

## Emergency rollback (production)

If a deployed build exhibits any of the above under live load:

1. Flip the execution policy to conservative (the `.conservative()` builder on
   `PaneExecutionPolicy`). This is the single highest-leverage, lowest-risk
   action; it pins the certified path.
2. If a specific screen is implicated and was shipped behind a flag, disable the
   pane variant for that screen (it falls back to the `Flex`/`Grid` path per the
   [migration guide](migration/flex-to-pane-and-versioning.md#rollback--fallback-during-integration)).
3. Capture the artifact bundle for the incident (soak JSONL, parity diff,
   gate JSON) before redeploying, so the regression can be reproduced.

---

## Post-release hardening backlog

Tracked follow-ups (filed at rollout close):

- **`bd-nqxa5` — Cross-job test-summary aggregation for the release gate.**
  Today the unit/e2e/parity/a11y/logging suites are recorded `declared` in the
  evidence bundle (each suite's own CI job is the real correctness gate). The GA
  `strict` gate should consume a `--test-summary` aggregated from those jobs so
  the bundle asserts every suite `green` end to end.
- **`bd-zpnp5` — Web-host facade exposure.** `ftui::pane` exposes the terminal
  keyboard adapter; the web host bindings live in `ftui-web` (not in the `ftui`
  facade, which is terminal-oriented). Decide whether a web-facing facade should
  re-export the pane web adapters.
- **`bd-77mdi` — Golden replay corpus expansion.** Broaden
  `scripts/pane_replay_golden.json` scenario coverage as new advanced workflows
  land, so the differential certification stays comprehensive.

When you close one of these, update this list and the
[gate policy](pane-release-gate-policy.md) if the GA bar changes.

---

## Quick command reference

```bash
# Reproduce a soak/rollback locally
./scripts/pane_soak_rollback.sh --rounds 20 --pressure-round 8 --out-dir target/pane-soak/local

# Re-certify perf behavior against the golden oracle
python3 scripts/pane_replay_artifacts.py certify \
  --index target/pane-profiling/ci/replay_artifact_index.json \
  --golden scripts/pane_replay_golden.json --require-match --json

# Re-render the go/no-go verdict
python3 scripts/pane_release_gate.py evaluate \
  --bundle target/pane-release/pane_release_evidence.json \
  --certification target/pane-profiling/ci/differential_certification.json \
  --mode strict --json
```

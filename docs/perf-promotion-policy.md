# Performance-Change Promotion Policy

**Canonical implementation:** `ftui-harness/src/promotion_scorecard.rs`
(`PromotionScorecard`, schema `perf-promotion-scorecard-v1`, bd-cn7eq).
This document is the reviewer-facing summary; the module is the source of
truth and the thing CI runs. If they ever disagree, the module wins and this
file has a bug.

## The question the scorecard answers

*Do we keep this optimization change — and how far do we roll it out?*
Evidence in, deterministic verdict out. Two reviewers with the same evidence
set must reach the same conclusion; disagreement means the policy table needs
a patch, not a debate.

## Tiers

| Tier | Meaning |
|------|---------|
| `local_adoption` | keep on a branch / behind an opt-in flag |
| `ci_acceptance` | merge to main behind the standing CI gates |
| `default_enablement` | the optimized path becomes the default |

Each tier gets a verdict: `go`, `go_with_review` (residual risks listed), or
`no_go` (hard blockers listed).

## Evidence categories and where they bind

Statuses: `missing`, `failing`, `degraded`, `passing`, `waived(reason)`.
Requirement levels: **R** = required (`missing`/`failing` block; `degraded`
is residual risk), **R!** = required-strict (`degraded` also blocks),
**A** = advisory (`failing` blocks; `missing`/`degraded` are residual risk),
— = not considered.

| Category | local | ci | default | waivable |
|----------|-------|----|---------|----------|
| `baseline_delta` | R | R | R! | no |
| `hotspot_movement` | — | A | A | yes |
| `proof_artifacts` | R | R | R! | no |
| `validation_matrix` (unit/property/integration/E2E/replay) | A | R | R! | no |
| `gauntlet_results` | R | R | R! | no |
| `tail_monitors` (`tail_regime_monitor` verdicts) | R | R | R! | no |
| `rollback_readiness` | — | R | R! | yes |
| `user_visible_benchmarks` | — | A | A | yes |
| `observability_quality` | A | R | R! | no |
| `robustness` (challenge fixtures + negative controls) | A | A | R! | no |

Consequences worth spelling out:

- **Missing required evidence is never promotable.** No test/replay/logging
  artifacts → `no_go` from CI acceptance upward, full stop.
- **Fast but opaque does not merge.** `observability_quality` is a hard CI
  requirement: if operators cannot debug the change when it fails, its
  numbers are irrelevant.
- **Wins must survive hostility.** Curated-suite numbers with failing or
  missing `robustness` evidence stop before `default_enablement`; failing
  robustness blocks CI acceptance too (failing evidence always blocks where
  considered).
- **Tail behavior gates broad rollout.** A hard tail-monitor failure blocks
  every tier. A tail warning caps the change at `ci_acceptance` with review.
- **Waivers are bounded and visible.** Only `hotspot_movement`,
  `rollback_readiness`, and `user_visible_benchmarks` may be waived, always
  with a recorded reason, always carried as residual risk. Attempting to
  waive anything else is itself a hard blocker.

## The average-vs-tail tradeoff (decided by rule, not vibes)

`ValueRule` (deltas in permille of baseline, negative = improvement):

- average improves ≥ 2% (`avg_delta ≤ -20‰`), **or**
- tail improves ≥ 5% (`tail_delta ≤ -50‰`) while the average regresses no
  more than 1% (`avg_delta ≤ +10‰`).

A change that meets neither has no demonstrated performance value: it cannot
reach `default_enablement`, and keeping it at `ci_acceptance` requires a
reviewed non-performance justification (simplification, correctness, etc.).

## Traceability

Every `CategoryEvidence` row carries `ledger_refs` — perf-evidence-ledger
entry ids (`perf-evidence-ledger-v1`, bd-rw97d). A verdict is navigable:
scorecard → category → ledger entry → artifact + replay command. Tail-monitor
verdicts (`tail-regime-monitor-v1`, bd-zzfhe) convert directly into evidence
statuses (`Pass→passing`, `Warn→degraded`, `HardFail→failing`).

## Using it

```rust
use ftui_harness::promotion_scorecard::*;

let decision = PromotionScorecard::default().evaluate(&evidence);
println!("{}", decision.to_json());          // machine-readable, deterministic
match decision.highest_admissible {
    Some(PromotionTier::DefaultEnablement) => { /* flip the default */ }
    Some(tier) => { /* stop at `tier`; review residual risks */ }
    None => { /* revert or fix the named hard blockers */ }
}
```

Downstream consumers: the rollout drill scripts (bd-lilcl) and the
perf-rollout CI gates (bd-cpwfc) consume `PromotionDecision::to_json()`
directly.

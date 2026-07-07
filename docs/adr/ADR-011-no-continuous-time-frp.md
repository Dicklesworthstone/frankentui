# ADR-011: No Continuous-Time FRP for the Animation System

Status: **accepted (defer indefinitely)** — 2026-07-06, from graveyard §6.9
(bd-16pal, EV score 1.5, below the adoption threshold).

## Context

Continuous-Time Functional Reactive Programming (Elliott & Hudak 1997) models
time-varying values as continuous functions (`Behavior A = Time -> A`) with
discrete events as point occurrences, composed via lifting
(`lift2(f, b1, b2) = t -> f(b1(t), b2(t))`). Applied to animation, a spring is
a `Behavior<Position>` defined by the ODE
`x'' + 2·damping·x' + stiffness·x = stiffness·target`. CT-FRP would unify ALL
of FrankenTUI's time-varying values — springs, easing, transitions, stagger
cascades, attention pulses — under one algebra, replacing the current
per-animation-type implementations in `ftui-core`'s animation module.

## Decision

Do **not** adopt CT-FRP. Keep the existing direct implementations: damped
spring physics (semi-implicit Euler with dt subdivision), analytic easing
curves, stagger distributions, and sine pulses.

## Alternatives considered

- **Full CT-FRP layer**: rejected — the highest-value primitive (damped
  springs) already exists and is deterministic; the continuous-time model
  conflicts with the discrete frame-rendering loop (every Behavior would be
  sampled at frame boundaries anyway); and the abstraction cost lands on
  every animation author for marginal expressive gain.
- **Partial adoption (Behaviors for transitions only)**: rejected — a second
  animation vocabulary alongside the existing one is worse than either alone.

## Consequences

- Animation kinds stay individually implemented; adding a new kind costs a
  small module rather than fitting an algebra.
- **Reconsideration trigger** (recorded in bd-16pal): if the animation system
  grows beyond ~5 distinct animation types with duplicated time-handling
  logic, re-evaluate CT-FRP against the acceptance criteria preserved in the
  bead (Behavior/Event semantics, frame-boundary sampling efficiency, no
  quality/perf regression, complexity justified by net code reduction, JSONL
  evidence integration).

## Test plan / verification

Nothing to verify now (no code change). If the trigger fires, the bead's
structured-logging contract applies: `ct_frp_sample` / `ct_frp_compose` /
`ct_frp_benchmark` JSONL ops with error-vs-expected and ad-hoc-comparison
fields, integrated with the evidence ledger.

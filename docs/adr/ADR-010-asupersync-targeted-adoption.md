# ADR-010: Targeted Asupersync Adoption For Runtime Shell And Doctor

Status: ACCEPTED (2026-10-16 — scope: effect-executor lane via `ftui-runtime/asupersync-executor`; `doctor_frankentui` orchestration unchanged)
Date: 2026-03-12

## Context

FrankenTUI already has strong deterministic boundaries in the render kernel and terminal lifecycle:

- `ftui-render` owns the hot-path buffer, diff, and presenter machinery.
- `ftui-core::TerminalSession` and `ftui-runtime::TerminalWriter` enforce RAII cleanup, screen-mode correctness,
  and the one-writer rule.
- `ftui-runtime::Program` already contains an effect queue, subscription reconciliation, process supervision, bounded
  shutdown waits, render evidence hooks, and tick strategy integration.
- `doctor_frankentui` already acts as an orchestration-heavy operator tool with retries, external process execution,
  artifact manifests, replay support, and verification scripts.

The Asupersync migration question is therefore not "where can async exist?" but "where does structured async and
capability threading improve user-visible behavior without contaminating deterministic core code?"

The seam inventory in `docs/spec/asupersync-frankentui-seam-inventory.md` shows a narrow answer:

- Yes: runtime shell orchestration, effect execution, subscription/process lifecycle management, cancellation and
  deadline propagation, and `doctor_frankentui` workflow/evidence plumbing.
- No: render-kernel algorithms, pure text/layout/widget logic, or broad public API churn.

Without an ADR, implementers are likely to drift toward one of two bad extremes:

1. a weak wrapper that adds new internal complexity without improving responsiveness, shutdown behavior, or evidence
   quality
2. an over-broad conversion that drags Asupersync concerns into pure crates or hot paths that do not benefit from it

## Decision

We will adopt Asupersync narrowly and only at orchestration seams.

### 1. Adoption Boundary

Asupersync-backed internals are allowed in:

- `ftui-runtime` effect execution and internal executor wiring
- runtime subscription lifecycle coordination
- runtime child-process supervision, cancellation, and shutdown propagation
- `doctor_frankentui` orchestration loops for capture, retries, timeouts, artifact collection, and replay support

Asupersync-backed internals are not allowed in:

- `ftui-render` hot-path buffer/diff/presenter code
- pure `ftui-layout`, `ftui-text`, and widget rendering logic
- public facade crates merely to "thread async through everything"

### 2. Public API Guardrails

This migration must preserve the current programming model at the public boundary:

- `Model`, `Cmd`, and the `ftui` facade remain the user-facing surface
- declarative subscription identity and reconciliation semantics remain intact
- terminal/session ownership and the one-writer rule remain intact
- screen-mode behavior, replayability, and deterministic event ordering remain mandatory

Any proposal that requires public API breakage to make the migration work is out of scope for this ADR and requires a
separate explicit API-contract decision.

### 3. Internal Integration Style

The migration must refine existing runtime and doctor seams rather than create a parallel system.

Concretely:

- `ftui-runtime` should gain an internal executor seam behind current effect execution machinery
- cancellation, deadlines, and shutdown joins should become more uniform and observable
- `doctor_frankentui` should use the same capability-aware discipline for bounded waits, retries, and evidence capture
- all new orchestration paths must emit deterministic machine-readable evidence rather than ad hoc logs

### 4. Explicit Anti-Goals

The migration is rejected if it does any of the following:

- rewrites or slows the render kernel in the name of architectural purity
- injects Asupersync concerns into pure layout, text, or widget crates
- forks a second public runtime model alongside `Model`/`Cmd`
- weakens RAII cleanup, terminal safety, or one-writer enforcement
- treats shadow mode, fallback, or replay evidence as optional follow-up work

### 5. Why This Is Better Than Whole-Project Conversion

Users benefit from this narrower approach because it improves the surfaces where they actually feel pain:

- better responsiveness when background work is slow
- more predictable shutdown and child-process cleanup
- clearer failure diagnostics and replay artifacts
- safer rollout because shadow/fallback reasoning stays localized to orchestration seams

Users do not benefit from pushing async orchestration into deterministic hot paths that are already correct and tuned.
That would create churn, regression risk, and performance uncertainty without solving a concrete problem.

Operators benefit because the migration stays observable and reversible:

- feature activation can be reasoned about at the runtime/doctor boundary
- evidence schemas can compare legacy and Asupersync lanes
- rollback drills remain small and comprehensible

## Alternatives Considered

### A. Whole-project Asupersync conversion

Rejected because it would:

- contaminate pure crates with orchestration concerns
- increase regression surface in hot render and layout paths
- create API churn without corresponding user-visible gains

### B. Keep the current internals and only add thin wrappers

Rejected because it would:

- preserve today’s fragmented cancellation/deadline behavior
- fail to improve shutdown latency or failure diagnosability in meaningful ways
- make later rollout work ambiguous because the real seam would remain implicit

### C. Introduce a second runtime/effect model for Asupersync users only

Rejected because it would:

- split documentation and validation effort across two programming models
- weaken determinism and replay comparability
- create long-lived migration debt in an early-stage project that explicitly does not value compatibility shims

## Consequences

### Positive

- The migration target stays narrow, explainable, and testable.
- Runtime and doctor work can improve cancellation, deadlines, retries, and evidence quality without destabilizing the
  render path.
- Future beads have a clear contract for what they may touch and what they must preserve.
- Rollout planning can focus on feature flags, shadow runs, fallback topology, and operator evidence where those
  concerns actually belong.

### Negative

- Implementers must work within stricter boundaries instead of taking the easiest broad refactor path.
- Some desired cleanup in adjacent areas may need to wait for separate decisions rather than piggybacking on this
  migration.
- Proof obligations become stricter because the narrower scope must still demonstrate meaningful user/operator value.

## Proof Obligations

Before rollout, the migration track must prove all of the following:

1. Public runtime/model/facade contracts remain intact.
2. Terminal safety invariants remain intact: one-writer rule, screen-mode correctness, and RAII cleanup.
3. Equivalent workloads preserve deterministic output and replay identity.
4. Shutdown latency, child-process cancellation behavior, and failure diagnosability measurably improve or remain
   bounded with no regression.
5. `doctor_frankentui` artifact completeness and replay usefulness improve, rather than merely relocating complexity.
6. Shadow mode and fallback policy are explicit, testable, and operationally understandable.

## Test Plan

Architecture proof for this ADR must be traced through the following work:

- `bd-3838s`: seam inventory and migration boundary source document
- `bd-1sder`: invariants, user-visible metrics, and canonical evidence schema
- `bd-3f24s`: dependency boundaries, feature flags, shadow mode, and fallback topology
- `bd-zkalo` / `bd-2vb3o` / `bd-392ka`: internal executor seam and runtime integration
- `bd-1dccp`: `doctor_frankentui` orchestration/evidence uplift

Implementation validation must preserve and extend these repo verification surfaces:

- `cargo test -p ftui-runtime --all-targets`
- `cargo bench -p ftui-runtime --bench tick_strategy_bench`
- `cargo test -p doctor_frankentui --all-targets -- --nocapture`
- `./scripts/doctor_frankentui_happy_e2e.sh /tmp/doctor_frankentui_ci/happy`
- `./scripts/doctor_frankentui_failure_e2e.sh /tmp/doctor_frankentui_ci/failure`
- `./scripts/doctor_frankentui_determinism_soak.sh /tmp/doctor_frankentui_ci/determinism 3`
- `./scripts/doctor_frankentui_replay_triage.py --run-root /tmp/doctor_frankentui_ci/failure --max-signals 8`
- `./scripts/doctor_frankentui_coverage.sh /tmp/doctor_frankentui_ci/coverage`

## References

- Bead: `bd-2st19`
- Related beads: `bd-3838s`, `bd-1sder`, `bd-3f24s`, `bd-zkalo`, `bd-2vb3o`, `bd-392ka`, `bd-1dccp`
- `docs/spec/asupersync-frankentui-seam-inventory.md`
- `docs/adr/ADR-001-inline-mode.md`
- `docs/adr/ADR-005-one-writer-rule.md`

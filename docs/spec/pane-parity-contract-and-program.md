# Pane Parity Contract and Execution Program

Status: Draft (execution in progress)  
Owners: `ftui-demo-showcase`, `ftui-showcase-wasm`, `ftui-web`, `ftui-layout`  
Last Updated: 2026-02-16

## 1. Why This Exists

FrankenTUI's architectural intent is terminal-first: the terminal path is canonical, and host variants (WASM/web) are adapters that preserve canonical behavior while optionally adding host niceties.

A divergence currently exists:

- Advanced pane interaction semantics (organic resize, reflow move, dock preview, group semantics, inertia, timeline operations) are implemented in the WASM runner path.
- Terminal/demo rendering remains screen-local and does not consume the same pane engine semantics as the WASM path.

This creates behavioral drift risk and violates the project objective of deterministic, equivalent behavior across hosts.

## 2. Problem Statement

Current behavior indicates two semantic paths:

- WASM-specific pane interaction pipeline in `crates/ftui-showcase-wasm/src/runner_core.rs`.
- Terminal screen rendering and interaction dispatch in `crates/ftui-demo-showcase/src/app.rs` + per-screen `view()`/`update()` implementations.

The result is not a guaranteed 1:1 behavior model between terminal and web for pane interaction semantics.

## 3. Parity Contract (Normative)

These rules are mandatory:

1. Terminal is canonical for semantics.
2. Web/WASM is an adapter, not an alternate semantics engine.
3. Pane interaction semantics must have exactly one source of truth in shared code.
4. Host-specific logic is limited to:
   - event capture/lifecycle quirks (for example pointer capture in browsers),
   - host-only visual overlays (for example glow/halo),
   - transport/serialization.
5. Undo/redo/replay event logs must be deterministic and host-portable.
6. Workspace snapshot import/export must be schema-identical and semantically equivalent across hosts.
7. Any pane-related feature change must include parity tests across terminal and web adapters.

## 4. Scope

In scope:

- Pane movement, resizing (including edge/corner semantics), group operations.
- Magnetic docking, ghost preview, pressure-sensitive snap.
- Inertial settle behavior.
- Adaptive layout intelligence modes (`Focus`, `Compare`, `Monitor`, `Compact`).
- Persistent timeline, undo/redo/replay, snapshot import/export.
- Dashboard and cross-screen integration policy.

Out of scope:

- Cosmetic-only host polish that does not alter semantic state transitions.

## 5. Architectural Boundaries

### 5.1 Shared Core (must be host-agnostic)

- Pane interaction state machine and state model.
- Layout operation planning and application orchestration over `PaneTree`.
- Policy functions (snap, magnetic field, threshold hysteresis, spring blend).
- Timeline mutation and rollback semantics.
- Snapshot and replay semantics.

### 5.2 Host Adapter Layer

- Input normalization from host events into canonical pane events.
- Pointer capture lifecycle (browser).
- Terminal-specific event quirks.

### 5.3 Presentation Layer

- Rendering overlays from shared state (ghosts, halo, hints).
- Styling and animation details that do not alter semantics.

## 6. Determinism Requirements

For equivalent event traces:

- final topology hash must match,
- timeline length/cursor must match,
- operation sequence must match,
- replay result must match.

Any mismatch is a parity bug.

## 7. Dashboard and Screen Integration Policy

The dashboard is the priority integration target and must use shared pane semantics.

For all showcase screens, each must be explicitly classified:

- integrated with shared pane shell, or
- intentionally excluded with written technical rationale and future plan.

No implicit/unknown status is allowed.

## 8. Migration Program (Execution Order)

## Phase A: Contract + Shared Primitive Extraction

- Land this contract and make it authoritative.
- Extract shared pane semantic primitives from WASM-owned files into shared demo/core modules.
- Keep behavior unchanged while reducing duplication.

## Phase B: Shared Engine Consolidation

- Move pane interaction controller/state from WASM-owned runner into shared module.
- Make both terminal and WASM use the same shared controller.

## Phase C: Terminal Canonical Wiring

- Route terminal mouse/pointer/wheel/modifier events through the shared pane controller.
- Ensure timeline/undo/redo/replay and mode transitions are driven by shared controller.

## Phase D: WASM Adapter Reduction

- Retain only adapter concerns in WASM path.
- Remove duplicate semantic logic from WASM runner.

## Phase E: Screen Integration Completion

- Integrate dashboard and prioritized high-value screens.
- Produce full screen integration status matrix.

## Phase F: Proof + Enforcement

- Add parity replay/golden suites.
- Add CI guard requiring parity evidence for pane-related changes.

## 9. Acceptance Criteria for “Parity Achieved”

All of the following must be true:

1. One shared pane semantics implementation exists.
2. Terminal and web both consume it.
3. Dashboard uses shared pane semantics in runtime behavior.
4. Screen coverage status is complete and explicit (integrated or justified exclusion).
5. Deterministic parity test suite passes.
6. Snapshot portability and replay equivalence pass.
7. Documentation and contributor guidance enforce no future divergence.

## 10. Risks and Mitigations

Risk: regressions during extraction.  
Mitigation: staged migration with invariant checks and replay parity tests per phase.

Risk: hidden host event differences.  
Mitigation: strict canonical event normalization and trace comparison tooling.

Risk: partial rollout ambiguity across screens.  
Mitigation: explicit screen integration matrix with ownership and deadlines.

## 11. Implementation Notes for Future Contributors

- Prefer moving code, not re-implementing semantics in parallel.
- If a host needs a visual enhancement, derive from shared state rather than introducing alternate logic.
- If a behavior appears host-dependent, add a canonical event/adapter mapping rather than branching semantics.


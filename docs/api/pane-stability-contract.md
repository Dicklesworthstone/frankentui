# Pane Workspace API Stability Contract

> Status: pre-1.0 (workspace `0.4.x`). This document defines what the pane
> workspace public surface promises, how breaking changes are signalled, and
> what is explicitly out of scope. It is the reference for `bd-1w0w4.1`.

FrankenTUI is pre-1.0 and the broader API is still evolving, but the pane
workspace is feature-complete and validated (see
[`pane_traceability_matrix.json`](../../tests/e2e/pane_traceability_matrix.json)).
This contract carves out a **curated, supported subset** so applications can
adopt panes today and know exactly which changes will be flagged ahead of time.

---

## 1. Canonical import path

There is exactly one supported import path for application code:

```rust
use ftui::pane::prelude::*;        // day-to-day: PaneTree, PaneOperation, …
use ftui::pane::{PaneSnapTuning, PaneDockZone, PaneInteractionTimeline};
```

`ftui::pane` is a curated re-export of the pane modules that live in
`ftui-layout` (plus the terminal keyboard adapter from `ftui-runtime`). The
raw crate paths (`ftui_layout::pane::*`, `ftui::layout::pane::*`) remain
reachable for advanced or experimental use, but only the names re-exported
through `ftui::pane` are covered by this contract.

| Path | Tier | Covered by this contract |
|------|------|--------------------------|
| `ftui::pane::*` | **Stable** | Yes |
| `ftui::pane::prelude::*` | **Stable** (common subset) | Yes |
| `ftui::pane::keyboard::*` (`runtime` feature) | **Stable** (terminal host adapter) | Yes |
| `ftui::pane::advanced::*` | **Unstable** (perf/execution internals) | No — may change in any minor release |
| `ftui::layout::pane*` (raw) | **Internal** | No |

---

## 2. Stability tiers

### Tier 1 — Stable (`ftui::pane`)

The canonical pane model and the surfaces an application interacts with every
frame. Changes here are governed by the deprecation policy in §4.

- **Model & tree:** `PaneTree`, `PaneId`, `PaneIdAllocator`, `PaneLeaf`,
  `PaneSplit`, `SplitAxis`, `PaneLayout`, `PaneConstraints`, `PaneNodeKind`,
  `PaneNodeRecord`, `PaneTreeSnapshot`, `PanePlacement`, `PaneSplitRatio`.
- **Structural operations:** `PaneOperation`, `PaneOperationKind`,
  `PaneOperationFamily`, `PaneOperationOutcome`, `PaneOperationError`,
  `PaneOperationFailure`, `PaneTransaction`, `PaneTransactionOutcome`,
  `PaneResizeDirection`, `PaneResizeTarget`, `PaneResizeGrip`.
- **Semantic interaction contract:** `PaneSemanticInputEvent`,
  `PaneSemanticInputEventKind`, `PaneSemanticInputTrace`,
  `PaneDragResizeMachine`, `PaneDragResizeState`, `PaneDragResizeEffect`,
  `PaneDragResizeTransition`, `PaneInteractionTimeline` (+ entries/diagnostics),
  `PanePointerButton`, `PanePointerPosition`, `PaneInputCoordinate`,
  `PaneModifierSnapshot`.
- **Snap / dock / motion affordances:** `PaneSnapTuning`, `PaneSnapDecision`,
  `PaneSnapReason`, `PaneDockZone`, `PaneDockPreview`, `PaneInertialThrow`,
  `PanePressureSnapProfile`, `PaneMotionVector`, `PaneAffordanceMotion`,
  `PaneLayoutIntelligenceMode`, `PaneSelectionState`.
- **Coordinate normalization:** `PaneCoordinateNormalizer`,
  `PaneNormalizedCoordinate`, `PaneScaleFactor`, `PaneCoordinateRoundingPolicy`.
- **Command & accessibility vocabulary:** `PaneCommand`, `PaneCommandEffect`,
  `PaneCommandResolution`, `PaneCardinalDirection`, `PaneFocusContext`,
  `PaneFocusOrdinal`, `PaneAnnouncement`, `PaneAnnouncer`,
  `PaneAccessibilityPreferences`, and the focus helpers `focus_order`,
  `focus_cyclic`, `focus_directional`, `focus_edge`, `resolve` (re-exported as
  `resolve` here, `resolve_pane_command` at the `ftui-layout` crate root).
- **Invariants & repair:** `PaneInvariantReport`, `PaneInvariantIssue`,
  `PaneInvariantCode`, `PaneInvariantSeverity`, `PaneRepairOutcome`,
  `PaneRepairAction`.
- **Persistence:** `VersionedPaneTree`, `PaneVersionStore`,
  `PaneVersionRetention`, `PaneVersioningReport`, `PersistentApplyStrategy`,
  `PersistentApplyError`, `PersistentNode`.
- **Errors:** `PaneModelError`, `PaneSemanticInputEventError`,
  `PaneSemanticInputTraceError`, `PaneCoordinateNormalizationError`,
  `PaneDragResizeMachineError`.
- **Schema constants:** see §3.

### Tier 1 prelude (`ftui::pane::prelude`)

The minimal set most apps need on the first import:
`PaneTree`, `PaneId`, `PaneLayout`, `PaneConstraints`, `SplitAxis`,
`PaneOperation`, `PaneOperationOutcome`, `PaneTransaction`,
`PaneSemanticInputEvent`, `PaneDragResizeMachine`, `PaneCommand`,
`VersionedPaneTree`.

### Tier 2 — Keyboard host adapter (`ftui::pane::keyboard`, `runtime` feature)

The terminal-side keyboard controller: `PaneKeyboardController`,
`PaneKeyOutcome`, `PaneFocusRing`, `PaneKeyHint`, `key_to_pane_command`,
`render_pane_focus_ring`, `pane_keyboard_hints`. Stable, but only compiled
when the `runtime` feature is enabled (it depends on `ftui-runtime`). The web
host binds keys through `ftui-web::pane_keyboard`, which is host-specific and
lives in that backend crate.

### Tier 3 — Advanced / unstable (`ftui::pane::advanced`)

Performance and execution-substrate tuning: `PaneExecutionPolicy`,
`PaneMemoryStrategy`, `PaneRetentionPolicy`, the assumption monitors
(`PaneAssumption`, `PaneMonitorReport`, …) and their telemetry. These exist so
operators can profile and tune; **they are not part of the stability
guarantee** and may change shape between minor releases as the adaptive
substrate evolves. Treat the defaults as a black box unless you are explicitly
performance-tuning (see [`docs/perf/`](../perf/)).

---

## 3. Schema version constants (the serialization contract)

Anything that crosses a process/storage/host boundary carries an explicit
`u16` schema version. These constants ARE the wire contract — they are bumped
whenever the corresponding serialized shape changes incompatibly.

| Constant | Current | Governs |
|----------|---------|---------|
| `PANE_TREE_SCHEMA_VERSION` | `1` | `PaneTreeSnapshot` / persisted tree layout |
| `PANE_SEMANTIC_INPUT_EVENT_SCHEMA_VERSION` | `1` | `PaneSemanticInputEvent` payloads |
| `PANE_SEMANTIC_INPUT_TRACE_SCHEMA_VERSION` | `1` | replay traces consumed by the harness |
| `PANE_MEMORY_TELEMETRY_SCHEMA_VERSION` | `1` | perf telemetry (advanced tier) |

**Rule:** if you serialize any pane artifact, persist the schema version
alongside it and refuse to load a version you do not understand. The
deterministic replay harness, the cross-host parity runner, and
`VersionedPaneTree` all follow this rule today.

---

## 4. Pre-1.0 versioning & deprecation policy

Until `1.0`, the pane surface follows a **soft-stability** policy:

1. **Additive by default.** New variants, fields, and constructors are added
   without a major bump. Match on FrankenTUI enums non-exhaustively where the
   variant set is expected to grow.
2. **Breaking changes are signalled, never silent.** A breaking change to a
   Tier-1 type requires, in order of preference:
   - a `#[deprecated]` shim kept for at least one minor release where feasible;
   - a `CHANGELOG.md` entry under a `### Pane API` heading;
   - a bump of the relevant schema-version constant in §3 if the serialized
     shape changed.
3. **Schema versions are monotonic.** They only ever increase, and a bump is a
   hard signal that persisted artifacts need migration (§5).
4. **The `advanced` tier is exempt.** It can change in any minor release with
   only a CHANGELOG note.
5. **Post-1.0**, this document is replaced by standard semver: Tier-1 breaking
   changes require a major bump.

---

## 5. Persistence & migration guarantees

`VersionedPaneTree` + `PaneVersionStore` are the supported way to persist and
restore workspaces. The guarantees:

- A snapshot written at `PANE_TREE_SCHEMA_VERSION = N` is **loadable** by any
  build whose constant is `>= N` for the same major line, or rejected with a
  typed `PersistentApplyError` — never silently corrupted.
- `PersistentApplyStrategy` selects how an incoming snapshot is reconciled with
  the live tree (replace / merge / validate-only); the choice is explicit, not
  inferred.
- Retention (`PaneVersionRetention`) bounds history growth deterministically;
  pruning never drops the current head.
- When the schema constant bumps, the migration path for persisted workspaces
  is documented in the migration guide
  ([`docs/migration/flex-to-pane-and-versioning.md`](../migration/flex-to-pane-and-versioning.md)).

---

## 6. Cross-host parity guarantee

The same canonical pane model drives both the terminal and web hosts. For the
canonical scenario corpus, terminal and web produce **observationally
identical** results (topology hash, applied-operation sequence, settle state);
intentional host differences (pointer-capture lifecycle, cancel reason, pixel
vs cell coordinates) are normalized away. This is enforced by
`crates/ftui-web/tests/pane_cross_host_parity.rs` and specified in
[`docs/spec/pane-parity-contract-and-program.md`](../spec/pane-parity-contract-and-program.md).
Host adapters must not fork model behavior; any divergence is a bug, not an
API affordance.

### 6.1 Web host surface (bd-zpnp5 decision)

**`ftui-web` is the canonical import path for the pane web adapters. The
terminal-oriented `ftui::pane` facade deliberately does not re-export them.**

Rationale:

1. **Host symmetry over path unification.** Each host crate curates its own
   pane surface (`ftui::pane::keyboard` for the terminal, `ftui_web::pane`
   for the web); cross-host alignment is guaranteed behaviorally by §6 and
   the `pane_cross_host_parity` suite, not by sharing an import path.
2. **Independent release cadence.** The `ftui` facade's stability tiers are
   versioned for terminal consumers; folding the web adapters in would couple
   web-adapter churn to the terminal contract (and vice versa).
3. **One canonical path per host.** Web consumers (all in-tree web E2E and
   examples) already import `ftui-web` directly; adding a second valid path
   through `ftui` would invite drift between the two.

The curated web surface is `ftui_web::pane`:

| Module | Re-exports | Mirrors (terminal side) |
|--------|-----------|-------------------------|
| `ftui_web::pane::pointer` | `PanePointerCaptureAdapter`, `PanePointerCaptureConfig`, dispatch/lifecycle/log types | `PaneTerminalAdapter` (ftui-runtime) |
| `ftui_web::pane::keyboard` | `PaneWebKeyboardController`, `key_to_pane_command`, `pane_accessibility_tree`, ARIA node types | `ftui::pane::keyboard` |

These re-exports carry the same pre-1.0 deprecation policy as §4: renames go
through a deprecated alias for one minor release. The raw
`ftui_web::pane_pointer_capture` / `ftui_web::pane_keyboard` module paths
remain valid but the curated module is the documented entry point.

---

## 7. Explicitly out of scope (not covered)

- The raw `ftui_layout::pane*` module paths and any item not re-exported
  through `ftui::pane`.
- The `advanced` tier (perf/execution/memory/retention/monitors).
- Internal helper types, `pub(crate)` items, and anything `#[doc(hidden)]`.
- Floating windows beyond the split-tree model and multi-user collaborative
  editing (separate future epics, per the `bd-gajnk` epic scope).
- Exact pixel/byte output of rendering — only the semantic model and its
  documented hashes are contractual.

---

## 8. Quick reference

```rust
use ftui::pane::prelude::*;

// Build a workspace.
let tree = PaneTree::singleton("root");
debug_assert!(tree.validate().is_ok());

// Persist it across sessions.
let snapshot = VersionedPaneTree::from_pane_tree(&tree);
assert_eq!(ftui::pane::PANE_TREE_SCHEMA_VERSION, 1);
```

See [`docs/guides/pane-101.md`](../guides/pane-101.md) for a guided
introduction and [`docs/cookbook/panes.md`](../cookbook/panes.md) for recipes.
</content>
